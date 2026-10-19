use super::*;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use sha2::Sha256;

use super::super::super::python_wheel::WheelScheme;
use super::super::super::python_wheel_plan::WheelReservedKind;
use super::super::prefix::{EnvironmentStore, PrefixLimits};
use super::super::runtime_layout::RuntimeLayoutLimits;
use super::super::runtime_layout::test_fixtures::{Flavor, descriptor, layout};
use super::super::runtime_source::{
    RuntimeInputExpectation, RuntimeInputPurpose, RuntimeSourceBinding, RuntimeSourceTrust,
    capture_runtime_sources,
};
use crate::python_runtime::PythonRuntimeHandle;

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    executable: PathBuf,
    loader: PathBuf,
    selected: PythonRuntimeDescriptor,
    prefix: UnpublishedPrefix,
    _owner: tempfile::TempDir,
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}
fn input(path: &Path, purpose: RuntimeInputPurpose) -> RuntimeInputExpectation {
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
    fn new(flavor: Flavor, framework: bool) -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir_all(source.join("bin")).unwrap();
        let executable = source.join("bin/runtime");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let handle = PythonRuntimeHandle::from_existing_executable(&executable).unwrap();
        let selected = descriptor(handle.executable(), flavor);
        let prefix = EnvironmentStore::open(
            &root.join("environments"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap()
        .allocate()
        .unwrap();
        assert!(prefix.final_path().is_dir());
        let loader = if framework {
            source.join("Python3")
        } else {
            source.join("bin/loader.dylib")
        };
        fs::write(&loader, b"fixed trusted loader input\0").unwrap();
        fs::set_permissions(&loader, fs::Permissions::from_mode(0o644)).unwrap();
        let mut observed = layout(&selected, &source, prefix.final_path(), flavor);
        observed["loader"]["images"] = serde_json::json!([executable, loader]);
        let layout_path = source.join("layout.json");
        fs::write(&layout_path, serde_json::to_vec(&observed).unwrap()).unwrap();
        // The copied fixture reports its actual invocation path. This simulates
        // the fixed protocol; only the separate ignored tests prove native
        // Mach-O/ELF loading, Python semantics or an implementation's layout.
        let mut encoded = serde_json::to_string(&selected).unwrap();
        let original = serde_json::to_string(executable.to_str().unwrap()).unwrap();
        encoded = encoded.replace(
            &format!("\"executable\":{original}"),
            "\"executable\":\"%s\"",
        );
        assert!(encoded.contains("%s"));
        let encoded = encoded
            .replace('%', "%%")
            .replace("%%s", "%s")
            .replace('\'', "'\\''");
        let script = format!(
            "#!/bin/sh\ncase \"$6\" in\n*PAKRO_RUNTIME_LAYOUT_V1*) /bin/cat {};;\n*) printf '{encoded}\\n' \"$0\";;\nesac\n",
            quoted(&layout_path)
        );
        fs::write(&executable, script).unwrap();
        Self {
            root,
            source,
            executable,
            loader,
            selected,
            prefix,
            _owner: owner,
        }
    }
    async fn sources(&self) -> BoundRuntimeSources {
        self.sources_with_extra(&[]).await
    }
    async fn sources_with_extra(&self, extra: &[PathBuf]) -> BoundRuntimeSources {
        let handle = PythonRuntimeHandle::from_existing_executable(&self.executable).unwrap();
        let mut inputs = vec![
            input(&self.executable, RuntimeInputPurpose::Executable),
            input(&self.loader, RuntimeInputPurpose::LoaderPayload),
        ];
        inputs.extend(
            extra
                .iter()
                .map(|path| input(path, RuntimeInputPurpose::LoaderPayload)),
        );
        capture_runtime_sources(
            &handle,
            &self.selected,
            RuntimeSourceBinding {
                trust: RuntimeSourceTrust::ExplicitExisting,
                authorized_roots: &[self.source.clone()],
                inputs: &inputs,
            },
            &self.prefix,
            RuntimeLayoutLimits::default(),
            Duration::from_secs(5),
        )
        .await
        .expect("D1 source binding must be GREEN before counting a D2 failure")
    }
    async fn plan(&self) -> RuntimeBootstrapPlan {
        prepare_runtime_bootstrap(
            self.sources().await,
            &self.prefix,
            RuntimeBootstrapLimits::default(),
        )
        .expect("D2 planning must be GREEN before counting a copy/startup failure")
    }
}

fn error<T: std::fmt::Debug>(result: Result<T>, diagnostic: &str) {
    let error = result.unwrap_err();
    assert!(error.to_string().contains(diagnostic), "{error:#}");
}

#[tokio::test]
async fn plan_binds_the_final_allocation_and_uses_observed_cpython_and_pypy_schemes() {
    for (flavor, leaf) in [
        (Flavor::Cpython39, "python3.9"),
        (Flavor::Cpython311, "python3.11"),
        (Flavor::Pypy312, "pypy3.12"),
    ] {
        let fixture = Fixture::new(flavor, false);
        let plan = prepare_runtime_bootstrap(
            fixture.sources().await,
            &fixture.prefix,
            RuntimeBootstrapLimits::default(),
        )
        .unwrap();
        assert_eq!(plan.final_prefix(), fixture.prefix.final_path());
        assert_eq!(plan.allocation_id(), fixture.prefix.allocation_id());
        assert_eq!(
            plan.executable(),
            fixture.prefix.final_path().join("bin/python")
        );
        assert_eq!(
            plan.scheme_candidate().root(WheelScheme::Purelib),
            Some(
                fixture
                    .prefix
                    .final_path()
                    .join("lib")
                    .join(leaf)
                    .join("site-packages")
                    .as_path()
            )
        );
        assert!(
            fs::read_dir(fixture.prefix.final_path())
                .unwrap()
                .next()
                .is_none(),
            "planning wrote a destination"
        );
    }
}

#[tokio::test]
async fn plan_preserves_framework_parent_and_pypy_adjacent_loader_destinations() {
    for (framework, relative) in [(true, "Python3"), (false, "bin/loader.dylib")] {
        let fixture = Fixture::new(Flavor::Cpython39, framework);
        let plan = prepare_runtime_bootstrap(
            fixture.sources().await,
            &fixture.prefix,
            RuntimeBootstrapLimits::default(),
        )
        .unwrap();
        let loader = plan
            .files()
            .iter()
            .find(|file| file.kind() == RuntimeBootstrapFileKind::LoaderPayload)
            .unwrap();
        assert_eq!(loader.relative_path(), Path::new(relative));
        assert_eq!(
            loader.sha256(),
            &input(&fixture.loader, RuntimeInputPurpose::LoaderPayload).sha256
        );
        assert_eq!(loader.mode(), 0o644);
    }
}

#[tokio::test]
async fn plan_freezes_interpreter_aliases_configuration_and_runtime_reserved_files() {
    let fixture = Fixture::new(Flavor::Pypy312, false);
    let plan = prepare_runtime_bootstrap(
        fixture.sources().await,
        &fixture.prefix,
        RuntimeBootstrapLimits::default(),
    )
    .unwrap();
    for relative in [
        "bin/python",
        "bin/python3",
        "bin/loader.dylib",
        "pyvenv.cfg",
    ] {
        assert!(
            plan.files()
                .iter()
                .any(|file| file.relative_path() == Path::new(relative)),
            "{relative}"
        );
        assert!(
            plan.reserved()
                .iter()
                .any(|slot| slot.kind == WheelReservedKind::File
                    && slot.path == fixture.prefix.final_path().join(relative)),
            "{relative}"
        );
    }
    assert_eq!(plan.files().len(), 4);
}

#[tokio::test]
async fn another_allocation_or_tainted_prefix_cannot_receive_the_plan() {
    let mut fixture = Fixture::new(Flavor::Cpython311, false);
    let sources = fixture.sources().await;
    let other = EnvironmentStore::open(
        &fixture.root.join("environments"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap()
    .allocate()
    .unwrap();
    error(
        prepare_runtime_bootstrap(sources, &other, RuntimeBootstrapLimits::default()),
        "bootstrap final prefix",
    );
    let sources = fixture.sources().await;
    fixture.prefix.mark_failed();
    error(
        prepare_runtime_bootstrap(sources, &fixture.prefix, RuntimeBootstrapLimits::default()),
        "unpublished prefix",
    );
}

#[tokio::test]
async fn planning_limits_fail_before_any_prefix_write() {
    for limits in [
        RuntimeBootstrapLimits {
            max_files: 0,
            ..Default::default()
        },
        RuntimeBootstrapLimits {
            max_files: 3,
            ..Default::default()
        },
        RuntimeBootstrapLimits {
            max_generated_bytes: 1,
            ..Default::default()
        },
        RuntimeBootstrapLimits {
            max_total_bytes: 1,
            ..Default::default()
        },
        RuntimeBootstrapLimits {
            max_path_bytes: 8,
            ..Default::default()
        },
    ] {
        let fixture = Fixture::new(Flavor::Cpython311, false);
        error(
            prepare_runtime_bootstrap(fixture.sources().await, &fixture.prefix, limits),
            "bootstrap limit",
        );
        assert!(
            fs::read_dir(fixture.prefix.final_path())
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[tokio::test]
async fn source_library_outside_the_relocatable_runtime_tree_cannot_escape_the_prefix() {
    let fixture = Fixture::new(Flavor::Cpython311, false);
    let external = fixture.root.join("external-runtime-library");
    fs::write(&external, b"explicit but non-relocatable loader").unwrap();
    let mut raw = layout(
        &fixture.selected,
        &fixture.source,
        fixture.prefix.final_path(),
        Flavor::Cpython311,
    );
    raw["loader"]["images"] = serde_json::json!([fixture.executable, external]);
    fs::write(
        fixture.source.join("layout.json"),
        serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
    let handle = PythonRuntimeHandle::from_existing_executable(&fixture.executable).unwrap();
    let inputs = [
        input(&fixture.executable, RuntimeInputPurpose::Executable),
        input(&external, RuntimeInputPurpose::LoaderPayload),
    ];
    let sources = capture_runtime_sources(
        &handle,
        &fixture.selected,
        RuntimeSourceBinding {
            trust: RuntimeSourceTrust::ExplicitExisting,
            authorized_roots: &[fixture.root.clone()],
            inputs: &inputs,
        },
        &fixture.prefix,
        RuntimeLayoutLimits::default(),
        Duration::from_secs(5),
    )
    .await
    .expect(
        "D1 must accept the explicitly authorized file before testing D2 destination containment",
    );
    error(
        prepare_runtime_bootstrap(sources, &fixture.prefix, RuntimeBootstrapLimits::default()),
        "bootstrap destination",
    );
    assert!(
        fs::read_dir(fixture.prefix.final_path())
            .unwrap()
            .next()
            .is_none()
    );
    assert_eq!(
        fs::read(external).unwrap(),
        b"explicit but non-relocatable loader"
    );
}

#[test]
fn configuration_uses_full_language_version_instead_of_pypy_implementation_version() {
    let selected = descriptor(Path::new("/runtime/bin/python"), Flavor::Pypy312);
    assert_eq!(
        render_pyvenv_cfg(Path::new("/runtime/bin"), &selected.language_version).unwrap(),
        b"home = /runtime/bin\ninclude-system-site-packages = false\nversion = 3.12.15\n"
    );
}

#[test]
fn configuration_rejects_relative_home_and_line_injection_without_escaping_it() {
    let selected = descriptor(Path::new("/runtime/bin/python"), Flavor::Cpython311);
    for home in [
        "relative/bin",
        "/runtime/bin\ninclude-system-site-packages = true",
        "/runtime/\rbin",
        "/runtime/\0bin",
    ] {
        error(
            render_pyvenv_cfg(Path::new(home), &selected.language_version),
            "bootstrap configuration home",
        );
    }
}

#[tokio::test]
async fn legacy_runtime_without_base_prefix_requires_the_copied_prefix_input_contract() {
    let fixture = Fixture::new(Flavor::Legacy31, false);
    error(
        prepare_runtime_bootstrap(
            fixture.sources().await,
            &fixture.prefix,
            RuntimeBootstrapLimits::default(),
        ),
        "copied-prefix inputs",
    );
}

#[test]
fn configuration_rejects_home_whitespace_that_the_runtime_would_strip() {
    let selected = descriptor(Path::new("/runtime/bin/python"), Flavor::Cpython311);
    for home in ["/trusted/runtime ", "/trusted/runtime\u{a0}"] {
        error(
            render_pyvenv_cfg(Path::new(home), &selected.language_version),
            "bootstrap configuration home",
        );
    }
    assert_eq!(
        render_pyvenv_cfg(Path::new("/trusted/中文 runtime/bin"), &selected.language_version).unwrap(),
        "home = /trusted/中文 runtime/bin\ninclude-system-site-packages = false\nversion = 3.11.17\n".as_bytes(),
    );
}

#[tokio::test]
async fn controlled_copy_has_independent_inodes_exact_modes_and_frozen_cfg_bytes() {
    let mut fixture = Fixture::new(Flavor::Cpython311, false);
    let plan = fixture.plan().await;
    let before_executable = fs::read(&fixture.executable).unwrap();
    let before_loader = fs::read(&fixture.loader).unwrap();
    let copied = bootstrap_runtime(plan, &mut fixture.prefix, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(copied.final_prefix(), fixture.prefix.final_path());
    assert_eq!(copied.allocation_id(), fixture.prefix.allocation_id());
    assert_eq!(
        copied.executable(),
        fixture.prefix.final_path().join("bin/python")
    );
    let target = copied.executable();
    assert_eq!(fs::read(target).unwrap(), before_executable);
    assert_ne!(
        fs::metadata(target).unwrap().ino(),
        fs::metadata(&fixture.executable).unwrap().ino()
    );
    assert_ne!(
        fs::metadata(target).unwrap().ino(),
        fs::metadata(fixture.prefix.final_path().join("bin/python3"))
            .unwrap()
            .ino()
    );
    assert_eq!(
        fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::read(fixture.prefix.final_path().join("bin/loader.dylib")).unwrap(),
        before_loader
    );
    let cfg = fs::read_to_string(fixture.prefix.final_path().join("pyvenv.cfg")).unwrap();
    assert_eq!(
        cfg,
        format!(
            "home = {}\ninclude-system-site-packages = false\nversion = 3.11.17\n",
            fixture.executable.parent().unwrap().display()
        )
    );
    assert_eq!(fs::read(&fixture.executable).unwrap(), before_executable);
    assert_eq!(fs::read(&fixture.loader).unwrap(), before_loader);
    assert_eq!(
        copied.observed_site_disabled().executable,
        copied.executable()
    );
    fixture.prefix.check_binding().unwrap();
}

#[tokio::test]
async fn framework_wrapper_requires_its_observed_nested_application_copy() {
    for include_application in [false, true] {
        let mut fixture = Fixture::new(Flavor::Cpython39, true);
        let relative = Path::new("Resources/Python.app/Contents/MacOS/Python");
        let application = fixture.source.join(relative);
        fs::create_dir_all(application.parent().unwrap()).unwrap();
        let application_script = fs::read_to_string(&fixture.executable)
            .unwrap()
            .replace("\"$0\"", "\"${PAKRO_FIXTURE_INVOKED_AS:-$0}\"");
        fs::write(&application, application_script).unwrap();
        fs::set_permissions(&application, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            &fixture.executable,
            b"#!/bin/sh\napplication=\"${0%/*}/../Resources/Python.app/Contents/MacOS/Python\"\nPAKRO_FIXTURE_INVOKED_AS=\"$0\" exec \"$application\" \"$@\"\n",
        )
        .unwrap();
        let mut observed = layout(
            &fixture.selected,
            &fixture.source,
            fixture.prefix.final_path(),
            Flavor::Cpython39,
        );
        observed["loader"]["images"] =
            serde_json::json!([fixture.executable, fixture.loader, application]);
        fs::write(
            fixture.source.join("layout.json"),
            serde_json::to_vec(&observed).unwrap(),
        )
        .unwrap();
        let extra = if include_application {
            vec![application.clone()]
        } else {
            vec![]
        };
        let plan = prepare_runtime_bootstrap(
            fixture.sources_with_extra(&extra).await,
            &fixture.prefix,
            RuntimeBootstrapLimits::default(),
        )
        .unwrap();
        let result = bootstrap_runtime(plan, &mut fixture.prefix, Duration::from_secs(5)).await;
        if include_application {
            let copied = result.unwrap();
            let target = fixture.prefix.final_path().join(relative);
            assert_eq!(fs::read(&target).unwrap(), fs::read(&application).unwrap());
            assert_ne!(
                fs::metadata(&target).unwrap().ino(),
                fs::metadata(&application).unwrap().ino()
            );
            assert_eq!(
                fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(
                copied.observed_site_disabled().executable,
                copied.executable()
            );
            fixture.prefix.check_binding().unwrap();
        } else {
            error(result, "bootstrap startup");
            assert!(fixture.prefix.check_binding().is_err());
        }
        fixture.prefix.discard().unwrap();
    }
}

#[tokio::test]
async fn copied_file_receipt_matches_actual_sha_size_mode_and_all_planned_files() {
    let mut fixture = Fixture::new(Flavor::Pypy312, false);
    let plan = fixture.plan().await;
    let expected = plan.files().to_vec();
    let copied = bootstrap_runtime(plan, &mut fixture.prefix, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(copied.files(), expected);
    for file in copied.files() {
        let target = fixture.prefix.final_path().join(file.relative_path());
        let bytes = fs::read(&target).unwrap();
        let sha256: [u8; 32] = Sha256::digest(&bytes).into();
        assert_eq!(file.sha256(), &sha256);
        assert_eq!(file.size(), bytes.len() as u64);
        assert_eq!(
            file.mode(),
            fs::metadata(target).unwrap().permissions().mode() & 0o777
        );
    }
}

#[tokio::test]
async fn copying_a_plan_into_a_different_cap_is_a_zero_write_error() {
    let fixture = Fixture::new(Flavor::Cpython311, false);
    let plan = fixture.plan().await;
    let mut other = EnvironmentStore::open(
        &fixture.root.join("environments"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap()
    .allocate()
    .unwrap();
    error(
        bootstrap_runtime(plan, &mut other, Duration::from_secs(5)).await,
        "bootstrap allocation",
    );
    assert!(fs::read_dir(other.final_path()).unwrap().next().is_none());
    other.check_binding().unwrap();
}

#[tokio::test]
async fn source_path_replacement_after_planning_does_not_replace_copied_snapshot_bytes() {
    let mut fixture = Fixture::new(Flavor::Cpython311, false);
    let plan = fixture.plan().await;
    let original = fs::read(&fixture.loader).unwrap();
    fs::remove_file(&fixture.loader).unwrap();
    fs::write(&fixture.loader, b"external replacement").unwrap();
    let _copied = bootstrap_runtime(plan, &mut fixture.prefix, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        fs::read(fixture.prefix.final_path().join("bin/loader.dylib")).unwrap(),
        original
    );
    assert_eq!(fs::read(&fixture.loader).unwrap(), b"external replacement");
}

#[tokio::test]
async fn entry_limit_after_one_completed_file_taints_the_failed_bootstrap() {
    let mut fixture = Fixture::new(Flavor::Cpython311, false);
    let source = fixture.sources().await;
    fixture.prefix = EnvironmentStore::open(
        &fixture.root.join("limited"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits {
            max_entries: 2,
            ..Default::default()
        },
    )
    .unwrap()
    .allocate()
    .unwrap();
    // Rebind a fresh source observation to this exact allocated target.
    let mut raw = layout(
        &fixture.selected,
        &fixture.source,
        fixture.prefix.final_path(),
        Flavor::Cpython311,
    );
    raw["loader"]["images"] = serde_json::json!([fixture.executable, fixture.loader]);
    fs::write(
        fixture.source.join("layout.json"),
        serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
    drop(source);
    let plan = fixture.plan().await;
    error(
        bootstrap_runtime(plan, &mut fixture.prefix, Duration::from_secs(5)).await,
        "bootstrap write",
    );
    assert!(fixture.prefix.check_binding().is_err());
    assert_eq!(
        fs::read(&fixture.loader).unwrap(),
        b"fixed trusted loader input\0"
    );
    fixture.prefix.discard().unwrap();
}

#[tokio::test]
async fn selected_runtime_startup_failure_never_returns_a_bootstrap_receipt() {
    let mut fixture = Fixture::new(Flavor::Cpython311, false);
    // This accepted trusted fixture runs at its source, but deliberately fails
    // when invoked from the final prefix, modeling a missing loader/startup.
    let source_bin = quoted(fixture.executable.parent().unwrap());
    let original = fs::read_to_string(&fixture.executable).unwrap();
    let injected=original.replacen("#!/bin/sh\n",&format!("#!/bin/sh\ncase \"$0\" in {source_bin}/*) ;; *) printf 'fixture startup failed' >&2; exit 73;; esac\n"),1);
    fs::write(&fixture.executable, injected).unwrap();
    let plan = fixture.plan().await;
    error(
        bootstrap_runtime(plan, &mut fixture.prefix, Duration::from_secs(5)).await,
        "bootstrap startup",
    );
    assert!(fixture.prefix.check_binding().is_err());
    fixture.prefix.discard().unwrap();
}

async fn live(executable_env: &str, root_env: &str, loaders_env: &str) {
    let executable = PathBuf::from(
        std::env::var_os(executable_env).expect("explicit existing runtime required"),
    );
    let root = PathBuf::from(
        std::env::var_os(root_env).expect("explicit authorized source root required"),
    )
    .canonicalize()
    .unwrap();
    let loaders = std::env::var_os(loaders_env)
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|path| !path.as_os_str().is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let handle = PythonRuntimeHandle::from_existing_executable(&executable).unwrap();
    let observed = crate::python_runtime::probe(&handle, Duration::from_secs(10))
        .await
        .unwrap();
    let mut inputs = vec![input(handle.executable(), RuntimeInputPurpose::Executable)];
    inputs.extend(loaders.iter().map(|path| {
        input(
            &path.canonicalize().unwrap(),
            RuntimeInputPurpose::LoaderPayload,
        )
    }));
    let owner = tempfile::tempdir().unwrap();
    let mut prefix = EnvironmentStore::open(
        &owner.path().canonicalize().unwrap().join("environments"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap()
    .allocate()
    .unwrap();
    let sources = capture_runtime_sources(
        &handle,
        &observed,
        RuntimeSourceBinding {
            trust: RuntimeSourceTrust::ExplicitExisting,
            authorized_roots: &[root],
            inputs: &inputs,
        },
        &prefix,
        RuntimeLayoutLimits::default(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let target = sources.layout().target_paths.clone();
    let plan =
        prepare_runtime_bootstrap(sources, &prefix, RuntimeBootstrapLimits::default()).unwrap();
    let copied = bootstrap_runtime(plan, &mut prefix, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        copied.observed_site_disabled().implementation,
        observed.implementation
    );
    assert_eq!(
        copied.observed_site_disabled().language_version,
        observed.language_version
    );
    assert_eq!(
        copied.observed_site_disabled().implementation_version,
        observed.implementation_version
    );
    assert_eq!(copied.observed_site_disabled().soabi, observed.soabi);
    assert_eq!(
        copied.observed_site_disabled().ext_suffix,
        observed.ext_suffix
    );
    assert_eq!(
        copied.scheme_candidate().root(WheelScheme::Purelib),
        Some(target["purelib"].as_path())
    );
    assert_ne!(
        fs::metadata(copied.executable()).unwrap().ino(),
        fs::metadata(handle.executable()).unwrap().ino()
    );
    prefix.check_binding().unwrap();
}

#[tokio::test]
#[ignore = "requires explicit PAKRO_TEST_CPYTHON39_EXECUTABLE/SOURCE_ROOT/LOADER_FILES"]
async fn live_cpython39_framework_copy_and_site_disabled_startup() {
    live(
        "PAKRO_TEST_CPYTHON39_EXECUTABLE",
        "PAKRO_TEST_CPYTHON39_SOURCE_ROOT",
        "PAKRO_TEST_CPYTHON39_LOADER_FILES",
    )
    .await;
}
#[tokio::test]
#[ignore = "requires explicit PAKRO_TEST_CPYTHON311_EXECUTABLE/SOURCE_ROOT/LOADER_FILES"]
async fn live_cpython311_copy_and_site_disabled_startup() {
    live(
        "PAKRO_TEST_CPYTHON311_EXECUTABLE",
        "PAKRO_TEST_CPYTHON311_SOURCE_ROOT",
        "PAKRO_TEST_CPYTHON311_LOADER_FILES",
    )
    .await;
}
#[tokio::test]
#[ignore = "requires explicit PAKRO_TEST_PYPY312_EXECUTABLE/SOURCE_ROOT/LOADER_FILES"]
async fn live_pypy312_adjacent_library_copy_and_site_disabled_startup() {
    live(
        "PAKRO_TEST_PYPY312_EXECUTABLE",
        "PAKRO_TEST_PYPY312_SOURCE_ROOT",
        "PAKRO_TEST_PYPY312_LOADER_FILES",
    )
    .await;
}

#[tokio::test]
async fn caller_deadline_is_not_reset_before_bootstrap_writes() {
    let mut fixture = Fixture::new(Flavor::Cpython311, false);
    let plan = fixture.plan().await;
    let result = bootstrap_runtime_at_deadline(
        plan,
        &mut fixture.prefix,
        tokio::time::Instant::now() - Duration::from_millis(1),
    )
    .await;
    error(result, "deadline");
    assert_eq!(
        fs::read_dir(fixture.prefix.final_path()).unwrap().count(),
        0
    );
    fixture.prefix.check_binding().unwrap();
}
