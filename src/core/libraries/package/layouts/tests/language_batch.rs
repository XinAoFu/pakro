//! One synchronous layout batch keeps a bounded number of complete language
//! walks, while standalone writers and the pre-publication boundary stay strict.

use super::{prepare_layout, wheel};
use crate::libraries::language::lease::{LanguageLease, full_inventory_checks};
use crate::libraries::language::repository::tests::{
    archive, install_spec, operation_context, prepare,
};
use crate::libraries::package::{manifest, tree};
use crate::packages::python_environment::prefix::UnpublishedPrefix;
use crate::packages::python_wheel_source::InspectedWheelArchive;
use crate::programs::OperationContext;
use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

const GRAPH: [u8; 32] = [171; 32];

fn sources(
    context: &OperationContext,
    language: &LanguageLease,
    count: usize,
) -> Vec<(manifest::PackageFacts, InspectedWheelArchive)> {
    ["batchone", "batchtwo", "batchthree"]
        .into_iter()
        .take(count)
        .enumerate()
        .map(|(index, name)| {
            let path = format!("{name}.py");
            wheel(
                context,
                language,
                name,
                [181 + index as u8; 32],
                vec![],
                vec![(path.as_str(), b"VALUE=1\n")],
                None,
            )
        })
        .collect()
}

async fn layout_walks(count: usize) -> usize {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(120));
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    let sources = sources(&context, &language, count);
    // Preparation/probing and wheel inspection are deliberately outside the
    // interval. The measured operation below contains no await or child test.
    let before = full_inventory_checks();
    let layout = prepare_layout(&context, &language, GRAPH, sources).unwrap();
    let walks = full_inventory_checks() - before;
    assert_eq!(layout.packages().len(), count);
    assert!(
        walks >= 2,
        "a layout batch must retain complete pre/post language verification"
    );
    walks
}

#[tokio::test(flavor = "current_thread")]
async fn s4_language_batch_full_inventory_walks_are_independent_of_new_package_count() {
    let one = layout_walks(1).await;
    let three = layout_walks(3).await;
    assert_eq!(
        three, one,
        "preparing three new package trees must not add a language walk per package"
    );
}

enum Mutation {
    File { path: PathBuf, original: Vec<u8> },
    Namespace { path: PathBuf, displaced: PathBuf },
}

struct FaultState {
    owned_root: PathBuf,
    mutation: Mutation,
    fired: bool,
}

impl FaultState {
    fn apply(&mut self, prefix: &UnpublishedPrefix) {
        assert!(
            prefix
                .final_path()
                .starts_with(self.owned_root.join("data/packages"))
        );
        self.fired = true;
        match &self.mutation {
            Mutation::File { path, original } => {
                assert!(path.starts_with(&self.owned_root));
                let before = fs::metadata(path).unwrap();
                assert!(before.is_file() && !original.is_empty());
                let mut changed = original.clone();
                changed[0] ^= 1;
                fs::write(path, &changed).unwrap();
                let after = fs::metadata(path).unwrap();
                assert_eq!(
                    (before.dev(), before.ino(), before.len()),
                    (after.dev(), after.ino(), after.len()),
                    "the fault must change bytes while preserving the published inode and size"
                );
                assert_ne!(fs::read(path).unwrap(), *original);
            }
            Mutation::Namespace { path, displaced } => {
                assert!(path.starts_with(&self.owned_root));
                assert!(displaced.starts_with(&self.owned_root));
                assert!(!displaced.exists());
                fs::rename(path, displaced).unwrap();
                fs::create_dir(path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
    }

    fn restore(self) -> std::io::Result<()> {
        if !self.fired {
            return Ok(());
        }
        match self.mutation {
            Mutation::File { path, original } => fs::write(path, original),
            Mutation::Namespace { path, displaced } => {
                if displaced.exists() {
                    if path.exists() {
                        // Never recursively erase an unexpected replacement.
                        fs::remove_dir(&path)?;
                    }
                    fs::rename(displaced, path)?;
                }
                Ok(())
            }
        }
    }
}

thread_local! {
    static FAULT: RefCell<Option<FaultState>> = const { RefCell::new(None) };
}

fn after_copy(prefix: &UnpublishedPrefix) {
    FAULT.with(|slot| slot.borrow_mut().as_mut().unwrap().apply(prefix));
}

struct FaultGuard;

impl FaultGuard {
    fn install(root: &Path, mutation: Mutation) -> Self {
        FAULT.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(FaultState {
                owned_root: root.into(),
                mutation,
                fired: false,
            });
        });
        tree::AFTER_WHEEL_COPY.with(|slot| {
            assert!(slot.replace(Some(after_copy)).is_none());
        });
        Self
    }

    fn assert_fired(&self) {
        assert!(FAULT.with(|slot| slot.borrow().as_ref().unwrap().fired));
        assert!(tree::AFTER_WHEEL_COPY.with(|slot| slot.get().is_none()));
    }
}

impl Drop for FaultGuard {
    fn drop(&mut self) {
        tree::AFTER_WHEEL_COPY.with(|slot| slot.set(None));
        let state = FAULT.with(|slot| slot.borrow_mut().take());
        if let Some(state) = state {
            if let Err(error) = state.restore() {
                if !std::thread::panicking() {
                    panic!("owned language fault could not be restored: {error}");
                }
            }
        }
    }
}

fn file_mutation(path: PathBuf) -> Mutation {
    let original = fs::read(&path).unwrap();
    Mutation::File { path, original }
}

fn assert_no_ready(context: &OperationContext, language: &LanguageLease, keys: &[[u8; 32]]) {
    assert!(manifest::enumerate_published(context).unwrap().is_empty());
    for key in keys {
        assert!(
            manifest::read_package(context, key).unwrap().is_none(),
            "language mutation must be rejected before any package becomes Ready"
        );
    }
    let key = super::super::layout_key(language.key().fingerprint(), GRAPH);
    let layout: Option<manifest::LayoutManifest> =
        manifest::read_json(context, &manifest::layout_name(&key)).unwrap();
    assert!(
        layout.is_none(),
        "a failed language batch must not publish a Ready layout"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn s4_language_batch_standalone_tree_rejects_mid_copy_language_content_change() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(120));
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    let (facts, archive) = sources(&context, &language, 1).pop().unwrap();
    let key = facts.key;
    let fault = FaultGuard::install(
        &root,
        file_mutation(language.root().join("lib/python3.15/stdlib-marker")),
    );
    let result = tree::prepare_package_tree(&context, &language, facts, archive);
    fault.assert_fired();
    assert!(
        result.is_err(),
        "a standalone tree writer must retain full language checks"
    );
    assert_no_ready(&context, &language, &[key]);
    drop(fault);
    language.check_binding().unwrap();
}

enum BatchFault {
    Content,
    Manifest,
    Namespace,
}

async fn rejects_batch_mutation(kind: BatchFault) {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, true, Duration::from_secs(120));
    let bytes = archive("3.15.0");
    let spec = install_spec(&bytes);
    let language = prepare(&spec, &context, &bytes).await.unwrap();
    let sources = sources(&context, &language, 3);
    let keys = sources
        .iter()
        .map(|(facts, _)| facts.key)
        .collect::<Vec<_>>();
    let catalog = context
        .requested_data_root()
        .join("languages/.control/catalog");
    let mutation = match kind {
        BatchFault::Content => file_mutation(language.root().join("lib/python3.15/stdlib-marker")),
        BatchFault::Manifest => {
            file_mutation(catalog.join(format!("{}.json", spec.key().filename())))
        }
        BatchFault::Namespace => Mutation::Namespace {
            displaced: catalog.with_file_name("catalog-language-batch-displaced"),
            path: catalog,
        },
    };
    let fault = FaultGuard::install(&root, mutation);
    let result = prepare_layout(&context, &language, GRAPH, sources);
    fault.assert_fired();
    assert!(
        result.is_err(),
        "changed language evidence must reject the entire pending batch"
    );
    assert_no_ready(&context, &language, &keys);
    drop(fault);
    language.check_binding().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn s4_language_batch_same_inode_same_size_content_change_is_rejected_before_ready() {
    rejects_batch_mutation(BatchFault::Content).await;
}

#[tokio::test(flavor = "current_thread")]
async fn s4_language_batch_manifest_change_is_rejected_before_ready() {
    rejects_batch_mutation(BatchFault::Manifest).await;
}

#[tokio::test(flavor = "current_thread")]
async fn s4_language_batch_namespace_change_is_rejected_before_ready() {
    rejects_batch_mutation(BatchFault::Namespace).await;
}
