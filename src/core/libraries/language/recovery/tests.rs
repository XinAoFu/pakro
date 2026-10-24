use super::*;
use crate::artifact_snapshot::{publish_verified_artifact, spool_verified_reader};
use crate::libraries::language::manifest::LanguageManifest;
use crate::libraries::language::repository::{
    LanguageInstallSpec, LanguageRepository, manifest_name, tests::*,
};
use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
use crate::plugins::core::python::native::{RuntimeArchiveTarget, RuntimePlatform};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

struct RecoveryFixture {
    _temporary: tempfile::TempDir,
    context: OperationContext,
    spec: LanguageInstallSpec,
    target: PublishedLanguage,
    before: LanguageManifest,
    before_bytes: Vec<u8>,
    archive: Vec<u8>,
}

fn bytecode_archive() -> Vec<u8> {
    let base = archive("3.11.17");
    let mut input = tar::Archive::new(GzDecoder::new(base.as_slice()));
    let mut output = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for entry in input.entries().unwrap() {
        let mut entry = entry.unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        output.append(entry.header(), bytes.as_slice()).unwrap();
    }
    for (index, path) in BYTECODE_PATHS.iter().enumerate() {
        let bytes = format!("fixture original bytecode {index}").into_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        output
            .append_data(&mut header, format!("bundle/{path}"), bytes.as_slice())
            .unwrap();
    }
    output.into_inner().unwrap().finish().unwrap()
}

impl RecoveryFixture {
    async fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let context = operation_context(&base, true, Duration::from_secs(60));
        let archive = bytecode_archive();
        let spec = install_spec_for(
            "3.11.17",
            &archive,
            &RuntimeArchiveTarget::new("aarch64", RuntimePlatform::Macos).unwrap(),
            "20261013",
        );
        drop(prepare(&spec, &context, &archive).await.unwrap());
        let cache = context.requested_cache_root().join("python/native-v1");
        let mut artifact = spool_verified_reader(
            archive.as_slice(),
            spec.plan().source().filename(),
            spec.plan().source().digest(),
            1024 * 1024,
            &cache,
        )
        .unwrap();
        publish_verified_artifact(&mut artifact, &cache, 1024 * 1024).unwrap();
        let target = LanguageRepository::open_readonly(&context)
            .unwrap()
            .enumerate_published()
            .unwrap()
            .remove(0);
        let before_bytes = fs::read(
            context
                .requested_data_root()
                .join("languages/.control/catalog")
                .join(manifest_name(&spec)),
        )
        .unwrap();
        let before = serde_json::from_slice(&before_bytes).unwrap();
        Self {
            _temporary: temporary,
            context,
            spec,
            target,
            before,
            before_bytes,
            archive,
        }
    }
    fn manifest_path(&self) -> PathBuf {
        self.context
            .requested_data_root()
            .join("languages/.control/catalog")
            .join(manifest_name(&self.spec))
    }
    fn damage(&self) {
        for (index, path) in BYTECODE_PATHS.iter().enumerate() {
            let destination = self.target.payload_root().join(path);
            let temporary = destination.with_extension("changed");
            fs::write(&temporary, format!("fixture recompiled bytecode {index}")).unwrap();
            fs::rename(temporary, destination).unwrap();
        }
    }
    fn assert_recovered(&self) {
        let current: LanguageManifest =
            serde_json::from_slice(&fs::read(self.manifest_path()).unwrap()).unwrap();
        for previous in self.before.inventory.entries() {
            let actual = current
                .inventory
                .entries()
                .iter()
                .find(|entry| entry.relative == previous.relative)
                .unwrap();
            if BYTECODE_PATHS
                .iter()
                .any(|path| Path::new(path) == previous.relative)
            {
                assert_eq!(actual.kind, previous.kind);
                assert_eq!(actual.mode, previous.mode);
                assert_eq!(actual.uid, previous.uid);
                assert_ne!(actual.identity, previous.identity);
                let TreeInventoryKind::File { sha256, .. } = actual.kind else {
                    panic!("recovered bytecode is not a file")
                };
                assert_eq!(
                    pakro_store::Digest::from_sha256_bytes(
                        &fs::read(self.target.payload_root().join(&actual.relative)).unwrap()
                    ),
                    pakro_store::Digest::Sha256(sha256),
                );
            } else {
                assert_eq!(actual, previous);
            }
        }
        assert_eq!(current.key, self.before.key);
        assert_eq!(current.target, self.before.target);
        assert_eq!(current.artifact, self.before.artifact);
        assert_eq!(current.descriptor, self.before.descriptor);
        assert_eq!(current.lock, self.before.lock);
        assert_eq!(current.allocation, self.before.allocation);
        assert_eq!(current.payload, self.before.payload);
        let repository = LanguageRepository::open_readonly(&self.context).unwrap();
        repository
            .acquire_by_key(&self.target.key())
            .unwrap()
            .unwrap();
        assert!(
            fs::read_dir(
                self.context
                    .requested_data_root()
                    .join("languages/.control/journals")
            )
            .unwrap()
            .next()
            .is_none()
        );
    }
}

#[tokio::test]
async fn f1_recovery_restores_only_four_archive_members_and_keeps_other_facts() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let report = restore_bytecode(&fixture.context, &fixture.target, false).unwrap();
    assert!(report.restored);
    assert!(!report.already_healthy);
    assert_eq!(report.paths.len(), 4);
    assert_eq!(
        report.unchanged_entries,
        fixture.before.inventory.entries().len() - 4
    );
    fixture.assert_recovered();
    let unchanged = fs::read(fixture.manifest_path()).unwrap();
    let report = restore_bytecode(&fixture.context, &fixture.target, false).unwrap();
    assert!(report.already_healthy);
    assert!(!report.restored);
    assert_eq!(fs::read(fixture.manifest_path()).unwrap(), unchanged);
}

#[tokio::test]
async fn f1_recovery_dry_run_has_no_library_effect() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let changed: Vec<_> = BYTECODE_PATHS
        .iter()
        .map(|path| fs::read(fixture.target.payload_root().join(path)).unwrap())
        .collect();
    let report = restore_bytecode(&fixture.context, &fixture.target, true).unwrap();
    assert!(!report.restored);
    assert!(!report.already_healthy);
    assert_eq!(
        fs::read(fixture.manifest_path()).unwrap(),
        fixture.before_bytes
    );
    for (path, before) in BYTECODE_PATHS.iter().zip(changed) {
        assert_eq!(
            fs::read(fixture.target.payload_root().join(path)).unwrap(),
            before
        );
    }
    assert!(
        fs::read_dir(
            fixture
                .context
                .requested_data_root()
                .join("languages/.control/journals")
        )
        .unwrap()
        .next()
        .is_none()
    );
}

#[tokio::test]
async fn f1_recovery_extra_damage_is_refused_before_payload_changes() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    fs::write(
        fixture
            .target
            .payload_root()
            .join("lib/python3.11/stdlib-marker"),
        b"preserve unknown modification",
    )
    .unwrap();
    let error = restore_bytecode(&fixture.context, &fixture.target, false).unwrap_err();
    assert!(error.to_string().contains("outside"), "{error:#}");
    assert_eq!(
        fs::read(fixture.manifest_path()).unwrap(),
        fixture.before_bytes
    );
}

#[tokio::test]
async fn f1_recovery_live_shared_lease_refuses_exclusive_mutation() {
    let fixture = RecoveryFixture::new().await;
    let lease = LanguageRepository::open_readonly(&fixture.context)
        .unwrap()
        .acquire_by_key(&fixture.target.key())
        .unwrap()
        .unwrap();
    fixture.damage();
    let error = restore_bytecode(&fixture.context, &fixture.target, false).unwrap_err();
    assert!(error.to_string().contains("in use"), "{error:#}");
    drop(lease);
    restore_bytecode(&fixture.context, &fixture.target, false).unwrap();
    fixture.assert_recovered();
}

#[tokio::test]
async fn f1_recovery_interrupted_capture_resumes_from_recorded_facts() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let error = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
        eyre::ensure!(
            step != RecoveryStep::AfterCapture(0),
            "injected capture stop"
        );
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().contains("injected"), "{error:#}");
    let report = restore_bytecode(&fixture.context, &fixture.target, false).unwrap();
    assert!(report.resumed);
    fixture.assert_recovered();
}

#[tokio::test]
async fn f1_recovery_every_durable_boundary_can_resume_without_adoption() {
    let mut steps = vec![
        RecoveryStep::AfterJournal,
        RecoveryStep::BeforeManifestCas,
        RecoveryStep::AfterManifestCas,
        RecoveryStep::BeforeJournalRemove,
    ];
    for index in 0..4 {
        steps.extend([
            RecoveryStep::AfterChmod(index),
            RecoveryStep::AfterCapture(index),
            RecoveryStep::AfterInstall(index),
            RecoveryStep::AfterBackupCleanup(index),
        ]);
    }
    for stop in steps {
        let fixture = RecoveryFixture::new().await;
        fixture.damage();
        let error = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
            eyre::ensure!(step != stop, "injected boundary stop {stop:?}");
            Ok(())
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("injected"),
            "stop {stop:?}: {error:#}"
        );
        let report = restore_bytecode(&fixture.context, &fixture.target, false).unwrap();
        assert!(report.resumed, "stop {stop:?}");
        fixture.assert_recovered();
    }
}

#[tokio::test]
async fn f1_recovery_stage_errors_clean_only_current_receipts_before_wal() {
    let mut stops: Vec<_> = (0..4).map(RecoveryStep::AfterStage).collect();
    stops.extend([
        RecoveryStep::BeforePreparedManifest,
        RecoveryStep::AfterPreparedManifest,
    ]);
    for stop in stops {
        let fixture = RecoveryFixture::new().await;
        fixture.damage();
        let damaged: Vec<_> = BYTECODE_PATHS
            .iter()
            .map(|path| fs::read(fixture.target.payload_root().join(path)).unwrap())
            .collect();
        let error = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
            eyre::ensure!(step != stop, "injected pre-WAL stage stop {stop:?}");
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("injected"), "{error:#}");
        assert!(
            fs::read_dir(
                fixture
                    .context
                    .requested_data_root()
                    .join("languages/.control/journals")
            )
            .unwrap()
            .next()
            .is_none()
        );
        assert_eq!(
            fs::read(fixture.manifest_path()).unwrap(),
            fixture.before_bytes
        );
        for (path, before) in BYTECODE_PATHS.iter().zip(damaged) {
            assert_eq!(
                fs::read(fixture.target.payload_root().join(path)).unwrap(),
                before
            );
        }
        let report = restore_bytecode(&fixture.context, &fixture.target, false).unwrap();
        assert!(!report.resumed);
        fixture.assert_recovered();
    }
}

#[tokio::test]
async fn f1_recovery_prepared_manifest_inode_is_recorded_before_cas_and_survives_restart() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let stopped = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
        eyre::ensure!(
            step != RecoveryStep::AfterJournal,
            "injected prepared-WAL stop"
        );
        Ok(())
    })
    .unwrap_err();
    assert!(
        stopped.to_string().contains("injected prepared-WAL stop"),
        "{stopped:#}"
    );
    let journals = fixture
        .context
        .requested_data_root()
        .join("languages/.control/journals");
    let journal: RecoveryJournal =
        serde_json::from_slice(&fs::read(journals.join(journal_name(&fixture.target))).unwrap())
            .unwrap();
    let prepared_path = journals.join(&journal.prepared.name);
    let prepared = fs::File::open(&prepared_path).unwrap();
    assert_eq!(
        FileIdentity::of(&prepared).unwrap(),
        journal.prepared.identity.file_identity()
    );
    assert_eq!(
        <[u8; 32]>::from(Sha256::digest(fs::read(&prepared_path).unwrap())),
        journal.prepared.sha256
    );
    assert_eq!(
        fs::read(fixture.manifest_path()).unwrap(),
        fixture.before_bytes
    );
    for (index, path) in BYTECODE_PATHS.iter().enumerate() {
        assert_eq!(
            fs::read(fixture.target.payload_root().join(path)).unwrap(),
            format!("fixture recompiled bytecode {index}").as_bytes()
        );
    }
    let stopped = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
        eyre::ensure!(
            step != RecoveryStep::AfterManifestCas,
            "injected prepared publication stop"
        );
        Ok(())
    })
    .unwrap_err();
    assert!(
        stopped
            .to_string()
            .contains("injected prepared publication stop"),
        "{stopped:#}"
    );
    assert!(!prepared_path.exists());
    assert_eq!(
        FileIdentity::of(&fs::File::open(fixture.manifest_path()).unwrap()).unwrap(),
        journal.prepared.identity.file_identity()
    );
    assert_eq!(fs::read(fixture.manifest_path()).unwrap(), {
        let mut bytes = Vec::new();
        (&prepared).read_to_end(&mut bytes).unwrap();
        bytes
    });
    let report = restore_bytecode(&fixture.context, &fixture.target, false).unwrap();
    assert!(report.resumed);
    fixture.assert_recovered();
}

#[tokio::test]
async fn f1_recovery_unknown_or_missing_prepared_manifest_is_refused_before_payload_effects() {
    for restart in [false, true] {
        for case in ["bytes", "inode", "missing"] {
            let fixture = RecoveryFixture::new().await;
            fixture.damage();
            let journals = fixture
                .context
                .requested_data_root()
                .join("languages/.control/journals");
            let change_prepared = || -> Result<PathBuf> {
                let journal: RecoveryJournal = serde_json::from_slice(&fs::read(
                    journals.join(journal_name(&fixture.target)),
                )?)?;
                let path = journals.join(journal.prepared.name);
                match case {
                    "bytes" => fs::write(&path, b"unknown prepared manifest")?,
                    "inode" => {
                        let old = fs::File::open(&path)?;
                        let replacement = journals.join("unknown-prepared-replacement");
                        fs::write(&replacement, fs::read(&path)?)?;
                        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))?;
                        fs::rename(replacement, &path)?;
                        assert_ne!(
                            FileIdentity::of(&old)?,
                            FileIdentity::of(&fs::File::open(&path)?)?
                        );
                    }
                    "missing" => fs::remove_file(&path)?,
                    _ => unreachable!(),
                }
                Ok(path)
            };
            let error = if restart {
                let stopped =
                    restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
                        eyre::ensure!(
                            step != RecoveryStep::AfterJournal,
                            "injected prepared inspection stop"
                        );
                        Ok(())
                    })
                    .unwrap_err();
                assert!(
                    stopped
                        .to_string()
                        .contains("injected prepared inspection stop"),
                    "{stopped:#}"
                );
                change_prepared().unwrap();
                restore_bytecode(&fixture.context, &fixture.target, false).unwrap_err()
            } else {
                restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
                    if step == RecoveryStep::AfterJournal {
                        change_prepared()?;
                    }
                    Ok(())
                })
                .unwrap_err()
            };
            assert!(
                error.to_string().contains("prepared"),
                "restart={restart} case={case}: {error:#}"
            );
            assert_eq!(
                fs::read(fixture.manifest_path()).unwrap(),
                fixture.before_bytes
            );
            assert!(journals.join(journal_name(&fixture.target)).is_file());
            for (index, path) in BYTECODE_PATHS.iter().enumerate() {
                assert_eq!(
                    fs::read(fixture.target.payload_root().join(path)).unwrap(),
                    format!("fixture recompiled bytecode {index}").as_bytes()
                );
            }
            let journal: RecoveryJournal = serde_json::from_slice(
                &fs::read(journals.join(journal_name(&fixture.target))).unwrap(),
            )
            .unwrap();
            let path = journals.join(journal.prepared.name);
            if case == "missing" {
                assert!(!path.exists());
            } else if case == "bytes" {
                assert_eq!(fs::read(path).unwrap(), b"unknown prepared manifest");
            } else {
                assert_eq!(
                    <[u8; 32]>::from(Sha256::digest(fs::read(path).unwrap())),
                    journal.prepared.sha256
                );
            }
        }
    }
}

#[tokio::test]
async fn f1_recovery_committed_retry_refuses_reintroduced_prepared_source() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let stopped = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
        eyre::ensure!(
            step != RecoveryStep::AfterManifestCas,
            "injected committed stop"
        );
        Ok(())
    })
    .unwrap_err();
    assert!(
        stopped.to_string().contains("injected committed stop"),
        "{stopped:#}"
    );
    let journals = fixture
        .context
        .requested_data_root()
        .join("languages/.control/journals");
    let journal: RecoveryJournal =
        serde_json::from_slice(&fs::read(journals.join(journal_name(&fixture.target))).unwrap())
            .unwrap();
    let path = journals.join(journal.prepared.name);
    fs::write(&path, b"unknown prepared source").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let manifest = fs::read(fixture.manifest_path()).unwrap();
    let error = restore_bytecode(&fixture.context, &fixture.target, false).unwrap_err();
    assert!(error.to_string().contains("prepared source"), "{error:#}");
    assert_eq!(fs::read(path).unwrap(), b"unknown prepared source");
    assert_eq!(fs::read(fixture.manifest_path()).unwrap(), manifest);
    assert!(journals.join(journal_name(&fixture.target)).is_file());
}

#[tokio::test]
async fn f1_recovery_unknown_stage_backup_and_parent_are_preserved_and_refused() {
    for case in ["stage", "stage_inode", "backup", "parent"] {
        let fixture = RecoveryFixture::new().await;
        fixture.damage();
        let stop = if case == "backup" {
            RecoveryStep::AfterCapture(0)
        } else {
            RecoveryStep::AfterJournal
        };
        restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
            eyre::ensure!(step != stop, "injected inspection stop");
            Ok(())
        })
        .unwrap_err();
        let journal_root = fixture
            .context
            .requested_data_root()
            .join("languages/.control/journals");
        let journal: RecoveryJournal = serde_json::from_slice(
            &fs::read(journal_root.join(journal_name(&fixture.target))).unwrap(),
        )
        .unwrap();
        let unknown = match case {
            "stage" => {
                let path = journal_root.join(&journal.files[0].stage);
                fs::write(&path, b"unknown stage content").unwrap();
                path
            }
            "stage_inode" => {
                let path = journal_root.join(&journal.files[0].stage);
                let replacement = journal_root.join("unknown-stage-replacement");
                fs::write(&replacement, fs::read(&path).unwrap()).unwrap();
                fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
                fs::rename(replacement, &path).unwrap();
                path
            }
            "backup" => {
                let path = journal_root.join(&journal.files[0].backup);
                fs::write(&path, b"unknown captured content").unwrap();
                path
            }
            "parent" => {
                let path = fixture
                    .target
                    .payload_root()
                    .join("lib/python3.11/encodings/__pycache__");
                let replacement = fixture._temporary.path().join("replacement-parent");
                fs::create_dir(&replacement).unwrap();
                for child in fs::read_dir(&path).unwrap() {
                    let child = child.unwrap();
                    fs::rename(child.path(), replacement.join(child.file_name())).unwrap();
                }
                fs::remove_dir(&path).unwrap();
                fs::rename(replacement, &path).unwrap();
                path.join("__init__.cpython-311.pyc")
            }
            _ => unreachable!(),
        };
        let before = fs::read(&unknown).unwrap();
        assert!(
            restore_bytecode(&fixture.context, &fixture.target, false).is_err(),
            "{case}"
        );
        assert_eq!(fs::read(&unknown).unwrap(), before, "{case}");
        assert_eq!(
            fs::read(fixture.manifest_path()).unwrap(),
            fixture.before_bytes,
            "{case}"
        );
    }
}

#[tokio::test]
async fn f1_recovery_control_inode_replacement_is_detected_before_next_effect() {
    for stop in [RecoveryStep::AfterJournal, RecoveryStep::AfterManifestCas] {
        let fixture = RecoveryFixture::new().await;
        fixture.damage();
        let error = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
            if step == stop {
                let path = if stop == RecoveryStep::AfterJournal {
                    fixture
                        .context
                        .requested_data_root()
                        .join("languages/.control/journals")
                        .join(journal_name(&fixture.target))
                } else {
                    fixture.manifest_path()
                };
                let replacement = path.with_extension("replacement");
                fs::write(&replacement, fs::read(&path)?)?;
                fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))?;
                fs::rename(replacement, path)?;
            }
            Ok(())
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("identity") || error.to_string().contains("changed"),
            "{error:#}"
        );
        if stop == RecoveryStep::AfterJournal {
            for (index, path) in BYTECODE_PATHS.iter().enumerate() {
                assert_eq!(
                    fs::read(fixture.target.payload_root().join(path)).unwrap(),
                    format!("fixture recompiled bytecode {index}").as_bytes()
                );
            }
        }
        // The fixed WAL remains available; unknown metadata was not removed.
        assert!(
            fixture
                .context
                .requested_data_root()
                .join("languages/.control/journals")
                .join(journal_name(&fixture.target))
                .is_file()
        );
    }
}

#[tokio::test]
async fn f1_recovery_restart_rejects_same_bytes_new_published_manifest_inode() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let stopped = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
        eyre::ensure!(
            step != RecoveryStep::AfterManifestCas,
            "injected after-CAS stop"
        );
        Ok(())
    })
    .unwrap_err();
    assert!(
        stopped.to_string().contains("injected after-CAS stop"),
        "{stopped:#}"
    );
    let path = fixture.manifest_path();
    let bytes = fs::read(&path).unwrap();
    let old_pin = fs::File::open(&path).unwrap();
    let replacement = fixture
        ._temporary
        .path()
        .join("replacement-published-manifest");
    fs::write(&replacement, &bytes).unwrap();
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(replacement, &path).unwrap();
    assert_ne!(
        old_pin.metadata().unwrap().ino(),
        fs::metadata(&path).unwrap().ino()
    );
    let error = restore_bytecode(&fixture.context, &fixture.target, false).unwrap_err();
    assert!(
        error.to_string().contains("identity") || error.to_string().contains("inode"),
        "{error:#}"
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(
        fixture
            .context
            .requested_data_root()
            .join("languages/.control/journals")
            .join(journal_name(&fixture.target))
            .is_file()
    );
}

#[tokio::test]
async fn f1_recovery_restart_rejects_same_bytes_new_original_manifest_inode() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let stopped = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
        eyre::ensure!(
            step != RecoveryStep::AfterCapture(0),
            "injected capture stop"
        );
        Ok(())
    })
    .unwrap_err();
    assert!(
        stopped.to_string().contains("injected capture stop"),
        "{stopped:#}"
    );
    let path = fixture.manifest_path();
    let bytes = fs::read(&path).unwrap();
    let old_pin = fs::File::open(&path).unwrap();
    let replacement = fixture
        ._temporary
        .path()
        .join("replacement-original-manifest");
    fs::write(&replacement, &bytes).unwrap();
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(replacement, &path).unwrap();
    assert_ne!(
        old_pin.metadata().unwrap().ino(),
        fs::metadata(&path).unwrap().ino()
    );
    let error = restore_bytecode(&fixture.context, &fixture.target, false).unwrap_err();
    assert!(
        error.to_string().contains("identity") || error.to_string().contains("inode"),
        "{error:#}"
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[tokio::test]
async fn f1_recovery_rejects_unknown_type_permissions_links_and_entries() {
    for case in [
        "extra",
        "missing",
        "directory",
        "symlink",
        "hardlink",
        "mode",
    ] {
        let fixture = RecoveryFixture::new().await;
        fixture.damage();
        let pyc = fixture.target.payload_root().join(BYTECODE_PATHS[0]);
        match case {
            "extra" => {
                fs::write(fixture.target.payload_root().join("untracked"), b"unknown").unwrap()
            }
            "missing" => fs::remove_file(
                fixture
                    .target
                    .payload_root()
                    .join("lib/python3.11/stdlib-marker"),
            )
            .unwrap(),
            "directory" => {
                fs::remove_file(&pyc).unwrap();
                fs::create_dir(&pyc).unwrap();
            }
            "symlink" => {
                fs::remove_file(&pyc).unwrap();
                std::os::unix::fs::symlink("../aliases.cpython-311.pyc", &pyc).unwrap();
            }
            "hardlink" => {
                fs::hard_link(&pyc, fixture._temporary.path().join("outside-link")).unwrap()
            }
            "mode" => fs::set_permissions(&pyc, fs::Permissions::from_mode(0o600)).unwrap(),
            _ => unreachable!(),
        }
        assert!(
            restore_bytecode(&fixture.context, &fixture.target, false).is_err(),
            "case {case}"
        );
        assert_eq!(
            fs::read(fixture.manifest_path()).unwrap(),
            fixture.before_bytes,
            "case {case}"
        );
        assert!(
            fs::read_dir(
                fixture
                    .context
                    .requested_data_root()
                    .join("languages/.control/journals")
            )
            .unwrap()
            .next()
            .is_none(),
            "case {case}"
        );
    }
}

#[tokio::test]
async fn f1_recovery_missing_or_corrupt_archive_is_refused_without_download() {
    for missing in [false, true] {
        let fixture = RecoveryFixture::new().await;
        fixture.damage();
        let cache = fixture
            .context
            .requested_cache_root()
            .join("python/native-v1/artifacts")
            .join(fixture.spec.plan().source().digest().to_string())
            .join(fixture.spec.plan().source().filename());
        if missing {
            fs::remove_file(cache).unwrap();
        } else {
            fs::write(cache, b"unknown archive").unwrap();
        }
        assert!(restore_bytecode(&fixture.context, &fixture.target, false).is_err());
        assert_eq!(
            fs::read(fixture.manifest_path()).unwrap(),
            fixture.before_bytes
        );
        assert!(
            fs::read_dir(
                fixture
                    .context
                    .requested_data_root()
                    .join("languages/.control/journals")
            )
            .unwrap()
            .next()
            .is_none()
        );
    }
}

#[tokio::test]
async fn f1_recovery_archive_members_must_match_original_published_hashes() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let mut value: serde_json::Value = serde_json::from_slice(&fixture.before_bytes).unwrap();
    let entry = value["inventory"]["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["relative"] == BYTECODE_PATHS[0])
        .unwrap();
    entry["kind"]["sha256"] = serde_json::json!(vec![0_u8; 32]);
    fs::write(fixture.manifest_path(), serde_json::to_vec(&value).unwrap()).unwrap();
    let before = fs::read(fixture.manifest_path()).unwrap();
    let error = restore_bytecode(&fixture.context, &fixture.target, false).unwrap_err();
    assert!(error.to_string().contains("archive"), "{error:#}");
    assert_eq!(fs::read(fixture.manifest_path()).unwrap(), before);
    assert!(
        fs::read_dir(
            fixture
                .context
                .requested_data_root()
                .join("languages/.control/journals")
        )
        .unwrap()
        .next()
        .is_none()
    );
}

#[tokio::test]
async fn f1_recovery_manifest_change_before_cas_is_refused_and_not_adopted() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let mut changed = fixture.before_bytes.clone();
    changed.push(b' ');
    let error = restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
        if step == RecoveryStep::BeforeManifestCas {
            fs::write(fixture.manifest_path(), &changed)?;
        }
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().contains("manifest"), "{error:#}");
    assert_eq!(fs::read(fixture.manifest_path()).unwrap(), changed);
    assert!(restore_bytecode(&fixture.context, &fixture.target, false).is_err());
    // Only the fixture restores the exact prior bytes on its retained inode.
    fs::write(fixture.manifest_path(), &fixture.before_bytes).unwrap();
    restore_bytecode(&fixture.context, &fixture.target, false).unwrap();
    fixture.assert_recovered();
}

#[tokio::test]
async fn f1_recovery_retry_rejects_unknown_target_and_preserves_it() {
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    restore_bytecode_inner(&fixture.context, &fixture.target, false, &mut |step| {
        eyre::ensure!(
            step != RecoveryStep::AfterCapture(0),
            "injected capture stop"
        );
        Ok(())
    })
    .unwrap_err();
    let target = fixture.target.payload_root().join(BYTECODE_PATHS[0]);
    fs::write(&target, b"unknown concurrent target").unwrap();
    assert!(restore_bytecode(&fixture.context, &fixture.target, false).is_err());
    assert_eq!(fs::read(target).unwrap(), b"unknown concurrent target");
    assert_eq!(
        fs::read(fixture.manifest_path()).unwrap(),
        fixture.before_bytes
    );
}

#[tokio::test]
async fn f1_recovery_dry_run_needs_no_write_policy_and_root_comparison_is_strict() {
    use crate::programs::{OperationContextInputs, capture_operation_context};
    let fixture = RecoveryFixture::new().await;
    fixture.damage();
    let mut policy = *fixture.context.policy();
    policy.writes.libraries = false;
    let readonly = capture_operation_context(OperationContextInputs {
        settings: fixture.context.settings(),
        policy,
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: fixture.context.requested_data_root().to_owned(),
        cache_root: fixture.context.requested_cache_root().to_owned(),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    restore_bytecode(&readonly, &fixture.target, true).unwrap();
    assert!(restore_bytecode(&readonly, &fixture.target, false).is_err());
    let baseline = &fixture.before.inventory;
    for field in ["root", "allocation", "identity", "uid", "mode"] {
        let mut value = serde_json::to_value(baseline).unwrap();
        value[field] = match field {
            "root" | "allocation" => serde_json::json!("unknown"),
            "identity" => serde_json::json!([0, 0]),
            "uid" => serde_json::json!(99999),
            _ => serde_json::json!(0),
        };
        let changed: crate::packages::python_environment::prefix::verify::TreeInventory =
            serde_json::from_value(value).unwrap();
        assert!(
            baseline
                .ensure_same_root_and_unaffected_entries(&changed, &paths())
                .is_err(),
            "{field}"
        );
    }
}

/// Explicit local-input producer for the real CLI test. It refuses nonempty
/// destinations and never uses the user's published library or a network.
#[tokio::test]
#[ignore = "requires the original verified local 3.11.17 PBS archive and isolated roots"]
async fn stage_real_bytecode_recovery() {
    use crate::config::Settings;
    use crate::lockfile::PlatformInfo;
    use crate::plugins::core::python::native::{RuntimeFlavor, pipeline::RuntimeInstallPlan};
    use crate::programs::install::runtime::prepare_language;
    use crate::programs::{OperationContextInputs, capture_operation_context};
    let source = PathBuf::from(
        std::env::var_os("PAKRO_BYTECODE_RECOVERY_ARCHIVE")
            .expect("explicit local archive required"),
    );
    // The unit-test constructor owns and resets PAKRO_DATA_DIR/CACHE_DIR.
    // Explicit producer destinations must survive that initialization.
    let data = PathBuf::from(
        std::env::var_os("PAKRO_BYTECODE_RECOVERY_STAGE_DATA_ROOT")
            .expect("explicit isolated data root required"),
    );
    let cache = PathBuf::from(
        std::env::var_os("PAKRO_BYTECODE_RECOVERY_STAGE_CACHE_ROOT")
            .expect("explicit isolated cache root required"),
    );
    for path in [&data, &cache] {
        assert!(path.is_absolute());
        if path.exists() {
            assert!(
                fs::read_dir(path).unwrap().next().is_none(),
                "helper requires an empty isolated root"
            );
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let initial = operation_context(
        &temporary.path().canonicalize().unwrap(),
        true,
        Duration::from_secs(120),
    );
    let context = capture_operation_context(OperationContextInputs {
        settings: &Settings::default(),
        policy: *initial.policy(),
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: data,
        cache_root: cache,
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let filename = "cpython-3.11.17+20261013-aarch64-apple-darwin-install_only.tar.gz";
    let digest: pakro_store::Digest =
        "sha256-d81d757a9785850393cd695a9a686c765a6370fbdc83738589ef2ecd3e5c44b3"
            .parse()
            .unwrap();
    let plan = RuntimeInstallPlan::from_locked("3.11.17", &RuntimeArchiveTarget::new("aarch64", RuntimePlatform::Macos).unwrap(),
        RuntimeFlavor::InstallOnly, &PlatformInfo { url: Some(format!("https://github.com/astral-sh/python-build-standalone/releases/download/20261013/{filename}")),
            checksum: Some(digest.to_string().replacen('-', ":", 1)), ..Default::default() }).unwrap();
    let spec = LanguageInstallSpec::from_plan(&plan, true).unwrap();
    assert_eq!(
        spec.key().filename(),
        "language-670a04e9cf8e3636a5fd4a9fcfcd15e2d2153f1b1acb7be6e0ebd66e9018244e"
    );
    let native_cache = context.requested_cache_root().join("python/native-v1");
    let lease = prepare_language(&spec, &context, || async {
        let mut artifact = spool_verified_reader(
            fs::File::open(&source)?,
            filename,
            digest,
            MAX_ARCHIVE_BYTES,
            &native_cache,
        )?;
        publish_verified_artifact(&mut artifact, &native_cache, MAX_ARCHIVE_BYTES)?;
        Ok(artifact)
    })
    .await
    .unwrap();
    println!("RECOVERY_STAGED_PYTHON={}", lease.entrypoint().display());
    println!(
        "RECOVERY_STAGED_ARCHIVE={}",
        native_cache
            .join("artifacts")
            .join(digest.to_string())
            .join(filename)
            .display()
    );
}
