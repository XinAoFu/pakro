//! Observations from an explicitly bound, already installed Python runtime.
//!
//! The fixed standard-library script runs with site disabled and a cleared
//! environment. Its paths describe a base runtime, not an activated environment.
//! Runtime-provided standard libraries can consult platform helpers; this module
//! does not provide an operating-system process sandbox.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use eyre::{Context, ContextCompat, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::toolset::ToolVersion;

const SCRIPT: &str = include_str!("python_runtime_probe.py");
const FLAGS: [&str; 5] = ["-E", "-s", "-S", "-B", "-c"];
const STDOUT_LIMIT: u64 = 1024 * 1024;
const STDERR_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Clone)]
/// An existing interpreter selected without PATH lookup or installation.
pub struct PythonRuntimeHandle {
    install_root: Option<PathBuf>,
    executable: PathBuf,
}

impl PythonRuntimeHandle {
    /// Bind a resolved, installed core Python tool using its actual layout.
    /// Existing backend resolution can consult the loaded application config.
    pub fn from_tool_version(tv: &ToolVersion) -> Result<Self> {
        ensure!(
            tv.ba().backend_type() == crate::backend::backend_type::BackendType::Core
                && tv.ba().tool_name == "python",
            "expected a core Python ToolVersion"
        );
        let root = tv
            .install_path()
            .canonicalize()
            .context("Python install root")?;
        ensure!(root.is_dir(), "Python install root is not a directory");
        let executable = crate::plugins::core::python::python_path(tv);
        let mut handle = Self::from_existing_executable(&executable.canonicalize()?)?;
        handle.install_root = Some(root);
        Ok(handle)
    }

    /// Bind an absolute, existing file and resolve its symbolic links.
    /// Executability is checked when the probe is launched; no runtime is fetched.
    pub fn from_existing_executable(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute(),
            "Python executable must be an explicit absolute path"
        );
        let executable = path.canonicalize().context("Python executable")?;
        ensure!(executable.is_file(), "Python executable is not a file");
        Ok(Self {
            install_root: None,
            executable,
        })
    }

    /// The canonical interpreter path used to launch the probe.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The canonical installation root when bound through a tool version.
    pub fn install_root(&self) -> Option<&Path> {
        self.install_root.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Python implementations supported by this observation protocol.
pub enum PythonImplementation {
    CPython,
    PyPy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// The interpreter field supplying the implementation version.
pub enum ImplementationVersionSource {
    SysVersionInfo,
    SysImplementationVersion,
    PypyVersionInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Release levels reported in Python version tuples.
pub enum ReleaseLevel {
    Alpha,
    Beta,
    Candidate,
    Final,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A version tuple observation, independent of a distribution's release label.
pub struct PythonVersion {
    pub major: u32,
    pub minor: u32,
    pub micro: u32,
    pub release_level: ReleaseLevel,
    pub serial: u32,
}

impl PythonVersion {
    /// Format the full patch and prerelease version without ordering versions.
    pub fn full_version(&self) -> String {
        let base = format!("{}.{}.{}", self.major, self.minor, self.micro);
        match self.release_level {
            ReleaseLevel::Alpha => format!("{base}a{}", self.serial),
            ReleaseLevel::Beta => format!("{base}b{}", self.serial),
            ReleaseLevel::Candidate => format!("{base}rc{}", self.serial),
            ReleaseLevel::Final => base,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// The eleven PEP 508 marker values observed in the selected interpreter.
pub struct PythonMarkerValues {
    pub implementation_name: String,
    pub implementation_version: String,
    pub os_name: String,
    pub platform_machine: String,
    pub platform_python_implementation: String,
    pub platform_release: String,
    pub platform_system: String,
    pub platform_version: String,
    pub python_full_version: String,
    pub python_version: String,
    pub sys_platform: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Local observations validated by [`probe`] before it returns.
///
/// ABI fields and paths do not prove component availability or complete wheel
/// compatibility. Paths are local bindings, not a portable lockfile projection.
/// Direct deserialization or mutation of public fields does not run validation.
pub struct PythonRuntimeDescriptor {
    pub schema: u32,
    pub observation_mode: String,
    pub probe_flags: Vec<String>,
    pub executable: PathBuf,
    pub implementation: PythonImplementation,
    pub language_version: PythonVersion,
    pub implementation_version: PythonVersion,
    pub implementation_version_source: ImplementationVersionSource,
    pub markers: PythonMarkerValues,
    pub soabi: Option<String>,
    pub ext_suffix: Option<String>,
    pub abiflags: Option<String>,
    pub extension_suffixes: Vec<String>,
    pub gil_disabled: Option<bool>,
    pub sysconfig_paths: BTreeMap<String, String>,
}

fn decode(handle: &PythonRuntimeHandle, bytes: &[u8]) -> Result<PythonRuntimeDescriptor> {
    let text = std::str::from_utf8(bytes).context("invalid Python runtime probe UTF-8")?;
    let descriptor: PythonRuntimeDescriptor =
        serde_json::from_str(text).context("invalid Python runtime probe JSON")?;
    ensure!(
        descriptor.schema == 1,
        "unsupported Python runtime probe schema"
    );
    ensure!(
        descriptor.observation_mode == "base_runtime_site_disabled"
            && descriptor.probe_flags.iter().map(String::as_str).eq(FLAGS),
        "unexpected Python runtime probe observation mode"
    );
    ensure!(
        descriptor.executable.is_absolute(),
        "probe executable is not absolute"
    );
    ensure!(
        descriptor.executable.canonicalize()? == handle.executable,
        "probe reported a different executable"
    );
    let language = &descriptor.language_version;
    let implementation = &descriptor.implementation_version;
    ensure!(language.major == 3, "only Python 3.x is in scope");
    ensure!(
        language.release_level != ReleaseLevel::Final || language.serial == 0,
        "invalid final language version"
    );
    ensure!(
        implementation.release_level != ReleaseLevel::Final || implementation.serial == 0,
        "invalid final implementation version"
    );
    let (name, display) = match descriptor.implementation {
        PythonImplementation::CPython => {
            ensure!(
                implementation == language,
                "CPython implementation version differs from language version"
            );
            ensure!(
                descriptor.implementation_version_source
                    != ImplementationVersionSource::PypyVersionInfo,
                "invalid CPython implementation version source"
            );
            ("cpython", "CPython")
        }
        PythonImplementation::PyPy => {
            ensure!(
                descriptor.implementation_version_source
                    == ImplementationVersionSource::PypyVersionInfo,
                "PyPy requires pypy_version_info as its implementation version source"
            );
            ("pypy", "PyPy")
        }
    };
    let markers = &descriptor.markers;
    ensure!(
        markers.implementation_name == name
            && markers.platform_python_implementation == display
            && markers.implementation_version == implementation.full_version()
            && markers.python_full_version == language.full_version()
            && markers.python_version == format!("{}.{}", language.major, language.minor),
        "runtime versions and marker values disagree"
    );
    ensure!(
        markers.os_name == "posix" && !markers.platform_machine.is_empty(),
        "invalid POSIX runtime platform observation"
    );
    ensure!(
        (markers.sys_platform == "darwin" && markers.platform_system == "Darwin")
            || (markers.sys_platform.starts_with("linux") && markers.platform_system == "Linux"),
        "only macOS/Linux runtime descriptors are in scope"
    );
    Ok(descriptor)
}

fn probe_command(handle: &PythonRuntimeHandle, cwd: &Path) -> Command {
    let mut command = Command::new(handle.executable());
    command.args(FLAGS).arg(SCRIPT);
    command
        .env_clear()
        .env("LC_ALL", "C")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn read_bounded(reader: impl AsyncRead + Unpin, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Python runtime probe output exceeds {limit} bytes"),
        ));
    }
    Ok(bytes)
}

async fn kill_and_reap(child: &mut tokio::process::Child) -> Result<()> {
    child.start_kill().context("killing Python runtime probe")?;
    child.wait().await.context("reaping Python runtime probe")?;
    Ok(())
}

/// Observe a base Python runtime with a fixed standard-library script.
///
/// Accepts at most 1 MiB of stdout and 64 KiB of stderr, reading one extra byte
/// to detect overflow. The cleared environment may prevent runtimes needing
/// additional loader variables from launching. This does not install a runtime.
///
/// # Errors
/// Returns errors for unsupported platforms, zero timeouts, launch failures,
/// output overflow, nonzero exits, invalid or inconsistent observations, and
/// timeout. Timeout and read-error paths kill and wait for the direct child.
///
/// # Cancellation
/// Dropping the future requests termination through `kill_on_drop`; Tokio only
/// provides best-effort reaping in that case, rather than an awaited guarantee.
pub async fn probe(
    handle: &PythonRuntimeHandle,
    timeout: Duration,
) -> Result<PythonRuntimeDescriptor> {
    ensure!(
        cfg!(any(target_os = "macos", target_os = "linux")),
        "Python runtime probe currently supports macOS/Linux"
    );
    ensure!(
        !timeout.is_zero(),
        "Python runtime probe timeout must be positive"
    );
    let cwd = tempfile::tempdir().context("creating empty Python probe cwd")?;
    let mut child = probe_command(handle, cwd.path())
        .spawn()
        .context("launching selected Python runtime")?;
    let stdout = child
        .stdout
        .take()
        .context("Python probe stdout pipe missing")?;
    let stderr = child
        .stderr
        .take()
        .context("Python probe stderr pipe missing")?;
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            child.wait(),
            read_bounded(stdout, STDOUT_LIMIT),
            read_bounded(stderr, STDERR_LIMIT)
        )
    })
    .await;
    let (status, stdout, stderr) = match result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            kill_and_reap(&mut child).await?;
            return Err(error).context("reading Python runtime probe output");
        }
        Err(_) => {
            kill_and_reap(&mut child).await?;
            bail!("Python runtime probe timed out");
        }
    };
    if !status.success() {
        let stderr: String = String::from_utf8_lossy(&stderr)
            .chars()
            .take(1024)
            .collect();
        bail!("Python runtime probe failed with {}: {}", status, stderr);
    }
    decode(handle, &stdout)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use crate::toolset::{ToolRequest, ToolSource};
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    use std::sync::Arc;

    fn selected() -> (tempfile::TempDir, PythonRuntimeHandle) {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("python");
        std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let handle = PythonRuntimeHandle::from_existing_executable(&executable).unwrap();
        (root, handle)
    }

    fn fixture(handle: &PythonRuntimeHandle, pypy: bool) -> Value {
        let language = json!({"major":3,"minor":10,"micro":14,"release_level":"final","serial":0});
        let implementation = if pypy {
            json!({"major":7,"minor":3,"micro":17,"release_level":"final","serial":0})
        } else {
            language.clone()
        };
        json!({
            "schema":1,"observation_mode":"base_runtime_site_disabled",
            "probe_flags":["-E","-s","-S","-B","-c"],"executable":handle.executable(),
            "implementation":if pypy {"pypy"} else {"cpython"},
            "language_version":language,"implementation_version":implementation,
            "implementation_version_source":if pypy {"pypy_version_info"} else {"sys_version_info"},
            "markers":{
                "implementation_name":if pypy {"pypy"} else {"cpython"},
                "implementation_version":if pypy {"7.3.17"} else {"3.10.14"},
                "os_name":"posix","platform_machine":"aarch64",
                "platform_python_implementation":if pypy {"PyPy"} else {"CPython"},
                "platform_release":"6.1.0","platform_system":"Linux","platform_version":"fixture kernel",
                "python_full_version":"3.10.14","python_version":"3.10","sys_platform":"linux"
            },
            "soabi":null,"ext_suffix":null,"abiflags":null,"extension_suffixes":[],
            "gil_disabled":null,"sysconfig_paths":{"stdlib":"/fixture/lib/python3.10"}
        })
    }

    fn parsed(handle: &PythonRuntimeHandle, value: &Value) -> Result<PythonRuntimeDescriptor> {
        decode(handle, &serde_json::to_vec(value).unwrap())
    }

    fn rewrite(handle: &PythonRuntimeHandle, body: &str) {
        std::fs::write(handle.executable(), format!("#!/bin/sh\n{body}\n")).unwrap();
    }

    #[test]
    fn versions_include_patch_and_prerelease() {
        let mut version = PythonVersion {
            major: 3,
            minor: 12,
            micro: 0,
            release_level: ReleaseLevel::Alpha,
            serial: 1,
        };
        assert_eq!(version.full_version(), "3.12.0a1");
        version.release_level = ReleaseLevel::Beta;
        assert_eq!(version.full_version(), "3.12.0b1");
        version.release_level = ReleaseLevel::Candidate;
        assert_eq!(version.full_version(), "3.12.0rc1");
        version.release_level = ReleaseLevel::Final;
        version.serial = 0;
        assert_eq!(version.full_version(), "3.12.0");
    }

    #[test]
    fn cpython_has_full_patch_and_unknown_abi_is_preserved() {
        let (_root, handle) = selected();
        let descriptor = parsed(&handle, &fixture(&handle, false)).unwrap();
        assert_eq!(descriptor.markers.python_full_version, "3.10.14");
        assert_eq!(descriptor.markers.python_version, "3.10");
        assert_eq!(descriptor.implementation_version.full_version(), "3.10.14");
        assert_eq!(descriptor.soabi, None);
        assert_eq!(descriptor.ext_suffix, None);
        assert_eq!(descriptor.abiflags, None);
        assert_eq!(descriptor.gil_disabled, None);
    }

    #[test]
    fn pypy_language_and_implementation_versions_are_distinct() {
        let (_root, handle) = selected();
        let descriptor = parsed(&handle, &fixture(&handle, true)).unwrap();
        assert_eq!(descriptor.language_version.full_version(), "3.10.14");
        assert_eq!(descriptor.markers.implementation_version, "7.3.17");
        assert_eq!(
            descriptor.implementation_version_source,
            ImplementationVersionSource::PypyVersionInfo
        );
    }

    #[test]
    fn markers_keep_observed_platform_instead_of_build_host() {
        let (_root, handle) = selected();
        let mut value = fixture(&handle, false);
        for platform in ["linux2", "linux3"] {
            value["markers"]["sys_platform"] = json!(platform);
            assert_eq!(
                parsed(&handle, &value).unwrap().markers.sys_platform,
                platform
            );
        }
        value["markers"]["sys_platform"] = json!("darwin");
        value["markers"]["platform_system"] = json!("Darwin");
        value["markers"]["platform_machine"] = json!("arm64");
        assert_eq!(
            parsed(&handle, &value).unwrap().markers.platform_machine,
            "arm64"
        );
    }

    #[test]
    fn corrupt_or_inconsistent_descriptors_fail() {
        let (_root, handle) = selected();
        assert!(decode(&handle, b"not json").is_err());
        let base = fixture(&handle, false);
        let mut value = base.clone();
        value["schema"] = json!(2);
        assert!(parsed(&handle, &value).is_err());
        let mut value = base.clone();
        value["implementation"] = json!("graalpy");
        assert!(parsed(&handle, &value).is_err());
        let mut value = base.clone();
        value["markers"]["python_full_version"] = json!("3.10");
        assert!(parsed(&handle, &value).is_err());
        let mut value = base.clone();
        value["language_version"]
            .as_object_mut()
            .unwrap()
            .remove("micro");
        assert!(parsed(&handle, &value).is_err());
        let mut value = base;
        value["language_version"]["major"] = json!(2);
        assert!(parsed(&handle, &value).is_err());
        let mut value = fixture(&handle, true);
        value["implementation_version_source"] = json!("sys_implementation_version");
        assert!(parsed(&handle, &value).is_err());
        let mut value = fixture(&handle, false);
        value["unexpected"] = json!(true);
        assert!(parsed(&handle, &value).is_err());
    }

    #[test]
    fn executable_must_be_selected_absolute_existing_file() {
        let (_root, handle) = selected();
        let (_other_root, other) = selected();
        let mut value = fixture(&handle, false);
        value["executable"] = json!(other.executable());
        assert!(parsed(&handle, &value).is_err());
        value["executable"] = json!("python");
        assert!(parsed(&handle, &value).is_err());
        assert!(PythonRuntimeHandle::from_existing_executable(Path::new("python")).is_err());
        assert!(
            PythonRuntimeHandle::from_existing_executable(&handle.executable().join("missing"))
                .is_err()
        );
    }

    #[test]
    fn handle_honors_real_tool_version_install_path() {
        let root = tempfile::tempdir().unwrap();
        let custom = root.path().join("custom-runtime");
        std::fs::create_dir_all(custom.join("bin")).unwrap();
        let executable = custom.join("bin/python");
        std::fs::write(&executable, b"fixture").unwrap();
        let backend = Arc::new(crate::plugins::core::new_backend_arg("python"));
        let request = ToolRequest::new(backend, "3.10.14", ToolSource::Argument).unwrap();
        let mut tv = ToolVersion::new(request, "3.10.14".into());
        tv.install_path = Some(custom.clone());
        let handle = PythonRuntimeHandle::from_tool_version(&tv).unwrap();
        assert_eq!(handle.executable(), executable.canonicalize().unwrap());
        assert_eq!(
            handle.install_root(),
            Some(custom.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn explicit_core_python_honors_custom_install_path() {
        let root = tempfile::tempdir().unwrap();
        let custom = root.path().join("custom-runtime");
        std::fs::create_dir_all(custom.join("bin")).unwrap();
        let executable = custom.join("bin/python");
        std::fs::write(&executable, b"fixture").unwrap();
        let backend = Arc::new(crate::args::BackendArg::new(
            "core:python".into(),
            Some("core:python".into()),
        ));
        let request = ToolRequest::new(backend, "3.10.14", ToolSource::Argument).unwrap();
        let mut tv = ToolVersion::new(request, "3.10.14".into());
        tv.install_path = Some(custom.clone());
        let handle = PythonRuntimeHandle::from_tool_version(&tv).unwrap();
        assert_eq!(handle.executable(), executable.canonicalize().unwrap());
        assert_eq!(
            handle.install_root(),
            Some(custom.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn python_shorthand_for_plugin_backend_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let custom = root.path().join("custom-runtime");
        std::fs::create_dir_all(custom.join("bin")).unwrap();
        std::fs::write(custom.join("bin/python"), b"fixture").unwrap();
        let backend = Arc::new(crate::args::BackendArg::new(
            "python".into(),
            Some("asdf:python".into()),
        ));
        let request = ToolRequest::new(backend, "3.10.14", ToolSource::Argument).unwrap();
        let mut tv = ToolVersion::new(request, "3.10.14".into());
        tv.install_path = Some(custom.clone());
        let error = PythonRuntimeHandle::from_tool_version(&tv).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expected a core Python ToolVersion")
        );
    }

    #[test]
    fn command_uses_legacy_compatible_flags_and_empty_environment() {
        let (_root, handle) = selected();
        let cwd = tempfile::tempdir().unwrap();
        let command = probe_command(&handle, cwd.path());
        let command = command.as_std();
        let args: Vec<_> = command
            .get_args()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..5], ["-E", "-s", "-S", "-B", "-c"]);
        assert!(!args.iter().any(|arg| arg == "-I" || arg == "-P"));
        assert_eq!(command.get_program(), handle.executable().as_os_str());
        assert_eq!(command.get_current_dir(), Some(cwd.path()));
        let env: Vec<_> = command.get_envs().collect();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "LC_ALL");
        assert_eq!(env[0].1.unwrap(), "C");
    }

    #[tokio::test]
    async fn process_success_is_decoded() {
        let (_root, handle) = selected();
        let json = serde_json::to_string(&fixture(&handle, false)).unwrap();
        let quoted = json.replace('\'', "'\"'\"'");
        rewrite(&handle, &format!("printf '%s\\n' '{quoted}'"));
        let descriptor = probe(&handle, Duration::from_secs(2)).await.unwrap();
        assert_eq!(descriptor.markers.python_full_version, "3.10.14");
    }

    #[tokio::test]
    async fn inherited_home_is_not_visible_to_probe() {
        assert!(
            std::env::var_os("HOME").is_some(),
            "test harness must define HOME"
        );
        let (_root, handle) = selected();
        let json = serde_json::to_string(&fixture(&handle, false)).unwrap();
        let quoted = json.replace('\'', "'\"'\"'");
        rewrite(
            &handle,
            &format!(
                "if [ -n \"${{HOME+x}}\" ]; then printf 'inherited HOME' >&2; exit 8; fi\nprintf '%s\\n' '{quoted}'"
            ),
        );
        assert!(probe(&handle, Duration::from_secs(2)).await.is_ok());
    }

    #[tokio::test]
    async fn nonzero_exit_and_invalid_json_do_not_fallback() {
        let (_root, handle) = selected();
        rewrite(&handle, "printf 'fixture failure' >&2; exit 7");
        let error = probe(&handle, Duration::from_secs(2)).await.unwrap_err();
        assert!(error.to_string().contains("fixture failure"));
        rewrite(&handle, "printf 'invalid json'");
        assert!(probe(&handle, Duration::from_secs(2)).await.is_err());
    }

    #[tokio::test]
    async fn timeout_is_explicit_and_zero_timeout_fails() {
        let (_root, handle) = selected();
        rewrite(&handle, "exec /bin/sleep 2");
        let error = probe(&handle, Duration::from_millis(20)).await.unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(probe(&handle, Duration::ZERO).await.is_err());
    }

    #[tokio::test]
    async fn timeout_terminates_the_observed_process() {
        let (root, handle) = selected();
        let pid_file = root.path().join("probe.pid");
        let quoted = pid_file.to_str().unwrap().replace('\'', "'\"'\"'");
        rewrite(
            &handle,
            &format!("printf '%s\\n' \"$$\" > '{quoted}'\nexec /bin/sleep 10"),
        );
        let error = probe(&handle, Duration::from_secs(5)).await.unwrap_err();
        assert!(error.to_string().contains("timed out"));
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH),
        );
    }

    #[tokio::test]
    async fn stdout_and_stderr_are_bounded_before_json_parsing() {
        let (_root, handle) = selected();
        rewrite(&handle, "exec /usr/bin/head -c 1048577 /dev/zero");
        let error = probe(&handle, Duration::from_secs(2)).await.unwrap_err();
        assert!(format!("{error:#}").contains("exceeds 1048576 bytes"));
        rewrite(&handle, "exec /usr/bin/head -c 65537 /dev/zero >&2");
        let error = probe(&handle, Duration::from_secs(2)).await.unwrap_err();
        assert!(format!("{error:#}").contains("exceeds 65536 bytes"));
    }

    fn real_handle() -> PythonRuntimeHandle {
        let executable = std::env::var_os("PAK_TEST_PYTHON")
            .expect("set PAK_TEST_PYTHON to an already existing absolute interpreter");
        PythonRuntimeHandle::from_existing_executable(Path::new(&executable)).unwrap()
    }

    async fn fixture_output(handle: &PythonRuntimeHandle, script: &str) -> std::process::Output {
        let cwd = tempfile::tempdir().unwrap();
        let mut command = Command::new(handle.executable());
        command
            .args(FLAGS)
            .arg(script)
            .env_clear()
            .env("LC_ALL", "C")
            .current_dir(cwd.path())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        tokio::time::timeout(Duration::from_secs(10), command.output())
            .await
            .expect("script fixture timed out")
            .expect("script fixture launch failed")
    }

    const SCRIPT_FIXTURE: &str = r#"
import sys
import json
import os
import types
import builtins
import importlib.machinery

modern_config = types.ModuleType("sysconfig")
modern_config.get_config_var = lambda key: None
modern_config.get_paths = lambda: {"purelib": "/fixture/sysconfig"}
sys.modules["sysconfig"] = modern_config

legacy_config = types.ModuleType("distutils.sysconfig")
legacy_config.get_config_var = lambda key: None
legacy_config.get_python_lib = lambda plat_specific=False, standard_lib=False: "/fixture/distutils"
legacy_config.get_python_inc = lambda plat_specific=False: "/fixture/include"
distutils = types.ModuleType("distutils")
distutils.__path__ = []
distutils.sysconfig = legacy_config
sys.modules["distutils"] = distutils
sys.modules["distutils.sysconfig"] = legacy_config

legacy_imp = types.ModuleType("imp")
legacy_imp.C_EXTENSION = 3
legacy_imp.get_suffixes = lambda: [(".legacy-fixture.so", "rb", 3)]
sys.modules["imp"] = legacy_imp
importlib.machinery.EXTENSION_SUFFIXES = [".modern-fixture.so"]

display = "PyPy" if CASE == "old-pypy" else "CPython"
platform = types.ModuleType("platform")
platform.python_implementation = lambda: display
sys.modules["platform"] = platform

if CASE == "old-cpython":
    sys.version_info = (3, 0, 1, "final", 0)
    if hasattr(sys, "implementation"):
        del sys.implementation
elif CASE == "old-pypy":
    sys.version_info = (3, 7, 10, "final", 0)
    sys.implementation = types.SimpleNamespace(
        name="pypy", version=sys.version_info, cache_tag=None)
    sys.pypy_version_info = (7, 3, 5, "final", 0)
else:
    sys.version_info = (3, 12, 0, "final", 0)
    sys.implementation = types.SimpleNamespace(
        name="cpython", version=sys.version_info, cache_tag=None)

if CASE in ("broken-sysconfig", "broken-importlib"):
    original_import = builtins.__import__
    def controlled_import(name, globals=None, locals=None, fromlist=(), level=0):
        if CASE == "broken-sysconfig" and name == "sysconfig":
            raise ImportError("fixture broken sysconfig dependency")
        if CASE == "broken-importlib" and name == "importlib.machinery":
            raise ImportError("fixture broken importlib dependency")
        return original_import(name, globals, locals, fromlist, level)
    builtins.__import__ = controlled_import
"#;

    #[tokio::test]
    #[ignore = "requires PAK_TEST_PYTHON pointing to an existing modern Python 3.3+ host"]
    async fn script_executes_legacy_and_pypy_branches() {
        let handle = real_handle();
        for case in ["old-cpython", "old-pypy", "modern-cpython"] {
            let script = format!("CASE = {case:?}\n{SCRIPT_FIXTURE}\n{SCRIPT}");
            let output = fixture_output(&handle, &script).await;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let descriptor = decode(&handle, &output.stdout).unwrap();
            match case {
                "old-cpython" => {
                    assert_eq!(descriptor.language_version.full_version(), "3.0.1");
                    assert_eq!(
                        descriptor.implementation_version_source,
                        ImplementationVersionSource::SysVersionInfo
                    );
                    assert_eq!(descriptor.sysconfig_paths["purelib"], "/fixture/distutils");
                    assert_eq!(descriptor.extension_suffixes, [".legacy-fixture.so"]);
                }
                "old-pypy" => {
                    assert_eq!(descriptor.language_version.full_version(), "3.7.10");
                    assert_eq!(descriptor.implementation_version.full_version(), "7.3.5");
                    assert_eq!(
                        descriptor.implementation_version_source,
                        ImplementationVersionSource::PypyVersionInfo
                    );
                    assert_eq!(descriptor.sysconfig_paths["purelib"], "/fixture/sysconfig");
                }
                _ => {
                    assert_eq!(descriptor.language_version.full_version(), "3.12.0");
                    assert_eq!(
                        descriptor.implementation_version_source,
                        ImplementationVersionSource::SysImplementationVersion
                    );
                    assert_eq!(descriptor.extension_suffixes, [".modern-fixture.so"]);
                }
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires PAK_TEST_PYTHON pointing to an existing modern Python 3.3+ host"]
    async fn damaged_modern_stdlib_does_not_fall_back() {
        let handle = real_handle();
        for case in ["broken-sysconfig", "broken-importlib"] {
            let script = format!("CASE = {case:?}\n{SCRIPT_FIXTURE}\n{SCRIPT}");
            let output = fixture_output(&handle, &script).await;
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("fixture broken"));
        }
    }

    #[tokio::test]
    #[ignore = "requires PAK_TEST_PYTHON pointing to an already existing absolute interpreter"]
    async fn cwd_and_pythonpath_cannot_supply_probe_modules() {
        let handle = real_handle();
        let poison = tempfile::tempdir().unwrap();
        for name in [
            "json.py",
            "platform.py",
            "sitecustomize.py",
            "usercustomize.py",
        ] {
            std::fs::write(
                poison.path().join(name),
                "raise RuntimeError('loaded project poison')\n",
            )
            .unwrap();
        }
        let mut command = probe_command(&handle, poison.path());
        command.env("PYTHONPATH", poison.path());
        let output = tokio::time::timeout(Duration::from_secs(10), command.output())
            .await
            .expect("pollution test timed out")
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            decode(&handle, &output.stdout)
                .unwrap()
                .language_version
                .major,
            3
        );
        assert!(poison.path().read_dir().unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("__pycache__")
        }));
    }

    #[tokio::test]
    #[ignore = "requires PAK_TEST_PYTHON pointing to an already existing absolute interpreter"]
    async fn explicitly_bound_real_interpreter_smoke() {
        let handle = real_handle();
        let descriptor = probe(&handle, Duration::from_secs(10)).await.unwrap();
        assert_eq!(descriptor.language_version.major, 3);
        println!(
            "implementation={:?} language={} implementation_version={} source={:?}",
            descriptor.implementation,
            descriptor.language_version.full_version(),
            descriptor.implementation_version.full_version(),
            descriptor.implementation_version_source
        );
    }
}
