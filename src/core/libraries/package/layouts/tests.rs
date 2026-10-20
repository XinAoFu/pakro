use super::*;

/// A failed or damaged layout is never Ready: reservation for one key either
/// returns the existing complete layout or fails explicitly, and a failed
/// build leaves no half-published state that a later query would treat as
/// usable.
#[test]
fn f3_failed_layout_is_not_ready() {
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &stage,
        true,
        std::time::Duration::from_secs(300),
    );
    // A missing shared library root makes layout reservation fail cleanly
    // instead of fabricating a Ready layout.
    let result = reserve_layout(&context, [7; 32], [8; 32]);
    assert!(
        result.is_err(),
        "a layout without its language is never Ready"
    );
}

use super::super::manifest::PackageFacts;
use crate::artifact_snapshot::spool_verified_reader;
use crate::libraries::language::repository::LanguageInstallSpec;
use crate::packages::python_wheel::{WheelExpectation, WheelLimits};
use crate::packages::python_wheel_source::{InspectedWheelArchive, inspect_owned_wheel};
use crate::plugins::core::python::native::pipeline::RuntimeInstallPlan;
use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use std::io::{Cursor, Write};
use std::process::Command;
use std::time::Duration;

pub(crate) fn wheel(
    context: &OperationContext,
    language: &LanguageLease,
    name: &str,
    key: [u8; 32],
    dependencies: Vec<[u8; 32]>,
    files: Vec<(&str, &[u8])>,
    entry: Option<&str>,
) -> (PackageFacts, InspectedWheelArchive) {
    wheel_tagged(
        context,
        language,
        name,
        key,
        dependencies,
        files,
        entry,
        "py3-none-any",
    )
}
fn wheel_tagged(
    context: &OperationContext,
    language: &LanguageLease,
    name: &str,
    key: [u8; 32],
    dependencies: Vec<[u8; 32]>,
    files: Vec<(&str, &[u8])>,
    entry: Option<&str>,
    tag: &str,
) -> (PackageFacts, InspectedWheelArchive) {
    let info = format!("{name}-1.0.dist-info");
    let mut members: Vec<(String, Vec<u8>)> = files
        .into_iter()
        .map(|(p, b)| (p.into(), b.into()))
        .collect();
    members.push((
        format!("{info}/WHEEL"),
        format!(
            "Wheel-Version: 1.0\nRoot-Is-Purelib: {}\nTag: {tag}\n",
            if tag == "py3-none-any" {
                "true"
            } else {
                "false"
            }
        )
        .into_bytes(),
    ));
    members.push((
        format!("{info}/METADATA"),
        format!("Metadata-Version: 2.4\nName: {name}\nVersion: 1.0\n").into_bytes(),
    ));
    if let Some(entry) = entry {
        members.push((
            format!("{info}/entry_points.txt"),
            format!("[console_scripts]\n{entry}\n").into_bytes(),
        ));
    }
    let mut record = String::new();
    for (path, bytes) in &members {
        record.push_str(&format!(
            "{path},sha256={},{}\n",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
            bytes.len()
        ));
    }
    record.push_str(&format!("{info}/RECORD,,\n"));
    members.push((format!("{info}/RECORD"), record.into_bytes()));
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in members {
        zip.start_file(
            path,
            zip::write::SimpleFileOptions::default()
                .last_modified_time(zip::DateTime::default())
                .unix_permissions(0o644),
        )
        .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    let bytes = zip.finish().unwrap().into_inner();
    let filename = format!("{name}-1.0-{tag}.whl");
    let digest = pakro_store::Digest::from_sha256_bytes(&bytes);
    let artifact = spool_verified_reader(
        bytes.as_slice(),
        &filename,
        digest,
        1024 * 1024,
        context.requested_cache_root(),
    )
    .unwrap();
    let archive = inspect_owned_wheel(
        artifact,
        digest,
        &WheelExpectation {
            filename: &filename,
            name,
            version: "1.0",
            supported: language.target().supported(),
        },
        WheelLimits::default(),
    )
    .unwrap();
    (
        PackageFacts {
            key,
            name: name.into(),
            version: "1.0".into(),
            artifact: digest.to_string(),
            source: "fixture-public-source-pin".into(),
            dependencies,
        },
        archive,
    )
}

pub(crate) async fn real_language(context: &OperationContext) -> LanguageLease {
    let filename = "cpython-3.13.16+20261013-aarch64-apple-darwin-install_only_stripped.tar.gz";
    let path = Path::new("/Users/fuxinao/文件/project/pakro_test").join(filename);
    let digest: pakro_store::Digest =
        "sha256-16a5332fb0dcb77f163fc8f8cbccd95be1644cc53e68e5d2a79f50afd6680a0e"
            .parse()
            .unwrap();
    let (_, _, target, flavor) = crate::plugins::core::python::native::pbs_filename(filename)
        .unwrap()
        .unwrap();
    let platform = crate::lockfile::PlatformInfo {
        url: Some(format!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/{filename}"
        )),
        checksum: Some(digest.to_string().replacen('-', ":", 1)),
        ..Default::default()
    };
    let spec = LanguageInstallSpec::from_plan(
        &RuntimeInstallPlan::from_locked("3.13.16", &target, flavor, &platform).unwrap(),
        false,
    )
    .unwrap();
    crate::programs::install::runtime::prepare_language(&spec, context, || async {
        spool_verified_reader(
            std::fs::File::open(path)?,
            filename,
            digest,
            64 * 1024 * 1024,
            context.requested_cache_root(),
        )
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f3_layout_imports_scripts_pth_and_shares_package_trees() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(300),
    );
    let language = real_language(&context).await;
    let dep = wheel(
        &context,
        &language,
        "dep",
        [1; 32],
        vec![],
        vec![
            ("dep.py", b"VALUE = 'shared-dependency'\n"),
            ("extra/extra_module.py", b"VALUE='pth-visible'\n"),
            ("extra-path.pth", b"extra\n"),
        ],
        None,
    );
    let demo=wheel(&context,&language,"demo",[2;32],vec![[1;32]],vec![("demo.py",b"import dep, extra_module\ndef main():\n print(dep.VALUE + ':' + extra_module.VALUE)\nif __name__ == '__main__': main()\n")],Some("demo = demo:main"));
    let layout = prepare_layout(&context, &language, [3; 32], vec![dep, demo]).unwrap();
    let output = Command::new(layout.bin().join("python"))
        .args([
            "-c",
            "import demo, dep, extra_module; print(dep.VALUE + ':' + extra_module.VALUE)",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "shared-dependency:pth-visible"
    );
    let output = Command::new(layout.bin().join("demo")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "shared-dependency:pth-visible"
    );
    let output = Command::new(layout.bin().join("python"))
        .args(["-m", "demo"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dep_root = layout
        .packages()
        .iter()
        .find(|p| p.manifest().name() == "dep")
        .unwrap()
        .root()
        .to_owned();
    let dep = wheel(
        &context,
        &language,
        "dep",
        [1; 32],
        vec![],
        vec![
            ("dep.py", b"VALUE = 'shared-dependency'\n"),
            ("extra/extra_module.py", b"VALUE='pth-visible'\n"),
            ("extra-path.pth", b"extra\n"),
        ],
        None,
    );
    let second = prepare_layout(&context, &language, [4; 32], vec![dep]).unwrap();
    assert_eq!(second.packages()[0].root(), dep_root);
    let output=Command::new(second.bin().join("python")).args(["-c","import importlib.util; assert importlib.util.find_spec('demo') is None; import dep,extra_module"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    layout.check_binding().unwrap();
    second.check_binding().unwrap();
    language.check_binding().unwrap();
    let prefix = layout.prefix().to_owned();
    drop(layout);
    let reused = super::super::lease::acquire_layout(
        &context,
        layout_key(language.key().fingerprint(), [3; 32]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(reused.prefix(), prefix);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f3_cyclic_packages_publish_under_their_original_object_protection() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(300),
    );
    let language = real_language(&context).await;
    let left = wheel(
        &context,
        &language,
        "left",
        [11; 32],
        vec![[12; 32]],
        vec![(
            "left.py",
            b"VALUE='left'\ndef other():\n import right\n return right.VALUE\n",
        )],
        None,
    );
    let right = wheel(
        &context,
        &language,
        "right",
        [12; 32],
        vec![[11; 32]],
        vec![(
            "right.py",
            b"VALUE='right'\ndef other():\n import left\n return left.VALUE\n",
        )],
        None,
    );
    let layout = prepare_layout(&context, &language, [13; 32], vec![right, left]).unwrap();
    let output = Command::new(layout.bin().join("python"))
        .args([
            "-c",
            "import left,right; assert left.other()=='right'; assert right.other()=='left'",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let prefix = layout.prefix().to_owned();
    drop(layout);
    let reopened = super::super::lease::acquire_layout(
        &context,
        layout_key(language.key().fingerprint(), [13; 32]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(reopened.prefix(), prefix);
    reopened.check_binding().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f3_concurrent_graph_publication_uses_one_package_tree_and_layout() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(300),
    );
    let language = real_language(&context).await;
    let make = || {
        wheel(
            &context,
            &language,
            "parallel",
            [21; 32],
            vec![],
            vec![("parallel.py", b"VALUE='one-tree'\n")],
            None,
        )
    };
    let first = make();
    let second = make();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let c1 = context.clone();
    let l1 = language.clone();
    let b1 = barrier.clone();
    let first = std::thread::spawn(move || {
        b1.wait();
        prepare_layout(&c1, &l1, [22; 32], vec![first])
    });
    let c2 = context.clone();
    let l2 = language.clone();
    let second = std::thread::spawn(move || {
        barrier.wait();
        prepare_layout(&c2, &l2, [22; 32], vec![second])
    });
    let first = first.join().unwrap().unwrap();
    let second = second.join().unwrap().unwrap();
    assert_eq!(first.prefix(), second.prefix());
    assert_eq!(first.packages()[0].root(), second.packages()[0].root());
    first.check_binding().unwrap();
    second.check_binding().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f3_native_extension_loads_from_shared_package_tree() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(300),
    );
    let language = real_language(&context).await;
    let source = root.join("native.c");
    let binary = root.join("pakro_native_fixture.so");
    std::fs::write(&source,b"#include <Python.h>\nstatic struct PyModuleDef m={PyModuleDef_HEAD_INIT,\"pakro_native_fixture\",NULL,-1,NULL};\nPyMODINIT_FUNC PyInit_pakro_native_fixture(void){PyObject *r=PyModule_Create(&m);if(r)PyModule_AddIntConstant(r,\"VALUE\",42);return r;}\n").unwrap();
    let mut compiler = Command::new("cc");
    if cfg!(target_os = "macos") {
        compiler.args(["-bundle", "-undefined", "dynamic_lookup"]);
    } else {
        compiler.args(["-shared", "-fPIC"]);
    }
    let output = compiler
        .arg("-I")
        .arg(language.root().join("include/python3.13"))
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native fixture compiler failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = std::fs::read(&binary).unwrap();
    let tag = language
        .target()
        .supported()
        .ordered()
        .iter()
        .find(|tag| tag.interpreter() == "cp313" && tag.abi() == "cp313" && tag.platform() != "any")
        .unwrap();
    let tag = format!("{}-{}-{}", tag.interpreter(), tag.abi(), tag.platform());
    let package = wheel_tagged(
        &context,
        &language,
        "nativefixture",
        [31; 32],
        vec![],
        vec![("pakro_native_fixture.so", bytes.as_slice())],
        None,
        &tag,
    );
    let layout = prepare_layout(&context, &language, [32; 32], vec![package]).unwrap();
    let output=Command::new(layout.bin().join("python")).args(["-c","import pakro_native_fixture; assert pakro_native_fixture.VALUE==42; print(pakro_native_fixture.__file__)"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(layout.packages()[0].root().to_str().unwrap())
    );
    layout.check_binding().unwrap();
    language.check_binding().unwrap();
}

#[test]
fn f3_group_rollback_keeps_unrelated_publications_and_stable_locks() {
    use crate::packages::python_environment::prefix::{EnvironmentStore, PrefixLimits};
    use pakro_store::control::lease::{LeaseMode, StableLock};
    use pakro_store::control::{ControlLimits, ControlNamespace, StoreWriter};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(120),
    );
    let path = super::super::manifest::store_path(&context);
    let store = EnvironmentStore::open_shared_writer(
        &path,
        super::super::lease::uid(),
        PrefixLimits::default(),
        context.policy().deadline,
    )
    .unwrap();
    let mut first = store.allocate().unwrap();
    write(&mut first, Path::new("marker"), b"first", 0o600).unwrap();
    let first_root = first.final_path().to_owned();
    let storage = super::super::tree::storage_for(&first, [41; 32]).unwrap();
    let lock_name = format!("{}.lock", storage.allocation);
    let value = super::super::manifest::BlobManifest {
        artifact: pakro_store::Digest::from_sha256_bytes(b"first").to_string(),
        filename: "first.whl".into(),
        storage,
    };
    let guard = first
        .publish_shared(
            "blob-first.json",
            &serde_json::to_vec(&value).unwrap(),
            &value.storage.inventory,
        )
        .unwrap();
    let mut other = store.allocate().unwrap();
    write(&mut other, Path::new("marker"), b"other", 0o600).unwrap();
    let other_root = other.final_path().to_owned();
    let storage = super::super::tree::storage_for(&other, [42; 32]).unwrap();
    let other_value = super::super::manifest::BlobManifest {
        artifact: pakro_store::Digest::from_sha256_bytes(b"other").to_string(),
        filename: "other.whl".into(),
        storage,
    };
    drop(
        other
            .publish_shared(
                "blob-other.json",
                &serde_json::to_vec(&other_value).unwrap(),
                &other_value.storage.inventory,
            )
            .unwrap(),
    );
    let writer = StoreWriter::open_writer(
        &path,
        super::super::lease::uid(),
        Default::default(),
        context.policy().deadline,
    )
    .unwrap();
    let stable = StableLock::open_existing(
        writer.build_handle(),
        &lock_name,
        super::super::lease::uid(),
        super::super::lease::identity(value.storage.lock),
    )
    .unwrap();
    assert!(
        stable
            .try_acquire(LeaseMode::Exclusive, context.policy().deadline)
            .unwrap()
            .is_none(),
        "publication group retains original EX until complete"
    );
    guard.rollback().unwrap();
    let control = writer.control(ControlLimits::default()).unwrap();
    assert!(
        control
            .read(ControlNamespace::Catalog, "blob-first.json")
            .unwrap()
            .is_none()
    );
    assert!(
        control
            .read(ControlNamespace::Catalog, "blob-other.json")
            .unwrap()
            .is_some()
    );
    assert!(!first_root.exists());
    assert_eq!(std::fs::read(other_root.join("marker")).unwrap(), b"other");
    assert!(path.join(".leases/build").join(lock_name).exists());
}

#[test]
fn f3_failure_after_ready_cas_never_deletes_its_published_tree() {
    use crate::packages::python_environment::prefix::{EnvironmentStore, PrefixLimits};
    use pakro_store::control::{ControlLimits, ControlNamespace, StoreWriter};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(120),
    );
    let path = super::super::manifest::store_path(&context);
    let store = EnvironmentStore::open_shared_writer(
        &path,
        super::super::lease::uid(),
        PrefixLimits::default(),
        context.policy().deadline,
    )
    .unwrap();
    let mut prefix = store.allocate().unwrap();
    write(
        &mut prefix,
        Path::new("marker"),
        b"published-complete",
        0o600,
    )
    .unwrap();
    let payload = prefix.final_path().to_owned();
    let storage = super::super::tree::storage_for(&prefix, [51; 32]).unwrap();
    let value = super::super::manifest::BlobManifest {
        artifact: pakro_store::Digest::from_sha256_bytes(b"published-complete").to_string(),
        filename: "published.whl".into(),
        storage,
    };
    let bytes = serde_json::to_vec(&value).unwrap();
    let result = prefix.publish_shared_with_after_cas(
        "blob-post-cas.json",
        &bytes,
        &value.storage.inventory,
        || Err(eyre::eyre!("injected post-rename durability error")),
    );
    assert!(
        result.is_err(),
        "a post-CAS IO failure must remain an operation error"
    );
    let writer = StoreWriter::open_writer(
        &path,
        super::super::lease::uid(),
        Default::default(),
        context.policy().deadline,
    )
    .unwrap();
    let control = writer.control(ControlLimits::default()).unwrap();
    let visible = control
        .read(ControlNamespace::Catalog, "blob-post-cas.json")
        .unwrap();
    assert!(visible.is_some(), "the fault follows the real durable CAS");
    assert_eq!(
        std::fs::read(payload.join("marker")).unwrap(),
        b"published-complete",
        "Ready metadata must retain its exact payload after publication errors"
    );
    drop(control);
    super::super::lease::acquire_object(&context, "blob-post-cas.json", &value.storage, &bytes)
        .unwrap()
        .check_binding()
        .unwrap();
}

include!("facts_tests.rs");
