//! Ordered compatibility tags from an explicitly observed Python target.
//!
//! Ordering follows packaging 26.3 (commit
//! `929fd4b1410ac7ef61ef3f45b2f5d7e87711a9b5`), with conservative rejection of
//! unknown targets and bounded output. No host inspection or subprocesses occur
//! here; callers must establish the provenance of every observation.

use std::collections::BTreeSet;

use crate::{IndexError, Result, SupportedWheelTags, WheelTag};

/// Language, interpreter ABI, and execution platform observed for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonTagTarget {
    pub language_major: u32,
    pub language_minor: u32,
    pub interpreter: PythonTagInterpreter,
    pub platform: PythonTagPlatform,
}

/// ABI observations; PyPy implementation versions are not language versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonTagInterpreter {
    CPython {
        debug: bool,
        gil_disabled: bool,
        pymalloc: Option<bool>,
        unicode_size: Option<u8>,
    },
    PyPy {
        soabi: String,
        ext_suffix: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PythonMacArch {
    X86_64,
    Arm64,
    I386,
    Ppc,
    Ppc64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PythonLinuxArch {
    X86_64,
    I686,
    Aarch64,
    Armv8l,
    Armv7l,
    Ppc64,
    Ppc64le,
    S390x,
    Loongarch64,
    Riscv64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonTagLibc {
    Glibc { major: u32, minor: u32 },
    Musl { major: u32, minor: u32 },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonManylinuxOverride {
    pub arch: PythonLinuxArch,
    pub major: u32,
    pub minor: u32,
    pub compatible: bool,
}

/// An observed manylinux policy, including explicit per-floor vetoes.
///
/// Empty overrides mean observed default policy, never an unknown observation.
/// The caller must validate callback coverage before projecting observations
/// into this type, and derive `abi_compatible` from actual executable ABI data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonManylinuxPolicy {
    pub abi_compatible: bool,
    pub overrides: Vec<PythonManylinuxOverride>,
}

/// The actual execution architecture and OS/libc version of the target.
///
/// macOS requires the real ProductVersion rather than a 10.16 compatibility
/// alias. Linux architecture must reflect the executable, including its bitness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonTagPlatform {
    Macos {
        product_major: u32,
        product_minor: u32,
        arch: PythonMacArch,
    },
    Linux {
        arch: PythonLinuxArch,
        libc: PythonTagLibc,
        manylinux: PythonManylinuxPolicy,
    },
}

const MAX_PLATFORM_TAGS: usize = 4096;

fn invalid_target(reason: &str) -> IndexError {
    IndexError::InvalidPythonTagTarget(reason.to_owned())
}

impl PythonLinuxArch {
    fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::I686 => "i686",
            Self::Aarch64 => "aarch64",
            Self::Armv8l => "armv8l",
            Self::Armv7l => "armv7l",
            Self::Ppc64 => "ppc64",
            Self::Ppc64le => "ppc64le",
            Self::S390x => "s390x",
            Self::Loongarch64 => "loongarch64",
            Self::Riscv64 => "riscv64",
        }
    }

    fn compatible_arches(self) -> Vec<Self> {
        if self == Self::Armv8l {
            vec![Self::Armv8l, Self::Armv7l]
        } else {
            vec![self]
        }
    }
}

fn push_platform(output: &mut Vec<String>, value: String) -> Result<()> {
    if output.len() >= MAX_PLATFORM_TAGS {
        return Err(invalid_target("platform tag resource limit exceeded"));
    }
    output.push(value);
    Ok(())
}

fn mac_formats(major: u32, minor: u32, arch: PythonMacArch) -> Vec<&'static str> {
    match arch {
        PythonMacArch::X86_64 if (major, minor) >= (10, 4) => vec![
            "x86_64",
            "intel",
            "fat64",
            "fat3",
            "universal2",
            "universal",
        ],
        PythonMacArch::I386 if (major, minor) >= (10, 4) => {
            vec!["i386", "intel", "fat3", "fat", "universal"]
        }
        PythonMacArch::Ppc64 if (10, 4) <= (major, minor) && (major, minor) <= (10, 5) => {
            vec!["ppc64", "fat64", "universal"]
        }
        PythonMacArch::Ppc if (major, minor) <= (10, 6) => vec!["ppc", "fat3", "fat", "universal"],
        PythonMacArch::Arm64 if major >= 11 => vec!["arm64", "universal2"],
        _ => Vec::new(),
    }
}

fn mac_platforms(major: u32, minor: u32, arch: PythonMacArch) -> Result<Vec<String>> {
    let valid = match arch {
        PythonMacArch::X86_64 => major >= 11 || (major == 10 && (4..=15).contains(&minor)),
        PythonMacArch::Arm64 => major >= 11,
        PythonMacArch::I386 => major == 10 && (4..=14).contains(&minor),
        PythonMacArch::Ppc => major == 10 && minor <= 6,
        PythonMacArch::Ppc64 => major == 10 && (4..=5).contains(&minor),
    };
    if !valid {
        return Err(invalid_target(
            "unsupported macOS product version/execution architecture",
        ));
    }
    let mut output = Vec::new();
    if major == 10 {
        for floor in (0..=minor).rev() {
            for format in mac_formats(10, floor, arch) {
                push_platform(&mut output, format!("macosx_10_{floor}_{format}"))?;
            }
        }
    } else {
        for floor in (11..=major).rev() {
            for format in mac_formats(floor, 0, arch) {
                push_platform(&mut output, format!("macosx_{floor}_0_{format}"))?;
            }
        }
        for floor in (4..=16).rev() {
            if arch == PythonMacArch::X86_64 {
                for format in mac_formats(10, floor, arch) {
                    push_platform(&mut output, format!("macosx_10_{floor}_{format}"))?;
                }
            } else {
                push_platform(&mut output, format!("macosx_10_{floor}_universal2"))?;
            }
        }
    }
    Ok(output)
}

fn linux_platforms(
    arch: PythonLinuxArch,
    libc: &PythonTagLibc,
    policy: &PythonManylinuxPolicy,
) -> Result<Vec<String>> {
    let arches = arch.compatible_arches();
    let minimum = if matches!(arch, PythonLinuxArch::X86_64 | PythonLinuxArch::I686) {
        5
    } else {
        17
    };
    match libc {
        PythonTagLibc::Unknown => return Err(invalid_target("libc observation is unknown")),
        PythonTagLibc::Glibc { major, minor } => {
            if *major != 2 {
                return Err(invalid_target("only observed glibc major 2 is supported"));
            }
            let mut keys = BTreeSet::new();
            for value in &policy.overrides {
                if !arches.contains(&value.arch)
                    || value.major != 2
                    || value.minor < minimum
                    || value.minor > *minor
                    || !keys.insert((value.arch, value.major, value.minor))
                {
                    return Err(invalid_target(
                        "duplicate or out-of-domain manylinux override",
                    ));
                }
            }
        }
        PythonTagLibc::Musl { major, .. } => {
            if *major == 0 {
                return Err(invalid_target("musl major must be positive"));
            }
            if policy.abi_compatible || !policy.overrides.is_empty() {
                return Err(invalid_target(
                    "musl target cannot carry a glibc manylinux policy",
                ));
            }
        }
    }
    let mut output = Vec::new();
    for value in &arches {
        push_platform(&mut output, format!("linux_{}", value.name()))?;
    }
    match libc {
        PythonTagLibc::Glibc { minor, .. } if policy.abi_compatible => {
            for value in arches {
                for floor in (minimum..=*minor).rev() {
                    let permitted = policy
                        .overrides
                        .iter()
                        .find(|item| item.arch == value && item.major == 2 && item.minor == floor)
                        .is_none_or(|item| item.compatible);
                    if !permitted {
                        continue;
                    }
                    push_platform(&mut output, format!("manylinux_2_{floor}_{}", value.name()))?;
                    let legacy = match floor {
                        17 => Some("manylinux2014"),
                        12 => Some("manylinux2010"),
                        5 => Some("manylinux1"),
                        _ => None,
                    };
                    if let Some(legacy) = legacy {
                        push_platform(&mut output, format!("{legacy}_{}", value.name()))?;
                    }
                }
            }
        }
        PythonTagLibc::Musl { major, minor } => {
            for value in arches {
                for floor in (0..=*minor).rev() {
                    push_platform(
                        &mut output,
                        format!("musllinux_{major}_{floor}_{}", value.name()),
                    )?;
                }
            }
        }
        _ => {}
    }
    Ok(output)
}

/// Return preferred platforms first, rejecting unknown or inconsistent targets.
///
/// Linux native platforms precede libc platforms. Output exceeding 4096 entries
/// is an error rather than a partially supported target.
pub fn python_platform_tags(platform: &PythonTagPlatform) -> Result<Vec<String>> {
    match platform {
        PythonTagPlatform::Macos {
            product_major,
            product_minor,
            arch,
        } => mac_platforms(*product_major, *product_minor, *arch),
        PythonTagPlatform::Linux {
            arch,
            libc,
            manylinux,
        } => linux_platforms(*arch, libc, manylinux),
    }
}

const MAX_SUPPORTED_TAGS: usize = 65536;

fn validate_language(major: u32, minor: u32) -> Result<()> {
    if major != 3 {
        return Err(invalid_target("only Python language major 3 is supported"));
    }
    if u64::from(minor) + 2 > MAX_SUPPORTED_TAGS as u64 {
        return Err(invalid_target(
            "language minor exceeds supported tag resource limit",
        ));
    }
    Ok(())
}

fn cpython_abis(
    minor: u32,
    debug: bool,
    gil_disabled: bool,
    pymalloc: Option<bool>,
    unicode_size: Option<u8>,
) -> Result<Vec<String>> {
    if gil_disabled && minor < 13 {
        return Err(invalid_target(
            "free-threaded CPython requires language 3.13 or newer",
        ));
    }
    if unicode_size.is_some_and(|width| !matches!(width, 2 | 4)) {
        return Err(invalid_target(
            "Unicode ABI width must be 2 or 4 when present",
        ));
    }
    let mut abi = format!("cp3{minor}");
    if gil_disabled {
        abi.push('t');
    }
    if debug {
        abi.push('d');
    }
    if minor < 8 {
        if pymalloc.ok_or_else(|| invalid_target("pre-3.8 CPython requires observed pymalloc"))? {
            abi.push('m');
        }
        if minor < 3 {
            let width = unicode_size.ok_or_else(|| {
                invalid_target("pre-3.3 CPython requires observed Unicode ABI width")
            })?;
            if width == 4 {
                abi.push('u');
            }
        }
    }
    let mut result = vec![abi];
    if minor >= 8 && debug {
        result.push(format!("cp3{minor}{}", if gil_disabled { "t" } else { "" }));
    }
    Ok(result)
}

fn pypy_abi(minor: u32, soabi: &str, ext_suffix: &str) -> Result<String> {
    fn safe(value: &str) -> bool {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }
    fn components(value: &str, minor: u32) -> Result<String> {
        if !safe(value) {
            return Err(invalid_target("invalid PyPy SOABI characters"));
        }
        let mut pieces = value.split('-');
        let family = pieces
            .next()
            .ok_or_else(|| invalid_target("missing PyPy language ABI"))?;
        let implementation = pieces
            .next()
            .ok_or_else(|| invalid_target("missing PyPy implementation ABI"))?;
        if family != format!("pypy3{minor}")
            || !implementation.strip_prefix("pp").is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
            })
            || pieces.any(str::is_empty)
        {
            return Err(invalid_target(
                "PyPy SOABI must contain matching pypyXY and ppNN components",
            ));
        }
        Ok(format!("{family}_{implementation}"))
    }
    let inner = ext_suffix
        .strip_prefix('.')
        .and_then(|value| value.strip_suffix(".so"))
        .ok_or_else(|| invalid_target("PyPy extension suffix must be .SOABI.so on macOS/Linux"))?;
    let from_soabi = components(soabi, minor)?;
    let from_suffix = components(inner, minor)?;
    if from_soabi != from_suffix {
        return Err(invalid_target("PyPy SOABI and extension suffix disagree"));
    }
    Ok(from_soabi)
}

fn push_tag(
    output: &mut Vec<WheelTag>,
    seen: &mut BTreeSet<WheelTag>,
    interpreter: &str,
    abi: &str,
    platform: &str,
) -> Result<()> {
    let tag = WheelTag::new(interpreter, abi, platform)?;
    if seen.contains(&tag) {
        return Ok(());
    }
    if output.len() >= MAX_SUPPORTED_TAGS {
        return Err(invalid_target("supported tag resource limit exceeded"));
    }
    seen.insert(tag.clone());
    output.push(tag);
    Ok(())
}

fn generic_interpreters(minor: u32) -> Vec<String> {
    let mut values = vec![format!("py3{minor}"), "py3".to_owned()];
    for older in (0..minor).rev() {
        values.push(format!("py3{older}"));
    }
    values
}

/// Return only the conservative generic `pyXY/py3-none-any` subset.
///
/// This does not include interpreter-specific pure tags and is not evidence of
/// native support. Callers must explain why this restricted capability is used.
pub fn pure_python_supported_tags(major: u32, minor: u32) -> Result<SupportedWheelTags> {
    validate_language(major, minor)?;
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for interpreter in generic_interpreters(minor) {
        push_tag(&mut output, &mut seen, &interpreter, "none", "any")?;
    }
    SupportedWheelTags::new(output)
}

/// Return all supported tag triples in preference order for an explicit target.
///
/// Unknown native observations are errors; this function never falls back to the
/// generic pure subset. More than 65536 triples is an error, without truncation.
pub fn python_supported_tags(target: &PythonTagTarget) -> Result<SupportedWheelTags> {
    validate_language(target.language_major, target.language_minor)?;
    let minor = target.language_minor;
    let platforms = python_platform_tags(&target.platform)?;
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    let compatible_interpreter = match &target.interpreter {
        PythonTagInterpreter::CPython {
            debug,
            gil_disabled,
            pymalloc,
            unicode_size,
        } => {
            let interpreter = format!("cp3{minor}");
            for abi in cpython_abis(minor, *debug, *gil_disabled, *pymalloc, *unicode_size)? {
                for platform in &platforms {
                    push_tag(&mut output, &mut seen, &interpreter, &abi, platform)?;
                }
            }
            // PEP 803 reserves older abi3t tags too; pinned packaging accepts them
            // down to 3.2 even though extension build support begins at 3.15.
            let stable = (minor >= 2).then_some(if *gil_disabled { "abi3t" } else { "abi3" });
            if let Some(abi) = stable {
                for platform in &platforms {
                    push_tag(&mut output, &mut seen, &interpreter, abi, platform)?;
                }
            }
            for platform in &platforms {
                push_tag(&mut output, &mut seen, &interpreter, "none", platform)?;
            }
            if let Some(abi) = stable {
                for older in (2..minor).rev() {
                    for platform in &platforms {
                        push_tag(
                            &mut output,
                            &mut seen,
                            &format!("cp3{older}"),
                            abi,
                            platform,
                        )?;
                    }
                }
            }
            interpreter
        }
        PythonTagInterpreter::PyPy { soabi, ext_suffix } => {
            let interpreter = format!("pp3{minor}");
            let abi = pypy_abi(minor, soabi, ext_suffix)?;
            for value in [&abi[..], "none"] {
                for platform in &platforms {
                    push_tag(&mut output, &mut seen, &interpreter, value, platform)?;
                }
            }
            "pp3".to_owned()
        }
    };
    let generics = generic_interpreters(minor);
    for interpreter in &generics {
        for platform in &platforms {
            push_tag(&mut output, &mut seen, interpreter, "none", platform)?;
        }
    }
    push_tag(
        &mut output,
        &mut seen,
        &compatible_interpreter,
        "none",
        "any",
    )?;
    for interpreter in &generics {
        push_tag(&mut output, &mut seen, interpreter, "none", "any")?;
    }
    SupportedWheelTags::new(output)
}
