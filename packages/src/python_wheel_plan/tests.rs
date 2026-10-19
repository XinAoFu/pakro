use super::*;

use base64::Engine;
use pakro_index::{SupportedWheelTags, WheelTag};
use sha2::{Digest as _, Sha256};
use std::io::{Cursor, Write};

use crate::packages::python_wheel::{WheelExpectation, WheelLimits, inspect_wheel};

type Entry = (String, Vec<u8>, u32);

fn file(path: &str, bytes: &[u8]) -> Entry {
    (path.into(), bytes.to_vec(), 0o644)
}

fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.into()
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

// Independent, fully recorded in-memory wheels. H2-A must already be GREEN
// before Root captures B RED, otherwise an empty A baseline is a setup failure.
fn wheel(name: &str, version: &str, pure: bool, entries: Vec<Entry>) -> WheelManifest {
    wheel_with_dist_info(
        name,
        version,
        pure,
        &format!("{name}-{version}.dist-info"),
        entries,
    )
}

fn wheel_with_dist_info(
    name: &str,
    version: &str,
    pure: bool,
    info: &str,
    mut entries: Vec<Entry>,
) -> WheelManifest {
    entries.push(file(
        &format!("{info}/WHEEL"),
        format!("Wheel-Version: 1.0\nRoot-Is-Purelib: {pure}\nTag: py3-none-any\n").as_bytes(),
    ));
    entries.push(file(
        &format!("{info}/METADATA"),
        format!("Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n").as_bytes(),
    ));
    let mut record = String::new();
    for (path, bytes, _) in &entries {
        record.push_str(&format!(
            "{},sha256={},{}\n",
            csv_field(path),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest(bytes)),
            bytes.len()
        ));
    }
    record.push_str(&format!("{info}/RECORD,,\n"));
    entries.push(file(&format!("{info}/RECORD"), record.as_bytes()));
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes, mode) in entries {
        writer
            .start_file(
                path,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored)
                    .unix_permissions(mode),
            )
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    let filename = format!("{}-{version}-py3-none-any.whl", name.replace('-', "_"));
    let supported =
        SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap();
    let manifest = inspect_wheel(
        Cursor::new(writer.finish().unwrap().into_inner()),
        &WheelExpectation {
            filename: &filename,
            name,
            version,
            supported: &supported,
        },
        WheelLimits::default(),
    )
    .unwrap();
    assert_eq!(manifest.name(), name, "H2-A fixture setup is not GREEN");
    assert!(
        !manifest.members().is_empty(),
        "H2-A fixture setup is not GREEN"
    );
    manifest
}

fn basic(name: &str, entries: Vec<Entry>) -> WheelManifest {
    wheel(name, "1.0", true, entries)
}

fn roots(prefix: &Path) -> BTreeMap<WheelScheme, PathBuf> {
    BTreeMap::from([
        (WheelScheme::Purelib, prefix.join("lib/site")),
        (WheelScheme::Platlib, prefix.join("lib/site")),
        (WheelScheme::Headers, prefix.join("include")),
        (WheelScheme::Scripts, prefix.join("bin")),
        (WheelScheme::Data, prefix.to_path_buf()),
    ])
}

fn scheme() -> WheelInstallScheme {
    let prefix = PathBuf::from("/explicit/final-prefix");
    WheelInstallScheme::new(prefix.clone(), roots(&prefix)).unwrap()
}

fn plan(
    manifests: &[WheelManifest],
    scheme: &WheelInstallScheme,
    generated: &[WheelGeneratedIntent],
    transformed: &[WheelTransformIntent],
) -> Result<WheelGraphPlan> {
    plan_wheel_graph(
        manifests,
        scheme,
        generated,
        transformed,
        &[],
        WheelPlanLimits::default(),
    )
}

fn at<'a>(plan: &'a WheelGraphPlan, path: &str) -> &'a WheelFilePlan {
    plan.files()
        .iter()
        .find(|file| file.destination() == Path::new(path))
        .unwrap()
}

fn generated(
    owner: &WheelManifest,
    target: WheelGeneratedTarget,
    operation: WheelWriteOperation,
    bytes: &[u8],
) -> WheelGeneratedIntent {
    WheelGeneratedIntent {
        owner: WheelOwner::from_manifest(owner),
        target,
        operation,
        sha256: digest(bytes),
        size: bytes.len() as u64,
        mode: if operation == WheelWriteOperation::GenerateEntryPoint {
            0o755
        } else {
            0o644
        },
    }
}

fn transform(
    owner: &WheelManifest,
    path: &str,
    operation: WheelWriteOperation,
    bytes: &[u8],
) -> WheelTransformIntent {
    let member = owner
        .members()
        .iter()
        .find(|member| member.archive_path() == path)
        .unwrap();
    WheelTransformIntent {
        owner: WheelOwner::from_manifest(owner),
        archive_index: member.archive_index(),
        operation,
        sha256: digest(bytes),
        size: bytes.len() as u64,
        mode: if operation == WheelWriteOperation::RewritePythonShebang {
            0o755
        } else {
            member.unix_mode().unwrap_or(0o644) & 0o777
        },
    }
}

fn complete_metadata(manifest: &WheelManifest) -> Vec<WheelGeneratedIntent> {
    // This unit fixture represents C's final-byte claims, not an installed
    // RECORD validation or a verified prefix. B only plans ownership/stat facts.
    vec![
        generated(
            manifest,
            WheelGeneratedTarget::InstalledRecord,
            WheelWriteOperation::GenerateRecord,
            format!("{}/RECORD,,\n", manifest.dist_info()).as_bytes(),
        ),
        generated(
            manifest,
            WheelGeneratedTarget::Installer,
            WheelWriteOperation::GenerateInstaller,
            b"pakro\n",
        ),
    ]
}

#[test]
fn explicit_five_scheme_roots_map_all_files_and_reserve_generated_metadata() {
    let manifest = basic(
        "alpha",
        vec![
            file("root.py", b"root"),
            file("alpha-1.0.data/purelib/pure.py", b"pure"),
            file("alpha-1.0.data/platlib/native.so", b"native"),
            file("alpha-1.0.data/headers/api.h", b"header"),
            file("alpha-1.0.data/scripts/tool", b"plain executable"),
            file("alpha-1.0.data/data/share/data.txt", b"data"),
        ],
    );
    let prefix = PathBuf::from("/explicit/final-prefix");
    let mut paths = roots(&prefix);
    paths.insert(WheelScheme::Platlib, prefix.join("lib/platform"));
    let scheme = WheelInstallScheme::new(prefix.clone(), paths).unwrap();
    let scripts = [transform(
        &manifest,
        "alpha-1.0.data/scripts/tool",
        WheelWriteOperation::Copy,
        b"plain executable",
    )];
    let plan = plan(&[manifest], &scheme, &[], &scripts).unwrap();
    for suffix in [
        "lib/site/root.py",
        "lib/site/pure.py",
        "lib/platform/native.so",
        "include/api.h",
        "bin/tool",
        "share/data.txt",
    ] {
        assert!(
            plan.files()
                .iter()
                .any(|file| file.destination() == prefix.join(suffix)),
            "{suffix}"
        );
    }
    assert_eq!(scheme.final_prefix(), prefix);
    assert_eq!(
        scheme.root(WheelScheme::Platlib),
        Some(prefix.join("lib/platform").as_path())
    );
    assert_eq!(plan.pending_metadata().len(), 2);
    assert!(
        plan.pending_metadata()
            .iter()
            .any(|slot| slot.target() == &WheelGeneratedTarget::InstalledRecord)
    );
    assert!(
        plan.files()
            .iter()
            .all(|file| !file.destination().ends_with("RECORD"))
    );
}

#[test]
fn coalesced_scheme_roots_merge_identical_files_with_both_sources() {
    let manifest = basic(
        "alpha",
        vec![
            file("shared.py", b"shared"),
            file("alpha-1.0.data/platlib/shared.py", b"shared"),
        ],
    );
    let owner = WheelOwner::from_manifest(&manifest);
    let plan = plan(&[manifest], &scheme(), &[], &[]).unwrap();
    let file = at(&plan, "/explicit/final-prefix/lib/site/shared.py");
    assert_eq!(file.sha256(), &digest(b"shared"));
    assert_eq!(file.size(), 6);
    assert_eq!(file.mode(), 0o644);
    assert_eq!(file.operation(), WheelWriteOperation::Copy);
    assert_eq!(file.owners(), &[owner]);
    assert_eq!(file.sources().len(), 2);
}

#[test]
fn identical_files_and_namespace_directories_have_multiple_sorted_owners() {
    let alpha = basic(
        "alpha",
        vec![
            file("namespace/shared.py", b"same"),
            file("namespace/a.py", b"a"),
        ],
    );
    let beta = basic(
        "beta",
        vec![
            file("namespace/shared.py", b"same"),
            file("namespace/b.py", b"b"),
        ],
    );
    let expected = [
        WheelOwner::from_manifest(&alpha),
        WheelOwner::from_manifest(&beta),
    ];
    let plan = plan(&[beta, alpha], &scheme(), &[], &[]).unwrap();
    assert_eq!(
        at(&plan, "/explicit/final-prefix/lib/site/namespace/shared.py").owners(),
        &expected
    );
    let directory = plan
        .directories()
        .iter()
        .find(|dir| dir.destination() == Path::new("/explicit/final-prefix/lib/site/namespace"))
        .unwrap();
    assert_eq!(directory.owners(), &expected);
    assert!(
        !plan
            .files()
            .iter()
            .any(|file| file.destination().ends_with("namespace/__init__.py"))
    );
}

#[test]
fn different_contents_modes_and_file_directory_collisions_fail_in_both_orders() {
    for (first, second, diagnostic) in [
        (file("shared", b"one"), file("shared", b"two"), "content"),
        (
            file("shared", b"same"),
            ("shared".into(), b"same".to_vec(), 0o755),
            "mode",
        ),
        (
            file("shared", b"file"),
            file("shared/child", b"child"),
            "directory",
        ),
    ] {
        let alpha = basic("alpha", vec![first]);
        let beta = basic("beta", vec![second]);
        for manifests in [
            vec![alpha.clone(), beta.clone()],
            vec![beta.clone(), alpha.clone()],
        ] {
            let error = plan(&manifests, &scheme(), &[], &[]).unwrap_err();
            assert!(error.to_string().contains(diagnostic), "{error:#}");
        }
    }
}

#[test]
fn duplicate_distribution_or_multiple_versions_are_rejected() {
    let alpha = basic("alpha", vec![file("a.py", b"a")]);
    for second in [
        alpha.clone(),
        wheel("alpha", "2.0", true, vec![file("b.py", b"b")]),
    ] {
        let error = plan(&[alpha.clone(), second], &scheme(), &[], &[]).unwrap_err();
        assert!(error.to_string().contains("distribution"), "{error:#}");
    }
}

#[test]
fn foreign_owner_cannot_write_into_another_distribution_metadata_tree() {
    let alpha = basic("alpha", vec![file("a.py", b"a")]);
    let beta = basic(
        "beta",
        vec![file(
            "beta-1.0.data/data/lib/site/alpha-1.0.dist-info/extra",
            b"foreign",
        )],
    );
    for manifests in [
        vec![alpha.clone(), beta.clone()],
        vec![beta.clone(), alpha.clone()],
    ] {
        let error = plan(&manifests, &scheme(), &[], &[]).unwrap_err();
        assert!(error.to_string().contains("metadata"), "{error:#}");
    }
}

#[test]
fn missing_relative_or_escaping_scheme_roots_are_rejected() {
    let prefix = PathBuf::from("/explicit/final-prefix");
    for invalid in [
        PathBuf::from("relative/site"),
        prefix.join("../outside/site"),
        PathBuf::from("/another-prefix/site"),
    ] {
        let mut paths = roots(&prefix);
        paths.insert(WheelScheme::Purelib, invalid);
        assert!(WheelInstallScheme::new(prefix.clone(), paths).is_err());
    }
    let mut paths = roots(&prefix);
    paths.remove(&WheelScheme::Headers);
    assert!(WheelInstallScheme::new(prefix, paths).is_err());
}

#[test]
fn generated_wrapper_conflicts_with_archive_script_or_ordinary_payload() {
    for archive_script in [false, true] {
        let path = if archive_script {
            "alpha-1.0.data/scripts/tool"
        } else {
            "alpha-1.0.data/data/bin/tool"
        };
        let mut entry = file(path, b"same bytes");
        entry.2 = 0o755;
        let alpha = basic("alpha", vec![entry]);
        let beta = basic("beta", vec![file("b.py", b"b")]);
        let transforms = if archive_script {
            vec![transform(
                &alpha,
                path,
                WheelWriteOperation::Copy,
                b"same bytes",
            )]
        } else {
            vec![]
        };
        let generated = [generated(
            &beta,
            WheelGeneratedTarget::Scripts("tool".into()),
            WheelWriteOperation::GenerateEntryPoint,
            b"same bytes",
        )];
        let error = plan(&[alpha, beta], &scheme(), &generated, &transforms).unwrap_err();
        assert!(error.to_string().contains("operation"), "{error:#}");
    }
}

#[test]
fn rewritten_script_and_copy_do_not_coalesce_even_with_identical_final_bytes() {
    let final_bytes = b"#!/final/python\npass\n";
    let alpha = basic(
        "alpha",
        vec![file("alpha-1.0.data/scripts/tool", b"#!python\npass\n")],
    );
    let mut copied = file("beta-1.0.data/data/bin/tool", final_bytes);
    copied.2 = 0o755;
    let beta = basic("beta", vec![copied]);
    let transforms = [transform(
        &alpha,
        "alpha-1.0.data/scripts/tool",
        WheelWriteOperation::RewritePythonShebang,
        final_bytes,
    )];
    let error = plan(&[alpha, beta], &scheme(), &[], &transforms).unwrap_err();
    assert!(error.to_string().contains("operation"), "{error:#}");
}

#[test]
fn scripts_require_one_bound_transform_and_copy_facts_cannot_be_forged() {
    let manifest = basic(
        "alpha",
        vec![file("alpha-1.0.data/scripts/tool", b"plain script")],
    );
    let error = plan(std::slice::from_ref(&manifest), &scheme(), &[], &[]).unwrap_err();
    assert!(error.to_string().contains("transform"), "{error:#}");
    let good = transform(
        &manifest,
        "alpha-1.0.data/scripts/tool",
        WheelWriteOperation::Copy,
        b"plain script",
    );
    let planned = plan(
        std::slice::from_ref(&manifest),
        &scheme(),
        &[],
        std::slice::from_ref(&good),
    )
    .unwrap();
    assert_eq!(
        at(&planned, "/explicit/final-prefix/bin/tool").sha256(),
        &digest(b"plain script")
    );
    for field in [
        "digest",
        "size",
        "source",
        "owner",
        "mode",
        "operation",
        "duplicate",
    ] {
        let mut value = good.clone();
        match field {
            "digest" => value.sha256 = digest(b"forged"),
            "size" => value.size += 1,
            "source" => value.archive_index = usize::MAX,
            "owner" => value.owner.name = "absent-owner".into(),
            "mode" => value.mode = 0o100755,
            "operation" => value.operation = WheelWriteOperation::GenerateEntryPoint,
            _ => {}
        }
        let values = if field == "duplicate" {
            vec![good.clone(), good.clone()]
        } else {
            vec![value]
        };
        assert!(
            plan(std::slice::from_ref(&manifest), &scheme(), &[], &values).is_err(),
            "{field}"
        );
    }
}

#[test]
fn generated_intents_bind_existing_owners_and_only_fixed_metadata_replacements() {
    let manifest = basic(
        "alpha",
        vec![file("alpha-1.0.dist-info/INSTALLER", b"previous installer")],
    );
    let good = complete_metadata(&manifest);
    let planned = plan(std::slice::from_ref(&manifest), &scheme(), &good, &[])
        .unwrap()
        .require_complete()
        .unwrap();
    assert_eq!(
        at(
            planned.plan(),
            "/explicit/final-prefix/lib/site/alpha-1.0.dist-info/INSTALLER"
        )
        .sha256(),
        &digest(b"pakro\n")
    );
    assert!(planned.plan().pending_metadata().is_empty());
    for invalid in [
        generated(
            &manifest,
            WheelGeneratedTarget::Scripts("../outside".into()),
            WheelWriteOperation::GenerateEntryPoint,
            b"bad",
        ),
        generated(
            &manifest,
            WheelGeneratedTarget::Scripts("tool".into()),
            WheelWriteOperation::GenerateInstaller,
            b"bad",
        ),
        generated(
            &manifest,
            WheelGeneratedTarget::Installer,
            WheelWriteOperation::GenerateEntryPoint,
            b"bad",
        ),
        WheelGeneratedIntent {
            owner: WheelOwner {
                name: "unknown".into(),
                version: "1.0".into(),
            },
            ..good[0].clone()
        },
    ] {
        assert!(plan(std::slice::from_ref(&manifest), &scheme(), &[invalid], &[]).is_err());
    }
    assert!(
        plan(
            &[manifest],
            &scheme(),
            &[good[0].clone(), good[0].clone()],
            &[]
        )
        .is_err()
    );
}

#[test]
fn unfilled_record_and_installer_slots_cannot_be_mistaken_for_a_complete_write_plan() {
    let manifest = basic("alpha", vec![file("a.py", b"a")]);
    let plan = plan(&[manifest], &scheme(), &[], &[]).unwrap();
    assert_eq!(plan.pending_metadata().len(), 2);
    for reservation in plan.pending_metadata() {
        assert_eq!(reservation.owner().name(), "alpha");
        assert_eq!(reservation.owner().version(), "1.0");
        assert!(
            reservation
                .destination()
                .starts_with("/explicit/final-prefix/lib/site/alpha-1.0.dist-info")
        );
    }
    let error = plan.require_complete().unwrap_err();
    assert!(error.to_string().contains("metadata"), "{error:#}");
}

#[test]
fn graph_order_and_generated_intent_order_cannot_change_the_plan() {
    let alpha = basic("alpha", vec![file("namespace/a.py", b"a")]);
    let beta = basic("beta", vec![file("namespace/b.py", b"b")]);
    let mut intents = complete_metadata(&alpha);
    intents.extend(complete_metadata(&beta));
    let first = plan(&[alpha.clone(), beta.clone()], &scheme(), &intents, &[]).unwrap();
    intents.reverse();
    let second = plan(&[beta, alpha], &scheme(), &intents, &[]).unwrap();
    assert!(!first.files().is_empty());
    assert_eq!(first, second);
}

#[test]
fn unicode_and_csv_resource_names_are_preserved_without_touching_the_prefix() {
    let root = tempfile::tempdir().unwrap();
    let prefix = root.path().join("not-created-final-prefix");
    let scheme = WheelInstallScheme::new(prefix.clone(), roots(&prefix)).unwrap();
    let relative = "namespace/中文,\"resource\n.py";
    let manifest = basic("alpha", vec![file(relative, b"unicode")]);
    let plan = plan(&[manifest], &scheme, &[], &[]).unwrap();
    assert_eq!(plan.final_prefix(), prefix);
    assert!(
        plan.files()
            .iter()
            .any(|file| file.destination() == prefix.join("lib/site").join(relative))
    );
    assert!(
        !prefix.exists(),
        "pure planning wrote into the final prefix"
    );
}

#[test]
fn zero_limits_and_excessive_graph_resources_fail_without_partial_success() {
    let manifest = basic("alpha", vec![file("a.py", b"payload")]);
    for limits in [
        WheelPlanLimits {
            max_wheels: 0,
            ..Default::default()
        },
        WheelPlanLimits {
            max_sources: 1,
            ..Default::default()
        },
        WheelPlanLimits {
            max_files: 1,
            ..Default::default()
        },
        WheelPlanLimits {
            max_directories: 1,
            ..Default::default()
        },
        WheelPlanLimits {
            max_total_bytes: 1,
            ..Default::default()
        },
        WheelPlanLimits {
            max_path_bytes: 1,
            ..Default::default()
        },
    ] {
        let error = plan_wheel_graph(
            std::slice::from_ref(&manifest),
            &scheme(),
            &[],
            &[],
            &[],
            limits,
        )
        .unwrap_err();
        assert!(error.to_string().contains("limit"), "{error:#}");
    }
}

fn reserved_plan(
    manifests: &[WheelManifest],
    generated: &[WheelGeneratedIntent],
    reserved: &[WheelReservedDestination],
) -> Result<WheelGraphPlan> {
    plan_wheel_graph(
        manifests,
        &scheme(),
        generated,
        &[],
        reserved,
        WheelPlanLimits::default(),
    )
}

#[test]
fn reserved_runtime_files_reject_archive_payloads_and_generated_wrappers() {
    let reserved = [WheelReservedDestination {
        path: PathBuf::from("/explicit/final-prefix/bin/python"),
        kind: WheelReservedKind::File,
    }];
    for generated_wrapper in [false, true] {
        let manifest = basic(
            "alpha",
            if generated_wrapper {
                vec![file("a.py", b"a")]
            } else {
                vec![file("alpha-1.0.data/data/bin/python", b"shadow runtime")]
            },
        );
        let generated = if generated_wrapper {
            vec![generated(
                &manifest,
                WheelGeneratedTarget::Scripts("python".into()),
                WheelWriteOperation::GenerateEntryPoint,
                b"shadow runtime",
            )]
        } else {
            vec![]
        };
        let error = reserved_plan(&[manifest], &generated, &reserved).unwrap_err();
        assert!(error.to_string().contains("reserved"), "{error:#}");
    }
}

#[test]
fn reserved_directories_allow_descendants_but_not_files_at_or_above_their_paths() {
    let reserved = [WheelReservedDestination {
        path: PathBuf::from("/explicit/final-prefix/share/runtime"),
        kind: WheelReservedKind::Directory,
    }];
    let good = basic(
        "alpha",
        vec![file(
            "alpha-1.0.data/data/share/runtime/resource",
            b"allowed",
        )],
    );
    let accepted = reserved_plan(&[good], &[], &reserved).unwrap();
    assert_eq!(
        at(&accepted, "/explicit/final-prefix/share/runtime/resource").sha256(),
        &digest(b"allowed")
    );
    for path in [
        "alpha-1.0.data/data/share/runtime",
        "alpha-1.0.data/data/share",
    ] {
        let manifest = basic("alpha", vec![file(path, b"conflicting file")]);
        let error = reserved_plan(&[manifest], &[], &reserved).unwrap_err();
        assert!(error.to_string().contains("reserved"), "{error:#}");
    }
}

#[test]
fn exclusive_reserved_trees_reject_payload_generated_and_pending_metadata_targets() {
    let manifest = basic(
        "alpha",
        vec![
            file("a.py", b"a"),
            file("alpha-1.0.data/data/lib/runtime/inject.py", b"bad"),
        ],
    );
    let reserved = [WheelReservedDestination {
        path: PathBuf::from("/explicit/final-prefix/lib/runtime"),
        kind: WheelReservedKind::ExclusiveTree,
    }];
    let error = reserved_plan(&[manifest], &[], &reserved).unwrap_err();
    assert!(error.to_string().contains("reserved"), "{error:#}");
    let manifest = basic("alpha", vec![file("a.py", b"a")]);
    let allowed = reserved_plan(std::slice::from_ref(&manifest), &[], &reserved).unwrap();
    assert_eq!(
        at(&allowed, "/explicit/final-prefix/lib/site/a.py").sha256(),
        &digest(b"a")
    );
    for (path, generated) in [
        (
            "/explicit/final-prefix/bin",
            vec![generated(
                &manifest,
                WheelGeneratedTarget::Scripts("tool".into()),
                WheelWriteOperation::GenerateEntryPoint,
                b"wrapper",
            )],
        ),
        (
            "/explicit/final-prefix/lib/site/alpha-1.0.dist-info/RECORD",
            vec![],
        ),
    ] {
        let reserved = [WheelReservedDestination {
            path: path.into(),
            kind: WheelReservedKind::ExclusiveTree,
        }];
        let error =
            reserved_plan(std::slice::from_ref(&manifest), &generated, &reserved).unwrap_err();
        assert!(error.to_string().contains("reserved"), "{error:#}");
    }
}

#[test]
fn reserved_destinations_require_contained_absolute_paths_and_input_budget() {
    for path in [
        "relative/runtime",
        "/outside/runtime",
        "/explicit/final-prefix/../outside",
    ] {
        let reserved = [WheelReservedDestination {
            path: path.into(),
            kind: WheelReservedKind::File,
        }];
        let error = reserved_plan(&[], &[], &reserved).unwrap_err();
        assert!(error.to_string().contains("reserved"), "{error:#}");
    }
    let reserved = [WheelReservedDestination {
        path: "/explicit/final-prefix/bin/python".into(),
        kind: WheelReservedKind::File,
    }];
    let limits = WheelPlanLimits {
        max_sources: 0,
        ..Default::default()
    };
    let error = plan_wheel_graph(&[], &scheme(), &[], &[], &reserved, limits).unwrap_err();
    assert!(error.to_string().contains("limit"), "{error:#}");
}

#[test]
fn installed_metadata_tree_is_normalized_while_archive_source_identity_is_preserved() {
    let original = "Demo.Pkg-1.0.0.dist-info";
    let installed = "demo_pkg-1.0.0.dist-info";
    let license_path = format!("{original}/licenses/NOTICE");
    let manifest = wheel_with_dist_info(
        "demo-pkg",
        "1.0.0",
        true,
        original,
        vec![
            file("demo.py", b"payload"),
            file(&license_path, b"license"),
            file(&format!("{original}/INSTALLER"), b"old installer"),
        ],
    );
    assert_eq!(manifest.dist_info(), original);
    let original_index = manifest
        .members()
        .iter()
        .find(|member| member.archive_path() == license_path)
        .unwrap()
        .archive_index();
    let intents = [
        generated(
            &manifest,
            WheelGeneratedTarget::InstalledRecord,
            WheelWriteOperation::GenerateRecord,
            format!("{installed}/RECORD,,\n").as_bytes(),
        ),
        generated(
            &manifest,
            WheelGeneratedTarget::Installer,
            WheelWriteOperation::GenerateInstaller,
            b"pakro\n",
        ),
    ];
    let plan = plan(&[manifest], &scheme(), &intents, &[])
        .unwrap()
        .require_complete()
        .unwrap();
    for suffix in [
        "WHEEL",
        "METADATA",
        "licenses/NOTICE",
        "RECORD",
        "INSTALLER",
    ] {
        assert!(
            plan.plan().files().iter().any(|file| file.destination()
                == PathBuf::from("/explicit/final-prefix/lib/site")
                    .join(installed)
                    .join(suffix)),
            "{suffix}"
        );
    }
    assert!(plan.plan().files().iter().all(|file| {
        !file
            .destination()
            .components()
            .any(|part| part.as_os_str() == original)
    }));
    let license = at(
        plan.plan(),
        "/explicit/final-prefix/lib/site/demo_pkg-1.0.0.dist-info/licenses/NOTICE",
    );
    assert!(license.sources().iter().any(|source| matches!(source,
        WheelPlanSource::Archive { archive_index, archive_path, .. }
            if *archive_index == original_index && archive_path == &license_path)));
}
