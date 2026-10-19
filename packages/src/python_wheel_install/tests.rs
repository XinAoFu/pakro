use super::*;

use base64::Engine;
use pakro_index::{SupportedWheelTags, WheelTag};
use sha2::{Digest as _, Sha256};
use std::io::{Cursor, Read, Seek, Write};
use std::path::PathBuf;

use crate::artifact_snapshot::spool_verified_reader;
use crate::packages::python_wheel::{WheelExpectation, WheelScheme, WheelWarning};
use crate::packages::python_wheel_plan::{
    WheelGeneratedIntent, WheelPlanSource, WheelReservedKind, plan_wheel_graph,
};
use crate::packages::python_wheel_source::inspect_owned_wheel;

type Entry = (String, Vec<u8>, u32);

struct Workspace {
    _root: tempfile::TempDir,
    scratch: PathBuf,
    prefix: PathBuf,
    store: PathBuf,
    active: PathBuf,
}
impl Workspace {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let prefix = root.path().join("unpublished-prefix-not-created");
        let store = root.path().join("store-sentinel");
        let active = root.path().join("active-sentinel");
        std::fs::write(&store, b"unchanged store").unwrap();
        std::fs::write(&active, b"unchanged active").unwrap();
        Self {
            _root: root,
            scratch,
            prefix,
            store,
            active,
        }
    }
    fn scheme(&self) -> WheelInstallScheme {
        WheelInstallScheme::new(
            self.prefix.clone(),
            BTreeMap::from([
                (WheelScheme::Purelib, self.prefix.join("lib/pure")),
                (WheelScheme::Platlib, self.prefix.join("lib/plat")),
                (WheelScheme::Headers, self.prefix.join("include/packages")),
                (WheelScheme::Scripts, self.prefix.join("bin")),
                (WheelScheme::Data, self.prefix.clone()),
            ]),
        )
        .expect("B five-scheme prerequisite must already be GREEN")
    }
    fn assert_no_destination_effects(&self) {
        assert!(
            !self.prefix.exists(),
            "preparation must not create its final prefix"
        );
        assert_eq!(std::fs::read(&self.store).unwrap(), b"unchanged store");
        assert_eq!(std::fs::read(&self.active).unwrap(), b"unchanged active");
    }
}

fn file(path: &str, bytes: &[u8]) -> Entry {
    (path.into(), bytes.to_vec(), 0o644)
}
fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn hash_field(bytes: &[u8]) -> String {
    format!(
        "sha256={}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha256(bytes))
    )
}
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.into()
    }
}

fn archive_with_info(
    work: &Workspace,
    name: &str,
    version: &str,
    info: &str,
    pure: bool,
    mut entries: Vec<Entry>,
) -> InspectedWheelArchive {
    entries.push(file(
        &format!("{info}/WHEEL"),
        format!("Wheel-Version: 1.0\nRoot-Is-Purelib: {pure}\nTag: py3-none-any\n").as_bytes(),
    ));
    entries.push(file(
        &format!("{info}/METADATA"),
        format!("Metadata-Version: 2.6\nName: {name}\nVersion: {version}\n").as_bytes(),
    ));
    let mut record = String::new();
    for (path, bytes, _) in &entries {
        if path.ends_with("/RECORD.jws") || path.ends_with("/RECORD.p7s") {
            continue;
        }
        record.push_str(&format!(
            "{},{},{}\n",
            csv_field(path),
            hash_field(bytes),
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
    let filename = format!(
        "{}-{version}-py3-none-any.whl",
        name.replace(['.', '-'], "_")
    );
    let digest = Digest::from_sha256_bytes(&bytes);
    let snapshot = spool_verified_reader(
        Cursor::new(&bytes),
        &filename,
        digest,
        bytes.len() as u64,
        &work.scratch,
    )
    .expect("shared snapshot prerequisite must already be GREEN");
    let supported =
        SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap();
    inspect_owned_wheel(
        snapshot,
        digest,
        &WheelExpectation {
            filename: &filename,
            name,
            version,
            supported: &supported,
        },
        WheelLimits::default(),
    )
    .expect("A and C1 fixture prerequisite must already be GREEN, not C3 RED")
}
fn archive(work: &Workspace, name: &str, entries: Vec<Entry>) -> InspectedWheelArchive {
    archive_with_info(
        work,
        name,
        "1.0",
        &format!("{name}-1.0.dist-info"),
        true,
        entries,
    )
}
fn basic(work: &Workspace, name: &str) -> InspectedWheelArchive {
    archive(
        work,
        name,
        vec![file(&format!("{name}/__init__.py"), b"VALUE = 42\n")],
    )
}
fn prepare_custom(
    work: &Workspace,
    archives: Vec<InspectedWheelArchive>,
    scheme: &WheelInstallScheme,
    interpreter: &Path,
    reserved: &[WheelReservedDestination],
    (wheel, plan, install): (WheelLimits, WheelPlanLimits, WheelInstallLimits),
) -> Result<PreparedWheelGraph> {
    work.assert_no_destination_effects();
    let result = prepare_wheel_graph(
        archives,
        &WheelInstallTarget {
            scheme,
            interpreter,
            reserved,
        },
        wheel,
        plan,
        install,
        &work.scratch,
    );
    work.assert_no_destination_effects();
    result
}
fn prepare(work: &Workspace, archives: Vec<InspectedWheelArchive>) -> Result<PreparedWheelGraph> {
    prepare_custom(
        work,
        archives,
        &work.scheme(),
        &work.prefix.join("bin/python"),
        &[],
        (
            WheelLimits::default(),
            WheelPlanLimits::default(),
            WheelInstallLimits::default(),
        ),
    )
}
fn assert_error<T: fmt::Debug>(result: Result<T>, diagnostic: &str) {
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains(diagnostic),
        "expected {diagnostic:?}, got {error:#}"
    );
}
fn owner(prepared: &PreparedWheelGraph, name: &str) -> WheelOwner {
    prepared
        .sources()
        .iter()
        .find(|source| source.owner().name() == name)
        .unwrap()
        .owner()
        .clone()
}
fn output_bytes(prepared: &mut PreparedWheelGraph, key: &OutputKey) -> Vec<u8> {
    let output = prepared
        .outputs
        .get_mut(key)
        .expect("required prepared output");
    match &mut output.bytes {
        PreparedBytes::Memory(bytes) => bytes.clone(),
        PreparedBytes::Spool(file) => {
            file.rewind().unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            bytes
        }
    }
}
fn generated_bytes(
    prepared: &mut PreparedWheelGraph,
    name: &str,
    target: WheelGeneratedTarget,
) -> Vec<u8> {
    let key = OutputKey::Generated {
        owner: owner(prepared, name),
        target,
    };
    output_bytes(prepared, &key)
}
fn record(prepared: &mut PreparedWheelGraph, name: &str) -> Vec<u8> {
    generated_bytes(prepared, name, WheelGeneratedTarget::InstalledRecord)
}
fn relative(path: &Path, base: &Path) -> String {
    let left: Vec<_> = base.components().collect();
    let right: Vec<_> = path.components().collect();
    let common = left.iter().zip(&right).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_owned(); left.len() - common];
    parts.extend(
        right[common..]
            .iter()
            .map(|component| component.as_os_str().to_str().unwrap().to_owned()),
    );
    parts.join("/")
}

// Test-only independent CSV reader. Exact tricky-field bytes are additionally
// asserted below and the ignored live test delegates to Python stdlib csv.
fn rows(bytes: &[u8]) -> Vec<Vec<String>> {
    let text = std::str::from_utf8(bytes).unwrap();
    let mut result = vec![];
    let mut row = vec![];
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut field)),
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                assert_eq!(row.len(), 3);
                result.push(std::mem::take(&mut row));
            }
            ch => field.push(ch),
        }
    }
    assert!(!quoted && row.is_empty() && field.is_empty());
    result
}
fn assert_record_closes_plan(
    prepared: &mut PreparedWheelGraph,
    name: &str,
    metadata_parent: &Path,
) {
    let owner = owner(prepared, name);
    let actual = rows(&record(prepared, name));
    let mut expected: Vec<_> = prepared
        .plan()
        .files()
        .iter()
        .filter(|file| file.owners().contains(&owner))
        .map(|file| {
            let path = relative(file.destination(), metadata_parent);
            let own_record = file.operation() == WheelWriteOperation::GenerateRecord;
            vec![
                path,
                if own_record {
                    String::new()
                } else {
                    format!(
                        "sha256={}",
                        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(file.sha256())
                    )
                },
                if own_record {
                    String::new()
                } else {
                    file.size().to_string()
                },
            ]
        })
        .collect();
    expected.sort();
    assert_eq!(actual, expected);
    assert!(actual.iter().all(|row| !row[0].ends_with('/')));
}

#[test]
fn complete_preparation_owns_exact_sources_and_creates_no_destination() {
    let work = Workspace::new();
    let source = basic(&work, "demo");
    let expected = (source.filename().to_owned(), source.digest(), source.size());
    let prepared = prepare(&work, vec![source]).unwrap();
    assert!(prepared.plan().pending_metadata().is_empty());
    assert_eq!(prepared.plan().final_prefix(), work.prefix);
    assert_eq!(prepared.sources().len(), 1);
    let source = &prepared.sources()[0];
    assert_eq!(
        (source.filename(), source.digest(), source.size()),
        (expected.0.as_str(), expected.1, expected.2)
    );
    assert_eq!(
        (source.owner().name(), source.owner().version()),
        ("demo", "1.0")
    );
}

#[test]
fn all_five_explicit_roots_and_nonpure_root_are_respected() {
    let work = Workspace::new();
    for pure in [true, false] {
        let source = archive_with_info(
            &work,
            "demo",
            "1.0",
            "demo-1.0.dist-info",
            pure,
            vec![
                file("demo.py", b"payload"),
                file("demo-1.0.data/purelib/pure_resource", b"pure"),
                file("demo-1.0.data/platlib/plat_resource", b"plat"),
                file("demo-1.0.data/headers/api.h", b"header"),
                file("demo-1.0.data/scripts/tool", b"#!/bin/other\nBODY\n"),
                file("demo-1.0.data/data/share/resource", b"data"),
            ],
        );
        let prepared = prepare(&work, vec![source]).unwrap();
        let scheme = work.scheme();
        for (scheme_key, name) in [
            (
                if pure {
                    WheelScheme::Purelib
                } else {
                    WheelScheme::Platlib
                },
                "demo.py",
            ),
            (WheelScheme::Purelib, "pure_resource"),
            (WheelScheme::Platlib, "plat_resource"),
            (WheelScheme::Headers, "api.h"),
            (WheelScheme::Scripts, "tool"),
            (WheelScheme::Data, "share/resource"),
        ] {
            assert!(prepared.plan().files().iter().any(|entry| entry.destination() == scheme.root(scheme_key).unwrap().join(name)));
        }
    }
}

#[test]
fn legacy_metadata_tree_is_normalized_without_changing_archive_identity() {
    let work = Workspace::new();
    let source = archive_with_info(
        &work,
        "Demo.Pkg",
        "1.0.0",
        "Demo.Pkg-1.0.0.dist-info",
        true,
        vec![
            file("demo.py", b"ok"),
            file("Demo.Pkg-1.0.0.dist-info/licenses/LICENSE", b"license"),
            file("Demo.Pkg-1.0.0.dist-info/sboms/bom.json", b"{}"),
            file("Demo.Pkg-1.0.0.dist-info/REQUESTED", b"legacy marker"),
        ],
    );
    let mut prepared = prepare(&work, vec![source]).unwrap();
    let root = work.prefix.join("lib/pure/demo_pkg-1.0.0.dist-info");
    for leaf in [
        "WHEEL",
        "METADATA",
        "licenses/LICENSE",
        "sboms/bom.json",
        "REQUESTED",
        "INSTALLER",
        "RECORD",
    ] {
        assert!(
            prepared
                .plan()
                .files()
                .iter()
                .any(|entry| entry.destination() == root.join(leaf)),
            "missing {leaf}"
        );
    }
    assert!(prepared.plan().files().iter().flat_map(|file| file.sources()).any(|source|
        matches!(source, WheelPlanSource::Archive { archive_path, .. } if archive_path == "Demo.Pkg-1.0.0.dist-info/licenses/LICENSE")));
    assert_record_closes_plan(&mut prepared, "demo-pkg", &work.prefix.join("lib/pure"));
}

#[test]
fn every_archive_script_has_final_copy_or_rewrite_facts_before_planning() {
    let work = Workspace::new();
    let copy = b"#!/bin/other\nexact body\n";
    let raw = b"#!pythonw\r\nprint('hello')\r\n";
    let source = archive(
        &work,
        "demo",
        vec![
            ("demo-1.0.data/scripts/copy".into(), copy.to_vec(), 0o640),
            ("demo-1.0.data/scripts/rewrite".into(), raw.to_vec(), 0o640),
        ],
    );
    let rewrite_index = source
        .manifest()
        .members()
        .iter()
        .find(|member| member.relative_path() == "rewrite")
        .unwrap()
        .archive_index();
    let mut prepared = prepare(&work, vec![source]).unwrap();
    for (name, operation, mode) in [
        ("copy", WheelWriteOperation::Copy, 0o640),
        ("rewrite", WheelWriteOperation::RewritePythonShebang, 0o751),
    ] {
        let file = prepared
            .plan()
            .files()
            .iter()
            .find(|file| file.destination() == work.prefix.join("bin").join(name))
            .unwrap();
        assert_eq!((file.operation(), file.mode()), (operation, mode));
        if name == "copy" {
            assert_eq!(
                (file.sha256(), file.size()),
                (&sha256(copy), copy.len() as u64)
            );
        }
    }
    let key = OutputKey::Archive {
        owner: owner(&prepared, "demo"),
        archive_index: rewrite_index,
    };
    let bytes = output_bytes(&mut prepared, &key);
    assert!(bytes.ends_with(b"print('hello')\r\n"));
    assert!(!bytes.starts_with(b"#!python"));
    let file = prepared
        .plan()
        .files()
        .iter()
        .find(|file| file.destination() == work.prefix.join("bin/rewrite"))
        .unwrap();
    assert_eq!(
        (file.sha256(), file.size()),
        (&sha256(&bytes), bytes.len() as u64)
    );
    assert_ne!(file.sha256(), &sha256(raw));
}

#[test]
fn console_and_gui_wrappers_are_prepared_while_plugin_metadata_is_preserved() {
    let work = Workspace::new();
    let ini = b"[console_scripts]\nrun = pkg:Outer.call\n[gui_scripts]\nView = pkg:gui [old-extra]\n[plugins.group]\nplugin = pkg\n";
    let source = archive(
        &work,
        "demo",
        vec![file("demo-1.0.dist-info/entry_points.txt", ini)],
    );
    let mut prepared = prepare(&work, vec![source]).unwrap();
    for name in ["run", "View"] {
        let bytes = generated_bytes(
            &mut prepared,
            "demo",
            WheelGeneratedTarget::Scripts(name.into()),
        );
        assert!(bytes.starts_with(b"#!"));
        let file = prepared
            .plan()
            .files()
            .iter()
            .find(|file| file.destination() == work.prefix.join("bin").join(name))
            .unwrap();
        assert_eq!(
            (file.operation(), file.mode(), file.sha256(), file.size()),
            (
                WheelWriteOperation::GenerateEntryPoint,
                0o755,
                &sha256(&bytes),
                bytes.len() as u64
            )
        );
    }
    assert!(
        !prepared
            .plan()
            .files()
            .iter()
            .any(|file| file.destination() == work.prefix.join("bin/plugin"))
    );
    assert!(prepared.plan().files().iter().any(|file| {
        file.destination()
            .ends_with("demo-1.0.dist-info/entry_points.txt")
            && file.sha256() == &sha256(ini)
    }));
}

#[test]
fn wrapper_archive_script_and_cross_scheme_collisions_fail_before_destinations() {
    let work = Workspace::new();
    for path in ["demo-1.0.data/scripts/run", "demo-1.0.data/data/bin/run"] {
        let source = archive(
            &work,
            "demo",
            vec![
                file(path, b"collision"),
                file(
                    "demo-1.0.dist-info/entry_points.txt",
                    b"[console_scripts]\nrun = demo:main\n",
                ),
            ],
        );
        assert_error(prepare(&work, vec![source]), "conflict");
    }
}

#[test]
fn runtime_reservations_cover_both_archive_payload_and_generated_commands() {
    let work = Workspace::new();
    let reserved = [WheelReservedDestination {
        path: work.prefix.join("bin/python"),
        kind: WheelReservedKind::File,
    }];
    for generated in [false, true] {
        let source = archive(
            &work,
            "demo",
            vec![if generated {
                file(
                    "demo-1.0.dist-info/entry_points.txt",
                    b"[console_scripts]\npython = demo:main\n",
                )
            } else {
                file("demo-1.0.data/data/bin/python", b"replace runtime")
            }],
        );
        assert_error(
            prepare_custom(
                &work,
                vec![source],
                &work.scheme(),
                &work.prefix.join("bin/python"),
                &reserved,
                (
                    WheelLimits::default(),
                    WheelPlanLimits::default(),
                    WheelInstallLimits::default(),
                ),
            ),
            "reserved",
        );
    }
}

#[test]
fn one_owner_cannot_be_bound_to_two_owned_archives() {
    let work = Workspace::new();
    assert_error(
        prepare(
            &work,
            vec![
                basic(&work, "demo"),
                archive(&work, "demo", vec![file("other.py", b"other build")]),
            ],
        ),
        "owner",
    );
}

#[test]
fn a_shared_file_has_all_sources_and_appears_in_every_owner_record() {
    let work = Workspace::new();
    let shared = b"shared = True\n";
    let mut prepared = prepare(
        &work,
        vec![
            archive(&work, "alpha", vec![file("namespace/shared.py", shared)]),
            archive(&work, "beta", vec![file("namespace/shared.py", shared)]),
        ],
    )
    .unwrap();
    let file = prepared
        .plan()
        .files()
        .iter()
        .find(|file| file.destination().ends_with("namespace/shared.py"))
        .unwrap();
    assert_eq!(file.owners().len(), 2);
    assert_eq!(file.sources().len(), 2);
    for name in ["alpha", "beta"] {
        assert_record_closes_plan(&mut prepared, name, &work.prefix.join("lib/pure"));
        assert!(
            rows(&record(&mut prepared, name))
                .iter()
                .any(|row| row[0] == "namespace/shared.py")
        );
    }
}

#[test]
fn different_bytes_or_modes_at_a_shared_destination_are_rejected() {
    let work = Workspace::new();
    for mode_only in [false, true] {
        let first = archive(&work, "alpha", vec![file("shared.py", b"first")]);
        let mut second = file("shared.py", if mode_only { b"first" } else { b"other" });
        if mode_only {
            second.2 = 0o755;
        }
        assert_error(
            prepare(&work, vec![first, archive(&work, "beta", vec![second])]),
            "conflict",
        );
    }
}

#[test]
fn installed_record_covers_all_final_files_and_uses_empty_self_fields() {
    let work = Workspace::new();
    let source = archive(
        &work,
        "demo",
        vec![
            file("demo.py", b"payload"),
            file("demo-1.0.data/scripts/run", b"#!python\nprint(42)\n"),
            file("demo-1.0.dist-info/licenses/LICENSE", b"license"),
            file("demo-1.0.dist-info/sboms/bom.json", b"{}"),
            file("demo-1.0.dist-info/RECORD.jws", b"old signature"),
            file("demo-1.0.dist-info/REQUESTED", b"old marker"),
        ],
    );
    assert!(
        source
            .manifest()
            .warnings()
            .iter()
            .any(|warning| matches!(warning, WheelWarning::LegacySignature { .. }))
    );
    let mut prepared = prepare(&work, vec![source]).unwrap();
    assert_record_closes_plan(&mut prepared, "demo", &work.prefix.join("lib/pure"));
    let rows = rows(&record(&mut prepared, "demo"));
    assert!(rows.contains(&vec![
        "demo-1.0.dist-info/RECORD".into(),
        String::new(),
        String::new()
    ]));
    for path in [
        "../../bin/run",
        "demo-1.0.dist-info/INSTALLER",
        "demo-1.0.dist-info/licenses/LICENSE",
        "demo-1.0.dist-info/sboms/bom.json",
        "demo-1.0.dist-info/RECORD.jws",
        "demo-1.0.dist-info/REQUESTED",
    ] {
        assert!(
            rows.iter().any(|row| row[0] == path),
            "missing final file {path}"
        );
    }
}

#[test]
fn record_csv_quotes_unicode_comma_quote_lf_and_crlf_without_changing_paths() {
    let work = Workspace::new();
    let source = archive(
        &work,
        "demo",
        vec![
            file("资源/引号,\"换\n行\r\n.txt", b"data"),
            file("demo-1.0.data/headers/api.h", b"header"),
            file("demo-1.0.data/data/share/file", b"data"),
        ],
    );
    let mut prepared = prepare(&work, vec![source]).unwrap();
    let record = record(&mut prepared, "demo");
    let golden = format!(
        "\"资源/引号,\"\"换\n行\r\n.txt\",{},4\n",
        hash_field(b"data")
    );
    assert!(std::str::from_utf8(&record).unwrap().contains(&golden));
    let rows = rows(&record);
    assert!(rows.contains(&vec![
        "资源/引号,\"换\n行\r\n.txt".into(),
        hash_field(b"data"),
        "4".into()
    ]));
    assert!(
        rows.iter()
            .any(|row| row[0] == "../../include/packages/api.h")
    );
    assert!(rows.iter().any(|row| row[0] == "../../share/file"));
}

#[test]
fn record_uses_post_rewrite_script_hash_and_size_instead_of_archive_values() {
    let work = Workspace::new();
    let raw = b"#!python\nprint('payload')\n";
    let source = archive(&work, "demo", vec![file("demo-1.0.data/scripts/run", raw)]);
    let index = source
        .manifest()
        .members()
        .iter()
        .find(|member| member.scheme() == WheelScheme::Scripts)
        .unwrap()
        .archive_index();
    let mut prepared = prepare(&work, vec![source]).unwrap();
    let key = OutputKey::Archive {
        owner: owner(&prepared, "demo"),
        archive_index: index,
    };
    let final_bytes = output_bytes(&mut prepared, &key);
    let row = rows(&record(&mut prepared, "demo"))
        .into_iter()
        .find(|row| row[0] == "../../bin/run")
        .unwrap();
    assert_eq!(
        &row[1..],
        &[hash_field(&final_bytes), final_bytes.len().to_string()]
    );
    assert_ne!(row[1], hash_field(raw));
}

#[test]
fn installer_replaces_legacy_or_missing_slot_and_requested_does_not_select_roots() {
    let work = Workspace::new();
    for legacy_installer in [false, true] {
        let mut entries = vec![file("demo-1.0.dist-info/REQUESTED", b"unchanged requested")];
        if legacy_installer {
            entries.push(file("demo-1.0.dist-info/INSTALLER", b"other-tool\n"));
        }
        let mut prepared = prepare(&work, vec![archive(&work, "demo", entries)]).unwrap();
        assert_eq!(
            generated_bytes(&mut prepared, "demo", WheelGeneratedTarget::Installer),
            b"pakro\n"
        );
        let requested = prepared
            .plan()
            .files()
            .iter()
            .find(|file| file.destination().ends_with("demo-1.0.dist-info/REQUESTED"))
            .unwrap();
        assert_eq!(
            (requested.operation(), requested.sha256()),
            (WheelWriteOperation::Copy, &sha256(b"unchanged requested"))
        );
        assert!(
            !prepared
                .plan()
                .files()
                .iter()
                .any(|file| file.destination().ends_with("direct_url.json"))
        );
        assert_record_closes_plan(&mut prepared, "demo", &work.prefix.join("lib/pure"));
    }
}

#[test]
fn input_owner_order_does_not_change_final_paths_stats_or_record_bytes() {
    let work = Workspace::new();
    let mut forward = prepare(&work, vec![basic(&work, "alpha"), basic(&work, "beta")]).unwrap();
    let mut reverse = prepare(&work, vec![basic(&work, "beta"), basic(&work, "alpha")]).unwrap();
    for name in ["alpha", "beta"] {
        assert_eq!(record(&mut forward, name), record(&mut reverse, name));
    }
    assert_eq!(forward.plan(), reverse.plan());
    assert_eq!(forward.sources(), reverse.sources());
}

#[test]
fn an_empty_graph_is_complete_without_generating_metadata() {
    let work = Workspace::new();
    let prepared = prepare(&work, vec![]).unwrap();
    assert!(
        prepared.plan().files().is_empty()
            && prepared.sources().is_empty()
            && prepared.outputs.is_empty()
    );
}

#[test]
fn installed_record_row_and_byte_limits_fail_without_truncation() {
    let work = Workspace::new();
    for row_limit in [false, true] {
        let mut limits = WheelInstallLimits::default();
        if row_limit {
            limits.max_record_rows = 1;
        } else {
            limits.max_record_bytes = 1;
        }
        assert_error(
            prepare_custom(
                &work,
                vec![basic(&work, "demo")],
                &work.scheme(),
                &work.prefix.join("bin/python"),
                &[],
                (WheelLimits::default(), WheelPlanLimits::default(), limits),
            ),
            "record",
        );
    }
}

#[test]
fn generated_byte_budget_covers_metadata_wrappers_and_rewritten_outputs() {
    let work = Workspace::new();
    for kind in 0..3 {
        let mut entries = vec![];
        match kind {
            1 => entries.push(file(
                "demo-1.0.dist-info/entry_points.txt",
                format!(
                    "[console_scripts]\nrun = {}:main\n",
                    "long_module".repeat(100)
                )
                .as_bytes(),
            )),
            2 => entries.push(file(
                "demo-1.0.data/scripts/run",
                format!("#!python\n{}", "#body\n".repeat(1000)).as_bytes(),
            )),
            _ => {}
        }
        let limits = WheelInstallLimits {
            max_generated_bytes: if kind == 0 { 1 } else { 1024 },
            ..WheelInstallLimits::default()
        };
        assert_error(
            prepare_custom(
                &work,
                vec![archive(&work, "demo", entries)],
                &work.scheme(),
                &work.prefix.join("bin/python"),
                &[],
                (WheelLimits::default(), WheelPlanLimits::default(), limits),
            ),
            "generated",
        );
    }
}

#[test]
fn every_install_and_script_limit_must_be_nonzero_even_on_an_empty_graph() {
    let work = Workspace::new();
    let mut cases = vec![];
    let mut limits = WheelInstallLimits::default();
    limits.max_record_rows = 0;
    cases.push(limits);
    let mut limits = WheelInstallLimits::default();
    limits.max_record_bytes = 0;
    cases.push(limits);
    let mut limits = WheelInstallLimits::default();
    limits.max_generated_bytes = 0;
    cases.push(limits);
    let mut limits = WheelInstallLimits::default();
    limits.scripts.max_input_bytes = 0;
    cases.push(limits);
    for limits in cases {
        assert_error(
            prepare_custom(
                &work,
                vec![],
                &work.scheme(),
                &work.prefix.join("bin/python"),
                &[],
                (WheelLimits::default(), WheelPlanLimits::default(), limits),
            ),
            "limit",
        );
    }
}

#[test]
fn lower_caller_wheel_and_graph_limits_are_not_bypassed_by_prior_inspection() {
    let work = Workspace::new();
    for wheel_limit in [false, true] {
        let mut wheel = WheelLimits::default();
        let mut plan = WheelPlanLimits::default();
        if wheel_limit {
            wheel.max_archive_bytes = 1;
        } else {
            plan.max_files = 1;
        }
        assert_error(
            prepare_custom(
                &work,
                vec![basic(&work, "demo")],
                &work.scheme(),
                &work.prefix.join("bin/python"),
                &[],
                (wheel, plan, WheelInstallLimits::default()),
            ),
            "limit",
        );
    }
}

#[test]
fn bounded_entrypoint_metadata_errors_are_not_treated_as_no_commands() {
    let work = Workspace::new();
    for malformed in [false, true] {
        let source = archive(
            &work,
            "demo",
            vec![file(
                "demo-1.0.dist-info/entry_points.txt",
                if malformed {
                    b"[console_scripts]\nrun : demo:main\n"
                } else {
                    b"[console_scripts]\nrun = demo:main\n"
                },
            )],
        );
        let mut limits = WheelInstallLimits::default();
        if !malformed {
            limits.scripts.max_entrypoints_bytes = 1;
        }
        assert_error(
            prepare_custom(
                &work,
                vec![source],
                &work.scheme(),
                &work.prefix.join("bin/python"),
                &[],
                (WheelLimits::default(), WheelPlanLimits::default(), limits),
            ),
            if malformed { "entry point" } else { "limit" },
        );
    }
}

#[test]
fn script_targets_are_explicit_ascii_paths_inside_the_final_prefix() {
    let work = Workspace::new();
    for interpreter in [
        work.prefix.join("../outside/python"),
        work.prefix.join("bin/解释器"),
    ] {
        assert_error(
            prepare_custom(
                &work,
                vec![basic(&work, "demo")],
                &work.scheme(),
                &interpreter,
                &[],
                (
                    WheelLimits::default(),
                    WheelPlanLimits::default(),
                    WheelInstallLimits::default(),
                ),
            ),
            "target",
        );
    }
}

#[test]
fn preparation_needs_no_installed_interpreter_and_does_not_import_user_code() {
    let work = Workspace::new();
    // The explicit destination does not exist: no runtime/PATH lookup is needed
    // to prepare wrappers. A valid object reference must not execute its target.
    let source = archive(
        &work,
        "demo",
        vec![
            file(
                "demo-1.0.dist-info/entry_points.txt",
                b"[console_scripts]\nrun = module_that_does_not_exist:main\n",
            ),
            file(
                "module_that_does_not_exist.py",
                b"raise RuntimeError('must not import during preparation')\n",
            ),
        ],
    );
    let prepared = prepare(&work, vec![source]).unwrap();
    assert!(
        prepared
            .plan()
            .files()
            .iter()
            .any(|file| file.destination() == work.prefix.join("bin/run"))
    );
    assert!(!work.prefix.join("bin/python").exists());
}

fn memory_output(bytes: &[u8], operation: WheelWriteOperation) -> PreparedOutput {
    PreparedOutput {
        bytes: PreparedBytes::Memory(bytes.to_vec()),
        sha256: sha256(bytes),
        size: bytes.len() as u64,
        mode: 0o644,
        operation,
    }
}

// A/C1 and B construct this input independently of C3's prepare stub. Thus
// binding tests reach their own gate and never mistake prepare/setup failure
// for a successful rejection of mismatched owned bytes.
fn binding_fixture(work: &Workspace, archives: Vec<InspectedWheelArchive>) -> PreparedWheelGraph {
    let manifests: Vec<_> = archives
        .iter()
        .map(|archive| archive.manifest().clone())
        .collect();
    let mut intents = vec![];
    let mut outputs = BTreeMap::new();
    let mut sources = vec![];
    let mut owned = BTreeMap::new();
    for archive in archives {
        let owner = WheelOwner::from_manifest(archive.manifest());
        for (target, operation, bytes) in [
            (
                WheelGeneratedTarget::Installer,
                WheelWriteOperation::GenerateInstaller,
                b"pakro\n".as_slice(),
            ),
            (
                WheelGeneratedTarget::InstalledRecord,
                WheelWriteOperation::GenerateRecord,
                b"demo-1.0.dist-info/RECORD,,\n".as_slice(),
            ),
        ] {
            let output = memory_output(bytes, operation);
            intents.push(WheelGeneratedIntent {
                owner: owner.clone(),
                target: target.clone(),
                operation,
                sha256: output.sha256,
                size: output.size,
                mode: output.mode,
            });
            outputs.insert(
                OutputKey::Generated {
                    owner: owner.clone(),
                    target,
                },
                output,
            );
        }
        sources.push(PreparedWheelSource {
            owner: owner.clone(),
            filename: archive.filename().into(),
            digest: archive.digest(),
            size: archive.size(),
        });
        assert!(owned.insert(owner, archive).is_none());
    }
    let complete = plan_wheel_graph(
        &manifests,
        &work.scheme(),
        &intents,
        &[],
        &[],
        WheelPlanLimits::default(),
    )
    .and_then(WheelGraphPlan::require_complete)
    .expect("B complete-plan prerequisite must already be GREEN");
    PreparedWheelGraph {
        archives: owned,
        outputs,
        complete,
        sources,
    }
}

#[test]
fn binding_gate_accepts_matching_real_sources_and_generated_statistics() {
    let work = Workspace::new();
    let mut prepared = binding_fixture(&work, vec![basic(&work, "demo")]);
    validate_prepared_bindings(&mut prepared).unwrap();
    work.assert_no_destination_effects();
}

#[test]
fn binding_gate_rejects_missing_or_mismatched_actual_owner() {
    let work = Workspace::new();
    for missing in [false, true] {
        let mut prepared = binding_fixture(&work, vec![basic(&work, "demo")]);
        let key = prepared.archives.keys().next().unwrap().clone();
        prepared.archives.remove(&key).unwrap();
        if !missing {
            prepared.archives.insert(key, basic(&work, "other"));
        }
        assert_error(validate_prepared_bindings(&mut prepared), "source");
    }
}

#[test]
fn binding_gate_rejects_stale_member_index_path_raw_hash_and_raw_size() {
    let work = Workspace::new();
    for variant in 0..4 {
        let mut prepared = binding_fixture(&work, vec![basic(&work, "demo")]);
        let entries = match variant {
            0 => vec![
                file("extra.py", b""),
                file("demo/__init__.py", b"VALUE = 42\n"),
            ],
            1 => vec![file("other.py", b"VALUE = 42\n")],
            2 => vec![file("demo/__init__.py", b"VALUE = 43\n")],
            _ => vec![file("demo/__init__.py", b"VALUE = 4242424242\n")],
        };
        let replacement = archive(&work, "demo", entries);
        let owner = WheelOwner::from_manifest(replacement.manifest());
        // Refresh scalar source envelope so this test isolates B's member facts
        // against the new real archive rather than failing on outer identity.
        prepared.sources[0] = PreparedWheelSource {
            owner: owner.clone(),
            filename: replacement.filename().into(),
            digest: replacement.digest(),
            size: replacement.size(),
        };
        prepared.archives.insert(owner, replacement);
        assert_error(validate_prepared_bindings(&mut prepared), "source");
    }
}

#[test]
fn binding_gate_checks_every_source_of_a_coalesced_file() {
    let work = Workspace::new();
    let mut prepared = binding_fixture(
        &work,
        vec![
            archive(&work, "alpha", vec![file("shared.py", b"same")]),
            archive(&work, "beta", vec![file("shared.py", b"same")]),
        ],
    );
    assert_eq!(
        prepared
            .plan()
            .files()
            .iter()
            .find(|file| file.destination().ends_with("shared.py"))
            .unwrap()
            .sources()
            .len(),
        2
    );
    let replacement = archive(&work, "beta", vec![file("shared.py", b"evil")]);
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
    assert_error(validate_prepared_bindings(&mut prepared), "source");
}

#[test]
fn binding_gate_checks_outer_filename_digest_size_and_unique_summary_owner() {
    let work = Workspace::new();
    for variant in 0..5 {
        let mut prepared = binding_fixture(&work, vec![basic(&work, "demo")]);
        match variant {
            0 => prepared.sources[0].filename = "demo-1.0-1-py3-none-any.whl".into(),
            1 => prepared.sources[0].digest = Digest::from_sha256_bytes(b"different outer source"),
            2 => prepared.sources[0].size += 1,
            3 => prepared.sources.push(prepared.sources[0].clone()),
            _ => prepared.sources.clear(),
        }
        assert_error(validate_prepared_bindings(&mut prepared), "source");
    }
}

#[test]
fn binding_gate_rejects_missing_or_orphan_generated_outputs() {
    let work = Workspace::new();
    for orphan in [false, true] {
        let mut prepared = binding_fixture(&work, vec![basic(&work, "demo")]);
        if orphan {
            prepared.outputs.insert(
                OutputKey::Generated {
                    owner: owner(&prepared, "demo"),
                    target: WheelGeneratedTarget::Scripts("unplanned".into()),
                },
                memory_output(b"extra", WheelWriteOperation::GenerateEntryPoint),
            );
        } else {
            let key = prepared.outputs.keys().next().unwrap().clone();
            prepared.outputs.remove(&key).unwrap();
        }
        assert_error(validate_prepared_bindings(&mut prepared), "output");
    }
}

#[test]
fn binding_gate_rehashes_actual_outputs_and_checks_size_mode_operation_and_plan() {
    let work = Workspace::new();
    for variant in 0..6 {
        let mut prepared = binding_fixture(&work, vec![basic(&work, "demo")]);
        let output = prepared.outputs.values_mut().next().unwrap();
        match variant {
            0 => output.sha256 = sha256(b"incorrect facts"),
            1 => output.size += 1,
            2 => output.mode = 0o755,
            3 => output.operation = WheelWriteOperation::Copy,
            4 => output.bytes = PreparedBytes::Memory(b"different real bytes".to_vec()),
            _ => {
                output.bytes =
                    PreparedBytes::Memory(b"forged bytes with consistent local stats".to_vec());
                output.sha256 = sha256(b"forged bytes with consistent local stats");
                output.size = b"forged bytes with consistent local stats".len() as u64;
            }
        }
        assert_error(validate_prepared_bindings(&mut prepared), "output");
    }
}

#[test]
fn binding_gate_rewinds_private_spools_and_checks_complete_actual_bytes() {
    let work = Workspace::new();
    let mut prepared = binding_fixture(&work, vec![basic(&work, "demo")]);
    let output = prepared.outputs.values_mut().next().unwrap();
    let bytes = match &output.bytes {
        PreparedBytes::Memory(bytes) => bytes.clone(),
        PreparedBytes::Spool(_) => unreachable!(),
    };
    let mut spool = tempfile::tempfile_in(&work.scratch).unwrap();
    spool.write_all(&bytes).unwrap(); // Cursor deliberately left at EOF.
    output.bytes = PreparedBytes::Spool(spool);
    validate_prepared_bindings(&mut prepared).unwrap();
    let output = prepared.outputs.values_mut().next().unwrap();
    if let PreparedBytes::Spool(file) = &mut output.bytes {
        file.set_len(bytes.len() as u64 + 1).unwrap();
    }
    assert_error(validate_prepared_bindings(&mut prepared), "output");
}

#[test]
fn prepared_debug_hides_metadata_and_generated_script_contents() {
    let work = Workspace::new();
    let source = archive(
        &work,
        "demo",
        vec![
            file(
                "demo-1.0.data/scripts/script",
                b"#!python\nSECRET_FROM_ARCHIVE = 'do not log'\n",
            ),
            file(
                "demo-1.0.dist-info/entry_points.txt",
                b"[console_scripts]\nrun = private_module_secret:private_callable_secret\n",
            ),
        ],
    );
    let prepared = prepare(&work, vec![source]).unwrap();
    let debug = format!("{prepared:?}");
    for secret in [
        "SECRET_FROM_ARCHIVE",
        "private_module_secret",
        "private_callable_secret",
        "do not log",
    ] {
        assert!(!debug.contains(secret));
    }
    for output in prepared.outputs.values() {
        let debug = format!("{output:?}");
        assert!(!debug.contains("SECRET_FROM_ARCHIVE") && !debug.contains("private_module_secret"));
    }
}

#[test]
#[ignore = "requires an explicit absolute PAKRO_TEST_PYTHON for independent stdlib CSV validation"]
fn live_installed_record_roundtrips_with_the_selected_python_stdlib_csv() {
    use std::process::{Command, Stdio};
    let interpreter =
        PathBuf::from(std::env::var_os("PAKRO_TEST_PYTHON").expect("explicit interpreter"));
    assert!(interpreter.is_absolute());
    let work = Workspace::new();
    let mut prepared = prepare(
        &work,
        vec![archive(
            &work,
            "demo",
            vec![
                file("资源/逗号,引号\"换\n行\r\n.txt", b"golden"),
                file("demo-1.0.data/scripts/tool", b"#!python\nprint(42)\n"),
            ],
        )],
    )
    .unwrap();
    let bytes = record(&mut prepared, "demo");
    let expected = rows(&bytes);
    let mut child = Command::new(interpreter).args(["-B", "-c",
        "import csv,io,json,sys; print(json.dumps(list(csv.reader(io.StringIO(sys.stdin.buffer.read().decode('utf-8'), newline=''))), ensure_ascii=True))"])
        .env_clear().env("LC_ALL", "C.UTF-8").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    let actual: Vec<Vec<String>> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual, expected);
    work.assert_no_destination_effects();
}
