use super::*;

use base64::Engine;
use pakro_index::{SupportedWheelTags, WheelTag};
use pakro_store::Digest;
use sha2::Sha256;
use std::fs;
use std::io::{Cursor, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use crate::artifact_snapshot::spool_verified_reader;
use crate::packages::python_environment::prefix::{EnvironmentStore, PrefixLimits};
use crate::packages::python_wheel::{WheelExpectation, WheelLimits, WheelScheme};
use crate::packages::python_wheel_install::{
    OutputKey, PreparedBytes, WheelInstallLimits, WheelInstallTarget, prepare_wheel_graph,
};
use crate::packages::python_wheel_plan::{WheelInstallScheme, WheelPlanLimits};
use crate::packages::python_wheel_source::{InspectedWheelArchive, inspect_owned_wheel};

type Entry = (String, Vec<u8>, u32);
struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    scratch: PathBuf,
    active: PathBuf,
    cas: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        assert!(root.as_os_str().as_encoded_bytes().is_ascii());
        let scratch = root.join("scratch");
        fs::create_dir(&scratch).unwrap();
        let active = root.join("active-sentinel");
        fs::write(&active, b"old active").unwrap();
        let cas = root.join("cas-sentinel");
        fs::write(&cas, b"old cas").unwrap();
        Self {
            _root: owner,
            root,
            scratch,
            active,
            cas,
        }
    }
    fn allocate(&self, limits: PrefixLimits) -> UnpublishedPrefix {
        let store = EnvironmentStore::open(
            &self.root.join("environments"),
            nix::unistd::geteuid().as_raw(),
            limits,
        )
        .expect("real DE0 store prerequisite must already be GREEN");
        let prefix = store
            .allocate()
            .expect("real DE0 allocation prerequisite must already be GREEN");
        assert!(prefix.final_path().is_dir());
        prefix.check_binding().unwrap();
        prefix
    }
    fn assert_unpublished(&self) {
        assert_eq!(fs::read(&self.active).unwrap(), b"old active");
        assert_eq!(fs::read(&self.cas).unwrap(), b"old cas");
    }
}
fn file(path: &str, bytes: &[u8]) -> Entry {
    (path.into(), bytes.to_vec(), 0o644)
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.into()
    }
}
fn scheme(prefix: &UnpublishedPrefix, flat: bool) -> WheelInstallScheme {
    let root = prefix.final_path();
    WheelInstallScheme::new(
        root.into(),
        [
            (WheelScheme::Purelib, "lib/pure"),
            (WheelScheme::Platlib, "lib/plat"),
            (WheelScheme::Headers, "include/packages"),
            (WheelScheme::Scripts, "bin"),
            (WheelScheme::Data, ""),
        ]
        .into_iter()
        .map(|(key, path)| (key, if flat { root.into() } else { root.join(path) }))
        .collect(),
    )
    .unwrap()
}
fn archive(
    fixture: &Fixture,
    name: &str,
    mut entries: Vec<Entry>,
) -> (Vec<u8>, InspectedWheelArchive) {
    let info = format!("{name}-1.0.dist-info");
    entries.push(file(
        &format!("{info}/WHEEL"),
        b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    ));
    entries.push(file(
        &format!("{info}/METADATA"),
        format!("Metadata-Version: 2.6\nName: {name}\nVersion: 1.0\n").as_bytes(),
    ));
    let mut record = String::new();
    for (path, bytes, _) in &entries {
        if path.ends_with("/RECORD.jws") {
            continue;
        }
        record.push_str(&format!(
            "{},sha256={},{}\n",
            csv_field(path),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash(bytes)),
            bytes.len()
        ));
    }
    record.push_str(&format!("{info}/RECORD,,\n"));
    entries.push(file(&format!("{info}/RECORD"), record.as_bytes()));
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes, mode) in entries {
        zip.start_file(
            path,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(mode),
        )
        .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    let bytes = zip.finish().unwrap().into_inner();
    let filename = format!("{name}-1.0-py3-none-any.whl");
    let digest = Digest::from_sha256_bytes(&bytes);
    let cap = spool_verified_reader(
        Cursor::new(&bytes),
        &filename,
        digest,
        bytes.len() as u64,
        &fixture.scratch,
    )
    .expect("shared source prerequisite must already be GREEN");
    let supported =
        SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap();
    let inspected = inspect_owned_wheel(
        cap,
        digest,
        &WheelExpectation {
            filename: &filename,
            name,
            version: "1.0",
            supported: &supported,
        },
        WheelLimits::default(),
    )
    .expect("real A/C1 prerequisite must be GREEN, not C4 RED");
    (bytes, inspected)
}
fn prepare(
    fixture: &Fixture,
    prefix: &UnpublishedPrefix,
    archives: Vec<InspectedWheelArchive>,
    flat: bool,
) -> PreparedWheelGraph {
    let scheme = scheme(prefix, flat);
    let interpreter = prefix.final_path().join("bin/python");
    prepare_wheel_graph(
        archives,
        &WheelInstallTarget {
            scheme: &scheme,
            interpreter: &interpreter,
            reserved: &[],
        },
        WheelLimits::default(),
        WheelPlanLimits::default(),
        WheelInstallLimits::default(),
        &fixture.scratch,
    )
    .expect("real C3 prerequisite must be GREEN before any C4 target RED")
}

fn assert_error<T: fmt::Debug>(result: Result<T>, diagnostic: &str) {
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains(diagnostic),
        "expected {diagnostic:?}, got {error:#}"
    );
}
fn assert_empty(prefix: &UnpublishedPrefix) {
    assert_eq!(fs::read_dir(prefix.final_path()).unwrap().count(), 0);
    prefix.check_binding().unwrap();
}
fn check_receipt(
    receipt: &WheelMaterializationReceipt,
    prefix: &UnpublishedPrefix,
    expected: &super::super::WheelGraphPlan,
) {
    assert_eq!(
        (receipt.final_prefix(), receipt.allocation_id()),
        (prefix.final_path(), prefix.allocation_id())
    );
    assert_eq!(receipt.files().len(), expected.files().len());
    assert_eq!(receipt.directories().len(), expected.directories().len());
    for (actual, planned) in receipt.files().iter().zip(expected.files()) {
        let relative = planned
            .destination()
            .strip_prefix(prefix.final_path())
            .unwrap();
        assert_eq!(actual.relative_path(), relative);
        assert_eq!(
            (
                actual.sha256(),
                actual.size(),
                actual.mode(),
                actual.operation(),
                actual.owners()
            ),
            (
                planned.sha256(),
                planned.size(),
                planned.mode(),
                planned.operation(),
                planned.owners()
            )
        );
        let path = prefix.final_path().join(relative);
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_file() && !metadata.file_type().is_symlink());
        assert_eq!(metadata.permissions().mode() & 0o777, planned.mode());
        let bytes = fs::read(path).unwrap();
        assert_eq!(
            (hash(&bytes), bytes.len() as u64),
            (*planned.sha256(), planned.size())
        );
    }
    for (actual, planned) in receipt.directories().iter().zip(expected.directories()) {
        let relative = planned
            .destination()
            .strip_prefix(prefix.final_path())
            .unwrap();
        assert_eq!(
            (actual.relative_path(), actual.owners()),
            (relative, planned.owners())
        );
        let metadata = fs::symlink_metadata(prefix.final_path().join(relative)).unwrap();
        assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            if relative.as_os_str().is_empty() {
                0o700
            } else {
                0o755
            }
        );
    }
    prefix.check_binding().unwrap();
}

#[test]
fn complete_writes_match_all_five_schemes_scripts_metadata_and_actual_receipts() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let (_, source) = archive(
        &fixture,
        "demo",
        vec![
            file("demo.py", b"VALUE=42\n"),
            file("demo-1.0.data/platlib/native.bin", b"platlib"),
            file("demo-1.0.data/headers/demo.h", b"headers"),
            file("demo-1.0.data/data/share/demo/data.txt", b"data"),
            file(
                "demo-1.0.data/scripts/archive-run",
                b"#!python\nprint(42)\n",
            ),
            (
                "demo-1.0.data/scripts/copy-run".into(),
                b"#!/other\nexact\n".to_vec(),
                0o640,
            ),
            file(
                "demo-1.0.dist-info/entry_points.txt",
                b"[console_scripts]\nrun = demo:main\n",
            ),
            file("demo-1.0.dist-info/INSTALLER", b"old-tool\n"),
            file("demo-1.0.dist-info/REQUESTED", b"legacy requested"),
            file("demo-1.0.dist-info/licenses/LICENSE", b"license"),
            file("demo-1.0.dist-info/sboms/bom.json", b"{}"),
            file("demo-1.0.dist-info/RECORD.jws", b"legacy signature only"),
        ],
    );
    let prepared = prepare(&fixture, &prefix, vec![source], false);
    let plan = prepared.plan().clone();
    let sources = prepared.sources().to_vec();
    let receipt = materialize_wheel_graph(prepared, &mut prefix).unwrap();
    check_receipt(&receipt, &prefix, &plan);
    assert_eq!(receipt.sources(), sources);
    assert_eq!(
        fs::read(
            prefix
                .final_path()
                .join("lib/pure/demo-1.0.dist-info/INSTALLER")
        )
        .unwrap(),
        b"pakro\n"
    );
    assert_eq!(
        fs::read(prefix.final_path().join("bin/copy-run")).unwrap(),
        b"#!/other\nexact\n"
    );
    assert!(
        fs::read(prefix.final_path().join("bin/archive-run"))
            .unwrap()
            .ends_with(b"print(42)\n")
    );
    fixture.assert_unpublished();
}

#[test]
fn a_prepared_plan_cannot_write_into_a_different_real_allocation() {
    let fixture = Fixture::new();
    let correct = fixture.allocate(PrefixLimits::default());
    let mut different = fixture.allocate(PrefixLimits::default());
    let (_, source) = archive(&fixture, "demo", vec![file("demo.py", b"payload")]);
    let prepared = prepare(&fixture, &correct, vec![source], false);
    assert_error(materialize_wheel_graph(prepared, &mut different), "prefix");
    assert_empty(&correct);
    assert_empty(&different);
    fixture.assert_unpublished();
}

#[test]
fn a_source_envelope_mismatch_fails_before_any_mutation_and_does_not_taint() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let (_, source) = archive(&fixture, "demo", vec![file("demo.py", b"payload")]);
    let mut prepared = prepare(&fixture, &prefix, vec![source], false);
    prepared.sources[0].digest = Digest::from_sha256_bytes(b"wrong source");
    assert_error(materialize_wheel_graph(prepared, &mut prefix), "source");
    assert_empty(&prefix);
    fixture.assert_unpublished();
}

#[test]
fn a_missing_or_forged_generated_output_fails_before_mutation() {
    let fixture = Fixture::new();
    for missing in [false, true] {
        let mut prefix = fixture.allocate(PrefixLimits::default());
        let (_, source) = archive(&fixture, "demo", vec![file("demo.py", b"payload")]);
        let mut prepared = prepare(&fixture, &prefix, vec![source], false);
        if missing {
            let key = prepared.outputs.keys().next().unwrap().clone();
            prepared.outputs.remove(&key);
        } else {
            prepared.outputs.values_mut().next().unwrap().size += 1;
        }
        assert_error(materialize_wheel_graph(prepared, &mut prefix), "output");
        assert_empty(&prefix);
        fixture.assert_unpublished();
    }
}

#[test]
fn shared_archive_bytes_are_written_once_and_receipted_for_every_owner() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let (_, first) = archive(
        &fixture,
        "alpha",
        vec![file("shared.py", b"shared = True\n")],
    );
    let (_, second) = archive(
        &fixture,
        "beta",
        vec![file("shared.py", b"shared = True\n")],
    );
    let prepared = prepare(&fixture, &prefix, vec![first, second], false);
    let plan = prepared.plan().clone();
    let receipt = materialize_wheel_graph(prepared, &mut prefix).unwrap();
    check_receipt(&receipt, &prefix, &plan);
    let shared: Vec<_> = receipt
        .files()
        .iter()
        .filter(|file| file.relative_path() == Path::new("lib/pure/shared.py"))
        .collect();
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].owners().len(), 2);
    for name in ["alpha", "beta"] {
        let record = fs::read_to_string(
            prefix
                .final_path()
                .join(format!("lib/pure/{name}-1.0.dist-info/RECORD")),
        )
        .unwrap();
        assert!(
            record
                .lines()
                .any(|line| line.starts_with("shared.py,sha256="))
        );
    }
    fixture.assert_unpublished();
}

#[test]
fn every_secondary_archive_source_is_checked_before_a_shared_file_is_accepted() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let (_, first) = archive(&fixture, "alpha", vec![file("shared.py", b"same")]);
    let (_, second) = archive(&fixture, "beta", vec![file("shared.py", b"same")]);
    let mut prepared = prepare(&fixture, &prefix, vec![first, second], false);
    let (_, replacement) = archive(&fixture, "beta", vec![file("shared.py", b"evil")]);
    let owner = WheelOwner::from_manifest(replacement.manifest());
    *prepared
        .sources
        .iter_mut()
        .find(|source| source.owner() == &owner)
        .unwrap() = PreparedWheelSource {
        owner: owner.clone(),
        filename: replacement.filename().into(),
        digest: replacement.digest(),
        size: replacement.size(),
    };
    prepared.archives.insert(owner, replacement);
    assert_error(materialize_wheel_graph(prepared, &mut prefix), "source");
    assert_empty(&prefix);
    fixture.assert_unpublished();
}

#[test]
fn shared_generated_wrappers_keep_both_prepared_sources() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let mut sources = vec![];
    for name in ["alpha", "beta"] {
        sources.push(
            archive(
                &fixture,
                name,
                vec![file(
                    &format!("{name}-1.0.dist-info/entry_points.txt"),
                    b"[console_scripts]\nshared-run = shared:main\n",
                )],
            )
            .1,
        );
    }
    let prepared = prepare(&fixture, &prefix, sources, false);
    let receipt = materialize_wheel_graph(prepared, &mut prefix).unwrap();
    let wrapper = receipt
        .files()
        .iter()
        .find(|file| file.relative_path() == Path::new("bin/shared-run"))
        .unwrap();
    assert_eq!(wrapper.owners().len(), 2);
    assert_eq!(wrapper.operation(), WheelWriteOperation::GenerateEntryPoint);
    assert_eq!(
        fs::metadata(prefix.final_path().join("bin/shared-run"))
            .unwrap()
            .nlink(),
        1
    );
}

#[test]
fn separate_environment_payloads_and_the_external_archive_have_independent_inodes() {
    let fixture = Fixture::new();
    let mut paths = vec![];
    let mut allocations = vec![];
    let mut original = Vec::new();
    for _ in 0..2 {
        let mut prefix = fixture.allocate(PrefixLimits::default());
        let (bytes, source) = archive(&fixture, "demo", vec![file("demo.py", b"VALUE=42\n")]);
        original = bytes;
        let prepared = prepare(&fixture, &prefix, vec![source], false);
        materialize_wheel_graph(prepared, &mut prefix).unwrap();
        paths.push(prefix.final_path().join("lib/pure/demo.py"));
        allocations.push(prefix);
    }
    let external = fixture.root.join("external-wheel-cache");
    fs::write(&external, &original).unwrap();
    let first = fs::metadata(&paths[0]).unwrap();
    let second = fs::metadata(&paths[1]).unwrap();
    assert_ne!((first.dev(), first.ino()), (second.dev(), second.ino()));
    assert_eq!((first.nlink(), second.nlink()), (1, 1));
    fs::write(&paths[0], b"changed environment").unwrap();
    assert_eq!(fs::read(&paths[1]).unwrap(), b"VALUE=42\n");
    assert_eq!(fs::read(external).unwrap(), original);
    assert!(
        allocations
            .iter()
            .all(|prefix| prefix.check_binding().is_ok())
    );
    fixture.assert_unpublished();
}

#[test]
fn preexisting_leaf_or_parent_symlinks_never_clobber_external_sentinels() {
    let fixture = Fixture::new();
    for leaf in [false, true] {
        let mut prefix = fixture.allocate(PrefixLimits::default());
        let (_, source) = archive(&fixture, "demo", vec![file("demo.py", b"payload")]);
        let prepared = prepare(&fixture, &prefix, vec![source], false);
        let outside = fixture.root.join(if leaf {
            "outside-leaf"
        } else {
            "outside-parent"
        });
        if leaf {
            prefix.create_dir_all(Path::new("lib/pure")).unwrap();
            fs::write(&outside, b"external sentinel").unwrap();
            symlink(&outside, prefix.final_path().join("lib/pure/demo.py")).unwrap();
        } else {
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("sentinel"), b"external sentinel").unwrap();
            symlink(&outside, prefix.final_path().join("lib")).unwrap();
        }
        let error = materialize_wheel_graph(prepared, &mut prefix).unwrap_err();
        assert!(
            !error.to_string().contains("not implemented"),
            "must reach actual prefix rejection"
        );
        assert_eq!(
            fs::read(if leaf {
                outside.clone()
            } else {
                outside.join("sentinel")
            })
            .unwrap(),
            b"external sentinel"
        );
        fixture.assert_unpublished();
    }
}

#[test]
fn actual_volume_case_and_unicode_aliases_never_overwrite_the_first_file() {
    let fixture = Fixture::new();
    for (first, second) in [("Run", "run"), ("é", "e\u{301}")] {
        let (first, second) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        let probe = tempfile::tempdir_in(&fixture.root).unwrap();
        fs::write(probe.path().join(first), b"first").unwrap();
        let aliases = probe.path().join(second).exists();
        let mut prefix = fixture.allocate(PrefixLimits::default());
        let (_, source) = archive(
            &fixture,
            "demo",
            vec![file(first, b"first"), file(second, b"second")],
        );
        let prepared = prepare(&fixture, &prefix, vec![source], false);
        let result = materialize_wheel_graph(prepared, &mut prefix);
        if aliases {
            assert!(result.is_err());
            assert_eq!(
                fs::read(prefix.final_path().join("lib/pure").join(first)).unwrap(),
                b"first"
            );
            assert!(
                prefix.check_binding().is_err(),
                "an attempted namespace conflict must taint"
            );
        } else {
            result.unwrap();
            assert_eq!(
                fs::read(prefix.final_path().join("lib/pure").join(first)).unwrap(),
                b"first"
            );
            assert_eq!(
                fs::read(prefix.final_path().join("lib/pure").join(second)).unwrap(),
                b"second"
            );
        }
    }
}

#[test]
fn actual_prefix_byte_quota_failure_never_returns_a_successful_receipt() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits {
        max_total_bytes: 2,
        ..PrefixLimits::default()
    });
    let (_, source) = archive(
        &fixture,
        "demo",
        vec![file("a.py", b"payload larger than quota")],
    );
    let prepared = prepare(&fixture, &prefix, vec![source], false);
    let error = materialize_wheel_graph(prepared, &mut prefix).unwrap_err();
    assert!(!error.to_string().contains("not implemented"));
    assert!(prefix.check_binding().is_err());
    fixture.assert_unpublished();
}

#[test]
fn a_late_entry_quota_error_after_a_finished_file_taints_the_whole_attempt() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits {
        max_entries: 2,
        ..PrefixLimits::default()
    });
    let (_, source) = archive(&fixture, "demo", vec![file("a", b"one"), file("b", b"two")]);
    let prepared = prepare(&fixture, &prefix, vec![source], true);
    let error = materialize_wheel_graph(prepared, &mut prefix).unwrap_err();
    assert!(error.to_string().contains("entry limit"), "{error:#}");
    assert_eq!(
        fs::read(prefix.final_path().join("a")).unwrap(),
        b"one",
        "must reach actual write/finish before later failure"
    );
    assert!(
        prefix.check_binding().is_err(),
        "no unfinished PrefixFile exists here; central mutation error must mark_failed"
    );
    fixture.assert_unpublished();
}

#[test]
fn truncated_or_appended_prepared_script_spools_fail_before_any_destination_write() {
    let fixture = Fixture::new();
    for append in [false, true] {
        let mut prefix = fixture.allocate(PrefixLimits::default());
        let (_, source) = archive(
            &fixture,
            "demo",
            vec![file("demo-1.0.data/scripts/run", b"#!python\nprint(42)\n")],
        );
        let mut prepared = prepare(&fixture, &prefix, vec![source], false);
        let output = prepared
            .outputs
            .iter_mut()
            .find(|(key, _)| matches!(key, OutputKey::Archive { .. }))
            .unwrap()
            .1;
        if let PreparedBytes::Spool(file) = &mut output.bytes {
            file.set_len(if append {
                output.size + 1
            } else {
                output.size - 1
            })
            .unwrap();
        } else {
            panic!("rewritten script must be a private streaming spool")
        }
        assert_error(materialize_wheel_graph(prepared, &mut prefix), "output");
        assert_empty(&prefix);
    }
}

#[test]
fn successful_receipt_debug_does_not_expose_script_or_metadata_bodies() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let (_, source) = archive(
        &fixture,
        "demo",
        vec![file(
            "demo-1.0.data/scripts/run",
            b"#!python\nPRIVATE_SCRIPT_SECRET='redacted'\n",
        )],
    );
    let prepared = prepare(&fixture, &prefix, vec![source], false);
    let receipt = materialize_wheel_graph(prepared, &mut prefix).unwrap();
    assert!(!format!("{receipt:?}").contains("PRIVATE_SCRIPT_SECRET"));
    fixture.assert_unpublished();
}

#[test]
fn empty_complete_graph_returns_an_empty_receipt_and_keeps_the_real_prefix_owned() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let prepared = prepare(&fixture, &prefix, vec![], false);
    let receipt = materialize_wheel_graph(prepared, &mut prefix).unwrap();
    assert_eq!(
        (receipt.final_prefix(), receipt.allocation_id()),
        (prefix.final_path(), prefix.allocation_id())
    );
    assert!(
        receipt.files().is_empty()
            && receipt.directories().is_empty()
            && receipt.sources().is_empty()
    );
    assert_empty(&prefix);
    fixture.assert_unpublished();
}

#[test]
fn post_gate_private_spool_changes_are_detected_again_by_the_actual_writer() {
    let fixture = Fixture::new();
    for append in [false, true] {
        let mut prefix = fixture.allocate(PrefixLimits::default());
        let (_, source) = archive(
            &fixture,
            "demo",
            vec![file("demo-1.0.data/scripts/run", b"#!python\nprint(42)\n")],
        );
        let prepared = prepare(&fixture, &prefix, vec![source], false);
        let result = materialize_with_hook(prepared, &mut prefix, |prepared, prefix| {
            assert!(
                prefix.final_path().join("bin").is_dir(),
                "hook must run after actual directory creation"
            );
            let output = prepared
                .outputs
                .iter_mut()
                .find(|(key, _)| matches!(key, OutputKey::Archive { .. }))
                .unwrap()
                .1;
            if let PreparedBytes::Spool(file) = &mut output.bytes {
                file.set_len(if append {
                    output.size + 1
                } else {
                    output.size - 1
                })?;
            } else {
                panic!("rewritten script is a private spool");
            }
            Ok(())
        });
        assert_error(result, "output");
        assert!(
            prefix.check_binding().is_err(),
            "a failed writer cannot yield a ready generation"
        );
        fixture.assert_unpublished();
    }
}

#[test]
fn replacing_a_created_parent_cannot_redirect_writes_or_become_ready_after_restoration() {
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let (_, source) = archive(&fixture, "demo", vec![file("demo.py", b"payload")]);
    let prepared = prepare(&fixture, &prefix, vec![source], false);
    let outside = fixture.root.join("outside-race");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"external sentinel").unwrap();
    let retained = fixture.root.join("retained-created-parent");
    let parent = prefix.final_path().join("lib/pure");
    let result = materialize_with_hook(prepared, &mut prefix, |_, _| {
        assert!(
            parent.is_dir(),
            "the mutation must occur after owned directory creation"
        );
        fs::rename(&parent, &retained)?;
        symlink(&outside, &parent)?;
        Ok(())
    });
    let error = result.unwrap_err();
    assert!(!error.to_string().contains("not implemented"));
    assert_eq!(
        fs::read(outside.join("sentinel")).unwrap(),
        b"external sentinel"
    );
    assert!(!outside.join("demo.py").exists());
    fs::remove_file(&parent).unwrap();
    fs::rename(&retained, &parent).unwrap();
    assert!(
        prefix.check_binding().is_err(),
        "restoring the namespace cannot undo a failed mutation attempt"
    );
    fixture.assert_unpublished();
}

#[test]
#[ignore = "requires explicit absolute PAKRO_TEST_PYTHON for independent installed RECORD verification"]
fn live_installed_record_validates_actual_files_with_python_stdlib_csv_and_hashlib() {
    use std::process::Command;
    let interpreter =
        PathBuf::from(std::env::var_os("PAKRO_TEST_PYTHON").expect("explicit interpreter"));
    assert!(interpreter.is_absolute());
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let (_, source) = archive(
        &fixture,
        "demo",
        vec![
            file("资源/逗号,引号\"换\n行\r\n.txt", b"csv data"),
            file("demo-1.0.data/scripts/run", b"#!python\nprint(42)\n"),
            file("demo-1.0.data/headers/demo.h", b"header"),
        ],
    );
    let prepared = prepare(&fixture, &prefix, vec![source], false);
    let receipt = materialize_wheel_graph(prepared, &mut prefix).unwrap();
    let program = "import base64,csv,hashlib,os,sys\nroot,record=sys.argv[1:]\nbase=os.path.dirname(os.path.dirname(record))\nwith open(record,encoding='utf-8',newline='') as stream: rows=list(csv.reader(stream))\nassert len(rows)==int(os.environ['EXPECTED_ROWS'])\nfor relative,digest,size in rows:\n path=os.path.normpath(os.path.join(base,relative))\n assert os.path.commonpath([root,path])==root\n if path==record: assert digest==size==''; continue\n with open(path,'rb') as stream: body=stream.read()\n assert size==str(len(body))\n assert digest=='sha256='+base64.urlsafe_b64encode(hashlib.sha256(body).digest()).decode('ascii').rstrip('=')\nassert any('资源/' in row[0] and chr(10) in row[0] for row in rows)\n";
    let output = Command::new(interpreter)
        .args(["-B", "-c", program])
        .arg(prefix.final_path())
        .arg(
            prefix
                .final_path()
                .join("lib/pure/demo-1.0.dist-info/RECORD"),
        )
        .env_clear()
        .env("LC_ALL", "C.UTF-8")
        .env("EXPECTED_ROWS", receipt.files().len().to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    fixture.assert_unpublished();
}

#[test]
#[ignore = "requires explicit PAKRO_TEST_PYTHON and PAKRO_TEST_NATIVE_WHEEL built for that selected runtime"]
fn live_native_wheel_bytes_load_in_the_selected_base_runtime_after_real_copy() {
    use std::process::Command;
    let interpreter =
        PathBuf::from(std::env::var_os("PAKRO_TEST_PYTHON").expect("explicit interpreter"));
    let wheel =
        PathBuf::from(std::env::var_os("PAKRO_TEST_NATIVE_WHEEL").expect("explicit golden wheel"));
    assert!(interpreter.is_absolute() && wheel.is_absolute());
    let fixture = Fixture::new();
    let mut prefix = fixture.allocate(PrefixLimits::default());
    let bytes = fs::read(&wheel).unwrap();
    let filename = wheel.file_name().unwrap().to_str().unwrap();
    let parsed = pakro_index::parse_wheel_filename(filename).unwrap();
    let mut triples = vec![];
    for python in &parsed.tags.python {
        for abi in &parsed.tags.abi {
            for platform in &parsed.tags.platform {
                triples.push(WheelTag::new(python, abi, platform).unwrap());
            }
        }
    }
    let supported = SupportedWheelTags::new(triples).unwrap();
    let digest = Digest::from_sha256_bytes(&bytes);
    let artifact = spool_verified_reader(
        Cursor::new(&bytes),
        filename,
        digest,
        bytes.len() as u64,
        &fixture.scratch,
    )
    .unwrap();
    let source = inspect_owned_wheel(
        artifact,
        digest,
        &WheelExpectation {
            filename,
            name: "pakro-native-demo",
            version: "1.0",
            supported: &supported,
        },
        WheelLimits::default(),
    )
    .unwrap();
    let prepared = prepare(&fixture, &prefix, vec![source], false);
    let receipt = materialize_wheel_graph(prepared, &mut prefix).unwrap();
    // This proves copied extension bytes load in the explicitly selected base
    // runtime. It is deliberately not a final-prefix/runtime-bootstrap claim.
    let output = Command::new(interpreter).args(["-E", "-s", "-S", "-B", "-c",
        "import sys;sys.path[:0]=sys.argv[1:];import pakro_native_demo;assert pakro_native_demo.answer()==42;import pakro_native_pure,pakro_native_platform;assert pakro_native_pure.VALUE==43 and pakro_native_platform.VALUE==44"])
        .arg(prefix.final_path().join("lib/pure")).arg(prefix.final_path().join("lib/plat"))
        .env_clear().env("LC_ALL", "C.UTF-8").output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert!(
        receipt
            .files()
            .iter()
            .any(|file| file.relative_path() == Path::new("include/packages/pakro_native_demo.h"))
    );
    assert_eq!(
        fs::read(prefix.final_path().join("share/pakro-native-demo/data.txt")).unwrap(),
        b"VALUE=45\n"
    );
    fixture.assert_unpublished();
}
