//! Actual ExistingFileSlot -> expected replay -> existing A/C1 precedes the
//! new metadata constructor. Cache provenance never fabricates Simple facts.

use super::*;
use crate::packages::python_resolution::lock_model::*;
use crate::packages::python_resolution::lock_session::{FilePinLookup, ResolutionIntent};
use crate::packages::python_wheel::{WheelExpectation, inspect_wheel};
use crate::packages::python_wheel_source::inspect_owned_wheel;
use base64::Engine;
use pakro_index::python_metadata::{
    PythonMarkerSemantics, PythonMetadataField, PythonMetadataLimitKind, SdistDynamicPolicy,
};
use pakro_index::{SupportedWheelTags, WheelTag};
use sha2::Digest as _;
use std::fs;
use std::io::{Cursor, Write};
use std::time::Duration;

#[path = "../tests/support.rs"]
mod support;
use support::*;

fn tags() -> SupportedWheelTags {
    SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap()
}
fn metadata(name: &str, version: &str, dependencies: &[&str], extra: &str) -> String {
    let mut raw = format!("Metadata-Version: 2.6\nName: {name}\nVersion: {version}\n{extra}");
    for dependency in dependencies {
        raw.push_str(&format!("Requires-Dist: {dependency}\n"));
    }
    raw
}
fn wheel(filename: &str, name: &str, version: &str, raw: &str, corrupt_record: bool) -> Vec<u8> {
    let parsed = pakro_index::parse_wheel_filename(filename).unwrap();
    let mut headers = "Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_owned();
    if let Some(build) = parsed.build {
        headers.push_str(&format!("Build: {}\n", build.raw()));
    }
    let info = format!("{name}-{version}.dist-info");
    let mut entries = vec![
        (format!("{name}/__init__.py"), b"VALUE=1\n".to_vec()),
        (format!("{info}/WHEEL"), headers.into_bytes()),
        (format!("{info}/METADATA"), raw.as_bytes().to_vec()),
    ];
    let mut record = String::new();
    for (path, bytes) in &entries {
        record.push_str(&format!(
            "{path},sha256={},{}\n",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(bytes)),
            bytes.len()
        ));
    }
    record.push_str(&format!("{info}/RECORD,,\n"));
    if corrupt_record {
        entries[0].1[0] = b'X';
    }
    entries.push((format!("{info}/RECORD"), record.into_bytes()));
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in entries {
        zip.start_file(
            path,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn a_c1(bytes: &[u8], filename: &str, name: &str, version: &str) {
    let root = tempfile::tempdir().unwrap();
    let digest = Digest::from_sha256_bytes(bytes);
    let artifact = spool_verified_reader(
        Cursor::new(bytes),
        filename,
        digest,
        bytes.len() as u64,
        root.path(),
    )
    .unwrap();
    let supported = tags();
    inspect_owned_wheel(
        artifact,
        digest,
        &WheelExpectation {
            filename,
            name,
            version,
            supported: &supported,
        },
        Default::default(),
    )
    .expect("real A/C1 fixture prelude must be GREEN");
}
async fn replay<'lock>(
    fixture: &Fixture,
    lock: &'lock BoundPythonLock,
    file: &RegistryFileRef,
    network: NetworkPolicy,
) -> LockedRegistryArtifact<'lock> {
    let owner = acquire_locked_registry_file(slot(lock, file), fixture.context(network, &NO_ROUTE))
        .await
        .expect("real cache/old-pin replay prerequisite");
    owner.recheck_for(lock).unwrap();
    owner
}
fn ready(result: LockedMetadataResult<LockedMetadataState<'_>>) -> LockedWheelCandidate<'_> {
    match result.unwrap() {
        LockedMetadataState::Wheel(wheel) => wheel,
        LockedMetadataState::NeedsBuild(_) => panic!("wheel became fabricated build work"),
    }
}
fn good_bytes() -> Vec<u8> {
    wheel(
        FILENAME,
        "demo_pkg",
        "1.0",
        &metadata(
            "demo-pkg",
            "1.0",
            &["actual-dep>=1"],
            "Requires-Python: >=3.8\n",
        ),
        false,
    )
}
fn open_other(path: &Path) -> BoundPythonLock {
    BoundPythonLock::acquire(
        path,
        Default::default(),
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap()
}

#[tokio::test]
async fn warm_whole_wheel_metadata_requires_no_simple_facts_router_or_provider() {
    let bytes = good_bytes();
    a_c1(&bytes, FILENAME, "demo-pkg", "1.0");
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(&bytes);
    let lock = fixture.lock(&file, digest, bytes.len() as u64);
    fixture.seed_cache(FILENAME, digest, &bytes);
    let before = fs::read(&fixture.lock_path).unwrap();
    let source = replay(&fixture, &lock, &file, NetworkPolicy::Offline).await;
    let candidate = ready(route_locked_metadata(
        source,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    assert_eq!(candidate.origin(), LockedReplayOrigin::Cache);
    assert_eq!(candidate.source_key(), None);
    assert!(candidate.file_facts().is_none());
    assert_eq!(candidate.pin().file(), &file);
    assert_eq!(candidate.pin().digest(), digest);
    assert_eq!(candidate.manifest().name(), "demo-pkg");
    assert_eq!(candidate.facts().requirements()[0].name(), "actual-dep");
    assert_eq!(candidate.facts().requires_python(), Some(">=3.8"));
    candidate.recheck().unwrap();
    candidate.recheck_for(&lock).unwrap();
    assert!(peer.requests().is_empty());
    assert_eq!(fs::read(&fixture.lock_path).unwrap(), before);
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn cold_actual_source_facts_are_retained_without_an_extra_metadata_request() {
    let raw = metadata("demo-pkg", "1.0", &["actual-dep>=1"], "");
    let bytes = wheel(FILENAME, "demo_pkg", "1.0", &raw, false);
    a_c1(&bytes, FILENAME, "demo-pkg", "1.0");
    let size = bytes.len();
    let digest = Digest::from_sha512_bytes(&bytes);
    let served = bytes.clone();
    let files = Peer::new(move |_| Reply::file(&served));
    let mut entry = file_json(
        FILENAME,
        &format!("{}/signed?token=synthetic-cold-secret", files.url),
        serde_json::json!({}),
    );
    entry["core-metadata"] = serde_json::json!(true);
    let index = index(page(vec![entry]));
    let fixture = Fixture::new(&index);
    let file = fixture.file(FILENAME);
    let lock = fixture.lock(&file, digest, size as u64);
    let source = acquire_locked_registry_file(
        slot(&lock, &file),
        fixture.context(NetworkPolicy::Allowed, &ROUTE),
    )
    .await
    .unwrap();
    let key = source.source_key();
    assert!(key.is_some());
    let candidate = ready(route_locked_metadata(
        source,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    assert_eq!(candidate.origin(), LockedReplayOrigin::Registry);
    assert_eq!(candidate.source_key(), key);
    let facts = candidate.file_facts().unwrap();
    assert_eq!(facts.filename(), FILENAME);
    assert_eq!(
        facts.locator().url().query(),
        Some("token=synthetic-cold-secret")
    );
    assert!(matches!(
        facts.core_metadata(),
        pakro_index::simple::CoreMetadataAvailability::Available { .. }
    ));
    assert_eq!(candidate.manifest().core_metadata().raw(), raw.as_bytes());
    assert!(!format!("{candidate:?}").contains("synthetic-cold-secret"));
    assert_eq!(index.requests().len(), 1);
    assert_eq!(files.requests().len(), 1);
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn same_version_locked_files_do_not_share_another_files_dependency_observation() {
    let first = wheel(
        FILENAME,
        "demo_pkg",
        "1.0",
        &metadata("demo-pkg", "1.0", &["alpha==1"], ""),
        false,
    );
    let second = wheel(
        OTHER,
        "demo_pkg",
        "1.0",
        &metadata("demo-pkg", "1.0", &["beta==2"], ""),
        false,
    );
    a_c1(&first, FILENAME, "demo-pkg", "1.0");
    a_c1(&second, OTHER, "demo-pkg", "1.0");
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let other = fixture.file(OTHER);
    let limits = LockModelLimits::default();
    let first_digest = Digest::from_sha256_bytes(&first);
    let second_digest = Digest::from_sha256_bytes(&second);
    let pins = [
        ContentPin::new(
            &file,
            first_digest,
            first.len() as u64,
            PinEvidence::Advertised,
            limits,
        )
        .unwrap(),
        ContentPin::new(
            &other,
            second_digest,
            second.len() as u64,
            PinEvidence::Advertised,
            limits,
        )
        .unwrap(),
    ];
    let keys = [
        GroupKey::new("", "main", [3; 32], limits).unwrap(),
        GroupKey::new("", "dev", [3; 32], limits).unwrap(),
    ];
    let runtime = RuntimePin::new([4; 32], 1, [3; 32]).unwrap();
    let roots = [RootInput {
        declaration_index: 0,
        node: "demo-pkg",
    }];
    let first_nodes = [NodeInput {
        name: "demo-pkg",
        version: "1.0",
        source_pin: pins[0].id(),
    }];
    let second_nodes = [NodeInput {
        name: "demo-pkg",
        version: "1.0",
        source_pin: pins[1].id(),
    }];
    let graphs = [
        LockGraphInput {
            key: &keys[0],
            semantics: LockMarkerSemantics::Current2026,
            runtime,
            root_declarations: &["demo-pkg==1.0"],
            roots: &roots,
            nodes: &first_nodes,
            edges: &[],
        },
        LockGraphInput {
            key: &keys[1],
            semantics: LockMarkerSemantics::Current2026,
            runtime,
            root_declarations: &["demo-pkg==1.0"],
            roots: &roots,
            nodes: &second_nodes,
            edges: &[],
        },
    ];
    let model = PythonLockModel::from_parts(&pins, &graphs, limits).unwrap();
    fs::write(
        &fixture.lock_path,
        format!(
            "lockfile_version=4\n{}",
            model.encode_projection(limits).unwrap()
        ),
    )
    .unwrap();
    let lock = open_other(&fixture.lock_path);
    fixture.seed_cache(FILENAME, first_digest, &first);
    fixture.seed_cache(OTHER, second_digest, &second);
    let first = ready(route_locked_metadata(
        replay(&fixture, &lock, &file, NetworkPolicy::Offline).await,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    let second = ready(route_locked_metadata(
        replay(&fixture, &lock, &other, NetworkPolicy::Offline).await,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    assert_eq!(first.facts().version(), second.facts().version());
    assert_ne!(first.pin().id(), second.pin().id());
    assert_eq!(first.facts().requirements()[0].name(), "alpha");
    assert_eq!(second.facts().requirements()[0].name(), "beta");
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn actual_declarations_extras_dynamic_ranges_and_requires_python_are_unfiltered() {
    let declarations = [
        "alpha[docs]>=1; extra=='docs'",
        "never; 'one'=='two'",
        "member; python_version in '3.13'",
        "direct @ https://user:synthetic-source-secret@example.test/a;param?token=value ; os_name=='posix'",
    ];
    let raw = metadata(
        "demo-pkg",
        "1.0.0",
        &declarations,
        "Requires-Python: >=99\nProvides-Extra: docs\nDynamic: Requires-Dist\n",
    );
    let bytes = wheel(FILENAME, "demo_pkg", "1.0", &raw, false);
    a_c1(&bytes, FILENAME, "demo-pkg", "1.0");
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(&bytes);
    let lock = fixture.lock(&file, digest, bytes.len() as u64);
    fixture.seed_cache(FILENAME, digest, &bytes);
    let candidate = ready(route_locked_metadata(
        replay(&fixture, &lock, &file, NetworkPolicy::Offline).await,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    assert_eq!(candidate.facts().provided_extras(), ["docs"]);
    assert_eq!(candidate.facts().dynamic_fields(), ["requires-dist"]);
    assert_eq!(
        candidate.facts().sdist_dynamic_policy(),
        SdistDynamicPolicy::AppendMarkedMultipleUseFields
    );
    assert_eq!(candidate.facts().requires_python(), Some(">=99"));
    assert_eq!(candidate.manifest().version(), "1.0.0");
    for (item, raw) in candidate.facts().requirements().iter().zip(declarations) {
        assert_eq!(item.raw_for_semantic_resolution(), raw);
        assert_eq!(
            item.marker_semantics(),
            PythonMarkerSemantics::NeedsExactMarkerSemantics
        );
        assert_eq!(item.exact_marker().unwrap().comparisons().count(), 1);
    }
    let first = &candidate.facts().requirements()[0];
    assert_eq!(first.requested_extras().collect::<Vec<_>>(), vec!["docs"]);
    assert!(
        first
            .version_specifiers()
            .unwrap()
            .contains(&"2".parse().unwrap())
    );
    assert_eq!(
        candidate.facts().requirements()[3]
            .unresolved_reference()
            .unwrap()
            .raw_for_source_binding(),
        "https://user:synthetic-source-secret@example.test/a;param?token=value"
    );
    assert!(!format!("{candidate:?}").contains("synthetic-source-secret"));
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn inner_identity_and_explicit_tags_are_checked_against_the_old_pin_and_target() {
    for case in ["name", "version", "tags"] {
        let (name, version, filename) = match case {
            "name" => ("other", "1.0", "other-1.0-1-py3-none-any.whl"),
            "version" => ("demo_pkg", "2.0", "demo_pkg-2.0-1-py3-none-any.whl"),
            _ => ("demo_pkg", "1.0", FILENAME),
        };
        let bytes = wheel(
            filename,
            name,
            version,
            &metadata(name, version, &[], ""),
            false,
        );
        a_c1(&bytes, filename, name, version);
        let peer = Peer::new(|_| Reply::denied());
        let fixture = Fixture::new(&peer);
        let file = fixture.file(FILENAME);
        let digest = Digest::from_sha256_bytes(&bytes);
        let lock = fixture.lock(&file, digest, bytes.len() as u64);
        fixture.seed_cache(FILENAME, digest, &bytes);
        let supported = if case == "tags" {
            SupportedWheelTags::new(vec![WheelTag::new("py2", "none", "any").unwrap()]).unwrap()
        } else {
            tags()
        };
        assert!(matches!(
            route_locked_metadata(
                replay(&fixture, &lock, &file, NetworkPolicy::Offline).await,
                &supported,
                Default::default(),
                Default::default()
            )
            .unwrap_err(),
            LockedMetadataError::Archive(_)
        ));
        assert!(peer.requests().is_empty());
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn valid_outer_pin_cannot_skip_the_existing_full_record_inspection() {
    let raw = metadata("demo-pkg", "1.0", &[], "");
    let legal = wheel(FILENAME, "demo_pkg", "1.0", &raw, false);
    a_c1(&legal, FILENAME, "demo-pkg", "1.0");
    for bytes in [
        wheel(FILENAME, "demo_pkg", "1.0", &raw, true),
        PAYLOAD.to_vec(),
    ] {
        let supported = tags();
        assert!(
            inspect_wheel(
                &mut Cursor::new(&bytes),
                &WheelExpectation {
                    filename: FILENAME,
                    name: "demo-pkg",
                    version: "1.0",
                    supported: &supported
                },
                Default::default()
            )
            .is_err()
        );
        let peer = Peer::new(|_| Reply::denied());
        let fixture = Fixture::new(&peer);
        let file = fixture.file(FILENAME);
        let digest = Digest::from_sha256_bytes(&bytes);
        let lock = fixture.lock(&file, digest, bytes.len() as u64);
        fixture.seed_cache(FILENAME, digest, &bytes);
        assert!(matches!(
            route_locked_metadata(
                replay(&fixture, &lock, &file, NetworkPolicy::Offline).await,
                &tags(),
                Default::default(),
                Default::default()
            )
            .unwrap_err(),
            LockedMetadataError::Archive(_)
        ));
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn metadata_resource_and_error_boundaries_remain_typed_and_secret_safe() {
    let bytes = wheel(
        FILENAME,
        "demo_pkg",
        "1.0",
        &metadata("demo-pkg", "1.0", &["one", "two"], ""),
        false,
    );
    a_c1(&bytes, FILENAME, "demo-pkg", "1.0");
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(&bytes);
    let lock = fixture.lock(&file, digest, bytes.len() as u64);
    fixture.seed_cache(FILENAME, digest, &bytes);
    for (wl, ml, expected) in [
        (
            WheelLimits {
                max_members: 0,
                ..Default::default()
            },
            PythonMetadataLimits::default(),
            LockedMetadataError::InvalidLimits,
        ),
        (
            WheelLimits::default(),
            PythonMetadataLimits {
                max_requirements: 0,
                ..Default::default()
            },
            LockedMetadataError::InvalidLimits,
        ),
        (
            WheelLimits::default(),
            PythonMetadataLimits {
                max_requirements: 1,
                ..Default::default()
            },
            LockedMetadataError::Metadata(PythonMetadataError::LimitExceeded(
                PythonMetadataLimitKind::Requirements,
            )),
        ),
    ] {
        assert_eq!(
            route_locked_metadata(
                replay(&fixture, &lock, &file, NetworkPolicy::Offline).await,
                &tags(),
                wl,
                ml
            )
            .unwrap_err(),
            expected
        );
    }
    assert!(matches!(
        route_locked_metadata(
            replay(&fixture, &lock, &file, NetworkPolicy::Offline).await,
            &tags(),
            WheelLimits {
                max_archive_bytes: bytes.len() as u64 - 1,
                ..Default::default()
            },
            PythonMetadataLimits::default()
        )
        .unwrap_err(),
        LockedMetadataError::Archive(_)
    ));
    drop(lock);
    let bad = wheel(
        FILENAME,
        "demo_pkg",
        "1.0",
        &metadata(
            "demo-pkg",
            "1.0",
            &["bad;unknown_marker=='synthetic-marker-secret'"],
            "",
        ),
        false,
    );
    a_c1(&bad, FILENAME, "demo-pkg", "1.0");
    let digest = Digest::from_sha256_bytes(&bad);
    let lock = fixture.lock(&file, digest, bad.len() as u64);
    fixture.seed_cache(FILENAME, digest, &bad);
    let error = route_locked_metadata(
        replay(&fixture, &lock, &file, NetworkPolicy::Offline).await,
        &tags(),
        Default::default(),
        Default::default(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        LockedMetadataError::Metadata(PythonMetadataError::InvalidField {
            field: PythonMetadataField::RequiresDist,
            index: Some(0)
        })
    );
    assert!(!format!("{error:?} {error}").contains("synthetic-"));
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn a_real_existing_slot_edit_revokes_cached_metadata_without_repairing_it() {
    let bytes = good_bytes();
    a_c1(&bytes, FILENAME, "demo-pkg", "1.0");
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(&bytes);
    let lock = fixture.lock(&file, digest, bytes.len() as u64);
    let cached = fixture.seed_cache(FILENAME, digest, &bytes);
    let source = replay(&fixture, &lock, &file, NetworkPolicy::Offline).await;
    fs::write(&fixture.lock_path, "lockfile_version=4\n#external\n").unwrap();
    assert!(source.recheck().is_err());
    assert_eq!(
        route_locked_metadata(source, &tags(), Default::default(), Default::default()).unwrap_err(),
        LockedMetadataError::Revoked
    );
    assert_eq!(fs::read(cached).unwrap(), bytes);
    assert_eq!(
        fs::read_to_string(&fixture.lock_path).unwrap(),
        "lockfile_version=4\n#external\n"
    );
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn locked_metadata_is_bound_to_its_actual_project_even_when_lock_bytes_are_identical() {
    let bytes = good_bytes();
    a_c1(&bytes, FILENAME, "demo-pkg", "1.0");
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(FILENAME);
    let digest = Digest::from_sha256_bytes(&bytes);
    let lock = fixture.lock(&file, digest, bytes.len() as u64);
    fixture.seed_cache(FILENAME, digest, &bytes);
    let another = tempfile::tempdir().unwrap();
    let path = another.path().join("pakro.lock");
    fs::copy(&fixture.lock_path, &path).unwrap();
    let other = open_other(&path);
    let source = replay(&fixture, &lock, &file, NetworkPolicy::Offline).await;
    assert!(source.recheck_for(&other).is_err());
    let candidate = ready(route_locked_metadata(
        source,
        &tags(),
        Default::default(),
        Default::default(),
    ));
    candidate.recheck_for(&lock).unwrap();
    assert!(candidate.recheck_for(&other).is_err());
    assert_eq!(
        fs::read(path).unwrap(),
        fs::read(&fixture.lock_path).unwrap()
    );
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn a_locked_sdist_remains_the_original_build_continuation_without_final_metadata() {
    let filename = "demo_pkg-1.0.tar.gz";
    let bytes = include_bytes!("../../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz").to_vec();
    let peer = Peer::new(|_| Reply::denied());
    let fixture = Fixture::new(&peer);
    let file = fixture.file(filename);
    let digest = Digest::from_sha256_bytes(&bytes);
    let lock = fixture.lock(&file, digest, bytes.len() as u64);
    fixture.seed_cache(filename, digest, &bytes);
    let source = replay(&fixture, &lock, &file, NetworkPolicy::Offline).await;
    let LockedMetadataState::NeedsBuild(mut source) =
        route_locked_metadata(source, &tags(), Default::default(), Default::default()).unwrap()
    else {
        panic!("source archive became a fabricated ready wheel")
    };
    source.recheck_for(&lock).unwrap();
    assert_eq!(source.filename(), filename);
    assert_eq!(source.origin(), LockedReplayOrigin::Cache);
    assert_eq!(source.source_key(), None);
    assert_eq!(read(&mut source), bytes);
    assert!(peer.requests().is_empty());
    fixture.assert_no_authority_writes();
}

#[tokio::test]
async fn fresh_and_checked_metadata_carriers_forward_the_actual_project_scope_proof() {
    use crate::packages::python_resolution::source::fresh::acquire_fresh_registry_file;
    use crate::packages::python_resolution::source::fresh::wheel::sidecar::{
        SidecarLimits, verify_advertised_core_metadata,
    };
    use crate::packages::python_resolution::source::fresh::wheel::{
        FreshMetadataState, route_fresh_metadata,
    };
    let bytes = good_bytes();
    a_c1(&bytes, FILENAME, "demo-pkg", "1.0");
    let served = bytes.clone();
    let files = Peer::new(move |_| Reply::file(&served));
    let index = index(page(vec![file_json(
        FILENAME,
        &format!("{}/wheel", files.url),
        serde_json::json!({}),
    )]));
    let fixture = Fixture::new(&index);
    fs::write(&fixture.lock_path, "lockfile_version=4\n").unwrap();
    let lock = open_other(&fixture.lock_path);
    let another = tempfile::tempdir().unwrap();
    let other_path = another.path().join("pakro.lock");
    fs::copy(&fixture.lock_path, &other_path).unwrap();
    let other = open_other(&other_path);
    let name = NormalizedPythonName::new("demo-pkg").unwrap();
    let context = fixture.context(NetworkPolicy::Allowed, &ROUTE);
    let RegistryProjectLookup::Present(project) = fetch_registry_project(
        context.client,
        context.project_session,
        &name,
        context.policy,
        context.project_limits,
    )
    .await
    .unwrap() else {
        panic!("actual project disappeared")
    };
    let file = fixture.file(FILENAME);
    let FilePinLookup::Fresh(slot) = lock.lookup(&file, ResolutionIntent::Resolve).unwrap() else {
        panic!("actual empty lock did not grant fresh vacancy")
    };
    let context = fixture.context(NetworkPolicy::Allowed, &ROUTE);
    let source = acquire_fresh_registry_file(
        slot,
        project.select_exact_file(FILENAME).unwrap(),
        context.client,
        context.file_session,
        NetworkPolicy::Allowed,
        context.source_limits,
        context.lock_limits,
        &fixture.scratch,
    )
    .await
    .unwrap();
    source.recheck_for(&lock).unwrap();
    assert!(source.recheck_for(&other).is_err());
    let FreshMetadataState::Wheel(candidate) =
        route_fresh_metadata(source, &tags(), Default::default(), Default::default()).unwrap()
    else {
        panic!("actual wheel lost")
    };
    candidate.recheck_for(&lock).unwrap();
    assert!(candidate.recheck_for(&other).is_err());
    let context = fixture.context(NetworkPolicy::Offline, &NO_ROUTE);
    let checked = verify_advertised_core_metadata(
        candidate,
        context.client,
        context.file_session,
        NetworkPolicy::Offline,
        SidecarLimits {
            transfer: context.source_limits.transfer,
            max_spool_bytes: context.source_limits.max_spool_bytes,
            max_identity_bytes: context.source_limits.max_identity_bytes,
            deadline: context.deadline,
        },
        &fixture.root.path().join("sidecar-scratch"),
    )
    .await
    .unwrap();
    checked.recheck_for(&lock).unwrap();
    assert!(checked.recheck_for(&other).is_err());
    assert_eq!(files.requests().len(), 1);
    fixture.assert_no_authority_writes();
}
