use super::*;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use sha2::Sha256;

use super::super::prefix::{EnvironmentStore, PrefixLimits};
use super::super::runtime_layout::test_fixtures::{Flavor, descriptor, layout};

struct Fixture {
    root: PathBuf,
    handle: PythonRuntimeHandle,
    selected: PythonRuntimeDescriptor,
    prefix: UnpublishedPrefix,
    inputs: Vec<RuntimeInputExpectation>,
    roots: Vec<PathBuf>,
    _owner: tempfile::TempDir,
}

fn shell_path(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

fn expectation(path: &Path, purpose: RuntimeInputPurpose) -> RuntimeInputExpectation {
    let bytes = fs::read(path).unwrap();
    RuntimeInputExpectation {
        path: path.to_path_buf(),
        purpose,
        sha256: Sha256::digest(&bytes).into(),
        size: bytes.len() as u64,
        mode: fs::metadata(path).unwrap().permissions().mode() & 0o777,
    }
}

impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let parent = owner.path().canonicalize().unwrap();
        let root = parent.join("中文-runtime");
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/python");
        let selected_json = root.join("runtime.json");
        let layout_json = root.join("layout.json");
        let log = root.join("runs");
        let script = format!(
            "#!/bin/sh\n[ \"$1 $2 $3 $4 $5\" = '-E -s -S -B -c' ] || exit 81\n[ -z \"${{PYTHONPATH+x}}\" ] || exit 82\nprintf 'run\\n' >> {}\ncase \"$6\" in\n*PAKRO_RUNTIME_LAYOUT_V1*) /bin/cat {};;\n*) /bin/cat {};;\nesac\n",
            shell_path(&log),
            shell_path(&layout_json),
            shell_path(&selected_json),
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("bin/loader.dylib"),
            b"fixed independent loader fixture\0",
        )
        .unwrap();
        fs::set_permissions(
            root.join("bin/loader.dylib"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let handle = PythonRuntimeHandle::from_existing_executable(&executable).unwrap();
        let selected = descriptor(handle.executable(), Flavor::Cpython311);
        selected.validate_compatibility().unwrap();
        let prefix = EnvironmentStore::open(
            &parent.join("environments"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap()
        .allocate()
        .unwrap();
        assert!(
            prefix.final_path().is_dir(),
            "real prefix allocation setup failed"
        );
        fs::write(&selected_json, serde_json::to_vec(&selected).unwrap()).unwrap();
        fs::write(
            &layout_json,
            serde_json::to_vec(&layout(
                &selected,
                &root,
                prefix.final_path(),
                Flavor::Cpython311,
            ))
            .unwrap(),
        )
        .unwrap();
        let inputs = vec![
            expectation(&executable, RuntimeInputPurpose::Executable),
            expectation(
                &root.join("bin/loader.dylib"),
                RuntimeInputPurpose::LoaderPayload,
            ),
        ];
        Self {
            roots: vec![root.clone()],
            root,
            handle,
            selected,
            prefix,
            inputs,
            _owner: owner,
        }
    }

    async fn capture(&self, inputs: &[RuntimeInputExpectation]) -> Result<BoundRuntimeSources> {
        capture_runtime_sources(
            &self.handle,
            &self.selected,
            RuntimeSourceBinding {
                trust: RuntimeSourceTrust::ExplicitExisting,
                authorized_roots: &self.roots,
                inputs,
            },
            &self.prefix,
            RuntimeLayoutLimits::default(),
            Duration::from_secs(3),
        )
        .await
    }
}

fn error(result: Result<BoundRuntimeSources>, diagnostic: &str) {
    let error = result.unwrap_err();
    assert!(error.to_string().contains(diagnostic), "{error:#}");
}

#[tokio::test]
async fn capture_reprobes_the_bound_runtime_and_retains_exact_source_statistics() {
    let fixture = Fixture::new();
    let mut sources = fixture.capture(&fixture.inputs).await.unwrap();
    assert_eq!(sources.observed_runtime(), &fixture.selected);
    assert_eq!(sources.layout().target_prefix, fixture.prefix.final_path());
    assert_eq!(sources.inputs().len(), 2);
    for input in &fixture.inputs {
        let index = sources
            .inputs()
            .iter()
            .position(|receipt| receipt.purpose() == input.purpose)
            .unwrap();
        let receipt = &sources.inputs()[index];
        assert_eq!(receipt.path(), input.path);
        assert_eq!(receipt.sha256(), &input.sha256);
        assert_eq!(receipt.size(), input.size);
        assert_eq!(receipt.mode(), input.mode);
        let mut bytes = Vec::new();
        let copied = sources.copy_input(index, &mut bytes).unwrap();
        assert_eq!(copied.size(), input.size);
        assert_eq!(bytes, fs::read(&input.path).unwrap());
    }
    assert_eq!(
        fs::read_to_string(fixture.root.join("runs"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[tokio::test]
async fn source_snapshots_survive_live_input_replacement_without_reopening_paths() {
    let fixture = Fixture::new();
    let original = fs::read(&fixture.inputs[1].path).unwrap();
    let mut sources = fixture.capture(&fixture.inputs).await.unwrap();
    fs::remove_file(&fixture.inputs[1].path).unwrap();
    fs::write(
        &fixture.inputs[1].path,
        b"replacement bytes from another owner",
    )
    .unwrap();
    let index = sources
        .inputs()
        .iter()
        .position(|input| input.purpose() == RuntimeInputPurpose::LoaderPayload)
        .unwrap();
    let mut output = Vec::new();
    let receipt = sources.copy_input(index, &mut output).unwrap();
    assert_eq!(output, original);
    assert_eq!(receipt.sha256(), &fixture.inputs[1].sha256);
}

#[tokio::test]
async fn wrong_digest_size_or_mode_is_rejected_before_runtime_execution() {
    for field in ["digest", "size", "mode"] {
        let fixture = Fixture::new();
        let mut inputs = fixture.inputs.clone();
        match field {
            "digest" => inputs[0].sha256[0] ^= 1,
            "size" => inputs[0].size += 1,
            _ => inputs[0].mode = 0o644,
        }
        error(fixture.capture(&inputs).await, "runtime input");
        assert!(
            !fixture.root.join("runs").exists(),
            "unverified executable was launched"
        );
    }
}

#[tokio::test]
async fn repeated_probe_must_match_the_supplied_full_runtime_descriptor() {
    let mut fixture = Fixture::new();
    fixture.selected.language_version.micro += 1;
    fixture.selected.markers.python_full_version = "3.11.18".into();
    fixture.selected.implementation_version.micro += 1;
    fixture.selected.markers.implementation_version = "3.11.18".into();
    error(
        fixture.capture(&fixture.inputs).await,
        "runtime descriptor changed",
    );
}

#[tokio::test]
async fn source_input_outside_authorized_roots_or_through_an_external_symlink_is_rejected() {
    for alias in [false, true] {
        let fixture = Fixture::new();
        let external = fixture.root.parent().unwrap().join("external-library");
        fs::write(&external, b"external library").unwrap();
        let mut inputs = fixture.inputs.clone();
        inputs[1] = expectation(&external, RuntimeInputPurpose::LoaderPayload);
        if alias {
            let link = fixture.root.join("bin/escape.dylib");
            symlink(&external, &link).unwrap();
            inputs[1].path = link;
        }
        error(fixture.capture(&inputs).await, "authorized runtime roots");
        assert_eq!(fs::read(&external).unwrap(), b"external library");
        assert!(!fixture.root.join("runs").exists());
    }
}

#[tokio::test]
async fn mandatory_executable_duplicate_inputs_and_unobserved_loader_inputs_are_rejected() {
    let fixture = Fixture::new();
    error(
        fixture.capture(&fixture.inputs[1..]).await,
        "runtime executable input",
    );
    let mut duplicate = fixture.inputs.clone();
    duplicate.push(duplicate[0].clone());
    error(fixture.capture(&duplicate).await, "duplicate runtime input");
    let extra = fixture.root.join("bin/not-loaded.dylib");
    fs::write(&extra, b"not actually loaded").unwrap();
    let mut inputs = fixture.inputs.clone();
    inputs[1] = expectation(&extra, RuntimeInputPurpose::LoaderPayload);
    error(fixture.capture(&inputs).await, "loader observation binding");
}

#[tokio::test]
async fn a_declared_library_file_fact_cannot_authenticate_a_header_input() {
    let fixture = Fixture::new();
    let mut inputs = fixture.inputs.clone();
    inputs[1].purpose = RuntimeInputPurpose::Header;
    let layout_path = fixture.root.join("layout.json");
    let mut raw: serde_json::Value =
        serde_json::from_slice(&fs::read(&layout_path).unwrap()).unwrap();
    raw["config_vars"]["LIBDIR"] = serde_json::json!(fixture.root.join("bin"));
    raw["config_vars"]["LDLIBRARY"] = serde_json::json!("loader.dylib");
    raw["files"]["libdir_ldlibrary"] = serde_json::json!({
        "advertised": inputs[1].path,
        "status": {"state":"present","canonical":inputs[1].path,
                   "regular_file":true,"size":inputs[1].size,"mode":inputs[1].mode}
    });
    assert_eq!(raw["files"]["python_h"]["status"]["state"], "missing");
    fs::write(layout_path, serde_json::to_vec(&raw).unwrap()).unwrap();
    error(
        fixture.capture(&inputs).await,
        "runtime header observation binding",
    );
}

#[tokio::test]
async fn privileged_mode_changes_during_the_probe_invalidate_the_source_binding() {
    let fixture = Fixture::new();
    let mut script = fs::read_to_string(fixture.handle.executable()).unwrap();
    script.push_str(&format!(
        "\n/bin/chmod 4644 {}\n",
        shell_path(&fixture.inputs[1].path)
    ));
    fs::write(fixture.handle.executable(), script).unwrap();
    let mut inputs = fixture.inputs.clone();
    inputs[0] = expectation(fixture.handle.executable(), RuntimeInputPurpose::Executable);
    error(
        fixture.capture(&inputs).await,
        "runtime input changed during observation",
    );
    assert_eq!(
        fs::metadata(&inputs[1].path).unwrap().permissions().mode() & 0o7777,
        0o4644
    );
}

#[tokio::test]
async fn capture_reads_only_explicit_inputs_and_does_not_snapshot_the_base_stdlib_tree() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("lib/python3.11/unselected-tree")).unwrap();
    fs::write(
        fixture
            .root
            .join("lib/python3.11/unselected-tree/not-an-input"),
        b"third-party payload must not be captured",
    )
    .unwrap();
    let sources = fixture.capture(&fixture.inputs).await.unwrap();
    assert_eq!(sources.inputs().len(), 2);
    assert!(sources.inputs().iter().all(|input| {
        fixture
            .inputs
            .iter()
            .any(|expected| expected.path == input.path())
    }));
}

#[tokio::test]
async fn source_limits_and_zero_timeout_fail_without_probing_or_returning_partial_capabilities() {
    let fixture = Fixture::new();
    for limits in [
        RuntimeLayoutLimits {
            max_input_files: 1,
            ..Default::default()
        },
        RuntimeLayoutLimits {
            max_file_bytes: 1,
            ..Default::default()
        },
        RuntimeLayoutLimits {
            max_total_bytes: 1,
            ..Default::default()
        },
        RuntimeLayoutLimits {
            max_paths: 0,
            ..Default::default()
        },
    ] {
        error(
            capture_runtime_sources(
                &fixture.handle,
                &fixture.selected,
                RuntimeSourceBinding {
                    trust: RuntimeSourceTrust::ExplicitExisting,
                    authorized_roots: &fixture.roots,
                    inputs: &fixture.inputs,
                },
                &fixture.prefix,
                limits,
                Duration::from_secs(3),
            )
            .await,
            "runtime source limit",
        );
        assert!(!fixture.root.join("runs").exists());
    }
    error(
        capture_runtime_sources(
            &fixture.handle,
            &fixture.selected,
            RuntimeSourceBinding {
                trust: RuntimeSourceTrust::ExplicitExisting,
                authorized_roots: &fixture.roots,
                inputs: &fixture.inputs,
            },
            &fixture.prefix,
            RuntimeLayoutLimits::default(),
            Duration::ZERO,
        )
        .await,
        "runtime source timeout",
    );
}

#[tokio::test]
async fn tainted_or_replaced_final_prefix_cannot_bind_a_runtime_layout() {
    let mut fixture = Fixture::new();
    {
        let mut output = fixture
            .prefix
            .create_file_exclusive(Path::new("unfinished"), 0o644)
            .unwrap();
        output.write_all(b"unfinished").unwrap();
    }
    error(fixture.capture(&fixture.inputs).await, "unpublished prefix");
    assert!(!fixture.root.join("runs").exists());
}

#[tokio::test]
async fn input_reader_indexes_are_bounded_and_copy_failures_do_not_return_receipts() {
    struct FailingWriter;
    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("test sink refused bytes"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let mut sources = fixture.capture(&fixture.inputs).await.unwrap();
    let error = sources.copy_input(usize::MAX, &mut Vec::new()).unwrap_err();
    assert!(error.to_string().contains("input index"), "{error:#}");
    let error = sources.copy_input(0, &mut FailingWriter).unwrap_err();
    assert!(
        error.to_string().contains("runtime input write"),
        "{error:#}"
    );
}

async fn live(env_name: &str) {
    let path = PathBuf::from(
        std::env::var_os(env_name)
            .expect("explicit selected executable environment variable is required"),
    );
    let handle = PythonRuntimeHandle::from_existing_executable(&path).unwrap();
    let descriptor = crate::python_runtime::probe(&handle, Duration::from_secs(10))
        .await
        .unwrap();
    let root = handle.executable().parent().unwrap().to_path_buf();
    let input = expectation(handle.executable(), RuntimeInputPurpose::Executable);
    let owner = tempfile::tempdir().unwrap();
    let prefix = EnvironmentStore::open(
        &owner.path().canonicalize().unwrap().join("environments"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap()
    .allocate()
    .unwrap();
    let sources = capture_runtime_sources(
        &handle,
        &descriptor,
        RuntimeSourceBinding {
            trust: RuntimeSourceTrust::ExplicitExisting,
            authorized_roots: &[root],
            inputs: &[input],
        },
        &prefix,
        RuntimeLayoutLimits::default(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(sources.observed_runtime(), &descriptor);
    assert_eq!(sources.layout().target_prefix, prefix.final_path());
    assert_eq!(
        sources.inputs().len(),
        1,
        "base stdlib was captured before a bootstrap policy was chosen"
    );
    assert!(
        sources
            .layout()
            .target_paths
            .values()
            .all(|path| path.starts_with(prefix.final_path()))
    );
}

#[tokio::test]
#[ignore = "requires PAKRO_TEST_CPYTHON39_EXECUTABLE explicitly selected existing runtime"]
async fn live_existing_cpython39_layout_and_executable_snapshot() {
    live("PAKRO_TEST_CPYTHON39_EXECUTABLE").await;
}

#[tokio::test]
#[ignore = "requires PAKRO_TEST_CPYTHON311_EXECUTABLE explicitly selected existing runtime"]
async fn live_existing_cpython311_layout_and_executable_snapshot() {
    live("PAKRO_TEST_CPYTHON311_EXECUTABLE").await;
}

#[tokio::test]
#[ignore = "requires PAKRO_TEST_PYPY312_EXECUTABLE explicitly selected existing runtime"]
async fn live_existing_pypy312_layout_and_executable_snapshot() {
    live("PAKRO_TEST_PYPY312_EXECUTABLE").await;
}

#[tokio::test]
async fn caller_deadline_is_not_reset_by_runtime_source_capture() {
    let fixture = Fixture::new();
    let result = capture_runtime_sources_at_deadline(
        &fixture.handle,
        &fixture.selected,
        RuntimeSourceBinding {
            trust: RuntimeSourceTrust::ExplicitExisting,
            authorized_roots: &fixture.roots,
            inputs: &fixture.inputs,
        },
        &fixture.prefix,
        RuntimeLayoutLimits::default(),
        tokio::time::Instant::now() - Duration::from_millis(1),
    )
    .await;
    let error = result.unwrap_err();
    assert!(format!("{error:#}").contains("deadline"), "{error:#}");
    assert!(
        !fixture.root.join("runs").exists(),
        "expired capture started a child"
    );
    fixture.prefix.check_binding().unwrap();
}
