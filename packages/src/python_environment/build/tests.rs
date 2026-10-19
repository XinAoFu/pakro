use super::*;
use crate::artifact_snapshot::spool_verified_reader;
use crate::packages::python_environment::journal::{
    JournalLimits, JournalPrepare, JournalRecord, JournalTarget, prepare_journal_metadata,
};
use crate::packages::python_environment::model::{
    InputKind, InputReference, ModelLimits, SelectionKey, SelectionStamp,
};
use crate::packages::python_environment::prefix::control::ControlLimits;
use crate::packages::python_environment::prefix::control::ownership::OwnershipStreamLimits;
use crate::packages::python_environment::prefix::control::ownership::events::{
    OwnershipEvent, OwnershipEventLimits, decode_ownership_event,
};
use crate::packages::python_environment::prefix::{
    EnvironmentStore, OwnershipObserverLimits, PrefixLimits,
};
use crate::packages::python_environment::runtime_layout::test_fixtures::{
    Flavor, descriptor, layout,
};
use crate::packages::python_environment::runtime_snapshot::{
    RuntimeSnapshotLimits, snapshot_runtime,
};
use crate::packages::python_environment::runtime_startup::{
    StartupCacheBinding, StartupFilePin, StartupLoaderKind,
};
use crate::packages::python_wheel::WheelExpectation;
use crate::packages::python_wheel_source::inspect_owned_wheel;
use crate::python_runtime::PythonRuntimeHandle;
use pakro_index::{SupportedWheelTags, WheelTag};
use pakro_store::Digest;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::fs;
use std::io::{Cursor, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

struct Fixture {
    _owner: tempfile::TempDir,
    raw_source: PathBuf,
    store: EnvironmentStore,
    snapshot: Option<OwnedRuntimeSnapshot>,
    prefix: Option<UnpublishedPrefix>,
    startup: StartupAdapterBinding,
    journal: JournalRecord,
    scratch: PathBuf,
    audit: PathBuf,
    active: PathBuf,
    cas: PathBuf,
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}
fn encode_printf(value: &impl serde::Serialize, executable: &Path) -> String {
    let encoded = serde_json::to_string(value).unwrap().replace(
        &format!(
            "\"executable\":{}",
            serde_json::to_string(executable).unwrap()
        ),
        "\"executable\":\"%s\"",
    );
    assert!(encoded.contains("%s"));
    encoded
        .replace('%', "%%")
        .replace("%%s", "%s")
        .replace('\'', "'\\''")
}

impl Fixture {
    async fn new() -> Self {
        Self::with_delay(false).await
    }
    async fn with_delay(delay: bool) -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let snapshot_store = EnvironmentStore::open(
            &root.join("runtimes"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        let snapshot_prefix = snapshot_store.allocate().unwrap();
        let runtime_root = snapshot_prefix.final_path().to_owned();
        let runtime_executable = runtime_root.join("bin/runtime");
        let store = EnvironmentStore::open(
            &root.join("environments"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap();
        let mut prefix = store.allocate().unwrap();
        let raw_source = root.join("raw-runtime");
        let source_stdlib = raw_source.join("lib/python3.11");
        fs::create_dir_all(raw_source.join("bin")).unwrap();
        fs::create_dir_all(source_stdlib.join("__pycache__")).unwrap();
        let site_bytes =
            b"import sys\ndef main():\n    pass\nif not sys.flags.no_site:\n    main()\n";
        fs::write(source_stdlib.join("site.py"), site_bytes).unwrap();
        fs::set_permissions(
            source_stdlib.join("site.py"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::write(
            raw_source.join("bin/loader.dylib"),
            b"fixed fixture loader\0",
        )
        .unwrap();
        fs::set_permissions(
            raw_source.join("bin/loader.dylib"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let selected = descriptor(&runtime_executable, Flavor::Cpython311);
        let mut observed = layout(
            &selected,
            &runtime_root,
            prefix.final_path(),
            Flavor::Cpython311,
        );
        observed["sys_path"]
            .as_array_mut()
            .unwrap()
            .push(json!(runtime_root.join("lib/python311.zip")));
        let source_path = runtime_root.join("lib/python3.11/site.py");
        let cache_path = runtime_root.join("lib/python3.11/__pycache__/site.fixture.pyc");
        let startup = StartupAdapterBinding {
            source: StartupFilePin {
                path: source_path.clone(),
                sha256: Sha256::digest(site_bytes).into(),
                size: site_bytes.len() as u64,
                mode: 0o644,
            },
            cache: Some(StartupCacheBinding::Missing {
                path: cache_path.clone(),
            }),
            loader: StartupLoaderKind::Source,
        };
        let sites = vec![observed["target_paths"]["purelib"].clone()];
        let intercepted = json!({
            "schema":1,"observation_mode":"cfg_recognition_intercepted",
            "executable":prefix.final_path().join("bin/python"),"prefix":prefix.final_path(),"exec_prefix":prefix.final_path(),
            "base_prefix":runtime_root,"base_exec_prefix":runtime_root,"loader":"source","site_origin":source_path,"cache_path":cache_path,
            "source_sha256":hex::encode(startup.source.sha256),"cache_sha256":null,"code_selection":"source",
            "cfg_path":prefix.final_path().join("pyvenv.cfg"),"home":runtime_root.join("bin"),"user_site_enabled":false,
            "no_site":true,"ignore_environment":true,"dont_write_bytecode":true,"requested_sites":sites,
            "suppressed_hooks":["addsitepackages","addsitedir","addusersitepackages","execsitecustomize","execusercustomize","enablerlcompleter"],
            "sys_path":observed["sys_path"],"default_paths":observed["base_paths"],"final_paths":observed["target_paths"]
        });
        let mut normal = intercepted.clone();
        normal["observation_mode"] = json!("normal_empty_environment");
        normal["no_site"] = json!(false);
        normal["suppressed_hooks"] = json!([]);
        let mut normal_paths = observed["sys_path"].as_array().unwrap().clone();
        normal_paths.extend(sites);
        normal["sys_path"] = json!(normal_paths);
        fs::write(
            raw_source.join("startup.json"),
            serde_json::to_vec(&intercepted).unwrap(),
        )
        .unwrap();
        fs::write(
            raw_source.join("normal.json"),
            serde_json::to_vec(&normal).unwrap(),
        )
        .unwrap();
        let audit = root.join("audit");
        let pause = if delay { "/bin/sleep 0.08\n" } else { "" };
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$0\" >> {}\n{pause}case \"$*\" in\n*PAKRO_RUNTIME_STARTUP_V1*) /bin/cat >/dev/null; /bin/cat {};;\n*PAKRO_RUNTIME_EMPTY_V1*) /bin/cat >/dev/null; /bin/cat {};;\n*PAKRO_RUNTIME_LAYOUT_V1*) printf '{}\\n' \"$0\";;\n*) printf '{}\\n' \"$0\";;\nesac\n",
            quote(&audit),
            quote(&runtime_root.join("startup.json")),
            quote(&runtime_root.join("normal.json")),
            encode_printf(&observed, &runtime_executable),
            encode_printf(&selected, &runtime_executable)
        );
        fs::write(raw_source.join("bin/runtime"), script).unwrap();
        fs::set_permissions(
            raw_source.join("bin/runtime"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let snapshot = snapshot_runtime(
            &raw_source,
            Path::new("bin/runtime"),
            snapshot_prefix,
            RuntimeSnapshotLimits::default(),
        )
        .expect("actual complete snapshot prerequisite");
        let handle = PythonRuntimeHandle::from_existing_executable(snapshot.entrypoint()).unwrap();
        let actual = crate::python_runtime::probe(&handle, Duration::from_secs(5))
            .await
            .expect("actual selected snapshot protocol prerequisite");
        assert_eq!(actual, selected);
        let raw_layout = crate::python_runtime::probe_layout(
            &handle,
            prefix.final_path(),
            1024 * 1024,
            64 * 1024,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .unwrap();
        crate::packages::python_environment::runtime_layout::decode_runtime_layout(
            &raw_layout,
            &actual,
            prefix.final_path(),
            RuntimeLayoutLimits::default(),
        )
        .expect("actual snapshot layout protocol prerequisite");
        fs::write(&audit, []).unwrap();
        let identity = prefix.generation_identity().unwrap();
        let key = SelectionKey::new(
            uuid::Uuid::from_u128(1),
            "local-build",
            [3; 32],
            ModelLimits::default(),
        )
        .unwrap();
        let expected = SelectionStamp::default();
        let refs = [InputReference::new(
            InputKind::Runtime,
            Digest::Sha256(*snapshot.content_tree_sha256()),
        )];
        let journal = store
            .with_control(ControlLimits::default(), |control| {
                let journal = prepare_journal_metadata(
                    control,
                    &JournalPrepare {
                        nonce: uuid::Uuid::from_u128(101),
                        key: &key,
                        expected: &expected,
                        target: JournalTarget::Generation {
                            identity: &identity,
                            graph: [4; 32],
                            inputs: &refs,
                        },
                    },
                    JournalLimits::default(),
                )?;
                control.attach_prefix_ownership(
                    &mut prefix,
                    &journal,
                    JournalLimits::default(),
                    OwnershipObserverLimits::default(),
                )?;
                Ok(journal)
            })
            .expect("real journal and ownership observer prerequisite");
        let scratch = root.join("scratch");
        fs::create_dir(&scratch).unwrap();
        let active = root.join("active-sentinel");
        let cas = root.join("cas-sentinel");
        fs::write(&active, b"old active").unwrap();
        fs::write(&cas, b"old CAS").unwrap();
        Self {
            _owner: owner,
            raw_source,
            store,
            snapshot: Some(snapshot),
            prefix: Some(prefix),
            startup,
            journal,
            scratch,
            audit,
            active,
            cas,
        }
    }
    fn archive(
        &self,
        name: &str,
        foreign: bool,
        requires_python: Option<&str>,
    ) -> InspectedWheelArchive {
        let stem = name.replace('-', "_");
        let info = format!("{stem}-1.0.dist-info");
        let data = format!("{stem}-1.0.data");
        let tag = if foreign {
            WheelTag::new("cp999", "cp999", "macosx_99_0_arm64").unwrap()
        } else {
            WheelTag::new("py3", "none", "any").unwrap()
        };
        let tag_text = format!("{}-{}-{}", tag.interpreter(), tag.abi(), tag.platform());
        let mut metadata = format!("Metadata-Version: 2.6\nName: {name}\nVersion: 1.0\n");
        if let Some(requires) = requires_python {
            metadata.push_str(&format!("Requires-Python: {requires}\n"));
        }
        let mut entries = vec![
            (
                format!("{stem}.py"),
                b"VALUE=42\ndef main():\n    return 0\n".to_vec(),
                0o644,
            ),
            (
                format!("{data}/platlib/{stem}_plat.py"),
                b"PLAT=1\n".to_vec(),
                0o644,
            ),
            (
                format!("{data}/headers/{stem}.h"),
                b"#define LOCAL 42\n".to_vec(),
                0o644,
            ),
            (
                format!("{data}/data/share/{stem}/data.txt"),
                b"owned data\n".to_vec(),
                0o644,
            ),
            (
                format!("{data}/scripts/{stem}-legacy"),
                b"#!python\nprint('legacy')\n".to_vec(),
                0o755,
            ),
            (
                format!("{info}/WHEEL"),
                format!("Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: {tag_text}\n")
                    .into_bytes(),
                0o644,
            ),
            (format!("{info}/METADATA"), metadata.into_bytes(), 0o644),
            (
                format!("{info}/entry_points.txt"),
                format!("[console_scripts]\n{name}-cli = {stem}:main\n").into_bytes(),
                0o644,
            ),
        ];
        let mut record = String::new();
        for (path, bytes, _) in &entries {
            let hash = base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                Sha256::digest(bytes),
            );
            record.push_str(&format!("{path},sha256={hash},{}\n", bytes.len()));
        }
        record.push_str(&format!("{info}/RECORD,,\n"));
        entries.push((format!("{info}/RECORD"), record.into_bytes(), 0o644));
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        // These are charged by the original directory receipt but omitted
        // from the regular-file manifest. Batch intake must count both.
        zip.add_directory(
            "empty-one/",
            zip::write::SimpleFileOptions::default().unix_permissions(0o755),
        )
        .unwrap();
        zip.add_directory(
            "empty-two/",
            zip::write::SimpleFileOptions::default().unix_permissions(0o755),
        )
        .unwrap();
        for (path, bytes, mode) in entries {
            zip.start_file(
                path,
                zip::write::SimpleFileOptions::default().unix_permissions(mode),
            )
            .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let digest = Digest::from_sha256_bytes(&bytes);
        let filename = format!("{stem}-1.0-{tag_text}.whl");
        let source = spool_verified_reader(
            Cursor::new(&bytes),
            &filename,
            digest,
            bytes.len() as u64,
            &self.scratch,
        )
        .expect("actual owned outer source prerequisite");
        inspect_owned_wheel(
            source,
            digest,
            &WheelExpectation {
                filename: &filename,
                name,
                version: "1.0",
                supported: &SupportedWheelTags::new(vec![tag]).unwrap(),
            },
            WheelLimits::default(),
        )
        .expect("actual A/C1 prerequisite, possibly inspected for a different target")
    }
    async fn build(
        &mut self,
        archives: Vec<InspectedWheelArchive>,
        limits: EnvironmentBuildLimits,
        deadline: Instant,
    ) -> Result<PreparedEnvironmentBuild> {
        prepare_environment_build(
            self.snapshot.take().unwrap(),
            archives,
            self.prefix.take().unwrap(),
            self.startup.clone(),
            limits,
            deadline,
        )
        .await
    }
    fn unchanged(&self) {
        assert_eq!(fs::read(&self.active).unwrap(), b"old active");
        assert_eq!(fs::read(&self.cas).unwrap(), b"old CAS");
    }
    fn no_environment_creates(&self) {
        let log = self
            .store
            .with_control(ControlLimits::default(), |control| {
                control.read_ownership_stream(
                    &self.journal,
                    JournalLimits::default(),
                    OwnershipStreamLimits::default(),
                )
            })
            .unwrap()
            .unwrap();
        assert!(
            !log.records().iter().any(|record| matches!(
                decode_ownership_event(record.payload(), OwnershipEventLimits::default()).unwrap(),
                OwnershipEvent::CreateIntent { .. }
            )),
            "local build wrote environment output before its preflight gate"
        );
        self.unchanged();
    }
}

#[tokio::test]
async fn a_complete_five_scheme_batch_produces_a_closed_local_build_with_real_seal_facts() {
    let mut fixture = Fixture::new().await;
    let archive = fixture.archive("alpha", false, Some(">=3.11,<3.12"));
    let runtime_root = fixture.snapshot.as_ref().unwrap().root().to_owned();
    let content = *fixture.snapshot.as_ref().unwrap().content_tree_sha256();
    let allocation = fixture.prefix.as_ref().unwrap().allocation_id().to_owned();
    let prepared = fixture
        .build(
            vec![archive],
            EnvironmentBuildLimits::default(),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .expect("production local build is required");
    assert_eq!(prepared.allocation_id(), allocation);
    assert_eq!(prepared.runtime_root(), runtime_root);
    assert_eq!(prepared.runtime_content_tree_sha256(), &content);
    assert_ne!(prepared.runtime_inventory_sha256(), &[0; 32]);
    assert_eq!(
        prepared.executable(),
        prepared.final_prefix().join("bin/python")
    );
    assert_eq!(prepared.sources().len(), 1);
    assert!(
        fs::read_to_string(&fixture.audit)
            .unwrap()
            .lines()
            .any(|path| Path::new(path) == prepared.executable())
    );
    for suffix in [
        "alpha.py",
        "alpha_plat.py",
        "alpha.h",
        "share/alpha/data.txt",
        "alpha-legacy",
        "alpha-cli",
    ] {
        assert!(
            prepared
                .verification()
                .files()
                .iter()
                .any(|file| file.relative_path().to_string_lossy().ends_with(suffix)),
            "missing {suffix}"
        );
    }
    assert!(
        fs::read_to_string(prepared.final_prefix().join("bin/alpha-cli"))
            .unwrap()
            .contains(prepared.executable().to_str().unwrap())
    );
    fixture.unchanged();
}

#[tokio::test]
async fn a_local_build_uses_only_the_snapshot_after_the_original_source_tree_is_removed() {
    let mut fixture = Fixture::new().await;
    let archive = fixture.archive("alpha", false, None);
    fs::remove_dir_all(&fixture.raw_source).unwrap();
    let prepared = fixture
        .build(
            vec![archive],
            EnvironmentBuildLimits::default(),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert!(prepared.final_prefix().join("bin/alpha-cli").is_file());
    assert!(
        prepared
            .runtime_root()
            .join("lib/python3.11/site.py")
            .is_file()
    );
    fixture.unchanged();
}

#[tokio::test]
async fn an_archive_inspected_for_another_target_is_rejected_before_any_environment_create() {
    let mut fixture = Fixture::new().await;
    let archive = fixture.archive("foreign", true, None);
    assert!(
        fixture
            .build(
                vec![archive],
                EnvironmentBuildLimits::default(),
                Instant::now() + Duration::from_secs(10)
            )
            .await
            .is_err()
    );
    fixture.no_environment_creates();
}

#[tokio::test]
async fn requires_python_is_rechecked_against_this_snapshot_runtime_before_writes() {
    for requires in [">=99", "invalid constraint"] {
        let mut fixture = Fixture::new().await;
        let archive = fixture.archive("alpha", false, Some(requires));
        assert!(
            fixture
                .build(
                    vec![archive],
                    EnvironmentBuildLimits::default(),
                    Instant::now() + Duration::from_secs(10)
                )
                .await
                .is_err()
        );
        fixture.no_environment_creates();
    }
}

#[tokio::test]
async fn the_entire_owned_archive_batch_is_charged_before_environment_writes() {
    for limits in [
        EnvironmentBuildLimits {
            max_archives: 1,
            ..EnvironmentBuildLimits::default()
        },
        EnvironmentBuildLimits {
            max_total_archive_bytes: 1,
            ..EnvironmentBuildLimits::default()
        },
        EnvironmentBuildLimits {
            max_total_members: 1,
            ..EnvironmentBuildLimits::default()
        },
        EnvironmentBuildLimits {
            max_total_members: 18,
            ..EnvironmentBuildLimits::default()
        },
        EnvironmentBuildLimits {
            max_total_uncompressed_bytes: 1,
            ..EnvironmentBuildLimits::default()
        },
        EnvironmentBuildLimits {
            wheel: WheelLimits {
                max_members: 1,
                ..WheelLimits::default()
            },
            ..EnvironmentBuildLimits::default()
        },
    ] {
        let mut fixture = Fixture::new().await;
        let archives = vec![
            fixture.archive("alpha", false, None),
            fixture.archive("beta", false, None),
        ];
        assert!(
            fixture
                .build(archives, limits, Instant::now() + Duration::from_secs(10))
                .await
                .is_err()
        );
        fixture.no_environment_creates();
    }
}

#[tokio::test]
async fn startup_binding_must_describe_the_actual_owned_snapshot_before_environment_writes() {
    for foreign in [false, true] {
        let mut fixture = Fixture::new().await;
        let archive = fixture.archive("alpha", false, None);
        if foreign {
            fixture.startup.source.path = fixture.raw_source.join("lib/python3.11/site.py");
        } else {
            fixture.startup.source.sha256[0] ^= 1;
        }
        assert!(
            fixture
                .build(
                    vec![archive],
                    EnvironmentBuildLimits::default(),
                    Instant::now() + Duration::from_secs(10)
                )
                .await
                .is_err()
        );
        fixture.no_environment_creates();
    }
}

#[tokio::test]
async fn an_expired_or_shared_deadline_cannot_be_reset_by_sequential_build_phases() {
    for delayed in [false, true] {
        let mut fixture = Fixture::with_delay(delayed).await;
        let archive = fixture.archive("alpha", false, None);
        let deadline = if delayed {
            Instant::now() + Duration::from_millis(200)
        } else {
            Instant::now() - Duration::from_millis(1)
        };
        let started = Instant::now();
        assert!(
            fixture
                .build(vec![archive], EnvironmentBuildLimits::default(), deadline)
                .await
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        fixture.no_environment_creates();
    }
}

#[tokio::test]
async fn the_prepared_build_keeps_both_real_roots_alive_until_its_drop() {
    let mut fixture = Fixture::new().await;
    let archive = fixture.archive("alpha", false, None);
    let runtime = fixture.snapshot.as_ref().unwrap().root().to_owned();
    let environment = fixture.prefix.as_ref().unwrap().final_path().to_owned();
    let prepared = fixture
        .build(
            vec![archive],
            EnvironmentBuildLimits::default(),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert!(runtime.is_dir() && environment.is_dir());
    drop(prepared);
    assert!(!runtime.exists() && !environment.exists());
    fixture.unchanged();
}

// Explicit host inputs; these tests run no manager and never mutate the source.
async fn real_snapshot_build(source: &Path, executable: &Path) {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let store = |name: &str| {
        EnvironmentStore::open(
            &root.join(name),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap()
    };
    let snapshots = store("snapshots");
    let environments = store("environments");
    let snapshot = snapshot_runtime(
        source,
        executable,
        snapshots.allocate().unwrap(),
        RuntimeSnapshotLimits::default(),
    )
    .expect("complete actual runtime snapshot prerequisite");
    let prefix = environments.allocate().unwrap();
    let handle = PythonRuntimeHandle::from_existing_executable(snapshot.entrypoint()).unwrap();
    let descriptor = crate::python_runtime::probe(&handle, Duration::from_secs(10))
        .await
        .unwrap();
    let raw = crate::python_runtime::probe_layout(
        &handle,
        prefix.final_path(),
        1024 * 1024,
        64 * 1024,
        Instant::now() + Duration::from_secs(10),
    )
    .await
    .unwrap();
    let observed = crate::packages::python_environment::runtime_layout::decode_runtime_layout(
        &raw,
        &descriptor,
        prefix.final_path(),
        RuntimeLayoutLimits::default(),
    )
    .unwrap();
    let site = observed.base_paths["stdlib"].join("site.py");
    // Observe the actual loader and cache naming under -S. This fixed helper
    // imports no user code and does not execute site or rewrite a user's CLI.
    let mut child = tokio::process::Command::new(snapshot.entrypoint());
    child.args(["-E", "-s", "-S", "-B", "-c",
        "import importlib.util,json,sys; s=importlib.util.find_spec('site'); print(json.dumps({'origin':s.origin,'cache':importlib.util.cache_from_source(sys.argv[1])}))"])
        .arg(&site).env_clear().env("LC_ALL", "C").current_dir(&root).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stdout.len() < 8192 && output.stderr.is_empty(),
        "{output:?}"
    );
    let loader: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let pin = |path: &Path| {
        let relative = path.strip_prefix(snapshot.root()).unwrap();
        let file = snapshot
            .files()
            .iter()
            .find(|file| file.relative == relative)
            .unwrap();
        StartupFilePin {
            path: path.to_owned(),
            sha256: file.sha256,
            size: file.size,
            mode: file.mode,
        }
    };
    let (loader_kind, cache) = if loader["origin"] == "frozen" {
        (StartupLoaderKind::Frozen, None)
    } else {
        assert_eq!(
            Path::new(loader["origin"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            site.canonicalize().unwrap()
        );
        let path = PathBuf::from(loader["cache"].as_str().unwrap());
        let cache = if fs::symlink_metadata(&path).is_ok() {
            StartupCacheBinding::Pinned(pin(&path))
        } else {
            StartupCacheBinding::Missing { path }
        };
        (StartupLoaderKind::Source, Some(cache))
    };
    let binding = StartupAdapterBinding {
        source: pin(&site),
        cache,
        loader: loader_kind,
    };
    // This existing fixture supplies only a genuine owned five-scheme wheel;
    // none of its shell runtime facts enter this real build.
    let archive_factory = Fixture::new().await;
    let archive = archive_factory.archive("live-build", false, None);
    let runtime_root = snapshot.root().to_owned();
    let image_count = match &observed.loader {
        super::RuntimeLoaderObservation::Observed { images } => images.len(),
        super::RuntimeLoaderObservation::Unknown { .. } => {
            panic!("real loader observation is required")
        }
    };
    eprintln!(
        "actual build selected={:?} runtime_root={:?} loader={loader_kind:?} image_count={image_count}",
        descriptor.executable, runtime_root
    );
    let prepared = prepare_environment_build(
        snapshot,
        vec![archive],
        prefix,
        binding,
        EnvironmentBuildLimits::default(),
        Instant::now() + Duration::from_secs(120),
    )
    .await
    .expect("real snapshot through complete local build");
    assert_eq!(prepared.sources().len(), 1);
    assert_eq!(prepared.runtime_root(), runtime_root);
    let pure_file = prepared
        .verification()
        .files()
        .iter()
        .find(|file| {
            file.relative_path().file_name() == Some(std::ffi::OsStr::new("live_build.py"))
        })
        .unwrap();
    let pure_root = prepared
        .final_prefix()
        .join(pure_file.relative_path())
        .parent()
        .unwrap()
        .to_owned();
    let mut child = tokio::process::Command::new(prepared.executable());
    child.args(["-E", "-s", "-S", "-B", "-c",
        "import sys;sys.path.insert(0,sys.argv[1]);import live_build;assert live_build.VALUE==42"])
        .arg(&pure_root).env_clear().env("LC_ALL", "C").current_dir(&root).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success() && output.stdout.is_empty(),
        "{output:?}"
    );
    let console = prepared
        .verification()
        .files()
        .iter()
        .find(|file| {
            file.relative_path().file_name() == Some(std::ffi::OsStr::new("live-build-cli"))
        })
        .unwrap();
    let mut child =
        tokio::process::Command::new(prepared.final_prefix().join(console.relative_path()));
    child
        .env_clear()
        .env("LC_ALL", "C")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(&root)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success() && output.stdout.is_empty(),
        "{output:?}"
    );
    prepared
        .environment
        .recheck(PrefixInventoryLimits::default())
        .unwrap();
    prepared
        .snapshot
        .recheck(PrefixInventoryLimits::default())
        .unwrap();
}

#[tokio::test]
#[ignore = "explicit real CLT snapshot through D1/D2/D3/C3/C4/D4/local seal"]
async fn real_clt_snapshot_complete_local_build() {
    real_snapshot_build(
        Path::new(
            "/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9",
        ),
        Path::new("bin/python3.9"),
    )
    .await;
}

#[tokio::test]
#[ignore = "explicit real owned311 snapshot through D1/D2/D3/C3/C4/D4/local seal"]
async fn real_owned311_snapshot_complete_local_build() {
    real_snapshot_build(
        Path::new("/private/tmp/pakro-d3-owned-cpython311-2b8nnsx3"),
        Path::new("bin/python3.11"),
    )
    .await;
}

#[tokio::test]
#[ignore = "explicit real PyPy snapshot through D1/D2/D3/C3/C4/D4/local seal"]
async fn real_pypy_snapshot_complete_local_build() {
    real_snapshot_build(
        Path::new("/private/tmp/pakro-python-live/data/installs/python/pypy3.12-8.0.1"),
        Path::new("bin/pypy3.12"),
    )
    .await;
}
