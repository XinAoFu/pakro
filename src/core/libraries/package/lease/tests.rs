//! Real wheel trees keep complete verification without reopening the control
//! store for every payload chunk, or walking a reacquired graph twice.

use super::{
    PackageLease, acquire_layout, acquire_package, full_inventory_checks, store_binding_checks,
};
use crate::libraries::language::lease::LanguageLease;
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};
use crate::libraries::package::layouts::{prepare_layout, tests::wheel};
use crate::libraries::package::{manifest, tree};
use crate::programs::OperationContext;
use eyre::Result;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;

async fn fixture() -> (tempfile::TempDir, OperationContext, LanguageLease) {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(120));
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    (owner, context, language)
}

fn package(
    context: &OperationContext,
    language: &LanguageLease,
    name: &str,
    key: [u8; 32],
    payload: &[u8],
) -> PackageLease {
    let path = format!("{name}.bin");
    let (facts, archive) = wheel(
        context,
        language,
        name,
        key,
        vec![],
        vec![(path.as_str(), payload)],
        None,
    );
    tree::prepare_package_tree(context, language, facts, archive).unwrap()
}

fn measured_check(package: &PackageLease) -> usize {
    let key = package.manifest().key();
    let full_before = full_inventory_checks(key);
    let store_before = store_binding_checks();
    package.check_binding().unwrap();
    let store_walks = store_binding_checks() - store_before;
    assert_eq!(
        full_inventory_checks(key) - full_before,
        1,
        "one package check must retain one complete inventory observer"
    );
    assert!(
        store_walks >= 2,
        "the complete observer must remain surrounded by store binding checks"
    );
    store_walks
}

#[tokio::test(flavor = "current_thread")]
async fn s4_package_inventory_store_checks_do_not_grow_with_payload_chunks() {
    let (_owner, context, language) = fixture().await;
    let small = package(&context, &language, "smallchunks", [61; 32], &[17; 1024]);
    let large = package(
        &context,
        &language,
        "largechunks",
        [62; 32],
        &vec![29; 192 * 1024],
    );
    // Both objects have the same inventory shape; only the payload byte count
    // differs. These measured calls are synchronous and contain no await.
    let small_checks = measured_check(&small);
    let large_checks = measured_check(&large);
    assert_eq!(
        large_checks, small_checks,
        "control-store checks must not repeat for every 64 KiB payload block"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn s4_package_inventory_layout_reacquisition_walks_each_object_once() {
    let (_owner, context, language) = fixture().await;
    let mut sources = Vec::new();
    for (name, key) in [("layoutwalkone", [71; 32]), ("layoutwalktwo", [72; 32])] {
        let path = format!("{name}.py");
        sources.push(wheel(
            &context,
            &language,
            name,
            key,
            vec![],
            vec![(path.as_str(), b"VALUE = 1\n")],
            None,
        ));
    }
    let layout = prepare_layout(&context, &language, [73; 32], sources).unwrap();
    let keys = [[71; 32], [72; 32], layout.key()];
    let before = keys.map(full_inventory_checks);
    let acquired = acquire_layout(&context, layout.key()).unwrap().unwrap();
    assert_eq!(acquired.packages().len(), 2);
    assert_eq!(acquired.key(), layout.key());
    for (key, before) in keys.into_iter().zip(before) {
        assert_eq!(
            full_inventory_checks(key) - before,
            1,
            "canonical layout acquisition must completely verify each bound object once"
        );
    }
    let key = [71; 32];
    let before = full_inventory_checks(key);
    let independent = acquire_package(&context, key).unwrap().unwrap();
    assert_eq!(independent.manifest().key(), key);
    assert_eq!(
        full_inventory_checks(key) - before,
        1,
        "standalone package acquisition must still perform a complete observer"
    );
}

struct NamespaceFault {
    catalog: PathBuf,
    displaced: PathBuf,
    changed: bool,
}

impl NamespaceFault {
    fn new(context: &OperationContext) -> Self {
        let catalog = manifest::store_path(context).join(".control/catalog");
        Self {
            displaced: catalog.with_file_name("catalog-inventory-displaced"),
            catalog,
            changed: false,
        }
    }

    fn apply(&mut self) -> Result<()> {
        assert!(!self.displaced.exists());
        fs::rename(&self.catalog, &self.displaced)?;
        self.changed = true;
        fs::create_dir(&self.catalog)?;
        fs::set_permissions(&self.catalog, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn restore(&mut self) -> std::io::Result<()> {
        if self.changed {
            if self.catalog.exists() {
                // Refuse to remove unexpected contents rather than recurse.
                fs::remove_dir(&self.catalog)?;
            }
            fs::rename(&self.displaced, &self.catalog)?;
            self.changed = false;
        }
        Ok(())
    }
}

impl Drop for NamespaceFault {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            if !std::thread::panicking() {
                panic!("owned package namespace fault could not be restored: {error}");
            }
        }
    }
}

struct ByteFault {
    path: PathBuf,
    original: Vec<u8>,
    changed: bool,
}

impl ByteFault {
    fn new(path: PathBuf) -> Self {
        let original = fs::read(&path).unwrap();
        assert!(!original.is_empty());
        Self {
            path,
            original,
            changed: false,
        }
    }

    fn apply_at(&mut self, offset: usize) -> Result<()> {
        let before = fs::metadata(&self.path)?;
        let mut changed = self.original.clone();
        changed[offset] ^= 1;
        self.changed = true;
        fs::write(&self.path, &changed)?;
        let after = fs::metadata(&self.path)?;
        assert_eq!(
            (before.dev(), before.ino(), before.len()),
            (after.dev(), after.ino(), after.len()),
            "the fault must preserve the published inode and byte count"
        );
        assert_ne!(fs::read(&self.path)?, self.original);
        Ok(())
    }

    fn restore(&mut self) -> std::io::Result<()> {
        if self.changed {
            fs::write(&self.path, &self.original)?;
            self.changed = false;
        }
        Ok(())
    }
}

impl Drop for ByteFault {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            if !std::thread::panicking() {
                panic!("owned package byte fault could not be restored: {error}");
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn s4_package_inventory_rechecks_catalog_namespace_after_full_walk() {
    let (_owner, context, language) = fixture().await;
    let package = package(
        &context,
        &language,
        "namespaceguard",
        [81; 32],
        b"VALUE=1\n",
    );
    let key = package.manifest().key();
    let before = full_inventory_checks(key);
    let mut fault = NamespaceFault::new(&context);
    let mut fired = false;
    let result = package.protection.check_binding_with_after_walk(|| {
        assert_eq!(full_inventory_checks(key), before + 1);
        fault.apply()?;
        fired = true;
        Ok(())
    });
    assert!(
        fired,
        "the namespace fault must follow a real complete walk"
    );
    assert!(
        result.is_err(),
        "a complete hash cannot authenticate a replaced catalog namespace"
    );
    fault.restore().unwrap();
    package.check_binding().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn s4_package_inventory_rechecks_manifest_bytes_after_full_walk() {
    let (_owner, context, language) = fixture().await;
    let package = package(&context, &language, "manifestguard", [82; 32], b"VALUE=1\n");
    let key = package.manifest().key();
    let path = manifest::store_path(&context)
        .join(".control/catalog")
        .join(manifest::package_name(&key));
    let mut fault = ByteFault::new(path);
    // Keep the JSON valid while changing an existing source fact on the same
    // inode. Identity-only or parse-only checks cannot authenticate this edit.
    let source = b"fixture-public-source-pin";
    let offset = fault
        .original
        .windows(source.len())
        .position(|part| part == source)
        .unwrap();
    let before = full_inventory_checks(key);
    let mut fired = false;
    let result = package.protection.check_binding_with_after_walk(|| {
        assert_eq!(full_inventory_checks(key), before + 1);
        fault.apply_at(offset)?;
        let _: serde_json::Value = serde_json::from_slice(&fs::read(&fault.path)?)?;
        fired = true;
        Ok(())
    });
    assert!(fired, "the manifest fault must follow a real complete walk");
    assert!(
        result.is_err(),
        "same-inode Ready manifest bytes must be rechecked after hashing"
    );
    fault.restore().unwrap();
    package.check_binding().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn s4_package_inventory_standalone_acquisition_rejects_same_size_payload_change() {
    let (_owner, context, language) = fixture().await;
    let package = package(&context, &language, "payloadguard", [83; 32], &[41; 1024]);
    let key = package.manifest().key();
    let mut fault = ByteFault::new(package.root().join("site-packages/payloadguard.bin"));
    fault.apply_at(0).unwrap();
    let before = full_inventory_checks(key);
    let result = acquire_package(&context, key);
    assert!(
        result.is_err(),
        "an independent acquisition must reject changed payload bytes"
    );
    assert_eq!(full_inventory_checks(key), before + 1);
    fault.restore().unwrap();
    acquire_package(&context, key)
        .unwrap()
        .unwrap()
        .check_binding()
        .unwrap();
}
