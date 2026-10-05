//! Wheel filename parsing per the binary distribution format spec:
//! `{distribution}-{version}(-{build})?-{python}-{abi}-{platform}.whl`, where
//! each of the last three fields may hold a `.`-compressed set of tags.

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
