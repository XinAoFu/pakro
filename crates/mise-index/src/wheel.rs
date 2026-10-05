//! Wheel filename parsing per the binary distribution format spec:
//! `{distribution}-{version}(-{build})?-{python}-{abi}-{platform}.whl`, where
//! each of the last three fields may hold a `.`-compressed set of tags.

use mise_store::{Arch, Libc, Os, Platform};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelTags {
    pub python: Vec<String>,
    pub abi: Vec<String>,
    pub platform: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelFile {
    pub name: String,
    pub version: String,
    pub tags: WheelTags,
}

pub fn parse_wheel_filename(filename: &str) -> Option<WheelFile> {
    let stem = filename.strip_suffix(".whl")?;
    let parts: Vec<&str> = stem.split('-').collect();
    if !matches!(parts.len(), 5 | 6) {
        return None;
    }
    let name = parts[0];
    let version = parts[1];
    if name.is_empty() || version.is_empty() {
        return None;
    }
    let [python, abi, platform] = parts[parts.len() - 3..] else {
        return None;
    };
    let expand = |field: &str| -> Vec<String> { field.split('.').map(str::to_string).collect() };
    let tags = WheelTags {
        python: expand(python),
        abi: expand(abi),
        platform: expand(platform),
    };
    if tags.python.iter().any(String::is_empty)
        || tags.abi.iter().any(String::is_empty)
        || tags.platform.iter().any(String::is_empty)
    {
        return None;
    }
    Some(WheelFile {
        name: name.to_string(),
        version: version.to_string(),
        tags,
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
