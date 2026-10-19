//! Wheel archive inspection from explicit inputs. No extraction or host reads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Read, Seek};

use eyre::{Result, ensure, eyre};
use pakro_index::{
    SupportedWheelTags, WheelBuildTag, WheelTag, normalize_py_name, parse_wheel_filename,
};
use pakro_resolver::solve::{Pep440, VersionAdapter};
use sha2::Digest as _;

mod member_session;
mod metadata;

pub(crate) use member_session::WheelDirectoryReceipt;
mod record;
mod usage;
mod zip_directory;

pub(crate) struct WheelExpectation<'a> {
    pub filename: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub supported: &'a SupportedWheelTags,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WheelLimits {
    pub max_archive_bytes: u64,
    pub max_directory_bytes: u64,
    pub max_members: usize,
    pub max_member_bytes: u64,
    pub max_total_uncompressed: u64,
    pub max_metadata_bytes: usize,
    pub max_record_rows: usize,
    pub max_record_field_bytes: usize,
    pub max_header_fields: usize,
    pub max_tag_triples: usize,
}

impl Default for WheelLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 16 * 1024 * 1024 * 1024,
            max_directory_bytes: 64 * 1024 * 1024,
            max_members: 65_536,
            max_member_bytes: 8 * 1024 * 1024 * 1024,
            max_total_uncompressed: 32 * 1024 * 1024 * 1024,
            max_metadata_bytes: 16 * 1024 * 1024,
            max_record_rows: 65_536,
            max_record_field_bytes: 128 * 1024,
            max_header_fields: 16_384,
            max_tag_triples: 65_536,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum WheelScheme {
    Purelib,
    Platlib,
    Headers,
    Scripts,
    Data,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WheelMemberRole {
    Payload,
    Record,
    LegacySignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordHashAlgorithm {
    Sha256,
    Sha384,
    Sha512,
    Sha3_256,
    Sha3_384,
    Sha3_512,
    Blake2b,
    Blake2s,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WheelRecordHash {
    algorithm: RecordHashAlgorithm,
    digest: Vec<u8>,
}

impl WheelRecordHash {
    pub(crate) fn algorithm(&self) -> RecordHashAlgorithm {
        self.algorithm
    }
    pub(crate) fn digest(&self) -> &[u8] {
        &self.digest
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WheelMember {
    archive_index: usize,
    archive_path: String,
    scheme: WheelScheme,
    relative_path: String,
    size: u64,
    unix_mode: Option<u32>,
    role: WheelMemberRole,
    record_hash: Option<WheelRecordHash>,
    observed_sha256: [u8; 32],
}

impl WheelMember {
    pub(crate) fn archive_index(&self) -> usize {
        self.archive_index
    }
    pub(crate) fn archive_path(&self) -> &str {
        &self.archive_path
    }
    pub(crate) fn scheme(&self) -> WheelScheme {
        self.scheme
    }
    pub(crate) fn relative_path(&self) -> &str {
        &self.relative_path
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn unix_mode(&self) -> Option<u32> {
        self.unix_mode
    }
    pub(crate) fn role(&self) -> WheelMemberRole {
        self.role
    }
    pub(crate) fn record_hash(&self) -> Option<&WheelRecordHash> {
        self.record_hash.as_ref()
    }
    pub(crate) fn observed_sha256(&self) -> &[u8; 32] {
        &self.observed_sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WheelWarning {
    NewerWheelMinor { major: u32, minor: u32 },
    NewerMetadataMinor { major: u32, minor: u32 },
    LegacySignature { path: String },
}

#[derive(Clone)]
pub(crate) struct WheelCoreMetadata {
    metadata_version: (u32, u32),
    fields: BTreeMap<String, Vec<String>>,
    description: String,
    raw: Vec<u8>,
}

impl WheelCoreMetadata {
    pub(crate) fn metadata_version(&self) -> (u32, u32) {
        self.metadata_version
    }
    pub(crate) fn values(&self, field: &str) -> &[String] {
        self.fields
            .get(&field.to_ascii_lowercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    pub(crate) fn description(&self) -> &str {
        &self.description
    }
    pub(crate) fn raw(&self) -> &[u8] {
        &self.raw
    }
}

impl fmt::Debug for WheelCoreMetadata {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("WheelCoreMetadata")
            .field("metadata_version", &self.metadata_version)
            .field("field_count", &self.fields.len())
            .field("description_bytes", &self.description.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WheelManifest {
    name: String,
    version: String,
    dist_info: String,
    root_scheme: WheelScheme,
    tags: Vec<WheelTag>,
    build: Option<WheelBuildTag>,
    core_metadata: WheelCoreMetadata,
    members: Vec<WheelMember>,
    warnings: Vec<WheelWarning>,
}

impl WheelManifest {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
    pub(crate) fn dist_info(&self) -> &str {
        &self.dist_info
    }
    pub(crate) fn root_scheme(&self) -> WheelScheme {
        self.root_scheme
    }
    pub(crate) fn tags(&self) -> &[WheelTag] {
        &self.tags
    }
    pub(crate) fn build(&self) -> Option<&WheelBuildTag> {
        self.build.as_ref()
    }
    pub(crate) fn core_metadata(&self) -> &WheelCoreMetadata {
        &self.core_metadata
    }
    pub(crate) fn members(&self) -> &[WheelMember] {
        &self.members
    }
    pub(crate) fn warnings(&self) -> &[WheelWarning] {
        &self.warnings
    }
}

pub(super) fn validate_limits(limits: WheelLimits) -> Result<()> {
    ensure!(
        [
            limits.max_archive_bytes,
            limits.max_directory_bytes,
            limits.max_member_bytes,
            limits.max_total_uncompressed
        ]
        .into_iter()
        .all(|value| value != 0)
            && [
                limits.max_members,
                limits.max_metadata_bytes,
                limits.max_record_rows,
                limits.max_record_field_bytes,
                limits.max_header_fields,
                limits.max_tag_triples
            ]
            .into_iter()
            .all(|value| value != 0),
        "wheel limits must all be nonzero"
    );
    Ok(())
}

fn filename_tags(
    wheel: &pakro_index::WheelFile,
    limits: WheelLimits,
    usage: &mut usage::InspectionUsage,
) -> Result<Vec<WheelTag>> {
    let count = wheel
        .tags
        .python
        .len()
        .checked_mul(wheel.tags.abi.len())
        .and_then(|count| count.checked_mul(wheel.tags.platform.len()))
        .ok_or_else(|| eyre!("wheel Tag product limit overflow"))?;
    ensure!(
        count <= limits.max_tag_triples,
        "wheel Tag product limit exceeded"
    );
    usage.tag_triples = usage.tag_triples.max(count);
    let mut tags = BTreeSet::new();
    for python in &wheel.tags.python {
        for abi in &wheel.tags.abi {
            for platform in &wheel.tags.platform {
                tags.insert(
                    WheelTag::new(python, abi, platform)
                        .map_err(|_| eyre!("wheel expectation contains invalid tags"))?,
                );
            }
        }
    }
    Ok(tags.into_iter().collect())
}

fn read_metadata<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: usize,
    limits: WheelLimits,
) -> Result<Vec<u8>> {
    let mut member = archive
        .by_index(index)
        .map_err(|_| eyre!("ZIP metadata member cannot be read"))?;
    ensure!(
        member.size() <= limits.max_metadata_bytes as u64,
        "wheel metadata byte limit exceeded"
    );
    let mut bytes = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = member
            .read(&mut buffer)
            .map_err(|_| eyre!("ZIP metadata read or CRC failed"))?;
        if count == 0 {
            break;
        }
        ensure!(
            count <= limits.max_metadata_bytes.saturating_sub(bytes.len()),
            "wheel metadata byte limit exceeded"
        );
        bytes.extend_from_slice(&buffer[..count]);
    }
    ensure!(
        bytes.len() as u64 == member.size(),
        "ZIP metadata size disagrees with actual bytes"
    );
    Ok(bytes)
}

struct RecordEntry {
    hash: Option<WheelRecordHash>,
    size: Option<u64>,
}

fn logical_destination(
    path: &str,
    data: Option<&str>,
    root: WheelScheme,
) -> Result<(WheelScheme, String)> {
    let Some(data) = data else {
        return Ok((root, path.to_owned()));
    };
    ensure!(path != data, "wheel .data scheme container is a file");
    let Some(tail) = path
        .strip_prefix(data)
        .and_then(|tail| tail.strip_prefix('/'))
    else {
        return Ok((root, path.to_owned()));
    };
    let (scheme, relative) = tail
        .split_once('/')
        .ok_or_else(|| eyre!("wheel .data scheme has no payload path"))?;
    ensure!(
        !relative.is_empty(),
        "wheel .data scheme has empty payload path"
    );
    let scheme = match scheme {
        "purelib" => WheelScheme::Purelib,
        "platlib" => WheelScheme::Platlib,
        "headers" => WheelScheme::Headers,
        "scripts" => {
            ensure!(
                !relative.contains('/'),
                "wheel scripts scheme forbids nested members"
            );
            WheelScheme::Scripts
        }
        "data" => WheelScheme::Data,
        _ => return Err(eyre!("wheel .data scheme is unsupported")),
    };
    Ok((scheme, relative.to_owned()))
}

/// Inspect an explicitly supplied reader without returning its capability.
pub(crate) fn inspect_wheel<R: Read + Seek>(
    input: R,
    expected: &WheelExpectation<'_>,
    limits: WheelLimits,
) -> Result<WheelManifest> {
    inspect_with_receipt(input, expected, limits).map(|(manifest, _)| manifest)
}

/// Retain the exact reader and already-checked bounded directory for repeated
/// member sessions. Source authentication remains the caller's responsibility.
pub(crate) fn inspect_with_receipt<R: Read + Seek>(
    mut input: R,
    expected: &WheelExpectation<'_>,
    limits: WheelLimits,
) -> Result<(WheelManifest, WheelDirectoryReceipt<R>)> {
    validate_limits(limits)?;
    ensure!(
        metadata::valid_name(expected.name) && Pep440::parse_version(expected.version).is_some(),
        "wheel expectation has invalid distribution identity"
    );
    let wheel = parse_wheel_filename(expected.filename)
        .ok_or_else(|| eyre!("wheel expectation has invalid filename"))?;
    let name = normalize_py_name(expected.name);
    ensure!(
        normalize_py_name(&wheel.name) == name
            && metadata::equal_version(&wheel.version, expected.version),
        "wheel expectation disagrees with filename identity"
    );
    let mut usage = usage::InspectionUsage::default();
    let tags = filename_tags(&wheel, limits, &mut usage)?;
    ensure!(
        expected
            .supported
            .best_rank(&wheel.tags)
            .map_err(|_| eyre!("wheel expectation has invalid supported tags"))?
            .is_some(),
        "wheel filename has no compatible tag"
    );
    let directory = zip_directory::preflight(&mut input, limits)?;
    let tag_triples = usage.tag_triples;
    usage = directory.usage;
    usage.tag_triples = tag_triples;
    let view = zip_directory::ValidatedZipView::new(input, directory.eocd_end)
        .map_err(|_| eyre!("ZIP validated reader cannot be opened"))?;
    let mut archive = zip::ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        view,
    )
    .map_err(|_| eyre!("ZIP archive metadata cannot be decoded"))?;
    ensure!(
        archive.len() == directory.entries.len(),
        "ZIP directory count changed during decode"
    );
    let mut modes = Vec::with_capacity(directory.entries.len());
    for (index, entry) in directory.entries.iter().enumerate() {
        let member = archive
            .by_index(index)
            .map_err(|_| eyre!("ZIP member cannot be decoded"))?;
        ensure!(
            member.name_raw() == entry.archive_path.as_bytes()
                && member.size() == entry.size
                && member.compressed_size() == entry.compressed_size
                && member.header_start() == entry.header_start
                && member.central_header_start() == entry.central_start
                && member.data_start() == Some(entry.data_start),
            "ZIP bounded directory disagrees with decoded member"
        );
        let mode = member.unix_mode();
        ensure!(
            !member.is_symlink()
                && mode.is_none_or(|mode| {
                    let kind = mode & 0o170000;
                    kind == 0
                        || if entry.is_dir {
                            kind == 0o040000
                        } else {
                            kind == 0o100000
                        }
                }),
            "ZIP member type is not supported"
        );
        modes.push(mode);
    }
    let mut info_dirs = BTreeSet::new();
    let mut data_dirs = BTreeSet::new();
    let mut indices = BTreeMap::new();
    let mut files = BTreeSet::new();
    for (index, entry) in directory.entries.iter().enumerate() {
        let root = entry.canonical_path.split('/').next().unwrap();
        if root.ends_with(".dist-info") {
            info_dirs.insert(root);
        }
        if root.ends_with(".data") {
            data_dirs.insert(root);
        }
        indices.insert(entry.canonical_path.as_str(), index);
        if !entry.is_dir {
            files.insert(entry.canonical_path.as_str());
        }
    }
    ensure!(
        info_dirs.len() == 1,
        "wheel missing or multiple dist-info containers"
    );
    let info = *info_dirs.first().unwrap();
    ensure!(
        metadata::matches_stem(
            info.strip_suffix(".dist-info").unwrap(),
            &name,
            expected.version
        ),
        "wheel dist-info identity disagrees with expected wheel"
    );
    ensure!(
        data_dirs.len() <= 1,
        "wheel .data identity requires one container"
    );
    let data = data_dirs.first().copied();
    if let Some(data) = data {
        ensure!(
            metadata::matches_stem(data.strip_suffix(".data").unwrap(), &name, expected.version),
            "wheel .data identity disagrees with expected wheel"
        );
    }
    let required_index = |suffix: &str| -> Result<usize> {
        let path = format!("{info}/{suffix}");
        ensure!(
            files.contains(path.as_str()),
            "wheel missing required {suffix}"
        );
        Ok(*indices.get(path.as_str()).unwrap())
    };
    let wheel_index = required_index("WHEEL")?;
    let metadata_index = required_index("METADATA")?;
    let record_index = required_index("RECORD")?;
    let wheel_bytes = read_metadata(&mut archive, wheel_index, limits)?;
    let metadata_bytes = read_metadata(&mut archive, metadata_index, limits)?;
    let record_bytes = read_metadata(&mut archive, record_index, limits)?;
    usage.metadata_bytes = wheel_bytes
        .len()
        .max(metadata_bytes.len())
        .max(record_bytes.len());
    let metadata_checksums: [(usize, [u8; 32]); 3] = [
        (wheel_index, sha2::Sha256::digest(&wheel_bytes).into()),
        (metadata_index, sha2::Sha256::digest(&metadata_bytes).into()),
        (record_index, sha2::Sha256::digest(&record_bytes).into()),
    ];
    let wheel_headers = metadata::parse_wheel(
        &wheel_bytes,
        &tags,
        wheel.build.as_ref(),
        limits,
        &mut usage,
    )?;
    let (core_metadata, version, mut metadata_warnings) =
        metadata::parse_core(metadata_bytes, &name, expected.version, limits, &mut usage)?;
    metadata::validate_licenses(&core_metadata, info, &files)?;
    let mut warnings = wheel_headers.warnings;
    warnings.append(&mut metadata_warnings);
    let record_path = format!("{info}/RECORD");
    let signature_paths = [format!("{info}/RECORD.jws"), format!("{info}/RECORD.p7s")];
    let mut records = BTreeMap::new();
    for [path, hash, size] in record::parse_csv(&record_bytes, limits, &mut usage)? {
        let path = zip_directory::normalize_path(&path, "RECORD path")?;
        ensure!(!records.contains_key(&path), "duplicate RECORD member path");
        ensure!(
            files.contains(path.as_str()),
            "RECORD member does not identify an archive file"
        );
        ensure!(
            !signature_paths.contains(&path),
            "RECORD signature sidecars must not have rows"
        );
        let size = record::parse_size(&size)?;
        let hash = if path == record_path {
            ensure!(hash.is_empty(), "RECORD self hash is forbidden");
            None
        } else {
            Some(record::parse_hash(&hash)?)
        };
        records.insert(path, RecordEntry { hash, size });
    }
    for entry in &directory.entries {
        if !entry.is_dir && !signature_paths.contains(&entry.canonical_path) {
            ensure!(
                records.contains_key(&entry.canonical_path),
                "RECORD member is missing"
            );
        }
    }
    let mut members = Vec::new();
    let mut destinations = BTreeSet::new();
    let mut total = 0_u64;
    let mut buffer = [0; 64 * 1024];
    for (index, entry) in directory.entries.iter().enumerate() {
        let recorded = records.get(&entry.canonical_path);
        let mut hasher = recorded
            .and_then(|row| row.hash.as_ref())
            .map(|hash| record::MemberHasher::new(hash.algorithm));
        let mut observed = sha2::Sha256::new();
        let mut actual_size = 0_u64;
        let mut member = archive
            .by_index(index)
            .map_err(|_| eyre!("ZIP member cannot be read"))?;
        loop {
            let count = member
                .read(&mut buffer)
                .map_err(|_| eyre!("ZIP member read or CRC failed"))?;
            if count == 0 {
                break;
            }
            actual_size = actual_size
                .checked_add(count as u64)
                .ok_or_else(|| eyre!("wheel member byte limit overflow"))?;
            total = total
                .checked_add(count as u64)
                .ok_or_else(|| eyre!("wheel expanded byte limit overflow"))?;
            ensure!(
                actual_size <= limits.max_member_bytes && total <= limits.max_total_uncompressed,
                "wheel expanded byte limit exceeded"
            );
            observed.update(&buffer[..count]);
            if let Some(hasher) = &mut hasher {
                hasher.update(&buffer[..count]);
            }
        }
        ensure!(
            actual_size == entry.size,
            "ZIP member size disagrees with actual bytes"
        );
        usage.member_bytes = usage.member_bytes.max(actual_size);
        usage.total_uncompressed = usage.total_uncompressed.max(total);
        let observed_sha256: [u8; 32] = observed.finalize().into();
        for (metadata_index, checksum) in &metadata_checksums {
            ensure!(
                *metadata_index != index || checksum == &observed_sha256,
                "ZIP metadata bytes changed between inspection reads"
            );
        }
        if let Some(recorded) = recorded {
            ensure!(
                recorded.size.is_none_or(|size| size == actual_size),
                "RECORD size disagrees with actual bytes"
            );
            if let Some(hasher) = hasher {
                ensure!(
                    hasher.finish() == recorded.hash.as_ref().unwrap().digest(),
                    "RECORD hash disagrees with actual bytes"
                );
            }
        }
        if entry.is_dir {
            continue;
        }
        let (scheme, relative_path) =
            logical_destination(&entry.canonical_path, data, wheel_headers.root_scheme)?;
        ensure!(
            destinations.insert((scheme, relative_path.clone())),
            "wheel destination collision within scheme"
        );
        let role = if entry.canonical_path == record_path {
            WheelMemberRole::Record
        } else if signature_paths.contains(&entry.canonical_path) {
            warnings.push(WheelWarning::LegacySignature {
                path: entry.archive_path.clone(),
            });
            WheelMemberRole::LegacySignature
        } else {
            WheelMemberRole::Payload
        };
        members.push(WheelMember {
            archive_index: index,
            archive_path: entry.archive_path.clone(),
            scheme,
            relative_path,
            size: actual_size,
            unix_mode: modes[index],
            role,
            record_hash: recorded.and_then(|row| row.hash.clone()),
            observed_sha256,
        });
    }
    let manifest = WheelManifest {
        name,
        version,
        dist_info: info.to_owned(),
        root_scheme: wheel_headers.root_scheme,
        tags: wheel_headers.tags,
        build: wheel_headers.build,
        core_metadata,
        members,
        warnings,
    };
    Ok((manifest, WheelDirectoryReceipt::new(archive, limits, usage)))
}

#[cfg(test)]
mod tests;
