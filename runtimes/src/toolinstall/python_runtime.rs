//! Observations from an explicitly bound, already installed Python runtime.
//!
//! The fixed standard-library script runs with site disabled and a cleared
//! environment. Its paths describe a base runtime, not an activated environment.
//! Runtime-provided standard libraries can consult platform helpers; this module
//! does not provide an operating-system process sandbox.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use eyre::{Context, ContextCompat, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::Instant;

use crate::toolset::ToolVersion;

pub(crate) const SCRIPT: &str = include_str!("python_runtime_probe.py");
pub(crate) const FLAGS: [&str; 5] = ["-E", "-s", "-S", "-B", "-c"];
const STDOUT_LIMIT: u64 = 1024 * 1024;
const STDERR_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Clone)]
/// An existing interpreter selected without PATH lookup or installation.
pub struct PythonRuntimeHandle {
    install_root: Option<PathBuf>,
    executable: PathBuf,
    protection: Option<Arc<dyn RuntimeProtection>>,
}

/// A live owner retains the actual stable lease and verifies its pinned tree.
/// Data-only descriptors cannot be promoted into this capability.
pub(crate) trait RuntimeProtection: std::fmt::Debug + Send + Sync {
    fn check_binding(&self) -> Result<()>;
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
            protection: None,
        })
    }

    pub(crate) fn from_protected_executable(
        path: &Path,
        protection: Arc<dyn RuntimeProtection>,
    ) -> Result<Self> {
        protection.check_binding()?;
        let mut handle = Self::from_existing_executable(path)?;
        protection.check_binding()?;
        handle.protection = Some(protection);
        Ok(handle)
    }

    fn check_protection(&self) -> Result<()> {
        if let Some(protection) = &self.protection {
            protection.check_binding()?;
        }
        Ok(())
    }

    fn check_protection_at_deadline(&self, deadline: Instant) -> Result<()> {
        ensure!(Instant::now() < deadline, "Python runtime probe timed out");
        self.check_protection()?;
        ensure!(Instant::now() < deadline, "Python runtime probe timed out");
        Ok(())
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

#[cfg(all(test, unix))]
#[path = "python_runtime_protection_tests.rs"]
mod protection_tests;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonOsVersion {
    pub major: u32,
    pub minor: u32,
    pub micro: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonLibcVersion {
    pub major: u32,
    pub minor: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonElfObservation {
    pub class: u8,
    pub little_endian: bool,
    pub machine: u16,
    pub flags: u32,
    pub interpreter: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case", deny_unknown_fields)]
pub enum PythonLibcObservation {
    Unknown,
    Glibc {
        major: u32,
        minor: u32,
    },
    Musl {
        version: Option<PythonLibcVersion>,
        loader: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonManylinuxOverride {
    pub arch: String,
    pub major: u32,
    pub minor: u32,
    pub compatible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PythonManylinuxObservation {
    Default,
    Unknown,
    Overrides {
        values: Vec<PythonManylinuxOverride>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "os", rename_all = "snake_case", deny_unknown_fields)]
pub enum PythonPlatformObservation {
    Macos {
        cpu_type: Option<i32>,
        product_version: Option<PythonOsVersion>,
    },
    Linux {
        elf: Option<PythonElfObservation>,
        libc: PythonLibcObservation,
        manylinux: PythonManylinuxObservation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonCompatibilityObservation {
    pub pointer_bits: u8,
    pub debug: Option<bool>,
    pub pymalloc: Option<bool>,
    pub unicode_size: Option<u8>,
    pub platform: PythonPlatformObservation,
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
    #[serde(default)]
    pub compatibility: Option<PythonCompatibilityObservation>,
    pub sysconfig_paths: BTreeMap<String, String>,
}

impl PythonRuntimeDescriptor {
    /// Validate compatibility observations before deriving any native target.
    /// Missing observations are retained as unknown capability, never defaults.
    pub fn validate_compatibility(&self) -> Result<()> {
        let language = &self.language_version;
        ensure!(self.schema == 1, "unsupported Python runtime probe schema");
        ensure!(
            self.observation_mode == "base_runtime_site_disabled"
                && self.probe_flags.iter().map(String::as_str).eq(FLAGS),
            "unexpected Python runtime probe observation mode"
        );
        ensure!(language.major == 3, "only Python 3.x is in scope");
        ensure!(
            language.release_level != ReleaseLevel::Final || language.serial == 0,
            "invalid final language version"
        );
        ensure!(
            self.implementation_version.release_level != ReleaseLevel::Final
                || self.implementation_version.serial == 0,
            "invalid final implementation version"
        );
        ensure!(
            self.markers.os_name == "posix" && !self.markers.platform_machine.is_empty(),
            "invalid POSIX runtime platform observation"
        );
        ensure!(
            (self.markers.sys_platform == "darwin" && self.markers.platform_system == "Darwin")
                || (self.markers.sys_platform.starts_with("linux")
                    && self.markers.platform_system == "Linux"),
            "only macOS/Linux runtime descriptors are in scope"
        );
        ensure!(
            self.markers.python_full_version == language.full_version()
                && self.markers.python_version == format!("{}.{}", language.major, language.minor),
            "runtime versions and marker values disagree"
        );
        match self.implementation {
            PythonImplementation::CPython => ensure!(
                self.implementation_version == *language
                    && self.markers.implementation_name == "cpython"
                    && self.markers.platform_python_implementation == "CPython"
                    && self.implementation_version_source
                        != ImplementationVersionSource::PypyVersionInfo,
                "invalid CPython compatibility identity"
            ),
            PythonImplementation::PyPy => ensure!(
                self.markers.implementation_name == "pypy"
                    && self.markers.platform_python_implementation == "PyPy"
                    && self.implementation_version_source
                        == ImplementationVersionSource::PypyVersionInfo,
                "invalid PyPy compatibility identity"
            ),
        }
        ensure!(
            self.markers.implementation_version == self.implementation_version.full_version(),
            "implementation version and marker disagree"
        );
        if self.implementation == PythonImplementation::CPython {
            ensure!(
                self.gil_disabled != Some(true) || language.minor >= 13,
                "free-threaded CPython requires Python 3.13 or later"
            );
            if let (Some(flags), Some(gil_disabled)) = (&self.abiflags, self.gil_disabled) {
                ensure!(
                    flags.contains('t') == gil_disabled,
                    "CPython free-threaded observations disagree"
                );
            }
        }
        let Some(observation) = &self.compatibility else {
            return Ok(());
        };
        ensure!(
            matches!(observation.pointer_bits, 32 | 64),
            "invalid Python pointer width"
        );
        ensure!(
            observation
                .unicode_size
                .is_none_or(|size| matches!(size, 2 | 4)),
            "invalid Python Unicode ABI width"
        );
        if self.implementation == PythonImplementation::CPython
            && let Some(flags) = &self.abiflags
        {
            if let Some(debug) = observation.debug {
                ensure!(
                    flags.contains('d') == debug,
                    "CPython debug observations disagree"
                );
            }
            if language.minor < 8
                && let Some(pymalloc) = observation.pymalloc
            {
                ensure!(
                    flags.contains('m') == pymalloc,
                    "CPython pymalloc observations disagree"
                );
            }
            if language.minor < 3
                && let Some(width) = observation.unicode_size
            {
                ensure!(
                    flags.contains('u') == (width == 4),
                    "CPython UCS observations disagree"
                );
            }
        }
        match &observation.platform {
            PythonPlatformObservation::Macos {
                cpu_type,
                product_version,
            } => {
                ensure!(
                    self.markers.os_name == "posix"
                        && self.markers.sys_platform == "darwin"
                        && self.markers.platform_system == "Darwin",
                    "macOS compatibility observation disagrees with markers"
                );
                if let Some(cpu) = cpu_type {
                    ensure!(*cpu > 0, "invalid Mach-O CPU type");
                    let bits = if cpu & 0x0100_0000 == 0 { 32 } else { 64 };
                    ensure!(
                        bits == observation.pointer_bits,
                        "Mach-O and Python pointer widths disagree"
                    );
                }
                if let Some(version) = product_version {
                    ensure!(version.major >= 10, "invalid macOS product version");
                    ensure!(
                        version.minor <= 1024 && version.micro <= 1024,
                        "macOS product version exceeds observation bounds"
                    );
                }
            }
            PythonPlatformObservation::Linux {
                elf,
                libc,
                manylinux,
            } => {
                ensure!(
                    self.markers.os_name == "posix"
                        && self.markers.sys_platform.starts_with("linux")
                        && self.markers.platform_system == "Linux",
                    "Linux compatibility observation disagrees with markers"
                );
                if let Some(elf) = elf {
                    let bits = match elf.class {
                        1 => 32,
                        2 => 64,
                        _ => bail!("invalid ELF class"),
                    };
                    ensure!(
                        bits == observation.pointer_bits,
                        "ELF and Python pointer widths disagree"
                    );
                    if let Some(interpreter) = &elf.interpreter {
                        ensure!(
                            interpreter.is_absolute(),
                            "ELF interpreter must be absolute"
                        );
                        ensure!(
                            interpreter.as_os_str().len() <= 4096,
                            "ELF interpreter path exceeds bounds"
                        );
                    }
                }
                match libc {
                    PythonLibcObservation::Unknown => {}
                    PythonLibcObservation::Glibc { major, minor } => {
                        ensure!(
                            *major > 0 && *major <= 1024 && *minor <= 1024,
                            "invalid glibc version observation"
                        );
                    }
                    PythonLibcObservation::Musl { version, loader } => {
                        ensure!(loader.is_absolute(), "musl loader must be absolute");
                        ensure!(
                            elf.as_ref().and_then(|value| value.interpreter.as_ref())
                                == Some(loader),
                            "musl loader differs from observed ELF interpreter"
                        );
                        if let Some(version) = version {
                            ensure!(
                                version.major > 0 && version.major <= 1024 && version.minor <= 1024,
                                "invalid musl version observation"
                            );
                        }
                    }
                }
                if let PythonManylinuxObservation::Overrides { values } = manylinux {
                    let PythonLibcObservation::Glibc { major, minor } = libc else {
                        bail!("manylinux overrides require observed glibc");
                    };
                    ensure!(values.len() <= 2048, "too many manylinux overrides");
                    let mut identities = BTreeSet::new();
                    for value in values {
                        ensure!(
                            matches!(
                                value.arch.as_str(),
                                "x86_64"
                                    | "i686"
                                    | "aarch64"
                                    | "armv8l"
                                    | "armv7l"
                                    | "ppc64"
                                    | "ppc64le"
                                    | "s390x"
                                    | "loongarch64"
                                    | "riscv64"
                            ),
                            "invalid manylinux override architecture"
                        );
                        ensure!(
                            value.major == *major && value.minor <= *minor,
                            "manylinux override exceeds observed libc"
                        );
                        ensure!(
                            identities.insert((value.arch.as_str(), value.major, value.minor)),
                            "duplicate manylinux override identity"
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn decode(handle: &PythonRuntimeHandle, bytes: &[u8]) -> Result<PythonRuntimeDescriptor> {
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
    descriptor.validate_compatibility()?;
    Ok(descriptor)
}

const MAX_HEADER_BYTES: u64 = 1024 * 1024;
const MAX_INTERPRETER_BYTES: usize = 4096;

fn elf_integer(bytes: &[u8], offset: usize, width: usize, little: bool) -> Result<u64> {
    let end = offset.checked_add(width).context("ELF offset overflow")?;
    let field = bytes.get(offset..end).context("truncated ELF field")?;
    let mut value = 0_u64;
    if little {
        for (index, byte) in field.iter().enumerate() {
            value |= u64::from(*byte) << (index * 8);
        }
    } else {
        for byte in field {
            value = (value << 8) | u64::from(*byte);
        }
    }
    Ok(value)
}

fn parse_elf_observation(bytes: &[u8]) -> Result<PythonElfObservation> {
    ensure!(
        bytes.len() <= MAX_HEADER_BYTES as usize,
        "ELF header region exceeds bounds"
    );
    ensure!(
        bytes.len() >= 52 && bytes.starts_with(b"\x7fELF"),
        "invalid ELF header"
    );
    ensure!(bytes[6] == 1, "unsupported ELF version");
    let class = bytes[4];
    ensure!(matches!(class, 1 | 2), "unsupported ELF class");
    ensure!(matches!(bytes[5], 1 | 2), "unsupported ELF endianness");
    let little = bytes[5] == 1;
    let (offset, size, count, flags, expected_size) = if class == 1 {
        (
            elf_integer(bytes, 28, 4, little)?,
            elf_integer(bytes, 42, 2, little)?,
            elf_integer(bytes, 44, 2, little)?,
            elf_integer(bytes, 36, 4, little)?,
            32,
        )
    } else {
        ensure!(bytes.len() >= 64, "truncated ELF64 header");
        (
            elf_integer(bytes, 32, 8, little)?,
            elf_integer(bytes, 54, 2, little)?,
            elf_integer(bytes, 56, 2, little)?,
            elf_integer(bytes, 48, 4, little)?,
            56,
        )
    };
    ensure!(count <= 256, "too many ELF program headers");
    ensure!(
        count == 0 || size == expected_size,
        "invalid ELF program header size"
    );
    let end = offset
        .checked_add(size.checked_mul(count).context("ELF table overflow")?)
        .context("ELF table offset overflow")?;
    ensure!(
        end <= bytes.len() as u64,
        "ELF program headers outside bounded region"
    );
    let mut interpreter = None;
    for index in 0..count {
        let entry = usize::try_from(offset + index * size).context("ELF entry offset")?;
        if elf_integer(bytes, entry, 4, little)? != 3 {
            continue;
        }
        ensure!(interpreter.is_none(), "duplicate ELF interpreter");
        let (location, length) = if class == 1 {
            (
                elf_integer(bytes, entry + 4, 4, little)?,
                elf_integer(bytes, entry + 16, 4, little)?,
            )
        } else {
            (
                elf_integer(bytes, entry + 8, 8, little)?,
                elf_integer(bytes, entry + 32, 8, little)?,
            )
        };
        ensure!(
            (2..=MAX_INTERPRETER_BYTES as u64).contains(&length),
            "invalid ELF interpreter length"
        );
        let end = location
            .checked_add(length)
            .context("ELF interpreter offset overflow")?;
        ensure!(
            end <= bytes.len() as u64,
            "ELF interpreter outside bounded region"
        );
        let raw = &bytes[usize::try_from(location)?..usize::try_from(end)?];
        ensure!(
            raw.last() == Some(&0) && !raw[..raw.len() - 1].contains(&0),
            "invalid ELF interpreter terminator"
        );
        let path = PathBuf::from(
            std::str::from_utf8(&raw[..raw.len() - 1]).context("ELF interpreter UTF-8")?,
        );
        ensure!(path.is_absolute(), "ELF interpreter must be absolute");
        interpreter = Some(path);
    }
    Ok(PythonElfObservation {
        class,
        little_endian: little,
        machine: u16::try_from(elf_integer(bytes, 18, 2, little)?)?,
        flags: u32::try_from(flags)?,
        interpreter,
    })
}

pub(crate) fn read_bound_elf(handle: &PythonRuntimeHandle) -> Result<PythonElfObservation> {
    let mut bytes = Vec::new();
    std::fs::File::open(handle.executable())?
        .take(MAX_HEADER_BYTES)
        .read_to_end(&mut bytes)?;
    parse_elf_observation(&bytes)
}

pub(crate) fn musl_version(stderr: &[u8]) -> Option<PythonLibcVersion> {
    let text = std::str::from_utf8(stderr).ok()?;
    let mut lines = text.lines();
    if !lines.next()?.trim().starts_with("musl libc (") {
        return None;
    }
    let text = lines.next()?.trim().strip_prefix("Version ")?;
    let parts: Vec<_> = text.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let major: u32 = parts[0].parse().ok()?;
    let minor: u32 = parts[1].parse().ok()?;
    let patch: u32 = parts[2].parse().ok()?;
    if major == 0 || major > 1024 || minor > 1024 || patch > 1024 {
        return None;
    }
    Some(PythonLibcVersion { major, minor })
}

fn musl_loader_command(loader: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(loader);
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

async fn observe_musl_loader(
    loader: &Path,
    cwd: &Path,
    deadline: Instant,
    stdout_limit: u64,
    stderr_limit: u64,
) -> Result<Option<PythonLibcVersion>> {
    ensure!(loader.is_absolute(), "musl loader must be absolute");
    ensure!(
        stdout_limit != 0 && stderr_limit != 0,
        "musl loader output limits must be positive"
    );
    ensure!(
        Instant::now() < deadline,
        "Python runtime probe timed out before musl observation"
    );
    let mut child = musl_loader_command(loader, cwd)
        .spawn()
        .context("launching observed musl loader")?;
    let stdout = child.stdout.take().context("musl loader stdout missing")?;
    let stderr = child.stderr.take().context("musl loader stderr missing")?;
    let result = tokio::time::timeout_at(deadline, async {
        tokio::try_join!(
            child.wait(),
            read_bounded(stdout, stdout_limit.min(STDOUT_LIMIT)),
            read_bounded(stderr, stderr_limit.min(STDERR_LIMIT))
        )
    })
    .await;
    let (_, _, stderr) = match result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            kill_and_reap(&mut child).await?;
            return Err(error).context("reading musl loader output");
        }
        Err(_) => {
            kill_and_reap(&mut child).await?;
            bail!("Python runtime probe timed out during musl observation");
        }
    };
    // A musl loader without a program normally exits nonzero after identifying
    // itself. Status is not used as the version parser; its stderr must match.
    Ok(musl_version(&stderr))
}

async fn complete_musl_observation(
    handle: &PythonRuntimeHandle,
    descriptor: &mut PythonRuntimeDescriptor,
    cwd: &Path,
    deadline: Instant,
    stdout_limit: u64,
    stderr_limit: u64,
) -> Result<()> {
    let Some(observation) = &mut descriptor.compatibility else {
        return Ok(());
    };
    let PythonPlatformObservation::Linux { elf, libc, .. } = &mut observation.platform else {
        return Ok(());
    };
    let PythonLibcObservation::Musl { version, loader } = libc else {
        return Ok(());
    };
    let bound =
        read_bound_elf(handle).context("verifying selected Python ELF before loader execution")?;
    ensure!(
        elf.as_ref() == Some(&bound),
        "bound Python ELF differs from process observation"
    );
    ensure!(
        bound.interpreter.as_ref() == Some(&*loader),
        "musl loader differs from bound ELF interpreter"
    );
    let loader = loader.canonicalize().context("observed musl loader")?;
    ensure!(loader.is_file(), "observed musl loader is not a file");
    *version = observe_musl_loader(&loader, cwd, deadline, stdout_limit, stderr_limit).await?;
    Ok(())
}

fn probe_command(handle: &PythonRuntimeHandle, cwd: &Path) -> Command {
    fixed_probe_command(handle, cwd, SCRIPT)
}

fn fixed_probe_command(handle: &PythonRuntimeHandle, cwd: &Path, script: &str) -> Command {
    let mut command = Command::new(handle.executable());
    command.args(FLAGS).arg(script);
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

async fn run_fixed_probe(
    handle: &PythonRuntimeHandle,
    mut command: Command,
    deadline: Instant,
    stdout_limit: u64,
    stderr_limit: u64,
) -> Result<Vec<u8>> {
    ensure!(Instant::now() < deadline, "Python runtime probe timed out");
    handle.check_protection_at_deadline(deadline)?;
    let mut child = command
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
    let result = tokio::time::timeout_at(deadline, async {
        tokio::try_join!(
            child.wait(),
            read_bounded(stdout, stdout_limit),
            read_bounded(stderr, stderr_limit)
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
    handle.check_protection_at_deadline(deadline)?;
    if !status.success() {
        let stderr: String = String::from_utf8_lossy(&stderr)
            .chars()
            .take(1024)
            .collect();
        bail!("Python runtime probe failed with {}: {}", status, stderr);
    }
    Ok(stdout)
}

/// Execute the one fixed, site-disabled layout observer under the caller's
/// existing deadline. No arbitrary script or environment is accepted.
pub(crate) async fn probe_layout(
    handle: &PythonRuntimeHandle,
    final_prefix: &Path,
    stdout_limit: u64,
    stderr_limit: u64,
    deadline: Instant,
) -> Result<Vec<u8>> {
    ensure!(
        cfg!(any(target_os = "macos", target_os = "linux")),
        "Python runtime layout probe currently supports macOS/Linux"
    );
    ensure!(
        stdout_limit != 0 && stderr_limit != 0,
        "Python layout output limits must be nonzero"
    );
    let cwd = tempfile::tempdir().context("creating empty Python layout probe cwd")?;
    let mut command = fixed_probe_command(
        handle,
        cwd.path(),
        include_str!("python_runtime_layout_probe.py"),
    );
    command.arg(final_prefix);
    run_fixed_probe(
        handle,
        command,
        deadline,
        stdout_limit.min(STDOUT_LIMIT),
        stderr_limit.min(STDERR_LIMIT),
    )
    .await
}

/// Execute only the fixed pre-site code/cfg observer with bounded owned input.
/// This wrong baseline awaits independent supervised-stdin behavior RED.
pub(crate) async fn probe_startup_intercept(
    handle: &PythonRuntimeHandle,
    request: Vec<u8>,
    stdout_limit: u64,
    stderr_limit: u64,
    deadline: Instant,
) -> Result<Vec<u8>> {
    run_startup_probe(handle, request, stdout_limit, stderr_limit, deadline, false).await
}

/// A separate fixed normal-empty observer; caller admission remains in D3.
pub(crate) async fn probe_empty_environment(
    handle: &PythonRuntimeHandle,
    request: Vec<u8>,
    stdout_limit: u64,
    stderr_limit: u64,
    deadline: Instant,
) -> Result<Vec<u8>> {
    run_startup_probe(handle, request, stdout_limit, stderr_limit, deadline, true).await
}

async fn run_startup_probe(
    handle: &PythonRuntimeHandle,
    request: Vec<u8>,
    stdout_limit: u64,
    stderr_limit: u64,
    deadline: Instant,
    normal: bool,
) -> Result<Vec<u8>> {
    ensure!(
        request.len() <= 32 * 1024 * 1024,
        "startup input limit exceeded"
    );
    ensure!(
        stdout_limit != 0 && stderr_limit != 0,
        "startup output limit must be nonzero"
    );
    ensure!(
        Instant::now() < deadline,
        "Python runtime startup probe timed out"
    );
    let cwd = tempfile::tempdir().context("creating empty Python startup probe cwd")?;
    let marker = if normal {
        "# PAKRO_RUNTIME_EMPTY_V1\n"
    } else {
        "# PAKRO_RUNTIME_STARTUP_V1\n"
    };
    let script = format!(
        "{marker}{}\n{}",
        include_str!("python_site_adapter.py"),
        include_str!("python_runtime_startup_probe.py")
    );
    let mut command = Command::new(handle.executable());
    if normal {
        command.args(["-E", "-s", "-B", "-c"]);
    } else {
        command.args(FLAGS);
    }
    command
        .arg(script)
        .env_clear()
        .env("LC_ALL", "C")
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    handle.check_protection_at_deadline(deadline)?;
    let mut child = command
        .spawn()
        .context("launching selected Python startup runtime")?;
    let mut stdin = child.stdin.take().context("Python startup stdin missing")?;
    let stdout = child
        .stdout
        .take()
        .context("Python startup stdout missing")?;
    let stderr = child
        .stderr
        .take()
        .context("Python startup stderr missing")?;
    let result = tokio::time::timeout_at(deadline, async {
        tokio::try_join!(
            child.wait(),
            read_bounded(stdout, stdout_limit.min(STDOUT_LIMIT)),
            read_bounded(stderr, stderr_limit.min(STDERR_LIMIT)),
            async move {
                stdin.write_all(&request).await?;
                stdin.shutdown().await
            }
        )
    })
    .await;
    let (status, stdout, stderr, ()) = match result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            kill_and_reap(&mut child).await?;
            return Err(error).context("reading output or writing Python startup input");
        }
        Err(_) => {
            kill_and_reap(&mut child).await?;
            bail!("Python runtime startup probe timed out");
        }
    };
    handle.check_protection_at_deadline(deadline)?;
    if !status.success() {
        let stderr: String = String::from_utf8_lossy(&stderr)
            .chars()
            .take(1024)
            .collect();
        bail!("Python runtime startup probe failed with {status}: {stderr}");
    }
    Ok(stdout)
}

#[cfg(all(test, unix))]
#[path = "python_runtime_startup_tests.rs"]
mod startup_tests;

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
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("Python runtime probe timeout overflow")?;
    probe_at_deadline(handle, deadline, STDOUT_LIMIT, STDERR_LIMIT).await
}

pub(crate) async fn probe_at_deadline(
    handle: &PythonRuntimeHandle,
    deadline: Instant,
    stdout_limit: u64,
    stderr_limit: u64,
) -> Result<PythonRuntimeDescriptor> {
    ensure!(
        cfg!(any(target_os = "macos", target_os = "linux")),
        "Python runtime probe currently supports macOS/Linux"
    );
    ensure!(
        stdout_limit != 0 && stderr_limit != 0 && Instant::now() < deadline,
        "Python runtime probe limits or deadline are invalid"
    );
    let cwd = tempfile::tempdir().context("creating empty Python probe cwd")?;
    let stdout = run_fixed_probe(
        handle,
        probe_command(handle, cwd.path()),
        deadline,
        stdout_limit.min(STDOUT_LIMIT),
        stderr_limit.min(STDERR_LIMIT),
    )
    .await?;
    let mut descriptor = decode(handle, &stdout)?;
    handle.check_protection_at_deadline(deadline)?;
    complete_musl_observation(
        handle,
        &mut descriptor,
        cwd.path(),
        deadline,
        stdout_limit,
        stderr_limit,
    )
    .await?;
    descriptor.validate_compatibility()?;
    handle.check_protection_at_deadline(deadline)?;
    Ok(descriptor)
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
        let result = probe(&handle, Duration::from_secs(2)).await;
        assert!(result.is_ok(), "HOME isolation probe failed: {result:?}");
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

    fn compatibility_fixture() -> Value {
        json!({
            "pointer_bits":64,"debug":false,"pymalloc":true,"unicode_size":null,
            "platform":{
                "os":"linux",
                "elf":{"class":2,"little_endian":true,"machine":183,"flags":0,
                       "interpreter":"/lib/ld-linux-aarch64.so.1"},
                "libc":{"family":"glibc","major":2,"minor":31},
                "manylinux":{"state":"default"}
            }
        })
    }

    #[test]
    fn compatibility_legacy_fixture_stays_unknown() {
        let (_root, handle) = selected();
        let descriptor = parsed(&handle, &fixture(&handle, false)).unwrap();
        assert_eq!(descriptor.compatibility, None);
        descriptor.validate_compatibility().unwrap();
    }

    #[test]
    fn compatibility_legacy_free_threaded_identity_is_rejected() {
        let (_root, handle) = selected();
        let mut value = fixture(&handle, false);
        value["gil_disabled"] = json!(true);
        assert!(parsed(&handle, &value).is_err());
        let mut descriptor = parsed(&handle, &fixture(&handle, false)).unwrap();
        descriptor.gil_disabled = Some(true);
        assert!(descriptor.validate_compatibility().is_err());
    }

    #[test]
    fn compatibility_legacy_free_threaded_flag_disagreement_is_rejected() {
        let (_root, handle) = selected();
        let mut value = fixture(&handle, false);
        value["abiflags"] = json!("t");
        value["gil_disabled"] = json!(false);
        assert!(parsed(&handle, &value).is_err());
    }

    #[test]
    fn compatibility_preserves_real_bits_and_elf() {
        let (_root, handle) = selected();
        let mut value = fixture(&handle, false);
        value["compatibility"] = compatibility_fixture();
        value["compatibility"]["pointer_bits"] = json!(32);
        value["compatibility"]["platform"]["elf"]["class"] = json!(1);
        value["compatibility"]["platform"]["elf"]["machine"] = json!(3);
        value["markers"]["platform_machine"] = json!("x86_64");
        let descriptor = parsed(&handle, &value).unwrap();
        assert_eq!(descriptor.compatibility.unwrap().pointer_bits, 32);
        // Host uname is not rewritten to the inferred process architecture.
        assert_eq!(descriptor.markers.platform_machine, "x86_64");
    }

    #[test]
    fn compatibility_rejects_bad_bits_and_elf_disagreement() {
        let (_root, handle) = selected();
        for (bits, class) in [(16, 2), (32, 2), (64, 1), (64, 0)] {
            let mut value = fixture(&handle, false);
            value["compatibility"] = compatibility_fixture();
            value["compatibility"]["pointer_bits"] = json!(bits);
            value["compatibility"]["platform"]["elf"]["class"] = json!(class);
            assert!(
                parsed(&handle, &value).is_err(),
                "bits={bits} class={class}"
            );
        }
    }

    #[test]
    fn compatibility_rejects_os_or_macho_width_conflict() {
        let (_root, handle) = selected();
        let mut value = fixture(&handle, false);
        value["compatibility"] = compatibility_fixture();
        value["compatibility"]["platform"] = json!({
            "os":"macos","cpu_type":16777228,
            "product_version":{"major":14,"minor":5,"micro":0}
        });
        assert!(parsed(&handle, &value).is_err());
        value["markers"]["platform_system"] = json!("Darwin");
        value["markers"]["sys_platform"] = json!("darwin");
        assert!(parsed(&handle, &value).is_ok());
        value["compatibility"]["pointer_bits"] = json!(32);
        assert!(parsed(&handle, &value).is_err());
    }

    #[test]
    fn compatibility_rejects_invalid_unicode_and_abi_flags() {
        let (_root, handle) = selected();
        let mut value = fixture(&handle, false);
        value["compatibility"] = compatibility_fixture();
        value["compatibility"]["unicode_size"] = json!(3);
        assert!(parsed(&handle, &value).is_err());
        value["compatibility"]["unicode_size"] = Value::Null;
        value["abiflags"] = json!("d");
        assert!(parsed(&handle, &value).is_err());
        value["compatibility"]["debug"] = json!(true);
        assert!(parsed(&handle, &value).is_ok());
        value["abiflags"] = json!("t");
        value["compatibility"]["debug"] = json!(false);
        value["gil_disabled"] = json!(false);
        assert!(parsed(&handle, &value).is_err());
    }

    #[test]
    fn compatibility_musl_loader_must_match_absolute_elf_interp() {
        let (_root, handle) = selected();
        for loader in ["ld-musl-aarch64.so.1", "/other/ld-musl-aarch64.so.1"] {
            let mut value = fixture(&handle, false);
            value["compatibility"] = compatibility_fixture();
            value["compatibility"]["platform"]["libc"] = json!({
                "family":"musl","version":{"major":1,"minor":2},"loader":loader
            });
            value["compatibility"]["platform"]["elf"]["interpreter"] =
                json!("/lib/ld-musl-aarch64.so.1");
            assert!(parsed(&handle, &value).is_err());
        }
    }

    #[test]
    fn compatibility_policy_is_bounded_unique_and_glibc_specific() {
        let (_root, handle) = selected();
        let row = json!({"arch":"aarch64","major":2,"minor":17,"compatible":false});
        let mut value = fixture(&handle, false);
        value["compatibility"] = compatibility_fixture();
        value["compatibility"]["platform"]["manylinux"] =
            json!({"state":"overrides","values":[row.clone()]});
        assert!(parsed(&handle, &value).is_ok());
        value["compatibility"]["platform"]["manylinux"]["values"] =
            json!([row.clone(), row.clone()]);
        assert!(parsed(&handle, &value).is_err());
        value["compatibility"]["platform"]["manylinux"]["values"] = json!(vec![row.clone(); 2049]);
        assert!(parsed(&handle, &value).is_err());
        value["compatibility"]["platform"]["manylinux"]["values"] = json!([row]);
        value["compatibility"]["platform"]["libc"] = json!({"family":"unknown"});
        assert!(parsed(&handle, &value).is_err());
    }

    #[test]
    fn compatibility_unknown_is_not_fabricated() {
        let (_root, handle) = selected();
        let mut value = fixture(&handle, false);
        value["compatibility"] = compatibility_fixture();
        value["compatibility"]["debug"] = Value::Null;
        value["compatibility"]["pymalloc"] = Value::Null;
        value["compatibility"]["platform"]["elf"] = Value::Null;
        value["compatibility"]["platform"]["libc"] = json!({"family":"unknown"});
        value["compatibility"]["platform"]["manylinux"] = json!({"state":"unknown"});
        let descriptor = parsed(&handle, &value).unwrap();
        let observation = descriptor.compatibility.unwrap();
        assert_eq!(observation.debug, None);
        assert_eq!(observation.pymalloc, None);
        assert!(matches!(
            observation.platform,
            PythonPlatformObservation::Linux {
                elf: None,
                libc: PythonLibcObservation::Unknown,
                manylinux: PythonManylinuxObservation::Unknown
            }
        ));
    }

    #[test]
    fn compatibility_public_mutation_is_revalidated() {
        let (_root, handle) = selected();
        let mut value = fixture(&handle, false);
        value["compatibility"] = compatibility_fixture();
        let mut descriptor = parsed(&handle, &value).unwrap();
        descriptor.compatibility.as_mut().unwrap().pointer_bits = 32;
        assert!(descriptor.validate_compatibility().is_err());
        descriptor.compatibility.as_mut().unwrap().pointer_bits = 64;
        descriptor.markers.platform_system = "Darwin".into();
        assert!(descriptor.validate_compatibility().is_err());
    }

    fn elf_fixture(class: u8, little: bool, machine: u16, loader: &str) -> Vec<u8> {
        fn field(bytes: &mut [u8], offset: usize, width: usize, value: u64, little: bool) {
            for index in 0..width {
                let shift = (if little { index } else { width - index - 1 }) * 8;
                bytes[offset + index] = ((value >> shift) & 255) as u8;
            }
        }
        let mut bytes = vec![0_u8; 512];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = class;
        bytes[5] = if little { 1 } else { 2 };
        bytes[6] = 1;
        field(&mut bytes, 18, 2, u64::from(machine), little);
        let program = 64;
        let (
            offset_field,
            offset_width,
            size_field,
            count_field,
            size,
            interpreter_offset_field,
            interpreter_length_field,
        ) = if class == 1 {
            (28, 4, 42, 44, 32, 4, 16)
        } else {
            (32, 8, 54, 56, 56, 8, 32)
        };
        field(
            &mut bytes,
            offset_field,
            offset_width,
            program as u64,
            little,
        );
        field(&mut bytes, size_field, 2, size, little);
        field(&mut bytes, count_field, 2, 1, little);
        field(&mut bytes, program, 4, 3, little);
        field(
            &mut bytes,
            program + interpreter_offset_field,
            offset_width,
            256,
            little,
        );
        field(
            &mut bytes,
            program + interpreter_length_field,
            offset_width,
            (loader.len() + 1) as u64,
            little,
        );
        bytes[256..256 + loader.len()].copy_from_slice(loader.as_bytes());
        bytes
    }

    #[test]
    fn target_elf_reads_actual_width_endianness_and_interp() {
        for (class, little, machine) in [(1, true, 3), (2, true, 183), (2, false, 21)] {
            let bytes = elf_fixture(class, little, machine, "/lib/ld-musl-fixture.so.1");
            let observed = parse_elf_observation(&bytes).unwrap();
            assert_eq!(observed.class, class);
            assert_eq!(observed.little_endian, little);
            assert_eq!(observed.machine, machine);
            assert_eq!(
                observed.interpreter,
                Some(PathBuf::from("/lib/ld-musl-fixture.so.1"))
            );
        }
    }

    #[test]
    fn target_elf_rejects_unbounded_or_malformed_headers() {
        let base = elf_fixture(2, true, 183, "/lib/ld-musl-fixture.so.1");
        assert!(parse_elf_observation(&base[..51]).is_err());
        let mut bytes = base.clone();
        bytes[5] = 0;
        assert!(parse_elf_observation(&bytes).is_err());
        let mut bytes = base.clone();
        bytes[56..58].copy_from_slice(&257_u16.to_le_bytes());
        assert!(parse_elf_observation(&bytes).is_err());
        let mut bytes = base.clone();
        bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse_elf_observation(&bytes).is_err());
        let mut bytes = base.clone();
        bytes[96..104].copy_from_slice(&4097_u64.to_le_bytes());
        assert!(parse_elf_observation(&bytes).is_err());
        let mut bytes = base;
        bytes[256] = b'x';
        assert!(parse_elf_observation(&bytes).is_err());
    }

    #[test]
    fn target_musl_version_needs_real_header_and_strict_numbers() {
        assert_eq!(
            musl_version(b"musl libc (aarch64)\nVersion 1.2.5\nDynamic Program Loader\n"),
            Some(PythonLibcVersion { major: 1, minor: 2 })
        );
        for text in [
            b"glibc\nVersion 1.2.5\n".as_slice(),
            b"musl libc (x86_64)\nVersion 1.2\n",
            b"musl libc (x86_64)\nVersion 1.-2.5\n",
            b"musl libc (x86_64)\nVersion 999999999999999.2.5\n",
        ] {
            assert_eq!(musl_version(text), None);
        }
    }

    #[test]
    fn target_loader_has_absolute_program_no_args_and_cleared_env() {
        let (_root, handle) = selected();
        let cwd = tempfile::tempdir().unwrap();
        let command = musl_loader_command(handle.executable(), cwd.path());
        let command = command.as_std();
        assert_eq!(command.get_program(), handle.executable().as_os_str());
        assert_eq!(command.get_args().count(), 0);
        assert_eq!(command.get_current_dir(), Some(cwd.path()));
        let env: Vec<_> = command.get_envs().collect();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "LC_ALL");
        assert_eq!(env[0].1.unwrap(), "C");
    }

    #[tokio::test]
    async fn target_loader_accepts_musl_nonzero_banner_without_manager() {
        let (_root, handle) = selected();
        let cwd = tempfile::tempdir().unwrap();
        rewrite(
            &handle,
            "printf 'musl libc (x86_64)\\nVersion 1.2.5\\nDynamic Program Loader\\n' >&2; exit 1",
        );
        assert_eq!(
            observe_musl_loader(
                handle.executable(),
                cwd.path(),
                Instant::now() + Duration::from_secs(2),
                STDOUT_LIMIT,
                STDERR_LIMIT,
            )
            .await
            .unwrap(),
            Some(PythonLibcVersion { major: 1, minor: 2 })
        );
        rewrite(&handle, "printf 'unrecognized libc' >&2; exit 1");
        assert_eq!(
            observe_musl_loader(
                handle.executable(),
                cwd.path(),
                Instant::now() + Duration::from_secs(2),
                STDOUT_LIMIT,
                STDERR_LIMIT,
            )
            .await
            .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn target_loader_obeys_shared_deadline_and_reaps_direct_child() {
        let (root, handle) = selected();
        let cwd = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("loader.pid");
        let quoted = pid_file.to_str().unwrap().replace('\'', "'\"'\"'");
        rewrite(
            &handle,
            &format!("printf '%s\\n' \"$$\" > '{quoted}'\nexec /bin/sleep 10"),
        );
        // A newly written executable can take over 100 ms to start on macOS.
        // Prime this fixture so its PID handshake is ready
        // within the short deadline whose termination behavior is under test.
        let mut warmup = musl_loader_command(handle.executable(), cwd.path())
            .spawn()
            .unwrap();
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            while !pid_file.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        kill_and_reap(&mut warmup).await.unwrap();
        ready.expect("loader fixture did not finish its cold launch");
        std::fs::remove_file(&pid_file).unwrap();
        let error = observe_musl_loader(
            handle.executable(),
            cwd.path(),
            Instant::now() + Duration::from_millis(100),
            STDOUT_LIMIT,
            STDERR_LIMIT,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH)
        );
        assert!(
            observe_musl_loader(
                handle.executable(),
                cwd.path(),
                Instant::now() - Duration::from_millis(1),
                STDOUT_LIMIT,
                STDERR_LIMIT,
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn target_loader_output_is_bounded_and_relative_program_rejected() {
        let (_root, handle) = selected();
        let cwd = tempfile::tempdir().unwrap();
        rewrite(&handle, "exec /usr/bin/head -c 65537 /dev/zero >&2");
        let error = observe_musl_loader(
            handle.executable(),
            cwd.path(),
            Instant::now() + Duration::from_secs(2),
            STDOUT_LIMIT,
            STDERR_LIMIT,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("exceeds 65536 bytes"));
        assert!(
            observe_musl_loader(
                Path::new("ld-musl.so"),
                cwd.path(),
                Instant::now() + Duration::from_secs(2),
                STDOUT_LIMIT,
                STDERR_LIMIT,
            )
            .await
            .is_err()
        );
    }

    // Paste inside python_runtime::tests. These exercise the public deadline helper
    // plus its real nested process on either host without a production test hook.
    // The selected temporary shell deliberately replaces only its own temporary
    // pathname with existing synthetic ELF bytes after launch. This is a protocol
    // budget fixture, not evidence that this host executed the supplied ELF image.

    async fn assert_nested_musl_caller_cap(stdout_case: bool) {
        fn quote(path: &Path) -> String {
            format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"))
        }
        let (root, handle) = selected();
        let loader = root.path().join("ld-musl-fixture");
        let sequence = root.path().join("sequence");
        let data = root.path().join("valid-frame.json");
        let image = root.path().join("bound-elf.bin");
        let mut frame = fixture(&handle, false);
        frame["compatibility"] = compatibility_fixture();
        frame["compatibility"]["platform"]["elf"]["interpreter"] = json!(loader);
        frame["compatibility"]["platform"]["libc"] =
            json!({"family":"musl","version":null,"loader":loader});
        frame["compatibility"]["platform"]["manylinux"] = json!({"state":"unknown"});
        let encoded = serde_json::to_vec(&frame).unwrap();
        std::fs::write(&data, &encoded).unwrap();
        std::fs::write(&image, elf_fixture(2, true, 183, loader.to_str().unwrap())).unwrap();
        // The stdout case leaves enough space for the complete first descriptor,
        // but deliberately overflows that same caller cap in the nested loader.
        let stdout_limit = if stdout_case {
            encoded.len() as u64
        } else {
            STDOUT_LIMIT
        };
        let stderr_limit = if stdout_case { STDERR_LIMIT } else { 1 };
        assert!(encoded.len() as u64 <= stdout_limit);
        let overflow_stdout = if stdout_case {
            format!("/usr/bin/head -c {} /dev/zero\n", stdout_limit + 1)
        } else {
            String::new()
        };
        std::fs::write(&loader, format!(
            "#!/bin/sh\nprintf 'loader\\n' >> {}\n{}printf 'musl libc (aarch64)\\nVersion 1.2.5\\nDynamic Program Loader\\n' >&2\nexit 1\n",
            quote(&sequence), overflow_stdout,
        )).unwrap();
        std::fs::set_permissions(&loader, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cwd = tempfile::tempdir().unwrap();
        // Existing default-contract helper must accept the exact same loader.
        assert_eq!(
            observe_musl_loader(
                &loader,
                cwd.path(),
                Instant::now() + Duration::from_secs(5),
                STDOUT_LIMIT,
                STDERR_LIMIT
            )
            .await
            .unwrap(),
            Some(PythonLibcVersion { major: 1, minor: 2 })
        );
        assert_eq!(
            std::fs::read_to_string(&sequence)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec!["loader"]
        );
        std::fs::remove_file(&sequence).unwrap();
        rewrite(
            &handle,
            &format!(
                // Parse the entire compound statement before replacing the selected
                // script's path; no assumption about shell input buffer size is needed.
                "{{ printf 'probe\\n' >> {}; /bin/cp {} \"$0\"; exec /bin/cat {}; }}",
                quote(&sequence),
                quote(&image),
                quote(&data),
            ),
        );
        let result = probe_at_deadline(
            &handle,
            Instant::now() + Duration::from_secs(5),
            stdout_limit,
            stderr_limit,
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(&sequence)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec!["probe", "loader"],
            "budget fixture failed before the real nested loader process"
        );
        let error = result.unwrap_err();
        let limit = if stdout_case {
            stdout_limit
        } else {
            stderr_limit
        };
        assert!(
            format!("{error:#}").contains(&format!("exceeds {limit} bytes")),
            "unexpected bounded-loader diagnostic: {error:#}"
        );
    }

    #[tokio::test]
    async fn caller_stderr_cap_also_bounds_the_nested_musl_loader() {
        assert_nested_musl_caller_cap(false).await;
    }

    #[tokio::test]
    async fn caller_stdout_cap_also_bounds_the_nested_musl_loader() {
        assert_nested_musl_caller_cap(true).await;
    }

    #[tokio::test]
    async fn target_musl_enrichment_rejects_script_executable_before_loader() {
        let (_root, handle) = selected();
        let cwd = tempfile::tempdir().unwrap();
        let mut value = fixture(&handle, false);
        value["compatibility"] = compatibility_fixture();
        value["compatibility"]["platform"]["elf"]["interpreter"] =
            json!("/lib/ld-musl-aarch64.so.1");
        value["compatibility"]["platform"]["libc"] =
            json!({"family":"musl","version":null,"loader":"/lib/ld-musl-aarch64.so.1"});
        value["compatibility"]["platform"]["manylinux"] = json!({"state":"unknown"});
        let mut descriptor = parsed(&handle, &value).unwrap();
        let error = complete_musl_observation(
            &handle,
            &mut descriptor,
            cwd.path(),
            Instant::now() + Duration::from_secs(2),
            STDOUT_LIMIT,
            STDERR_LIMIT,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("invalid ELF header"));
    }

    #[tokio::test]
    #[ignore = "requires PAKRO_TEST_PYTHON pointing to an existing modern Python"]
    async fn target_script_executes_observation_fixtures() {
        let handle = real_handle();
        let (helpers, _) = SCRIPT
            .split_once("display = platform.python_implementation()")
            .unwrap();
        let cases = r#"
import io
import types
import builtins
result = {}
config = {"Py_DEBUG":0,"WITH_PYMALLOC":1,"Py_UNICODE_SIZE":4}
sysconfig.get_config_var = lambda key: config.get(key)
result["bools"] = [bool_config("Py_DEBUG"),bool_config("WITH_PYMALLOC"),bool_config("missing")]
config["Py_DEBUG"] = "unexpected"
result["badbool"] = bool_config("Py_DEBUG")
original_version = sys.version_info
sys.version_info = (3,2,6,"final",0)
result["ucs"] = unicode_width()
sys.version_info = original_version
result["modern_ucs"] = unicode_width()
result["versions"] = [numeric_version("14.5.0"),numeric_version("26.0"),numeric_version("14.x"),numeric_version("999999999.2")]

def make_elf(bits,little,machine,loader):
    data = bytearray(512)
    data[:4] = b"\x7fELF"
    data[4],data[5],data[6] = (1 if bits == 32 else 2),(1 if little else 2),1
    endian = "<" if little else ">"
    struct.pack_into(endian+"H",data,18,machine)
    if bits == 32:
        struct.pack_into(endian+"I",data,28,64)
        struct.pack_into(endian+"HH",data,42,32,1)
        struct.pack_into(endian+"I",data,64,3)
        struct.pack_into(endian+"I",data,68,256)
        struct.pack_into(endian+"I",data,80,len(loader)+1)
    else:
        struct.pack_into(endian+"Q",data,32,64)
        struct.pack_into(endian+"HH",data,54,56,1)
        struct.pack_into(endian+"I",data,64,3)
        struct.pack_into(endian+"Q",data,72,256)
        struct.pack_into(endian+"Q",data,96,len(loader)+1)
    data[256:256+len(loader)] = loader.encode("ascii")
    return bytes(data)
result["elf32"] = read_elf(io.BytesIO(make_elf(32,True,3,"/lib/ld-linux.so.2")))
result["elfbe"] = read_elf(io.BytesIO(make_elf(64,False,21,"/lib/ld64.so.1")))
result["badelf"] = read_elf(io.BytesIO(b"not ELF"))
result["arches32"] = linux_arches(result["elf32"],"x86_64")
arm = {"class":1,"little_endian":True,"machine":40,"flags":0x05000400,"interpreter":None}
result["armhf"] = linux_arches(arm,"aarch64")
arm["flags"] = 0x05000200
result["armsoft"] = linux_arches(arm,"aarch64")

fake = types.ModuleType("_manylinux")
fake.manylinux_compatible = lambda major,minor,arch: False if (minor,arch) == (17,"aarch64") else None
sys.modules["_manylinux"] = fake
result["policy"] = manylinux_observation({"family":"glibc","major":2,"minor":18},["aarch64"])
def broken(major,minor,arch):
    raise RuntimeError("fixture callback broken")
fake.manylinux_compatible = broken
result["brokenpolicy"] = manylinux_observation({"family":"glibc","major":2,"minor":18},["aarch64"])
del fake.manylinux_compatible
fake.manylinux2014_compatible = False
result["legacypolicy"] = manylinux_observation({"family":"glibc","major":2,"minor":18},["aarch64"])
fake.manylinux2014_compatible = "false"
result["invalidpolicy"] = manylinux_observation({"family":"glibc","major":2,"minor":18},["aarch64"])
original_import = builtins.__import__
import importlib.util
original_find_spec = importlib.util.find_spec
del sys.modules["_manylinux"]
importlib.util.find_spec = lambda name: None
result["absentpolicy"] = manylinux_observation({"family":"glibc","major":2,"minor":18},["aarch64"])
importlib.util.find_spec = lambda name: object()
def broken_import(name,globals=None,locals=None,fromlist=(),level=0):
    if name == "_manylinux":
        raise ImportError("fixture missing dependency")
    return original_import(name,globals,locals,fromlist,level)
builtins.__import__ = broken_import
result["brokenimport"] = manylinux_observation({"family":"glibc","major":2,"minor":18},["aarch64"])
builtins.__import__ = original_import
importlib.util.find_spec = original_find_spec

import plistlib
original_open = builtins.open
def product_open(path,mode="r"):
    if path == "/System/Library/CoreServices/SystemVersion.plist":
        return io.BytesIO(b'<?xml version="1.0"?><plist version="1.0"><dict><key>ProductVersion</key><string>26.0</string></dict></plist>')
    return original_open(path,mode)
builtins.open = product_open
result["macproduct"] = mac_product_version()
def huge_product(path,mode="r"):
    return io.BytesIO(b"x"*(MAX_HEADER_BYTES+1))
builtins.open = huge_product
result["hugemacproduct"] = mac_product_version()
builtins.open = original_open

import ctypes
original_cdll = ctypes.CDLL
original_string_at = ctypes.string_at
class Function(object):
    def __init__(self,value): self.value = value
    def __call__(self,*args): return self.value
library = types.SimpleNamespace(_dyld_get_image_header=Function(1),gnu_get_libc_version=Function(b"2.31"))
ctypes.CDLL = lambda value: library
ctypes.string_at = lambda pointer,size: b"\xcf\xfa\xed\xfe"+struct.pack("<i",16777223)
result["macho_x64"] = macho_cpu_type()
ctypes.string_at = lambda pointer,size: b"\xcf\xfa\xed\xfe"+struct.pack("<i",16777228)
result["macho_arm64"] = macho_cpu_type()
result["glibc"] = libc_observation(None)
del library.gnu_get_libc_version
result["musl"] = libc_observation({"interpreter":"/lib/ld-musl-aarch64.so.1"})
result["unknownlibc"] = libc_observation({"interpreter":"/lib/unrecognized.so"})
ctypes.CDLL = original_cdll
ctypes.string_at = original_string_at
print(json.dumps(result,sort_keys=True))
"#;
        let output = fixture_output(&handle, &format!("{helpers}\n{cases}")).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(actual["bools"], json!([false, true, null]));
        assert_eq!(actual["badbool"], Value::Null);
        assert_eq!(actual["ucs"], json!(4));
        assert_eq!(actual["modern_ucs"], Value::Null);
        assert_eq!(actual["versions"], json!([[14, 5, 0], [26, 0], null, null]));
        assert_eq!(actual["elf32"]["class"], json!(1));
        assert_eq!(actual["elf32"]["machine"], json!(3));
        assert_eq!(actual["elfbe"]["little_endian"], json!(false));
        assert_eq!(actual["elfbe"]["machine"], json!(21));
        assert_eq!(actual["badelf"], Value::Null);
        assert_eq!(actual["arches32"], json!(["i686"]));
        assert_eq!(actual["armhf"], json!(["armv8l", "armv7l"]));
        assert_eq!(actual["armsoft"], json!([]));
        assert_eq!(
            actual["policy"],
            json!({"state":"overrides","values":[
            {"arch":"aarch64","major":2,"minor":18,"compatible":true},
            {"arch":"aarch64","major":2,"minor":17,"compatible":false}]})
        );
        assert_eq!(actual["legacypolicy"], actual["policy"]);
        for key in ["brokenpolicy", "invalidpolicy", "brokenimport"] {
            assert_eq!(actual[key], json!({"state":"unknown"}));
        }
        assert_eq!(actual["absentpolicy"], json!({"state":"default"}));
        assert_eq!(
            actual["macproduct"],
            json!({"major":26,"minor":0,"micro":0})
        );
        assert_eq!(actual["hugemacproduct"], Value::Null);
        assert_eq!(actual["macho_x64"], json!(16777223));
        assert_eq!(actual["macho_arm64"], json!(16777228));
        assert_eq!(
            actual["glibc"],
            json!({"family":"glibc","major":2,"minor":31})
        );
        assert_eq!(
            actual["musl"],
            json!({"family":"musl","version":null,"loader":"/lib/ld-musl-aarch64.so.1"})
        );
        assert_eq!(actual["unknownlibc"], json!({"family":"unknown"}));
    }

    #[tokio::test]
    #[ignore = "requires PAKRO_TEST_PYTHON pointing to an existing installed runtime"]
    async fn target_bound_runtime_emits_compatibility_without_external_packages() {
        let descriptor = probe(&real_handle(), Duration::from_secs(10))
            .await
            .unwrap();
        descriptor.validate_compatibility().unwrap();
        let observation = descriptor
            .compatibility
            .as_ref()
            .expect("fixed script must emit observations");
        assert!(matches!(observation.pointer_bits, 32 | 64));
        match &observation.platform {
            PythonPlatformObservation::Macos {
                product_version, ..
            } => {
                assert_eq!(descriptor.markers.platform_system, "Darwin");
                assert!(
                    product_version.is_some(),
                    "real macOS ProductVersion plist unavailable"
                );
            }
            PythonPlatformObservation::Linux { .. } => {
                assert_eq!(descriptor.markers.platform_system, "Linux")
            }
        }
    }

    fn real_handle() -> PythonRuntimeHandle {
        let executable = std::env::var_os("PAKRO_TEST_PYTHON")
            .expect("set PAKRO_TEST_PYTHON to an already existing absolute interpreter");
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
import struct
sys.abiflags = None

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
    #[ignore = "requires PAKRO_TEST_PYTHON pointing to an existing modern Python 3.3+ host"]
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
    #[ignore = "requires PAKRO_TEST_PYTHON pointing to an existing modern Python 3.3+ host"]
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
    #[ignore = "requires PAKRO_TEST_PYTHON pointing to an already existing absolute interpreter"]
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
    #[ignore = "requires PAKRO_TEST_PYTHON pointing to an already existing absolute interpreter"]
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
