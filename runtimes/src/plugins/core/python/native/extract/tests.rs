use super::super::pipeline::RuntimeInstallPlan;
use super::*;
use crate::python_runtime::PythonRuntimeDescriptor;
use eyre::Result;
use std::path::Path;
use std::time::Duration;
mod archive_modes;
mod owned;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use flate2::Compression;
use flate2::write::GzEncoder;
use pakro_store::Digest;
use serde_json::{Value, json};

use super::super::{
    RuntimeArchiveTarget, RuntimeFlavor, RuntimePlatform, parse_pbs_releases, parse_pypy_releases,
};

include!("../fixtures.rs");

const SYSCONFIG: &str = concat!(
    "# system configuration generated and used by the sysconfig module\n",
    "build_time_vars = {\"BINDIR\": \"/install/bin\", \"CFLAGS\": \"-I/install/include\"}\n",
);
const CONFIG_PATH: &str = "lib/python3.15/_sysconfigdata__darwin_darwin.py";

enum Entry {
    File(String, Vec<u8>, u32),
    Link(String, PathBuf),
}

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    archive: PathBuf,
    install: std::cell::RefCell<PathBuf>,
    calls: PathBuf,
    denied: PathBuf,
    owners: std::cell::RefCell<Vec<crate::libraries::language::repository::LanguageBuildOwner>>,
}

impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        Self {
            archive: root.join("held-private-archive"),
            install: std::cell::RefCell::new(root.join("安装 runtime 中文")),
            calls: root.join("fixed-probe-calls"),
            denied: root.join("unexpected-process-call"),
            root,
            _root: owner,
            owners: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn install_path(&self) -> PathBuf {
        self.install.borrow().clone()
    }

    fn observed(&self, pypy: bool, language: &str, implementation: &str, leaf: &str) -> Value {
        let tuple = |version: &str| {
            let parts: Vec<u32> = version
                .split('.')
                .map(|part| part.parse().unwrap())
                .collect();
            json!({"major":parts[0],"minor":parts[1],"micro":parts[2],"release_level":"final","serial":0})
        };
        let short = language.split('.').take(2).collect::<Vec<_>>().join(".");
        json!({
            "schema":1,"observation_mode":"base_runtime_site_disabled",
            "probe_flags":["-E","-s","-S","-B","-c"],"executable":self.install_path().join("bin").join(leaf),
            "implementation":if pypy {"pypy"} else {"cpython"},
            "language_version":tuple(language),"implementation_version":tuple(implementation),
            "implementation_version_source":if pypy {"pypy_version_info"} else {"sys_implementation_version"},
            "markers":{"implementation_name":if pypy {"pypy"} else {"cpython"},
                "implementation_version":implementation,"os_name":"posix","platform_machine":"arm64",
                "platform_python_implementation":if pypy {"PyPy"} else {"CPython"},
                "platform_release":"fixture kernel","platform_system":"Darwin","platform_version":"fixture kernel",
                "python_full_version":language,"python_version":short,"sys_platform":"darwin"},
            "soabi":null,"ext_suffix":null,"abiflags":if pypy {Value::Null} else {json!("")},
            "extension_suffixes":[],"gil_disabled":if pypy {Value::Null} else {json!(false)},
            "compatibility":{"pointer_bits":64,"debug":false,"pymalloc":true,"unicode_size":null,
                "platform":{"os":"macos","cpu_type":16777228,"product_version":{"major":14,"minor":5,"micro":0}}},
            "sysconfig_paths":{"stdlib":self.install_path().join("lib").join(format!("python{short}"))}
        })
    }

    fn script(&self, observation: &Value) -> Vec<u8> {
        let output = serde_json::to_string(observation)
            .unwrap()
            .replace(self.install_path().to_str().unwrap(), "@ROOT@");
        format!(
            "#!/bin/sh\nroot=$(CDPATH= cd -P -- \"$(/usr/bin/dirname -- \"$0\")/..\" && /bin/pwd)\nif [ \"$#\" -ne 6 ] || [ \"$1\" != '-E' ] || [ \"$2\" != '-s' ] || [ \"$3\" != '-S' ] || [ \"$4\" != '-B' ] || [ \"$5\" != '-c' ]; then\nprintf '%s\\n' \"$@\" >> {}\nexit 91\nfi\nprintf '%s\\n' \"$@\" >> {}\nprintf '%s\\n' {} | /usr/bin/sed \"s|@ROOT@|$root|g\"\n",
            quote(&self.denied), quote(&self.calls), shell_quote(&output),
        ).into_bytes()
    }

    fn make_archive(&self, entries: Vec<Entry>) {
        let file = fs::File::create(&self.archive).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            match entry {
                Entry::File(path, bytes, mode) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_mode(mode);
                    header.set_size(bytes.len() as u64);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, path, bytes.as_slice())
                        .unwrap();
                }
                Entry::Link(path, target) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_mode(0o777);
                    header.set_size(0);
                    builder.append_link(&mut header, path, target).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    fn plan(&self, pypy: bool) -> RuntimeInstallPlan {
        let target = RuntimeArchiveTarget::new("aarch64", RuntimePlatform::Macos).unwrap();
        let digest = Digest::from_sha256_bytes(&fs::read(&self.archive).unwrap());
        let releases = if pypy {
            let checksums = PYPY_CHECKSUMS.replace(
                "c10baeae4a61063a1183c4ee4e8324765ba736a3980323654efb31e79f1514db",
                &digest.hex(),
            );
            parse_pypy_releases(
                &serde_json::from_str(PYPY_FIXTURE).unwrap(),
                &checksums,
                &target,
            )
            .unwrap()
        } else {
            let mut raw: Value = serde_json::from_str(PBS_FIXTURE).unwrap();
            for asset in raw[0]["assets"].as_array_mut().unwrap() {
                asset["digest"] = json!(format!("sha256:{}", digest.hex()));
            }
            parse_pbs_releases(&raw, &target, RuntimeFlavor::InstallOnly, &BTreeMap::new()).unwrap()
        };
        RuntimeInstallPlan::from_release(&releases[0])
    }

    fn assert_only_fixed_probe(&self) {
        let calls = fs::read_to_string(&self.calls).unwrap();
        assert_eq!(
            calls.lines().take(5).collect::<Vec<_>>(),
            ["-E", "-s", "-S", "-B", "-c"]
        );
        assert!(
            !self.denied.exists(),
            "unexpected interpreter/manager invocation"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.owners.get_mut().clear();
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn quote(path: &Path) -> String {
    shell_quote(path.to_str().unwrap())
}

fn executable(path: &str, bytes: Vec<u8>) -> Entry {
    Entry::File(path.into(), bytes, 0o755)
}

fn owned_context(fixture: &Fixture, timeout: Duration) -> crate::programs::OperationContext {
    use crate::programs::{
        AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
        capture_operation_context,
    };
    capture_operation_context(OperationContextInputs {
        settings: &crate::config::Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: std::time::Instant::now() + timeout,
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 8,
                max_runtime_candidates: 8,
            },
            writes: AllowedWrites {
                libraries: true,
                project_selection: false,
                lockfile: false,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: fixture.root.join("data"),
        cache_root: fixture.root.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap()
}
async fn install_with_timeout(
    fixture: &Fixture,
    plan: &RuntimeInstallPlan,
    patch: bool,
    timeout: Duration,
) -> Result<PythonRuntimeDescriptor> {
    use crate::libraries::language::repository::{
        LanguageInstallSpec, LanguageRepository, LanguageReservation,
    };
    let context = owned_context(fixture, timeout);
    let spec = LanguageInstallSpec::from_plan(plan, patch)?;
    let LanguageReservation::Build(mut owner) =
        LanguageRepository::open_writer(&context)?.reserve_install(&spec)?
    else {
        eyre::bail!("fixture unexpectedly reused content")
    };
    fixture.install.replace(owner.final_path().to_path_buf());
    let mut archive = crate::artifact_snapshot::spool_verified_reader(
        fs::File::open(&fixture.archive)?,
        plan.source().filename(),
        plan.source().digest(),
        1024 * 1024,
        context.requested_cache_root(),
    )?;
    let result = install_into_owner(plan, &mut archive, &mut owner, patch)
        .await
        .map(|receipt| receipt.descriptor().clone());
    // Keep this actual allocation available for negative content assertions.
    fixture.owners.borrow_mut().push(owner);
    result
}
async fn install(
    fixture: &Fixture,
    plan: &RuntimeInstallPlan,
    patch: bool,
) -> Result<PythonRuntimeDescriptor> {
    install_with_timeout(fixture, plan, patch, Duration::from_secs(5)).await
}

#[tokio::test]
async fn matching_pbs_archive_strips_one_component_and_runs_only_the_fixed_probe() {
    let fixture = Fixture::new();
    let raw = fixture.observed(false, "3.15.0", "3.15.0", "python");
    fixture.make_archive(vec![executable("bundle/bin/python", fixture.script(&raw))]);
    let descriptor = install(&fixture, &fixture.plan(false), false)
        .await
        .unwrap();
    assert_eq!(descriptor.language_version.full_version(), "3.15.0");
    assert_eq!(
        descriptor.executable,
        fixture.install_path().join("bin/python")
    );
    assert!(!fixture.install_path().join("bundle").exists());
    fixture.assert_only_fixed_probe();
}

#[tokio::test]
async fn pypy_preserves_full_language_and_implementation_without_bootstrap() {
    for (language, implementation, accepted, error_text) in [
        ("3.12.15", "8.0.1", true, ""),
        ("3.12.14", "8.0.1", false, "language"),
        ("3.12.15", "8.0.2", false, "implementation"),
    ] {
        let fixture = Fixture::new();
        let raw = fixture.observed(true, language, implementation, "python");
        fixture.make_archive(vec![executable(
            "pypy-runtime/bin/python",
            fixture.script(&raw),
        )]);
        let result = install(&fixture, &fixture.plan(true), true).await;
        if accepted {
            let descriptor = result.unwrap();
            assert_eq!(descriptor.language_version.full_version(), language);
            assert_eq!(
                descriptor.implementation_version.full_version(),
                implementation
            );
        } else {
            let error = result.unwrap_err();
            assert!(format!("{error:#}").contains(error_text), "{error:#}");
        }
        fixture.assert_only_fixed_probe();
    }
}

#[tokio::test]
async fn mismatched_language_implementation_cpu_or_unknown_observations_are_errors() {
    for case in ["language", "implementation", "CPU", "compatibility", "GIL"] {
        let fixture = Fixture::new();
        let mut raw = fixture.observed(false, "3.15.0", "3.15.0", "python");
        match case {
            "language" => raw = fixture.observed(false, "3.15.1", "3.15.1", "python"),
            "implementation" => raw = fixture.observed(true, "3.15.0", "8.0.1", "python"),
            "CPU" => raw["compatibility"]["platform"]["cpu_type"] = json!(16777223),
            "compatibility" => raw["compatibility"] = Value::Null,
            _ => raw["gil_disabled"] = Value::Null,
        }
        fixture.make_archive(vec![executable("bundle/bin/python", fixture.script(&raw))]);
        let error = install(&fixture, &fixture.plan(false), false)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(case), "{case}: {error:#}");
        fixture.assert_only_fixed_probe();
    }
}

#[tokio::test]
async fn a_python3_only_archive_gets_a_contained_relative_alias() {
    let fixture = Fixture::new();
    let raw = fixture.observed(false, "3.15.0", "3.15.0", "python3");
    fixture.make_archive(vec![executable("bundle/bin/python3", fixture.script(&raw))]);
    let descriptor = install(&fixture, &fixture.plan(false), false)
        .await
        .unwrap();
    assert_eq!(
        descriptor.executable,
        fixture.install_path().join("bin/python3")
    );
    assert_eq!(
        fs::read_link(fixture.install_path().join("bin/python")).unwrap(),
        Path::new("python3")
    );
    fixture.assert_only_fixed_probe();
}

#[tokio::test]
async fn a_bad_layout_never_launches_an_interpreter() {
    let fixture = Fixture::new();
    let raw = fixture.observed(false, "3.15.0", "3.15.0", "python");
    fixture.make_archive(vec![executable(
        "bundle/no-bin/python",
        fixture.script(&raw),
    )]);
    let error = install(&fixture, &fixture.plan(false), false)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("entrypoint")
            || format!("{error:#}").contains("prefix inventory entry symlink is dangling"),
        "{error:#}"
    );
    assert!(!fixture.calls.exists());
    assert!(!fixture.denied.exists());
}

#[tokio::test]
async fn an_escaping_bin_or_interpreter_link_cannot_start_an_outside_process() {
    for bin_escape in [false, true] {
        let fixture = Fixture::new();
        let external = fixture.root.join("outside-bin");
        fs::create_dir(&external).unwrap();
        let outside_python = external.join("python");
        let mut raw = fixture.observed(false, "3.15.0", "3.15.0", "python");
        raw["executable"] = json!(outside_python);
        fs::write(&outside_python, fixture.script(&raw)).unwrap();
        fs::set_permissions(&outside_python, fs::Permissions::from_mode(0o755)).unwrap();
        let entry = if bin_escape {
            Entry::Link("bundle/bin".into(), external)
        } else {
            Entry::Link("bundle/bin/python".into(), outside_python)
        };
        fixture.make_archive(vec![entry]);
        let error = install(&fixture, &fixture.plan(false), false)
            .await
            .unwrap_err();
        let text = format!("{error:#}");
        let containment_error = text.contains("contained Python runtime entrypoint")
            && error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::InvalidData)
            });
        let unsafe_archive_link = text.contains("archive")
            && (text.contains("symlink")
                || text.contains("traversal")
                || text.contains("absolute archive path"));
        let owned_link_rejection = text.contains("extracting verified Python runtime archive")
            && text.contains("language link target is not relative");
        assert!(
            containment_error || unsafe_archive_link || owned_link_rejection,
            "{text}"
        );
        assert!(!fixture.calls.exists(), "outside process was started");
        assert!(!fixture.denied.exists());
    }
}

#[tokio::test]
async fn a_dangling_primary_alias_does_not_fall_back_and_start_python3() {
    let fixture = Fixture::new();
    let raw = fixture.observed(false, "3.15.0", "3.15.0", "python3");
    fixture.make_archive(vec![
        Entry::Link("bundle/bin/python".into(), PathBuf::from("missing")),
        executable("bundle/bin/python3", fixture.script(&raw)),
    ]);
    let error = install(&fixture, &fixture.plan(false), false)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("entrypoint")
            || format!("{error:#}").contains("prefix inventory entry symlink is dangling"),
        "{error:#}"
    );
    assert!(!fixture.calls.exists());
}

#[tokio::test]
async fn optional_sysconfig_patch_occurs_after_runtime_identity_validation() {
    for accepted in [true, false] {
        let fixture = Fixture::new();
        let version = if accepted { "3.15.0" } else { "3.15.1" };
        let raw = fixture.observed(false, version, version, "python");
        fixture.make_archive(vec![
            executable("bundle/bin/python", fixture.script(&raw)),
            Entry::File(
                format!("bundle/{CONFIG_PATH}"),
                SYSCONFIG.as_bytes().to_vec(),
                0o644,
            ),
        ]);
        let result = install(&fixture, &fixture.plan(false), true).await;
        if accepted {
            result.unwrap();
            let patched = fs::read_to_string(fixture.install_path().join(CONFIG_PATH)).unwrap();
            assert!(patched.contains("PYTHON_BUILD_STANDALONE"));
            assert!(!patched.contains("/install/bin"));
        } else {
            let error = result.unwrap_err();
            assert!(format!("{error:#}").contains("language"), "{error:#}");
            assert_eq!(
                fs::read_to_string(fixture.install_path().join(CONFIG_PATH)).unwrap(),
                SYSCONFIG
            );
        }
        fixture.assert_only_fixed_probe();
    }
}

#[tokio::test]
async fn sysconfig_patch_cannot_follow_escaping_library_directories() {
    for whole_lib in [true, false] {
        let fixture = Fixture::new();
        let external = fixture.root.join("outside-sysconfig");
        let external_config = if whole_lib {
            external.join("python3.15/_sysconfigdata__darwin_darwin.py")
        } else {
            external.join("_sysconfigdata__darwin_darwin.py")
        };
        fs::create_dir_all(external_config.parent().unwrap()).unwrap();
        fs::write(&external_config, SYSCONFIG).unwrap();
        let raw = fixture.observed(false, "3.15.0", "3.15.0", "python");
        fixture.make_archive(vec![
            executable("bundle/bin/python", fixture.script(&raw)),
            Entry::Link(
                if whole_lib {
                    "bundle/lib"
                } else {
                    "bundle/lib/python3.15"
                }
                .into(),
                external,
            ),
        ]);
        let plan = fixture.plan(false);

        // An outside standard library is no longer a valid unpatched input:
        // every owned extraction must refuse it before launching the probe.
        for patch in [false, true] {
            assert!(install(&fixture, &plan, patch).await.is_err());
            assert!(
                !fixture.calls.exists(),
                "unsafe standard library was probed"
            );
            assert_eq!(fs::read_to_string(&external_config).unwrap(), SYSCONFIG);
        }
    }
}

#[tokio::test]
async fn malformed_archive_returns_an_extraction_error_without_a_process() {
    let fixture = Fixture::new();
    fs::write(&fixture.archive, b"not a gzip runtime archive").unwrap();
    let error = install(&fixture, &fixture.plan(false), false)
        .await
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("extracting verified Python runtime archive"),
        "{text}"
    );
    assert!(text.contains("gzip"), "{text}");
    assert!(
        error
            .chain()
            .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
        "malformed compressed input must produce an actual I/O decoding error: {text}"
    );
    assert!(!fixture.calls.exists());
    assert!(!fixture.denied.exists());
}

#[tokio::test(start_paused = true)]
async fn the_fixed_probe_timeout_is_propagated() {
    use crate::libraries::language::repository::{
        LanguageInstallSpec, LanguageRepository, LanguageReservation,
    };
    let fixture = Fixture::new();
    let pid_file = fixture.root.join("probe.pid");
    fixture.make_archive(vec![executable(
        "bundle/bin/python",
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > {}\nprintf started > {}\nexec /bin/sleep 10\n",
            quote(&pid_file),
            quote(&fixture.calls)
        )
        .into_bytes(),
    )]);
    // Preparation uses the normal fixture budget; the owner deadline is never reset.
    let preparation_started = std::time::Instant::now();
    let context = owned_context(&fixture, Duration::from_secs(5));
    let plan = fixture.plan(false);
    let spec = LanguageInstallSpec::from_plan(&plan, false).unwrap();
    let LanguageReservation::Build(mut owner) = LanguageRepository::open_writer(&context)
        .unwrap()
        .reserve_install(&spec)
        .unwrap()
    else {
        panic!("fixture unexpectedly reused content")
    };
    fixture.install.replace(owner.final_path().to_path_buf());
    let deadline = tokio::time::Instant::from_std(owner.deadline());
    let mut archive = crate::artifact_snapshot::spool_verified_reader(
        fs::File::open(&fixture.archive).unwrap(),
        plan.source().filename(),
        plan.source().digest(),
        1024 * 1024,
        context.requested_cache_root(),
    )
    .unwrap();
    // Real child I/O must not let Tokio auto-advance the paused clock.
    let (hold_clock, release_clock) = std::sync::mpsc::channel::<()>();
    let clock_guard = tokio::task::spawn_blocking(move || {
        let _ = release_clock.recv();
    });
    let mut installing = Box::pin(install_into_owner(&plan, &mut archive, &mut owner, false));
    while fs::read(&fixture.calls).ok().as_deref() != Some(&b"started"[..]) {
        assert!(preparation_started.elapsed() < Duration::from_secs(5), "script did not start within the preparation budget");
        tokio::select! {
            result = &mut installing => panic!("installation ended before the script started: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
    }
    let pid = nix::unistd::Pid::from_raw(fs::read_to_string(&pid_file).unwrap().parse().unwrap());
    assert_eq!(nix::sys::signal::kill(pid, None), Ok(()));
    let remaining = deadline.duration_since(tokio::time::Instant::now());
    tokio::time::advance(remaining.checked_sub(Duration::from_millis(500)).unwrap()).await;
    assert!(futures_util::poll!(&mut installing).is_pending(), "installation ended before the original deadline");
    assert_eq!(nix::sys::signal::kill(pid, None), Ok(()));
    // Tokio timers have millisecond granularity; cross the fixed deadline by one tick.
    tokio::time::advance(Duration::from_millis(501)).await;
    drop(hold_clock);
    clock_guard.await.unwrap();
    let error = installing.await.unwrap_err();
    assert!(tokio::time::Instant::now() <= deadline + Duration::from_millis(1), "the original deadline was extended");
    assert!(format!("{error:#}").contains("timed out"), "{error:#}");
    assert!(format!("{error:#}").contains("probing extracted Python runtime"), "{error:#}");
    assert_eq!(
        fs::read(&fixture.calls).unwrap(),
        b"started",
        "the original deadline must expire during the real probe, after allocation"
    );
    assert_eq!(nix::sys::signal::kill(pid, None), Err(nix::errno::Errno::ESRCH));
    assert!(!fixture.denied.exists());
}
