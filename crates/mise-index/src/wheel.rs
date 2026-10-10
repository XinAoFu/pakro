//! Wheel filename parsing per the binary distribution format spec:
//! `{distribution}-{version}(-{build})?-{python}-{abi}-{platform}.whl`, where
//! each of the last three fields may hold a `.`-compressed set of tags.

use std::cmp::Ordering;
use std::str::FromStr;

use mise_store::{Arch, Libc, Os, Platform};
use pep440_rs::Version;
use pep508_rs::PackageName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelTags {
    pub python: Vec<String>,
    pub abi: Vec<String>,
    pub platform: Vec<String>,
}

/// A wheel build tuple, with arbitrary-precision decimal ordering.
/// Original text is diagnostic only and is excluded from equality/ordering.
#[derive(Debug, Clone)]
pub struct WheelBuildTag {
    raw: String,
    number: String,
    suffix: String,
}

impl WheelBuildTag {
    pub fn parse(value: &str) -> Option<Self> {
        if value.is_empty()
            || value
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '/' | '\\' | '-'))
        {
            return None;
        }
        let digits = value.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let normalized = value[..digits].trim_start_matches('0');
        Some(Self {
            raw: value.to_string(),
            number: if normalized.is_empty() {
                "0"
            } else {
                normalized
            }
            .to_string(),
            suffix: value[digits..].to_string(),
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn number(&self) -> &str {
        &self.number
    }

    pub fn suffix(&self) -> &str {
        &self.suffix
    }
}

impl PartialEq for WheelBuildTag {
    fn eq(&self, other: &Self) -> bool {
        self.number == other.number && self.suffix == other.suffix
    }
}

impl Eq for WheelBuildTag {}

impl Ord for WheelBuildTag {
    fn cmp(&self, other: &Self) -> Ordering {
        self.number
            .len()
            .cmp(&other.number.len())
            .then_with(|| self.number.cmp(&other.number))
            .then_with(|| self.suffix.cmp(&other.suffix))
    }
}

impl PartialOrd for WheelBuildTag {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelFile {
    pub name: String,
    pub version: String,
    pub build: Option<WheelBuildTag>,
    pub tags: WheelTags,
}

fn valid_tag_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_interpreter_token(value: &str) -> bool {
    valid_tag_token(value)
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
}

fn parse_compressed_field(value: &str, validate: fn(&str) -> bool) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for token in value.split('.') {
        if !validate(token) {
            return None;
        }
        values.push(token.to_ascii_lowercase());
    }
    values.sort();
    values.dedup();
    Some(values)
}

pub fn parse_wheel_filename(filename: &str) -> Option<WheelFile> {
    if filename
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '/' | '\\'))
    {
        return None;
    }
    let stem = filename.strip_suffix(".whl")?;
    let parts: Vec<&str> = stem.split('-').collect();
    if !matches!(parts.len(), 5 | 6) {
        return None;
    }
    let name = parts[0];
    let version = parts[1];
    if name.is_empty() {
        return None;
    }
    PackageName::from_str(name).ok()?;
    Version::from_str(version).ok()?;
    let build = if parts.len() == 6 {
        Some(WheelBuildTag::parse(parts[2])?)
    } else {
        None
    };
    let [python, abi, platform] = parts[parts.len() - 3..] else {
        return None;
    };
    Some(WheelFile {
        name: name.to_string(),
        version: version.to_string(),
        build,
        tags: WheelTags {
            python: parse_compressed_field(python, valid_interpreter_token)?,
            abi: parse_compressed_field(abi, valid_tag_token)?,
            platform: parse_compressed_field(platform, valid_tag_token)?,
        },
    })
}

/// The interpreter a package is being installed for. M1 targets CPython only,
/// so the implementation family is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub major: u32,
    pub minor: u32,
}

/// Whether any tag combination of a wheel fits the given interpreter and host
/// platform.
pub fn wheel_matches(tags: &WheelTags, runtime: &RuntimeInfo, platform: &Platform) -> bool {
    // with the stable abi, the python tag names the *minimum* version
    let stable_abi = tags.abi.iter().any(|t| t == "abi3");
    tags.python
        .iter()
        .any(|t| python_tag_matches(t, runtime, stable_abi))
        && tags.abi.iter().any(|t| abi_tag_matches(t, runtime))
        && tags
            .platform
            .iter()
            .any(|t| platform_tag_matches(t, platform))
}

fn python_tag_matches(tag: &str, runtime: &RuntimeInfo, stable_abi: bool) -> bool {
    match tag {
        "py3" => runtime.major == 3,
        "py2" => runtime.major == 2,
        _ => match parse_cp_tag(tag) {
            Some((major, minor)) => {
                runtime.major == major
                    && (runtime.minor == minor || (stable_abi && runtime.minor > minor))
            }
            None => false,
        },
    }
}

fn abi_tag_matches(tag: &str, runtime: &RuntimeInfo) -> bool {
    match tag {
        "none" => true,
        // the stable ABI applies to every 3.x from its introduction on
        "abi3" => runtime.major >= 3,
        _ => match parse_cp_tag(tag) {
            Some((major, minor)) => runtime.major == major && runtime.minor == minor,
            None => false,
        },
    }
}

fn parse_cp_tag(tag: &str) -> Option<(u32, u32)> {
    let rest = tag.strip_prefix("cp")?;
    if rest.len() < 2 {
        return None;
    }
    let (major, minor) = rest.split_at(1);
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn platform_tag_matches(tag: &str, platform: &Platform) -> bool {
    if tag == "any" {
        return true;
    }
    if let Some(rest) = tag.strip_prefix("macosx_") {
        return platform.os == Os::Macos && tag_arch_matches(rest, platform.arch);
    }
    if let Some(rest) = tag.strip_prefix("manylinux_") {
        return platform.os == Os::Linux
            && platform.libc != Libc::Musl
            && tag_arch_matches(rest, platform.arch);
    }
    if let Some(rest) = tag.strip_prefix("musllinux_") {
        return platform.os == Os::Linux
            && platform.libc == Libc::Musl
            && tag_arch_matches(rest, platform.arch);
    }
    if let Some(rest) = tag.strip_prefix("linux_") {
        // the legacy linux tag predates musl wheels and means glibc
        return platform.os == Os::Linux
            && platform.libc != Libc::Musl
            && tag_arch_matches(rest, platform.arch);
    }
    false
}

fn tag_arch_matches(rest: &str, arch: Arch) -> bool {
    if rest.ends_with("universal2") || rest.ends_with("universal") {
        return true;
    }
    if rest.ends_with("x86_64") || rest.ends_with("amd64") {
        return arch == Arch::X86_64;
    }
    if rest.ends_with("arm64") || rest.ends_with("aarch64") {
        return arch == Arch::Aarch64;
    }
    false
}
