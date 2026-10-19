use super::*;

#[path = "tests/installed.rs"]
mod installed;

#[path = "tests/seal.rs"]
mod seal;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::super::super::python_wheel::WheelScheme;
use super::super::prefix::{EnvironmentStore, PrefixLimits};
use super::super::runtime_bootstrap::{
    RuntimeBootstrapLimits, bootstrap_runtime, prepare_runtime_bootstrap,
};
use super::super::runtime_layout::RuntimeLayoutLimits;
use super::super::runtime_layout::test_fixtures::{Flavor, descriptor, layout};
use super::super::runtime_source::{
    RuntimeInputExpectation, RuntimeInputPurpose, RuntimeSourceBinding, RuntimeSourceTrust,
    capture_runtime_sources,
};
use crate::python_runtime::PythonRuntimeHandle;

struct Fixture {
    prefix: UnpublishedPrefix,
    runtime: BootstrappedRuntime,
    source: PathBuf,
    binding: StartupAdapterBinding,
    facts_path: PathBuf,
    normal_path: PathBuf,
    runs: PathBuf,
    probes: PathBuf,
    _owner: tempfile::TempDir,
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}
fn pin(path: &Path) -> StartupFilePin {
    let bytes = fs::read(path).unwrap();
    StartupFilePin {
        path: path.into(),
        sha256: Sha256::digest(&bytes).into(),
        size: bytes.len() as u64,
        mode: fs::metadata(path).unwrap().permissions().mode() & 0o777,
    }
}
fn executable_pin(path: &Path) -> RuntimeInputExpectation {
    let pin = pin(path);
    RuntimeInputExpectation {
        path: pin.path,
        sha256: pin.sha256,
        size: pin.size,
        mode: pin.mode,
        purpose: RuntimeInputPurpose::Executable,
    }
}

impl Fixture {
    async fn new() -> Self {
        Self::with_distinct_sites(false).await
    }
    async fn with_distinct_sites(distinct: bool) -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let source = root.join("中文-runtime");
        let stdlib = source.join("lib/python3.11");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(stdlib.join("__pycache__")).unwrap();
        let executable = source.join("bin/runtime");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let handle = PythonRuntimeHandle::from_existing_executable(&executable).unwrap();
        let selected = descriptor(handle.executable(), Flavor::Cpython311);
        let mut prefix = EnvironmentStore::open(
            &root.join("environments"),
            nix::unistd::geteuid().as_raw(),
            PrefixLimits::default(),
        )
        .unwrap()
        .allocate()
        .unwrap();
        let site = stdlib.join("site.py");
        fs::write(
            &site,
            b"import sys\ndef main():\n    pass\nif not sys.flags.no_site:\n    main()\n",
        )
        .unwrap();
        fs::set_permissions(&site, fs::Permissions::from_mode(0o644)).unwrap();
        let source_pin = pin(&site);
        let cache = stdlib.join("__pycache__/site.fixture.pyc");
        let binding = StartupAdapterBinding {
            source: source_pin,
            cache: Some(StartupCacheBinding::Missing {
                path: cache.clone(),
            }),
            loader: StartupLoaderKind::Source,
        };
        let mut raw_layout = layout(&selected, &source, prefix.final_path(), Flavor::Cpython311);
        raw_layout["sys_path"]
            .as_array_mut()
            .unwrap()
            .push(json!(source.join("lib/python311.zip")));
        if distinct {
            raw_layout["target_paths"]["platlib"] =
                json!(prefix.final_path().join("unused-platlib"));
        }
        let layout_path = source.join("layout.json");
        fs::write(&layout_path, serde_json::to_vec(&raw_layout).unwrap()).unwrap();
        let facts_path = source.join("startup.json");
        let normal_path = source.join("normal.json");
        let runs = source.join("runs");
        let probes = source.join("probes");
        let mut sites = vec![raw_layout["target_paths"]["purelib"].clone()];
        if distinct {
            sites.push(raw_layout["target_paths"]["platlib"].clone());
        }
        let facts = json!({
            "schema": 1, "observation_mode":"cfg_recognition_intercepted",
            "executable":prefix.final_path().join("bin/python"),
            "prefix":prefix.final_path(),"exec_prefix":prefix.final_path(),
            "base_prefix":source,"base_exec_prefix":source,
            "loader":"source","site_origin":site,"cache_path":cache,
            "source_sha256":hex::encode(binding.source.sha256),"cache_sha256":null,
            "code_selection":"source","cfg_path":prefix.final_path().join("pyvenv.cfg"),
            "home":source.join("bin"),"user_site_enabled":false,
            "no_site":true,"ignore_environment":true,"dont_write_bytecode":true,
            "requested_sites":sites,
            "suppressed_hooks":["addsitepackages","addsitedir","addusersitepackages","execsitecustomize","execusercustomize","enablerlcompleter"],
            "sys_path":raw_layout["sys_path"],
            "default_paths":raw_layout["base_paths"],"final_paths":raw_layout["target_paths"]
        });
        fs::write(&facts_path, serde_json::to_vec(&facts).unwrap()).unwrap();
        let mut normal = facts.clone();
        normal["observation_mode"] = json!("normal_empty_environment");
        normal["no_site"] = json!(false);
        normal["suppressed_hooks"] = json!([]);
        let mut normal_paths = raw_layout["sys_path"].as_array().unwrap().clone();
        normal_paths.extend(sites);
        normal["sys_path"] = json!(normal_paths);
        fs::write(&normal_path, serde_json::to_vec(&normal).unwrap()).unwrap();
        let mut encoded = serde_json::to_string(&selected).unwrap();
        encoded = encoded.replace(
            &format!(
                "\"executable\":{}",
                serde_json::to_string(&executable).unwrap()
            ),
            "\"executable\":\"%s\"",
        );
        assert!(encoded.contains("%s"));
        encoded = encoded
            .replace('%', "%%")
            .replace("%%s", "%s")
            .replace('\'', "'\\''");
        let mut encoded_layout = serde_json::to_string(&raw_layout).unwrap();
        encoded_layout = encoded_layout.replace(
            &format!(
                "\"executable\":{}",
                serde_json::to_string(&executable).unwrap()
            ),
            "\"executable\":\"%s\"",
        );
        assert!(encoded_layout.contains("%s"));
        encoded_layout = encoded_layout
            .replace('%', "%%")
            .replace("%%s", "%s")
            .replace('\'', "'\\''");
        let script = format!(
            "#!/bin/sh\nprintf 'probe\\n' >> {}\ncase \"$*\" in\n*PAKRO_RUNTIME_STARTUP_V1*) printf 'intercepted\\n' >> {}; /bin/cat >/dev/null; /bin/cat {};;\n*PAKRO_RUNTIME_EMPTY_V1*) printf 'normal\\n' >> {}; /bin/cat >/dev/null; /bin/cat {};;\n*PAKRO_RUNTIME_LAYOUT_V1*) printf '{encoded_layout}\\n' \"$0\";;\n*) printf '{encoded}\\n' \"$0\";;\nesac\n",
            quote(&probes),
            quote(&runs),
            quote(&facts_path),
            quote(&runs),
            quote(&normal_path),
        );
        fs::write(&executable, script).unwrap();
        let inputs = [executable_pin(&executable)];
        let sources = capture_runtime_sources(
            &handle,
            &selected,
            RuntimeSourceBinding {
                trust: RuntimeSourceTrust::ExplicitExisting,
                authorized_roots: &[source.clone()],
                inputs: &inputs,
            },
            &prefix,
            RuntimeLayoutLimits::default(),
            Duration::from_secs(5),
        )
        .await
        .expect("real D1 capture must succeed before D3 target assertions");
        let plan = prepare_runtime_bootstrap(sources, &prefix, RuntimeBootstrapLimits::default())
            .expect("real D2 plan must succeed before D3 target assertions");
        let runtime = bootstrap_runtime(plan, &mut prefix, Duration::from_secs(5))
            .await
            .expect("real D2 controlled copy/startup must succeed before D3 target assertions");
        prefix.check_binding().unwrap();
        fs::remove_file(&probes).unwrap();
        Self {
            prefix,
            runtime,
            source,
            binding,
            facts_path,
            normal_path,
            runs,
            probes,
            _owner: owner,
        }
    }
    fn facts(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.facts_path).unwrap()).unwrap()
    }
    fn set_facts(&self, facts: &Value) {
        fs::write(&self.facts_path, serde_json::to_vec(facts).unwrap()).unwrap();
    }
    async fn recognize(&self) -> Result<CfgRecognitionEvidence> {
        establish_cfg_recognition(
            &self.runtime,
            &self.prefix,
            self.binding.clone(),
            RuntimeStartupLimits::default(),
            Duration::from_secs(5),
        )
        .await
    }
    fn normal_runs(&self) -> usize {
        fs::read_to_string(&self.runs)
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == "normal")
            .count()
    }
}

#[path = "tests/wheels.rs"]
mod wheels;

#[path = "tests/installed_live.rs"]
mod installed_live;

fn error<T: std::fmt::Debug>(result: Result<T>, diagnostic: &str) {
    let error = result.unwrap_err();
    assert!(error.to_string().contains(diagnostic), "{error:#}");
}

#[tokio::test]
async fn recognition_binds_real_copied_runtime_cfg_and_never_starts_normal_site() {
    let fixture = Fixture::new().await;
    let evidence = fixture.recognize().await.unwrap();
    assert_eq!(evidence.allocation_id(), fixture.prefix.allocation_id());
    assert_eq!(evidence.final_prefix(), fixture.prefix.final_path());
    assert_eq!(
        evidence.requested_sites(),
        [fixture
            .prefix
            .final_path()
            .join("lib/python3.11/site-packages")]
    );
    assert_eq!(fixture.normal_runs(), 0);
    fixture.prefix.check_binding().unwrap();
}

#[tokio::test]
async fn source_digest_and_cache_digest_are_checked_before_startup_execution() {
    for cached in [false, true] {
        let mut fixture = Fixture::new().await;
        if cached {
            let path = fixture
                .source
                .join("lib/python3.11/__pycache__/site.fixture.pyc");
            fs::write(&path, b"owned cache bytes").unwrap();
            let mut cache = pin(&path);
            cache.sha256[0] ^= 1;
            fixture.binding.cache = Some(StartupCacheBinding::Pinned(cache));
        } else {
            fixture.binding.source.sha256[0] ^= 1;
        }
        error(fixture.recognize().await, "startup input digest");
        assert!(
            !fixture.runs.exists(),
            "invalid pin launched a startup child"
        );
        fixture.prefix.check_binding().unwrap();
    }
}

#[tokio::test]
async fn source_outside_bound_stdlib_and_unobserved_cache_path_are_rejected() {
    for outside_source in [false, true] {
        let mut fixture = Fixture::new().await;
        if outside_source {
            let path = fixture._owner.path().join("outside-site.py");
            fs::copy(&fixture.binding.source.path, &path).unwrap();
            fixture.binding.source = pin(&path);
        } else {
            fixture.binding.cache = Some(StartupCacheBinding::Missing {
                path: fixture.source.join("other-cache.pyc"),
            });
        }
        error(fixture.recognize().await, "startup input binding");
        assert_eq!(fixture.normal_runs(), 0);
    }
}

#[tokio::test]
async fn actual_loader_kind_origin_and_cache_selection_are_not_caller_booleans() {
    for (key, value) in [
        ("loader", json!("frozen")),
        ("site_origin", json!("/outside/site.py")),
        ("cache_path", json!("/outside/site.pyc")),
        ("code_selection", json!("unvalidated")),
    ] {
        let fixture = Fixture::new().await;
        let mut facts = fixture.facts();
        facts[key] = value;
        fixture.set_facts(&facts);
        error(fixture.recognize().await, "startup observation binding");
        assert_eq!(fixture.normal_runs(), 0);
    }
}

#[tokio::test]
async fn ignored_cfg_wrong_home_or_enabled_user_site_cannot_authorize_normal() {
    for (key, value) in [
        ("prefix", json!("/outside/base")),
        ("exec_prefix", json!("/outside/base")),
        ("home", json!("/other/bin")),
        ("user_site_enabled", json!(true)),
        ("no_site", json!(false)),
    ] {
        let fixture = Fixture::new().await;
        let mut facts = fixture.facts();
        facts[key] = value;
        fixture.set_facts(&facts);
        error(fixture.recognize().await, "cfg recognition");
        assert_eq!(fixture.normal_runs(), 0);
    }
}

#[tokio::test]
async fn outside_site_request_or_missing_interception_hook_is_not_recognition() {
    for outside in [false, true] {
        let fixture = Fixture::new().await;
        let mut facts = fixture.facts();
        if outside {
            facts["requested_sites"] = json!([fixture.source.join("lib/python3.11/site-packages")]);
        } else {
            facts["suppressed_hooks"].as_array_mut().unwrap().pop();
        }
        fixture.set_facts(&facts);
        error(fixture.recognize().await, "startup hook boundary");
        assert_eq!(fixture.normal_runs(), 0);
    }
}

#[tokio::test]
async fn startup_schema_unknown_fields_and_path_budgets_are_closed() {
    for malformed in 0..3 {
        let fixture = Fixture::new().await;
        let mut facts = fixture.facts();
        match malformed {
            0 => facts["schema"] = json!(2),
            1 => facts["unexpected"] = json!(true),
            _ => facts["requested_sites"] = json!(["relative/site-packages"]),
        }
        fixture.set_facts(&facts);
        error(fixture.recognize().await, "startup protocol");
        assert_eq!(fixture.normal_runs(), 0);
    }
}

#[tokio::test]
async fn input_and_raw_request_limits_fail_before_any_startup_process() {
    let fixture = Fixture::new().await;
    for limits in [
        RuntimeStartupLimits {
            max_source_bytes: 0,
            ..Default::default()
        },
        RuntimeStartupLimits {
            max_source_bytes: 1,
            ..Default::default()
        },
        RuntimeStartupLimits {
            max_request_bytes: 1,
            ..Default::default()
        },
        RuntimeStartupLimits {
            max_path_bytes: 1,
            ..Default::default()
        },
    ] {
        error(
            establish_cfg_recognition(
                &fixture.runtime,
                &fixture.prefix,
                fixture.binding.clone(),
                limits,
                Duration::from_secs(5),
            )
            .await,
            "startup limit",
        );
        assert!(!fixture.runs.exists());
    }
}

#[tokio::test]
async fn another_allocation_is_a_zero_write_zero_startup_error() {
    let fixture = Fixture::new().await;
    let other = EnvironmentStore::open(
        &fixture
            ._owner
            .path()
            .canonicalize()
            .unwrap()
            .join("environments"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap()
    .allocate()
    .unwrap();
    error(
        establish_cfg_recognition(
            &fixture.runtime,
            &other,
            fixture.binding.clone(),
            RuntimeStartupLimits::default(),
            Duration::from_secs(5),
        )
        .await,
        "startup allocation",
    );
    assert!(fs::read_dir(other.final_path()).unwrap().next().is_none());
    other.check_binding().unwrap();
    assert!(!fixture.runs.exists());
}

#[tokio::test]
async fn modified_cfg_bytes_are_rejected_before_intercepted_site_runs() {
    let fixture = Fixture::new().await;
    fs::write(
        fixture.prefix.final_path().join("pyvenv.cfg"),
        b"home = /outside\ninclude-system-site-packages = true\n",
    )
    .unwrap();
    error(fixture.recognize().await, "startup bootstrap inventory");
    assert!(!fixture.runs.exists());
}

#[tokio::test]
async fn normal_empty_layout_consumes_recognition_and_observes_actual_scheme() {
    let mut fixture = Fixture::new().await;
    let evidence = fixture
        .recognize()
        .await
        .expect("recognition must be GREEN before counting normal-stage RED");
    let observed = verify_empty_environment_layout(
        fixture.runtime,
        &mut fixture.prefix,
        evidence,
        RuntimeStartupLimits::default(),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(
        observed.actual_scheme().root(WheelScheme::Purelib),
        Some(
            fixture
                .prefix
                .final_path()
                .join("lib/python3.11/site-packages")
                .as_path()
        )
    );
    assert_eq!(observed.final_prefix(), fixture.prefix.final_path());
    assert_eq!(observed.allocation_id(), fixture.prefix.allocation_id());
    assert_eq!(
        observed.executable(),
        fixture.prefix.final_path().join("bin/python")
    );
    fixture.prefix.check_binding().unwrap();
}

#[tokio::test]
async fn nonempty_site_is_rejected_before_normal_startup_without_executing_a_trap() {
    let mut fixture = Fixture::new().await;
    let evidence = fixture
        .recognize()
        .await
        .expect("recognition must be GREEN before counting normal-stage RED");
    fixture
        .prefix
        .create_dir_all(Path::new("lib/python3.11/site-packages"))
        .unwrap();
    let mut output = fixture
        .prefix
        .create_file_exclusive(Path::new("lib/python3.11/site-packages/trap.pth"), 0o644)
        .unwrap();
    use std::io::Write;
    output
        .write_all(b"import os; raise RuntimeError('must not execute')\n")
        .unwrap();
    output.finish().unwrap();
    let runs = fixture.runs.clone();
    error(
        verify_empty_environment_layout(
            fixture.runtime,
            &mut fixture.prefix,
            evidence,
            RuntimeStartupLimits::default(),
            Duration::from_secs(5),
        )
        .await,
        "normal startup site is not empty",
    );
    assert!(
        !fs::read_to_string(runs)
            .unwrap_or_default()
            .lines()
            .any(|line| line == "normal")
    );
}

#[tokio::test]
async fn normal_wrong_prefix_or_scheme_never_returns_an_empty_layout() {
    for wrong_prefix in [false, true] {
        let mut fixture = Fixture::new().await;
        let evidence = fixture
            .recognize()
            .await
            .expect("recognition must be GREEN before counting normal-stage RED");
        let mut facts: Value =
            serde_json::from_slice(&fs::read(&fixture.normal_path).unwrap()).unwrap();
        if wrong_prefix {
            facts["prefix"] = json!("/outside/base");
        } else {
            facts["final_paths"]["purelib"] = json!("/outside/site-packages");
        }
        fs::write(&fixture.normal_path, serde_json::to_vec(&facts).unwrap()).unwrap();
        error(
            verify_empty_environment_layout(
                fixture.runtime,
                &mut fixture.prefix,
                evidence,
                RuntimeStartupLimits::default(),
                Duration::from_secs(5),
            )
            .await,
            "normal startup observation",
        );
    }
}

#[tokio::test]
async fn normal_final_verification_cannot_return_after_the_shared_deadline() {
    let mut fixture = Fixture::new().await;
    let evidence = fixture.recognize().await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let reached_final_phase = std::cell::Cell::new(false);
    let result = normal_attempt_with_finish_hook(
        fixture.runtime,
        &mut fixture.prefix,
        evidence,
        RuntimeStartupLimits::default(),
        deadline,
        || {
            reached_final_phase.set(true);
            // Model time spent in the last synchronous filesystem verification,
            // after both real child probes have already completed successfully.
            std::thread::sleep(
                deadline.saturating_duration_since(tokio::time::Instant::now())
                    + Duration::from_millis(5),
            );
        },
    )
    .await;
    assert!(
        reached_final_phase.get(),
        "final verification was not reached"
    );
    error(result, "normal startup deadline");
}

async fn live(label: &str) {
    let value = |suffix: &str| {
        std::env::var_os(format!("PAKRO_TEST_{label}_{suffix}"))
            .expect("explicit existing-runtime test input is required")
    };
    let executable = PathBuf::from(value("EXECUTABLE"));
    let source_root = PathBuf::from(value("SOURCE_ROOT")).canonicalize().unwrap();
    let loader_paths = std::env::var_os(format!("PAKRO_TEST_{label}_LOADER_FILES"))
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|path| !path.as_os_str().is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let handle = PythonRuntimeHandle::from_existing_executable(&executable).unwrap();
    let descriptor = crate::python_runtime::probe(&handle, Duration::from_secs(10))
        .await
        .unwrap();
    let mut inputs = vec![executable_pin(handle.executable())];
    inputs.extend(loader_paths.into_iter().map(|path| {
        let pin = pin(&path.canonicalize().unwrap());
        RuntimeInputExpectation {
            path: pin.path,
            sha256: pin.sha256,
            size: pin.size,
            mode: pin.mode,
            purpose: RuntimeInputPurpose::LoaderPayload,
        }
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
        &descriptor,
        RuntimeSourceBinding {
            trust: RuntimeSourceTrust::ExplicitExisting,
            authorized_roots: &[source_root],
            inputs: &inputs,
        },
        &prefix,
        RuntimeLayoutLimits::default(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let source_path = sources
        .layout()
        .base_paths
        .get("stdlib")
        .expect("selected runtime must observe stdlib")
        .join("site.py");
    let source_pin = pin(&source_path);
    let loader = match value("STARTUP_LOADER").to_str().unwrap() {
        "source" => StartupLoaderKind::Source,
        "frozen" => StartupLoaderKind::Frozen,
        _ => panic!("explicit observed source/frozen loader required"),
    };
    let cache = if loader == StartupLoaderKind::Source {
        let path = PathBuf::from(value("STARTUP_CACHE_PATH"));
        Some(if path.try_exists().unwrap() {
            StartupCacheBinding::Pinned(pin(&path))
        } else {
            StartupCacheBinding::Missing { path }
        })
    } else {
        None
    };
    let binding = StartupAdapterBinding {
        source: source_pin.clone(),
        cache,
        loader,
    };
    let runtime = bootstrap_runtime(
        prepare_runtime_bootstrap(sources, &prefix, RuntimeBootstrapLimits::default()).unwrap(),
        &mut prefix,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let evidence = establish_cfg_recognition(
        &runtime,
        &prefix,
        binding,
        RuntimeStartupLimits::default(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let observed = verify_empty_environment_layout(
        runtime,
        &mut prefix,
        evidence,
        RuntimeStartupLimits::default(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(observed.normal.prefix, prefix.final_path());
    assert_eq!(observed.normal.exec_prefix, prefix.final_path());
    assert!(!observed.normal.no_site);
    assert!(!observed.normal.user_site_enabled);
    assert_eq!(
        observed.runtime.observed_site_disabled().implementation,
        descriptor.implementation
    );
    assert_eq!(
        observed.runtime.observed_site_disabled().language_version,
        descriptor.language_version
    );
    assert_eq!(
        observed
            .runtime
            .observed_site_disabled()
            .implementation_version,
        descriptor.implementation_version
    );
    assert_eq!(
        observed.actual_scheme().root(WheelScheme::Purelib),
        Some(observed.normal.final_paths["purelib"].as_path())
    );
    assert!(
        observed
            .actual_scheme()
            .root(WheelScheme::Purelib)
            .unwrap()
            .starts_with(prefix.final_path())
    );
    // Preserve both facts, especially framework runtimes whose default sysconfig
    // roots can remain global while the explicit final scheme matches site.
    assert!(!observed.normal.default_paths.is_empty());
    assert_eq!(pin(&source_path).sha256, source_pin.sha256);
    prefix.check_binding().unwrap();
}

#[tokio::test]
#[ignore = "requires explicit CPYTHON39 existing runtime, source root, loader closure and site-loader inputs"]
async fn live_cpython39_framework_intercepted_then_normal_empty_prefix() {
    live("CPYTHON39").await;
}

#[tokio::test]
#[ignore = "requires explicit CPYTHON311 existing runtime, source root and frozen site-loader input"]
async fn live_cpython311_intercepted_then_normal_empty_prefix() {
    live("CPYTHON311").await;
}

#[tokio::test]
#[ignore = "requires explicit PYPY312 existing runtime, source root, loader closure and site-cache input"]
async fn live_pypy312_intercepted_then_normal_empty_prefix() {
    live("PYPY312").await;
}

#[tokio::test]
async fn caller_deadline_is_not_reset_before_normal_environment_creation() {
    let mut fixture = Fixture::new().await;
    let evidence = fixture.recognize().await.unwrap();
    let probes = fs::read(&fixture.probes).unwrap_or_default();
    let result = verify_empty_environment_layout_at_deadline(
        fixture.runtime,
        &mut fixture.prefix,
        evidence,
        RuntimeStartupLimits::default(),
        tokio::time::Instant::now() - Duration::from_millis(1),
    )
    .await;
    error(result, "deadline");
    assert_eq!(fs::read(&fixture.probes).unwrap_or_default(), probes);
    assert!(!fixture.prefix.final_path().join("lib").exists());
    fixture.prefix.check_binding().unwrap();
}
