//! UTF-8 wheel headers and semantic distribution identities.

use std::collections::{BTreeMap, BTreeSet};

use eyre::{Result, ensure, eyre};
use pakro_index::{WheelBuildTag, WheelTag, normalize_py_name};
use pakro_resolver::solve::{Pep440, VersionAdapter};

use super::usage::InspectionUsage;
use super::zip_directory::normalize_path;
use super::{WheelCoreMetadata, WheelLimits, WheelScheme, WheelWarning};

pub(super) fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn equal_version(left: &str, right: &str) -> bool {
    match (Pep440::parse_version(left), Pep440::parse_version(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

pub(super) fn matches_stem(stem: &str, name: &str, version: &str) -> bool {
    stem.match_indices('-')
        .filter(|(at, _)| {
            valid_name(&stem[..*at])
                && normalize_py_name(&stem[..*at]) == name
                && equal_version(&stem[*at + 1..], version)
        })
        .count()
        == 1
}

fn headers(
    bytes: &[u8],
    limits: WheelLimits,
    label: &str,
    usage: &mut InspectionUsage,
) -> Result<(BTreeMap<String, Vec<String>>, String)> {
    ensure!(
        bytes.len() <= limits.max_metadata_bytes,
        "wheel metadata byte limit exceeded"
    );
    usage.metadata_bytes = usage.metadata_bytes.max(bytes.len());
    let text = std::str::from_utf8(bytes).map_err(|_| eyre!("{label}: invalid UTF-8"))?;
    ensure!(!text.contains('\0'), "{label}: NUL in metadata");
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut previous: Option<String> = None;
    let mut offset = 0;
    let mut count = 0_usize;
    for raw in text.split_inclusive('\n') {
        offset += raw.len();
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        ensure!(!line.contains('\r'), "{label}: malformed header newline");
        if line.is_empty() {
            return Ok((fields, text[offset..].to_owned()));
        }
        if line.starts_with([' ', '\t']) {
            let key = previous
                .as_ref()
                .ok_or_else(|| eyre!("{label}: orphan continuation"))?;
            let value = fields.get_mut(key).unwrap().last_mut().unwrap();
            value.push(' ');
            value.push_str(line.trim_matches([' ', '\t']));
            continue;
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| eyre!("wheel header count limit overflow"))?;
        ensure!(
            count <= limits.max_header_fields,
            "wheel header count limit exceeded"
        );
        usage.header_fields = usage.header_fields.max(count);
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| eyre!("{label}: malformed header"))?;
        ensure!(
            !key.is_empty()
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "{label}: invalid header field name"
        );
        let key = key.to_ascii_lowercase();
        fields
            .entry(key.clone())
            .or_default()
            .push(value.trim_matches([' ', '\t']).to_owned());
        previous = Some(key);
    }
    Ok((fields, String::new()))
}

fn singleton<'a>(
    fields: &'a BTreeMap<String, Vec<String>>,
    key: &str,
    label: &str,
) -> Result<&'a str> {
    let values = fields
        .get(key)
        .ok_or_else(|| eyre!("{label}: missing required header"))?;
    ensure!(
        values.len() == 1 && !values[0].is_empty(),
        "{label}: duplicate or empty singleton header"
    );
    Ok(&values[0])
}

fn format_version(value: &str, label: &str) -> Result<(u32, u32)> {
    let (major, minor) = value
        .split_once('.')
        .ok_or_else(|| eyre!("{label}: invalid format version"))?;
    ensure!(
        !major.is_empty()
            && !minor.is_empty()
            && major
                .bytes()
                .chain(minor.bytes())
                .all(|byte| byte.is_ascii_digit()),
        "{label}: invalid format version"
    );
    Ok((
        major
            .parse()
            .map_err(|_| eyre!("{label}: format version overflow"))?,
        minor
            .parse()
            .map_err(|_| eyre!("{label}: format version overflow"))?,
    ))
}

pub(super) struct WheelHeaders {
    pub root_scheme: WheelScheme,
    pub tags: Vec<WheelTag>,
    pub build: Option<WheelBuildTag>,
    pub warnings: Vec<WheelWarning>,
}

pub(super) fn parse_wheel(
    bytes: &[u8],
    filename_tags: &[WheelTag],
    filename_build: Option<&WheelBuildTag>,
    limits: WheelLimits,
    usage: &mut InspectionUsage,
) -> Result<WheelHeaders> {
    let (fields, _) = headers(bytes, limits, "WHEEL", usage)?;
    let version = format_version(
        singleton(&fields, "wheel-version", "WHEEL Wheel-Version")?,
        "WHEEL Wheel-Version",
    )?;
    ensure!(version.0 == 1, "WHEEL Wheel-Version major is unsupported");
    let mut warnings = Vec::new();
    if version.1 > 0 {
        warnings.push(WheelWarning::NewerWheelMinor {
            major: version.0,
            minor: version.1,
        });
    }
    let root_scheme = match singleton(&fields, "root-is-purelib", "WHEEL Root-Is-Purelib")? {
        "true" => WheelScheme::Purelib,
        "false" => WheelScheme::Platlib,
        _ => return Err(eyre!("WHEEL Root-Is-Purelib must be true or false")),
    };
    let mut tags = BTreeSet::new();
    for value in fields
        .get("tag")
        .ok_or_else(|| eyre!("WHEEL missing Tag"))?
    {
        let parts: Vec<_> = value.split('-').collect();
        ensure!(parts.len() == 3, "WHEEL invalid expanded Tag");
        let tag = WheelTag::new(parts[0], parts[1], parts[2])
            .map_err(|_| eyre!("WHEEL invalid expanded Tag"))?;
        tags.insert(tag);
        ensure!(
            tags.len() <= limits.max_tag_triples,
            "wheel Tag count limit exceeded"
        );
        usage.tag_triples = usage.tag_triples.max(tags.len());
    }
    let tags: Vec<_> = tags.into_iter().collect();
    ensure!(
        tags == filename_tags,
        "WHEEL Tag set disagrees with filename"
    );
    let build = match fields.get("build") {
        Some(_) => Some(
            WheelBuildTag::parse(singleton(&fields, "build", "WHEEL Build")?)
                .ok_or_else(|| eyre!("WHEEL invalid Build"))?,
        ),
        None => None,
    };
    ensure!(
        build.as_ref() == filename_build,
        "WHEEL Build disagrees with filename"
    );
    // Repair tools can append their own informational Generator header. It
    // confers no installation authority; all header budgets and the original
    // WHEEL bytes' RECORD verification still apply.
    Ok(WheelHeaders {
        root_scheme,
        tags,
        build,
        warnings,
    })
}

pub(super) fn parse_core(
    bytes: Vec<u8>,
    name: &str,
    version: &str,
    limits: WheelLimits,
    usage: &mut InspectionUsage,
) -> Result<(WheelCoreMetadata, String, Vec<WheelWarning>)> {
    let (fields, description) = headers(&bytes, limits, "METADATA", usage)?;
    let metadata_version = format_version(
        singleton(&fields, "metadata-version", "METADATA Metadata-Version")?,
        "METADATA Metadata-Version",
    )?;
    ensure!(
        matches!(metadata_version, (1, 1 | 2) | (2, 1..)),
        "METADATA Metadata-Version is unsupported"
    );
    let mut warnings = Vec::new();
    if metadata_version.0 == 2 && metadata_version.1 > 6 {
        warnings.push(WheelWarning::NewerMetadataMinor {
            major: metadata_version.0,
            minor: metadata_version.1,
        });
    }
    let metadata_name = singleton(&fields, "name", "METADATA Name")?;
    let metadata_version_text = singleton(&fields, "version", "METADATA Version")?;
    ensure!(valid_name(metadata_name), "METADATA invalid Name");
    let parsed_version = Pep440::parse_version(metadata_version_text)
        .ok_or_else(|| eyre!("METADATA invalid Version"))?;
    ensure!(
        normalize_py_name(metadata_name) == name && equal_version(metadata_version_text, version),
        "METADATA identity disagrees with expected wheel"
    );
    for key in [
        "summary",
        "description",
        "description-content-type",
        "keywords",
        "home-page",
        "download-url",
        "author",
        "author-email",
        "maintainer",
        "maintainer-email",
        "license",
        "license-expression",
        "requires-python",
    ] {
        ensure!(
            fields.get(key).is_none_or(|values| values.len() == 1),
            "METADATA duplicate singleton header"
        );
    }
    Ok((
        WheelCoreMetadata {
            metadata_version,
            fields,
            description,
            raw: bytes,
        },
        parsed_version.to_string(),
        warnings,
    ))
}

pub(super) fn validate_licenses(
    metadata: &WheelCoreMetadata,
    info: &str,
    files: &BTreeSet<&str>,
) -> Result<()> {
    if metadata.metadata_version < (2, 4) {
        return Ok(());
    }
    for value in metadata.values("license-file") {
        ensure!(
            !value.split('/').any(|part| part == ".."),
            "METADATA License-File has parent components"
        );
        let path = normalize_path(value, "METADATA License-File")?;
        ensure!(
            files.contains(format!("{info}/licenses/{path}").as_str()),
            "METADATA License-File is missing from licenses inventory"
        );
    }
    Ok(())
}
