//! Shared file-fact normalization for JSON and HTML Simple representations.
//! Representation differences are explicit; source/document identity is retained.

use super::*;
use pep440_rs::{Version, VersionSpecifiers};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

pub(super) fn validate_limits(limits: SimpleLimits) -> SimpleResult<()> {
    if limits.max_response_bytes == 0
        || [
            limits.max_files,
            limits.max_versions,
            limits.max_hashes_per_file,
            limits.max_relations,
            limits.max_field_bytes,
            limits.max_object_fields,
            limits.max_json_depth,
            limits.max_json_values,
        ]
        .contains(&0)
    {
        return Err(SimpleError::InvalidLimits);
    }
    Ok(())
}

pub(super) fn validate_context(context: &SimpleParseContext<'_>) -> SimpleResult<()> {
    let expected = context
        .registry
        .project_url(context.project)
        .map_err(|_| SimpleError::InvalidContext)?;
    if expected != *context.canonical_document_url {
        return Err(SimpleError::InvalidContext);
    }
    Ok(())
}
#[derive(Default)]
pub(super) struct RawMeta {
    pub(super) api: Option<String>,
    pub(super) tracks: Vec<String>,
}

#[derive(Default)]
pub(super) struct RawStatus {
    pub(super) status: Option<String>,
    pub(super) reason: Option<String>,
}

#[derive(Default)]
pub(super) struct RawProject {
    pub(super) meta: Option<RawMeta>,
    pub(super) name: Option<String>,
    pub(super) versions: Option<Vec<String>>,
    pub(super) files: Option<Vec<RawFile>>,
    pub(super) alternate: Vec<String>,
    pub(super) status: Option<RawStatus>,
}

#[derive(Default)]
pub(super) struct RawFile {
    pub(super) filename: Option<String>,
    pub(super) url: Option<String>,
    pub(super) hashes: Option<BTreeMap<String, String>>,
    pub(super) requires_python: Option<String>,
    pub(super) yanked: Option<Yanked>,
    pub(super) core: Option<RawCore>,
    pub(super) legacy_core: Option<RawCore>,
    pub(super) size: Option<u64>,
    pub(super) upload_time: Option<String>,
    pub(super) provenance: Option<String>,
}

pub(super) enum RawCore {
    Absent,
    Available(BTreeMap<String, String>),
}

pub(super) fn invalid(reason: &'static str) -> SimpleError {
    SimpleError::InvalidResponse(reason)
}
fn required<T>(value: Option<T>, reason: &'static str) -> SimpleResult<T> {
    value.ok_or_else(|| invalid(reason))
}
fn api_version(value: &str) -> SimpleResult<SimpleApiVersion> {
    let mut parts = value.split('.');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || [major, minor]
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(invalid("expected a Major.Minor API version"));
    }
    let major = major.parse().map_err(|_| invalid("invalid API version"))?;
    let minor = minor.parse().map_err(|_| invalid("invalid API version"))?;
    if major != 1 {
        return Err(SimpleError::UnsupportedApiMajor(major));
    }
    Ok(SimpleApiVersion { major, minor })
}
pub(super) fn finish_project(
    raw: RawProject,
    context: &SimpleParseContext<'_>,
) -> SimpleResult<SimpleProject> {
    finish_project_for_representation(raw, context, context.canonical_document_url.as_url(), true)
}
pub(super) fn finish_project_for_document(
    raw: RawProject,
    context: &SimpleParseContext<'_>,
    base: &Url,
) -> SimpleResult<SimpleProject> {
    finish_project_for_representation(raw, context, base, true)
}
pub(super) fn finish_html_project(
    raw: RawProject,
    context: &SimpleParseContext<'_>,
    base: &Url,
) -> SimpleResult<SimpleProject> {
    finish_project_for_representation(raw, context, base, false)
}
fn finish_project_for_representation(
    raw: RawProject,
    context: &SimpleParseContext<'_>,
    base: &Url,
    json_representation: bool,
) -> SimpleResult<SimpleProject> {
    let meta = required(raw.meta, "missing meta")?;
    let api = api_version(&required(meta.api, "missing api-version")?)?;
    let name = NormalizedPythonName::new(&required(raw.name, "missing project name")?)
        .map_err(|_| invalid("invalid project name"))?;
    if &name != context.project {
        return Err(invalid(
            "project name differs from requested source context",
        ));
    }
    if json_representation && api.minor >= 1 && raw.versions.is_none() {
        return Err(invalid("missing versions"));
    }
    let declared_versions = raw.versions.is_some();
    let versions = raw.versions.unwrap_or_default();
    let mut unique_versions = BTreeSet::new();
    for version in &versions {
        if version.is_empty() || !unique_versions.insert(version.as_str()) {
            return Err(invalid("empty or duplicate version"));
        }
    }
    let parsed_versions: BTreeSet<_> = versions
        .iter()
        .filter_map(|value| Version::from_str(value).ok())
        .collect();
    let mut files = BTreeMap::new();
    let mut version_listing_mismatch = false;
    for raw in required(raw.files, "missing files")? {
        let file = finish_file(raw, context, api, base, json_representation)?;
        if declared_versions && let Some(version) = file.version() {
            let parsed = Version::from_str(version).map_err(|_| invalid("invalid file version"))?;
            if !parsed_versions.contains(&parsed) {
                // Versions lists release labels, including historical labels
                // that may differ from uploaded filenames. Preserve both
                // observations without inventing a release/file association.
                version_listing_mismatch = true;
            }
        }
        if let Some(previous) = files.get(file.filename()) {
            if previous != &file {
                return Err(SimpleError::ConflictingFile);
            }
        } else {
            files.insert(file.filename().to_owned(), file);
        }
    }
    let tracks = meta
        .tracks
        .into_iter()
        .map(|value| relation(&value, context, base))
        .collect::<SimpleResult<Vec<_>>>()?;
    let alternate_locations = raw
        .alternate
        .into_iter()
        .map(|value| relation(&value, context, base))
        .collect::<SimpleResult<Vec<_>>>()?;
    let mut warnings = Vec::new();
    if version_listing_mismatch {
        warnings.push(SimpleWarning::VersionListingMismatch);
    }
    if api.minor > 4 {
        warnings.push(SimpleWarning::NewerApiMinor {
            major: api.major,
            minor: api.minor,
        });
    }
    if files
        .values()
        .any(|file| matches!(file.kind(), PythonDistributionKind::HistoricalFormat))
    {
        warnings.push(SimpleWarning::HistoricalDistribution);
    }
    let status = raw.status.map(|value| {
        let raw = value.status.unwrap_or_else(|| "active".to_owned());
        let kind = match raw.as_str() {
            "active" => ProjectStatusKind::Active,
            "archived" => ProjectStatusKind::Archived,
            "deprecated" => ProjectStatusKind::Deprecated,
            "quarantined" => ProjectStatusKind::Quarantined,
            _ => ProjectStatusKind::Unknown,
        };
        if kind == ProjectStatusKind::Unknown {
            warnings.push(SimpleWarning::UnknownProjectStatus);
        }
        ProjectStatus {
            kind,
            raw,
            reason: value.reason,
        }
    });
    Ok(SimpleProject {
        project: name,
        source: context.registry.id().clone(),
        api_version: api,
        versions,
        files: files.into_values().collect(),
        relations: RepositoryRelations {
            tracks,
            alternate_locations,
        },
        status,
        warnings,
    })
}
fn finish_file(
    raw: RawFile,
    context: &SimpleParseContext<'_>,
    api: SimpleApiVersion,
    base: &Url,
    json_representation: bool,
) -> SimpleResult<SimpleFile> {
    let filename = required(raw.filename, "missing filename")?;
    if filename.is_empty()
        || matches!(filename.as_str(), "." | "..")
        || filename
            .chars()
            .any(|value| value.is_control() || matches!(value, '/' | '\\'))
    {
        return Err(invalid("unsafe artifact filename"));
    }
    let mut file_locator = locator(&required(raw.url, "missing file URL")?, base)?;
    let mut raw_hashes = required(raw.hashes, "missing hashes")?;
    if let Some(fragment) = file_locator.url.fragment() {
        let (algorithm, digest) = fragment
            .split_once('=')
            .ok_or_else(|| invalid("invalid artifact hash fragment"))?;
        let parsed = normalize_hashes(BTreeMap::from([(algorithm.to_owned(), digest.to_owned())]))?;
        let current = normalize_hashes(raw_hashes)?;
        raw_hashes = current.values;
        for (algorithm, digest) in parsed.values {
            if raw_hashes.get(&algorithm).is_some_and(|old| old != &digest) {
                return Err(invalid("conflicting artifact hash fragment"));
            }
            if !raw_hashes.contains_key(&algorithm)
                && raw_hashes.len() >= context.limits.max_hashes_per_file
            {
                return Err(SimpleError::LimitExceeded(SimpleLimitKind::Hashes));
            }
            raw_hashes.insert(algorithm, digest);
        }
        file_locator.url.set_fragment(None);
    }
    // HTML's anchor text is required to match the URL path filename.
    // JSON has an independent filename field; its URL can be an opaque
    // download endpoint. It never becomes a destination filesystem path.
    if !json_representation {
        let basename = file_locator
            .url
            .path()
            .rsplit('/')
            .next()
            .unwrap_or_default();
        if decode_component(basename)? != filename {
            return Err(invalid("artifact filename differs from URL basename"));
        }
    }
    let hashes = normalize_hashes(raw_hashes)?;
    let (kind, version) = distribution(&filename, context.project)?;
    if let Some(specifier) = &raw.requires_python {
        VersionSpecifiers::from_str(specifier).map_err(|_| invalid("invalid Requires-Python"))?;
    }
    let core_metadata = match raw.core.or(raw.legacy_core) {
        None => CoreMetadataAvailability::Unspecified,
        Some(RawCore::Absent) => CoreMetadataAvailability::Absent,
        Some(RawCore::Available(values)) => CoreMetadataAvailability::Available {
            hashes: normalize_hashes(values)?,
        },
    };
    if json_representation && api.minor >= 1 && raw.size.is_none() {
        return Err(invalid("missing file size"));
    }
    if raw
        .upload_time
        .as_deref()
        .is_some_and(|value| !valid_upload_time(value))
    {
        return Err(invalid("invalid file upload-time"));
    }
    let provenance = raw
        .provenance
        .map(|value| {
            let value = locator(&value, base)?;
            if value.url.fragment().is_some() || !secure_origin(&value.url) {
                return Err(invalid("provenance requires a secure origin"));
            }
            Ok(value)
        })
        .transpose()?;
    Ok(SimpleFile {
        filename,
        locator: file_locator,
        kind,
        version,
        hashes,
        requires_python: raw.requires_python,
        yanked: raw.yanked.unwrap_or(Yanked::No),
        core_metadata,
        size: raw.size,
        upload_time: raw.upload_time,
        provenance,
    })
}
pub(super) fn normalize_hashes(raw: BTreeMap<String, String>) -> SimpleResult<AdvertisedHashes> {
    let mut values = BTreeMap::new();
    for (algorithm, digest) in raw {
        if algorithm.is_empty()
            || !algorithm
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-'))
            || digest.is_empty()
            || digest.len() % 2 != 0
            || !digest.bytes().all(|value| value.is_ascii_hexdigit())
        {
            return Err(invalid("invalid hash name or hexadecimal digest"));
        }
        let algorithm = algorithm.to_ascii_lowercase();
        let digest = digest.to_ascii_lowercase();
        if (algorithm == "sha256" && digest.len() != 64)
            || (algorithm == "sha512" && digest.len() != 128)
        {
            return Err(invalid("invalid supported hash length"));
        }
        if values.get(&algorithm).is_some_and(|old| old != &digest) {
            return Err(invalid("conflicting normalized hash names"));
        }
        values.insert(algorithm, digest);
    }
    let preferred = ["sha256", "sha512"]
        .into_iter()
        .find_map(|algorithm| {
            values
                .get(algorithm)
                .map(|value| format!("{algorithm}-{value}"))
        })
        .map(|value| {
            value
                .parse()
                .map_err(|_| invalid("invalid supported digest"))
        })
        .transpose()?;
    Ok(AdvertisedHashes { values, preferred })
}
pub(super) fn locator(value: &str, document: &Url) -> SimpleResult<FileLocator> {
    if value
        .chars()
        .any(|value| value.is_control() || value.is_whitespace())
    {
        return Err(invalid("invalid artifact URL"));
    }
    let authority = if let Some(rest) = value.strip_prefix("//") {
        Some(rest)
    } else {
        value.split_once(':').map(|(_, rest)| rest)
    };
    if authority.is_some_and(|rest| {
        rest.trim_start_matches(['/', '\\'])
            .split(['/', '\\', '?', '#'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
    }) {
        return Err(invalid("credentials must use a reference"));
    }
    let url = document
        .join(value)
        .map_err(|_| invalid("invalid artifact URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid("expected a credential-free HTTP(S) locator"));
    }
    Ok(FileLocator { url })
}
fn relation(
    value: &str,
    context: &SimpleParseContext<'_>,
    base: &Url,
) -> SimpleResult<FileLocator> {
    let value = locator(value, base)?;
    if value.url.fragment().is_some() || !value.url.path().ends_with('/') {
        return Err(invalid("invalid repository relation"));
    }
    let name = value
        .url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let name = NormalizedPythonName::new(&decode_component(name)?)
        .map_err(|_| invalid("invalid repository relation project"))?;
    if &name != context.project {
        return Err(invalid("repository relation belongs to another project"));
    }
    Ok(value)
}
fn decode_component(value: &str) -> SimpleResult<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let source = value.as_bytes();
    let mut position = 0;
    while position < source.len() {
        if source[position] == b'%' {
            let digits = source
                .get(position + 1..position + 3)
                .ok_or_else(|| invalid("invalid URL percent encoding"))?;
            let digits =
                std::str::from_utf8(digits).map_err(|_| invalid("invalid URL percent encoding"))?;
            bytes.push(
                u8::from_str_radix(digits, 16)
                    .map_err(|_| invalid("invalid URL percent encoding"))?,
            );
            position += 3;
        } else {
            bytes.push(source[position]);
            position += 1;
        }
    }
    String::from_utf8(bytes).map_err(|_| invalid("URL filename is not UTF-8"))
}
fn distribution(
    filename: &str,
    project: &NormalizedPythonName,
) -> SimpleResult<(PythonDistributionKind, Option<String>)> {
    if filename.ends_with(".whl") {
        let wheel = crate::parse_wheel_filename(filename)
            .ok_or_else(|| invalid("invalid wheel filename"))?;
        let name = NormalizedPythonName::new(&wheel.name)
            .map_err(|_| invalid("invalid wheel distribution name"))?;
        if &name != project {
            return Err(invalid("wheel belongs to another project"));
        }
        let version = wheel.version.clone();
        return Ok((PythonDistributionKind::Wheel(wheel), Some(version)));
    }
    if let Some(stem) = filename
        .strip_suffix(".tar.gz")
        .or_else(|| filename.strip_suffix(".zip"))
    {
        // A known project's historical unescaped name can contain hyphens
        // (for example antlr4-python3-runtime). The last separator is accepted
        // only when its complete left side normalizes to this exact project
        // and its right side is already a canonical complete version.
        if let Some((name, version)) = stem.rsplit_once('-')
            && let Ok(parsed) = Version::from_str(version)
            && parsed.to_string() == version
            && NormalizedPythonName::new(name).is_ok_and(|name| &name == project)
        {
            return Ok((PythonDistributionKind::Sdist, Some(version.to_owned())));
        }
        // PEP 625's escaped name and normalized version contain exactly one
        // separator. Other historical spellings require their own metadata
        // adapter; guessing every hyphen can both misidentify and cost O(n²).
        if let Some((name, version)) = stem.split_once('-')
            && !version.contains('-')
            && let Ok(parsed) = Version::from_str(version)
            && parsed.to_string() == version
        {
            let name = NormalizedPythonName::new(name)
                .map_err(|_| invalid("invalid source distribution name"))?;
            if &name != project {
                return Err(invalid("source distribution belongs to another project"));
            }
            return Ok((PythonDistributionKind::Sdist, Some(version.to_owned())));
        }
        return Ok((PythonDistributionKind::HistoricalFormat, None));
    }
    if [".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".tar"]
        .iter()
        .any(|suffix| filename.ends_with(suffix))
    {
        return Ok((PythonDistributionKind::HistoricalFormat, None));
    }
    Ok((
        if filename.ends_with(".egg") {
            PythonDistributionKind::HistoricalFormat
        } else {
            PythonDistributionKind::Other
        },
        None,
    ))
}
fn secure_origin(url: &Url) -> bool {
    url.scheme() == "https"
        || url.scheme() == "http"
            && url.host_str().is_some_and(|host| {
                let host = host.trim_end_matches('.');
                host == "localhost"
                    || host.ends_with(".localhost")
                    || host
                        .trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            })
}
fn valid_upload_time(value: &str) -> bool {
    let b = value.as_bytes();
    if !value.is_ascii()
        || b.len() < 20
        || b.last() != Some(&b'Z')
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return false;
    }
    if b.len() != 20
        && (b.len() < 22
            || b.len() > 27
            || b[19] != b'.'
            || !b[20..b.len() - 1].iter().all(u8::is_ascii_digit))
    {
        return false;
    }
    let number = |start: usize, end: usize| {
        value[start..end]
            .bytes()
            .all(|value| value.is_ascii_digit())
            .then(|| value[start..end].parse::<u32>().ok())
            .flatten()
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    ) else {
        return false;
    };
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || year % 4 == 0 && year % 100 != 0 => 29,
        2 => 28,
        _ => return false,
    };
    day > 0 && day <= days && hour < 24 && minute < 60 && second <= 60
}
