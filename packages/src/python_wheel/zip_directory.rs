//! Bounded raw ZIP directory checks before Zip8 allocates its metadata.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};

use eyre::{Result, bail, ensure, eyre};

use super::WheelLimits;
use super::usage::InspectionUsage;

pub(super) struct CentralMember {
    pub archive_path: String,
    pub canonical_path: String,
    pub is_dir: bool,
    pub size: u64,
    pub compressed_size: u64,
    pub header_start: u64,
    pub central_start: u64,
    pub data_start: u64,
}

pub(super) struct DirectoryInventory {
    pub entries: Vec<CentralMember>,
    pub eocd_end: u64,
    pub usage: InspectionUsage,
}

pub(super) fn normalize_path(path: &str, label: &str) -> Result<String> {
    ensure!(
        !path.is_empty()
            && !path.contains(['\0', '\\'])
            && !path.starts_with('/')
            && !(path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic()),
        "{label}: unsafe path"
    );
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                ensure!(parts.pop().is_some(), "{label}: path escapes archive root");
            }
            _ => parts.push(part),
        }
    }
    ensure!(!parts.is_empty(), "{label}: empty path");
    Ok(parts.join("/"))
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
fn add(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right)
        .ok_or_else(|| eyre!("ZIP offset overflow"))
}
fn read_at<R: Read + Seek>(input: &mut R, at: u64, bytes: &mut [u8]) -> Result<()> {
    input
        .seek(SeekFrom::Start(at))
        .map_err(|_| eyre!("ZIP seek failed"))?;
    input
        .read_exact(bytes)
        .map_err(|_| eyre!("ZIP truncated header"))
}

/// Return only values whose legacy fields are sentinels, in APPNOTE order.
fn zip64_values(extra: &[u8], required: &[usize]) -> Result<Vec<u64>> {
    let mut at = 0;
    let mut found = None;
    while at < extra.len() {
        ensure!(extra.len() - at >= 4, "ZIP truncated extra TLV");
        let tag = u16_at(extra, at);
        let size = usize::from(u16_at(extra, at + 2));
        at += 4;
        ensure!(size <= extra.len() - at, "ZIP truncated extra TLV");
        if tag == 1 {
            ensure!(found.is_none(), "ZIP duplicate ZIP64 extra TLV");
            found = Some(&extra[at..at + size]);
        }
        at += size;
    }
    if required.is_empty() {
        return Ok(Vec::new());
    }
    let value = found.ok_or_else(|| eyre!("ZIP missing required ZIP64 extra TLV"))?;
    let mut at = 0;
    let mut values = Vec::with_capacity(required.len());
    for &width in required {
        ensure!(
            width <= value.len().saturating_sub(at),
            "ZIP truncated ZIP64 value"
        );
        values.push(if width == 8 {
            u64_at(value, at)
        } else {
            u64::from(u32_at(value, at))
        });
        at += width;
    }
    Ok(values)
}

fn descriptor_len(bytes: &[u8], crc: u32, compressed: u64, size: u64, wide: bool) -> Result<u64> {
    for skip in [0, 4] {
        if skip == 4 && (bytes.len() < 4 || u32_at(bytes, 0) != 0x0807_4b50) {
            continue;
        }
        let width = if wide { 8 } else { 4 };
        let length = skip + 4 + width * 2;
        if bytes.len() < length || u32_at(bytes, skip) != crc {
            continue;
        }
        let (actual_compressed, actual_size) = if wide {
            (u64_at(bytes, skip + 4), u64_at(bytes, skip + 12))
        } else {
            (
                u64::from(u32_at(bytes, skip + 4)),
                u64::from(u32_at(bytes, skip + 8)),
            )
        };
        if (actual_compressed, actual_size) == (compressed, size) {
            return Ok(length as u64);
        }
    }
    bail!("ZIP invalid data descriptor")
}

#[derive(Debug)]
struct InvalidCandidate(&'static str);
impl std::fmt::Display for InvalidCandidate {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(output, "ZIP {}", self.0)
    }
}
impl std::error::Error for InvalidCandidate {}

fn candidate_condition(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(eyre!(InvalidCandidate(message)))
    }
}

struct DirectoryBudget {
    remaining: u64,
    usage: InspectionUsage,
}
fn read_candidate_at<R: Read + Seek>(
    input: &mut R,
    budget: &mut DirectoryBudget,
    at: u64,
    bytes: &mut [u8],
) -> Result<()> {
    budget.remaining = budget
        .remaining
        .checked_sub(bytes.len() as u64)
        .ok_or_else(|| eyre!("ZIP directory candidate work byte limit exceeded"))?;
    budget.usage.candidate_work_bytes = budget
        .usage
        .candidate_work_bytes
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| eyre!("ZIP directory candidate work byte limit overflow"))?;
    read_at(input, at, bytes)
}

pub(super) fn preflight<R: Read + Seek>(
    input: &mut R,
    limits: WheelLimits,
) -> Result<DirectoryInventory> {
    let length = input
        .seek(SeekFrom::End(0))
        .map_err(|_| eyre!("ZIP seek failed"))?;
    ensure!(
        length <= limits.max_archive_bytes,
        "wheel archive byte limit exceeded"
    );
    ensure!(length >= 22, "ZIP missing end directory");
    let tail_size = usize::try_from(length.min(65_557)).unwrap();
    let tail_start = length - tail_size as u64;
    let mut tail = vec![0; tail_size];
    read_at(input, tail_start, &mut tail)?;
    let mut budget = DirectoryBudget {
        remaining: limits
            .max_directory_bytes
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(65_557))
            .ok_or_else(|| eyre!("ZIP directory candidate work byte limit overflow"))?,
        usage: InspectionUsage {
            archive_bytes: length,
            ..InspectionUsage::default()
        },
    };
    let mut selected = None;
    for at in (0..=tail.len() - 22).rev().filter(|&at| {
        &tail[at..at + 4] == b"PK\x05\x06"
            && at + 22 + usize::from(u16_at(&tail, at + 20)) == tail.len()
    }) {
        match preflight_candidate(
            input,
            limits,
            tail_start + at as u64,
            &tail[at..at + 22],
            &mut budget,
        ) {
            Ok(candidate) => {
                ensure!(selected.is_none(), "ZIP ambiguous end directory records");
                selected = Some(candidate);
            }
            // Only a structurally impossible footer/directory envelope is a
            // decoy. Policy, resource-limit and I/O failures remain fatal.
            Err(error) if error.downcast_ref::<InvalidCandidate>().is_some() => {}
            Err(error) => return Err(error),
        }
    }
    let mut selected =
        selected.ok_or_else(|| eyre!("ZIP no structurally consistent end directory"))?;
    // Include every candidate which reached a policy/resource check, even if
    // a later structural check discarded it. A tighter replay budget must not
    // turn such an originally accepted candidate into an unrecorded fatal one.
    selected.usage = budget.usage;
    Ok(selected)
}

struct WideFooter {
    position: u64,
    record_size: u64,
    disk: u32,
    directory_disk: u32,
    count_on_disk: u64,
    count: u64,
    directory_size: u64,
    directory_start: u64,
    locator_disk: u32,
    disks: u32,
}

// A locator magic alone is not a record. Complete fixed-header bounds and the
// ZIP64 record's end must be coherent before its unsupported/policy fields can
// influence candidate selection. No extensible sector is allocated here.
fn wide_footer<R: Read + Seek>(
    input: &mut R,
    budget: &mut DirectoryBudget,
    eocd_start: u64,
) -> Result<Option<WideFooter>> {
    let Some(locator_start) = eocd_start.checked_sub(20) else {
        return Ok(None);
    };
    let mut locator = [0; 20];
    read_candidate_at(input, budget, locator_start, &mut locator)?;
    if &locator[..4] != b"PK\x06\x07" {
        return Ok(None);
    }
    let position = u64_at(&locator, 8);
    if !position
        .checked_add(56)
        .is_some_and(|end| end <= locator_start)
    {
        return Ok(None);
    }
    let mut wide = [0; 56];
    read_candidate_at(input, budget, position, &mut wide)?;
    let record_size = u64_at(&wide, 4);
    if &wide[..4] != b"PK\x06\x06"
        || record_size < 44
        || position
            .checked_add(12)
            .and_then(|start| start.checked_add(record_size))
            != Some(locator_start)
    {
        return Ok(None);
    }
    Ok(Some(WideFooter {
        position,
        record_size,
        disk: u32_at(&wide, 16),
        directory_disk: u32_at(&wide, 20),
        count_on_disk: u64_at(&wide, 24),
        count: u64_at(&wide, 32),
        directory_size: u64_at(&wide, 40),
        directory_start: u64_at(&wide, 48),
        locator_disk: u32_at(&locator, 4),
        disks: u32_at(&locator, 16),
    }))
}

fn preflight_candidate<R: Read + Seek>(
    input: &mut R,
    limits: WheelLimits,
    eocd_start: u64,
    end: &[u8],
    budget: &mut DirectoryBudget,
) -> Result<DirectoryInventory> {
    let legacy_disk = u64::from(u16_at(end, 4));
    let legacy_directory_disk = u64::from(u16_at(end, 6));
    let legacy_count_on_disk = u64::from(u16_at(end, 8));
    let legacy_count = u64::from(u16_at(end, 10));
    let legacy_size = u64::from(u32_at(end, 12));
    let legacy_start = u64::from(u32_at(end, 16));
    let short_sentinel = u64::from(u16::MAX);
    let long_sentinel = u64::from(u32::MAX);
    let need_zip64 = [
        legacy_disk,
        legacy_directory_disk,
        legacy_count_on_disk,
        legacy_count,
    ]
    .contains(&short_sentinel)
        || [legacy_size, legacy_start].contains(&long_sentinel);
    let wide = wide_footer(input, budget, eocd_start)?;
    candidate_condition(
        !need_zip64 || wide.is_some(),
        "missing structurally valid ZIP64 footer",
    )?;
    let (
        disk,
        directory_disk,
        count_on_disk,
        count,
        directory_size,
        directory_start,
        directory_end,
    ) = if let Some(wide) = &wide {
        for (legacy, sentinel, actual) in [
            (legacy_disk, short_sentinel, u64::from(wide.disk)),
            (
                legacy_directory_disk,
                short_sentinel,
                u64::from(wide.directory_disk),
            ),
            (legacy_count_on_disk, short_sentinel, wide.count_on_disk),
            (legacy_count, short_sentinel, wide.count),
            (legacy_size, long_sentinel, wide.directory_size),
            (legacy_start, long_sentinel, wide.directory_start),
        ] {
            candidate_condition(
                legacy == sentinel || legacy == actual,
                "inconsistent ZIP64 footer",
            )?;
        }
        (
            u64::from(wide.disk),
            u64::from(wide.directory_disk),
            wide.count_on_disk,
            wide.count,
            wide.directory_size,
            wide.directory_start,
            wide.position,
        )
    } else {
        (
            legacy_disk,
            legacy_directory_disk,
            legacy_count_on_disk,
            legacy_count,
            legacy_size,
            legacy_start,
            eocd_start,
        )
    };
    // All cheap envelope relationships precede policy. In particular a disk
    // flag, mismatched count or sentinel inside a comment must not make an
    // impossible footer authoritative. Resource failures of a viable envelope
    // remain fatal, before any count-based allocation.
    candidate_condition(
        directory_start.checked_add(directory_size) == Some(directory_end),
        "directory bounds disagree",
    )?;
    candidate_condition(
        count <= directory_size / 46,
        "directory count cannot fit its headers",
    )?;
    candidate_condition(
        count_on_disk <= count,
        "directory count on disk exceeds total",
    )?;
    ensure!(
        disk == 0 && directory_disk == 0 && count_on_disk == count,
        "ZIP multi-disk archive is unsupported"
    );
    if let Some(wide) = &wide {
        ensure!(
            wide.locator_disk == 0 && wide.disks == 1,
            "ZIP multi-disk ZIP64 archive"
        );
        ensure!(
            wide.record_size <= limits.max_directory_bytes,
            "ZIP64 footer byte limit exceeded"
        );
        budget.usage.directory_bytes = budget.usage.directory_bytes.max(wide.record_size);
    }
    ensure!(
        count <= limits.max_members as u64,
        "ZIP member count limit exceeded"
    );
    budget.usage.members = budget.usage.members.max(count);
    ensure!(
        directory_size <= limits.max_directory_bytes,
        "ZIP directory byte limit exceeded"
    );
    budget.usage.directory_bytes = budget.usage.directory_bytes.max(directory_size);
    let capacity = usize::try_from(count).map_err(|_| eyre!("ZIP member count limit exceeded"))?;
    let mut entries = Vec::with_capacity(capacity);
    let mut canonical_names = BTreeMap::new();
    let mut cursor = directory_start;
    let mut total_size = 0_u64;
    let mut spans = Vec::with_capacity(capacity);
    for _ in 0..count {
        candidate_condition(
            cursor
                .checked_add(46)
                .is_some_and(|end| end <= directory_end),
            "truncated central header",
        )?;
        let mut header = [0; 46];
        read_candidate_at(input, budget, cursor, &mut header)?;
        candidate_condition(&header[..4] == b"PK\x01\x02", "invalid central header")?;
        let flags = u16_at(&header, 8);
        let method = u16_at(&header, 10);
        ensure!(flags & !0x080e == 0, "ZIP encrypted or unsupported flags");
        ensure!(
            matches!(method, 0 | 8),
            "ZIP unsupported compression method"
        );
        let name_len = usize::from(u16_at(&header, 28));
        let extra_len = usize::from(u16_at(&header, 30));
        let comment_len = u64::from(u16_at(&header, 32));
        let next = add(
            cursor,
            46 + name_len as u64 + extra_len as u64 + comment_len,
        )?;
        candidate_condition(
            next <= directory_end,
            "central variable fields exceed directory",
        )?;
        let mut name = vec![0; name_len];
        read_candidate_at(input, budget, cursor + 46, &mut name)?;
        let archive_path = String::from_utf8(name).map_err(|_| eyre!("ZIP name is not UTF-8"))?;
        let canonical_path = normalize_path(&archive_path, "ZIP path")?;
        let is_dir = archive_path.ends_with('/');
        let mode = u32_at(&header, 38) >> 16;
        let kind = mode & 0o170000;
        ensure!(
            kind == 0 || (kind == 0o040000 && is_dir) || (kind == 0o100000 && !is_dir),
            "ZIP member type is not a regular file or directory"
        );
        ensure!(
            canonical_names
                .insert(canonical_path.clone(), is_dir)
                .is_none(),
            "ZIP duplicate member path"
        );
        let mut extra = vec![0; extra_len];
        read_candidate_at(input, budget, cursor + 46 + name_len as u64, &mut extra)?;
        let sentinel_size = u32_at(&header, 24) == u32::MAX;
        let sentinel_compressed = u32_at(&header, 20) == u32::MAX;
        let sentinel_offset = u32_at(&header, 42) == u32::MAX;
        let sentinel_disk = u16_at(&header, 34) == u16::MAX;
        let mut required = Vec::new();
        for (needed, width) in [
            (sentinel_size, 8),
            (sentinel_compressed, 8),
            (sentinel_offset, 8),
            (sentinel_disk, 4),
        ] {
            if needed {
                required.push(width);
            }
        }
        let values = zip64_values(&extra, &required)?;
        let mut values = values.into_iter();
        let size = if sentinel_size {
            values.next().unwrap()
        } else {
            u64::from(u32_at(&header, 24))
        };
        let compressed_size = if sentinel_compressed {
            values.next().unwrap()
        } else {
            u64::from(u32_at(&header, 20))
        };
        let header_start = if sentinel_offset {
            values.next().unwrap()
        } else {
            u64::from(u32_at(&header, 42))
        };
        let disk = if sentinel_disk {
            values.next().unwrap()
        } else {
            u64::from(u16_at(&header, 34))
        };
        ensure!(disk == 0, "ZIP multi-disk member");
        ensure!(
            size <= limits.max_member_bytes,
            "wheel member byte limit exceeded"
        );
        budget.usage.member_bytes = budget.usage.member_bytes.max(size);
        total_size = total_size
            .checked_add(size)
            .ok_or_else(|| eyre!("wheel expanded byte limit overflow"))?;
        ensure!(
            total_size <= limits.max_total_uncompressed,
            "wheel expanded byte limit exceeded"
        );
        budget.usage.total_uncompressed = budget.usage.total_uncompressed.max(total_size);
        ensure!(!is_dir || size == 0, "ZIP directory has payload bytes");
        ensure!(
            add(header_start, 30)? <= directory_start,
            "ZIP local header exceeds payload area"
        );
        let mut local = [0; 30];
        read_candidate_at(input, budget, header_start, &mut local)?;
        ensure!(&local[..4] == b"PK\x03\x04", "ZIP invalid local header");
        ensure!(
            u16_at(&local, 6) == flags && u16_at(&local, 8) == method,
            "ZIP local and central flags or method disagree"
        );
        let local_name_len = usize::from(u16_at(&local, 26));
        let local_extra_len = usize::from(u16_at(&local, 28));
        let data_start = add(
            header_start,
            30 + local_name_len as u64 + local_extra_len as u64,
        )?;
        ensure!(
            data_start <= directory_start,
            "ZIP local variable fields exceed payload area"
        );
        let mut local_name = vec![0; local_name_len];
        read_candidate_at(input, budget, header_start + 30, &mut local_name)?;
        ensure!(
            local_name == archive_path.as_bytes(),
            "ZIP name differs between local and central header"
        );
        let mut local_extra = vec![0; local_extra_len];
        read_at(
            input,
            header_start + 30 + local_name_len as u64,
            &mut local_extra,
        )?;
        let local_wide_size = u32_at(&local, 22) == u32::MAX;
        let local_wide_compressed = u32_at(&local, 18) == u32::MAX;
        let local_required: Vec<_> = [local_wide_size, local_wide_compressed]
            .into_iter()
            .filter(|needed| *needed)
            .map(|_| 8)
            .collect();
        let local_values = zip64_values(&local_extra, &local_required)?;
        let mut local_values = local_values.into_iter();
        let local_size = if local_wide_size {
            local_values.next().unwrap()
        } else {
            u64::from(u32_at(&local, 22))
        };
        let local_compressed = if local_wide_compressed {
            local_values.next().unwrap()
        } else {
            u64::from(u32_at(&local, 18))
        };
        let crc = u32_at(&header, 16);
        if flags & 8 == 0 {
            ensure!(
                u32_at(&local, 14) == crc
                    && local_size == size
                    && local_compressed == compressed_size,
                "ZIP local and central CRC or size disagree"
            );
        } else {
            ensure!(
                (u32_at(&local, 14) == 0 || u32_at(&local, 14) == crc)
                    && (local_size == 0 || local_size == size)
                    && (local_compressed == 0 || local_compressed == compressed_size),
                "ZIP invalid descriptor local placeholders"
            );
        }
        let mut span_end = add(data_start, compressed_size)?;
        ensure!(
            span_end <= directory_start,
            "ZIP compressed data exceeds payload area"
        );
        if flags & 8 != 0 {
            let available = (directory_start - span_end).min(24) as usize;
            let mut descriptor = [0; 24];
            read_candidate_at(input, budget, span_end, &mut descriptor[..available])?;
            let wide =
                sentinel_size || sentinel_compressed || local_wide_size || local_wide_compressed;
            span_end = add(
                span_end,
                descriptor_len(&descriptor[..available], crc, compressed_size, size, wide)?,
            )?;
        }
        spans.push((header_start, span_end));
        entries.push(CentralMember {
            archive_path,
            canonical_path,
            is_dir,
            size,
            compressed_size,
            header_start,
            central_start: cursor,
            data_start,
        });
        cursor = next;
    }
    candidate_condition(cursor == directory_end, "directory count or size disagrees")?;
    for (path, is_dir) in &canonical_names {
        if !is_dir {
            // One ordered range lookup per file avoids scanning every ancestor
            // of attacker-controlled deeply nested member names.
            let prefix = format!("{path}/");
            ensure!(
                canonical_names
                    .range(prefix.clone()..)
                    .next()
                    .is_none_or(|(next, _)| !next.starts_with(&prefix)),
                "ZIP file-parent conflict"
            );
        }
    }
    spans.sort_unstable();
    if let Some(first) = spans.first() {
        ensure!(first.0 == 0, "ZIP executable prefix is unsupported");
    }
    ensure!(
        spans.windows(2).all(|pair| pair[0].1 <= pair[1].0),
        "ZIP member ranges overlap"
    );
    Ok(DirectoryInventory {
        entries,
        eocd_end: eocd_start + 22,
        usage: budget.usage,
    })
}

/// Zip8's footer finder accepts decoys in a legitimate ZIP comment. Hide the
/// already-validated comment and expose a zero comment length, without changing
/// any source bytes or buffering/reopening the payload. Known(0) disables SFX.
pub(super) struct ValidatedZipView<R> {
    input: R,
    end: u64,
    position: u64,
}
impl<R: Read + Seek> ValidatedZipView<R> {
    pub(super) fn new(mut input: R, end: u64) -> io::Result<Self> {
        input.seek(SeekFrom::Start(0))?;
        Ok(Self {
            input,
            end,
            position: 0,
        })
    }

    pub(super) fn into_input(self) -> R {
        self.input
    }
}
impl<R: Read + Seek> Read for ValidatedZipView<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let remaining = self.end.saturating_sub(self.position);
        let count = output
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let count = self.input.read(&mut output[..count])?;
        let patch_start = self.position.max(self.end - 2);
        let patch_end = (self.position + count as u64).min(self.end);
        if patch_start < patch_end {
            output[(patch_start - self.position) as usize..(patch_end - self.position) as usize]
                .fill(0);
        }
        self.position += count as u64;
        Ok(count)
    }
}
impl<R: Read + Seek> Seek for ValidatedZipView<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let destination = match from {
            SeekFrom::Start(at) => i128::from(at),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.end) + i128::from(offset),
        };
        if destination < 0 || destination > i128::from(self.end) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ZIP seek outside validated view",
            ));
        }
        self.position = self.input.seek(SeekFrom::Start(destination as u64))?;
        Ok(self.position)
    }
}
