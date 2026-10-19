use super::*;

use base64::Engine;
use sha2::Sha256;
use std::io::{Cursor, Write};

const INFO: &str = "demo-1.0.dist-info";
const FILE: &str = "demo-1.0-py3-none-any.whl";

#[derive(Clone)]
enum Kind {
    File,
    Directory,
    Symlink,
}

#[derive(Clone)]
struct Entry {
    path: String,
    bytes: Vec<u8>,
    kind: Kind,
}

fn file(path: impl Into<String>, bytes: impl AsRef<[u8]>) -> Entry {
    Entry {
        path: path.into(),
        bytes: bytes.as_ref().to_vec(),
        kind: Kind::File,
    }
}

fn base() -> Vec<Entry> {
    vec![
        file("demo/__init__.py", b"VALUE = 42\n"),
        file(
            format!("{INFO}/WHEEL"),
            b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
        ),
        file(
            format!("{INFO}/METADATA"),
            b"Metadata-Version: 2.6\nName: demo\nVersion: 1.0\n",
        ),
    ]
}

fn set(entries: &mut [Entry], suffix: &str, bytes: impl AsRef<[u8]>) {
    entries
        .iter_mut()
        .find(|entry| entry.path.ends_with(suffix))
        .unwrap()
        .bytes = bytes.as_ref().to_vec();
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn csv(rows: &[[String; 3]], newline: &str) -> Vec<u8> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|field| csv_field(field))
                .collect::<Vec<_>>()
                .join(",")
                + newline
        })
        .collect::<String>()
        .into_bytes()
}

fn rows(entries: &[Entry], info: &str) -> Vec<[String; 3]> {
    let mut rows: Vec<_> = entries
        .iter()
        .filter(|entry| matches!(entry.kind, Kind::File))
        .filter(|entry| {
            ![format!("{info}/RECORD.jws"), format!("{info}/RECORD.p7s")].contains(&entry.path)
        })
        .map(|entry| {
            [
                entry.path.clone(),
                format!(
                    "sha256={}",
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(Sha256::digest(&entry.bytes))
                ),
                entry.bytes.len().to_string(),
            ]
        })
        .collect();
    rows.push([format!("{info}/RECORD"), String::new(), String::new()]);
    rows
}

fn raw_zip(entries: Vec<Entry>) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for entry in entries {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        match entry.kind {
            Kind::Directory => writer.add_directory(&entry.path, options).unwrap(),
            Kind::Symlink => writer
                .add_symlink(
                    &entry.path,
                    std::str::from_utf8(&entry.bytes).unwrap(),
                    options,
                )
                .unwrap(),
            Kind::File => {
                writer.start_file(&entry.path, options).unwrap();
                writer.write_all(&entry.bytes).unwrap();
            }
        }
    }
    writer.finish().unwrap().into_inner()
}

fn with_record(info: &str, mut entries: Vec<Entry>, record: Vec<u8>) -> Vec<u8> {
    entries.push(file(format!("{info}/RECORD"), record));
    raw_zip(entries)
}

fn build(info: &str, entries: Vec<Entry>, change: impl FnOnce(&mut Vec<[String; 3]>)) -> Vec<u8> {
    let mut records = rows(&entries, info);
    change(&mut records);
    with_record(info, entries, csv(&records, "\n"))
}

fn inspect(
    bytes: Vec<u8>,
    filename: &str,
    name: &str,
    version: &str,
    limits: WheelLimits,
) -> Result<WheelManifest> {
    let tags = SupportedWheelTags::new(vec![
        WheelTag::new("cp310", "cp310", "linux_aarch64").unwrap(),
        WheelTag::new("py3", "none", "any").unwrap(),
    ])
    .unwrap();
    inspect_wheel(
        Cursor::new(bytes),
        &WheelExpectation {
            filename,
            name,
            version,
            supported: &tags,
        },
        limits,
    )
}

fn accepted(bytes: Vec<u8>) -> WheelManifest {
    inspect(bytes, FILE, "demo", "1.0", WheelLimits::default()).unwrap()
}
fn rejected(bytes: Vec<u8>, diagnostic: &str) {
    let error = inspect(bytes, FILE, "demo", "1.0", WheelLimits::default()).unwrap_err();
    assert!(
        error.to_string().contains(diagnostic),
        "expected {diagnostic:?}, got {error}"
    );
}

fn member<'a>(manifest: &'a WheelManifest, path: &str) -> &'a WheelMember {
    manifest
        .members()
        .iter()
        .find(|entry| entry.archive_path() == path)
        .unwrap()
}

// Test-only ZIP header mutation. Payload/RECORD bytes and their CRCs are not
// changed; this deliberately creates names the ordinary writer refuses to emit.
fn rename_headers(bytes: &mut [u8], old: &[u8], new: &[u8]) {
    assert_eq!(old.len(), new.len());
    let mut changed = 0;
    for start in 0..bytes.len().saturating_sub(46) {
        let (offset, length_offset) = match &bytes[start..start + 4] {
            b"PK\x03\x04" => (30, 26),
            b"PK\x01\x02" => (46, 28),
            _ => continue,
        };
        let length = u16::from_le_bytes(
            bytes[start + length_offset..start + length_offset + 2]
                .try_into()
                .unwrap(),
        ) as usize;
        if length == old.len() && bytes.get(start + offset..start + offset + length) == Some(old) {
            bytes[start + offset..start + offset + length].copy_from_slice(new);
            changed += 1;
        }
    }
    assert_eq!(changed, 2);
}

// Split/rebuild only fixture-produced single-disk classic archives. No product
// parsing helper participates in the expected results.
fn zip_parts(bytes: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
    let end = bytes.windows(4).rposition(|v| v == b"PK\x05\x06").unwrap();
    let count = u16::from_le_bytes(bytes[end + 10..end + 12].try_into().unwrap());
    let offset = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    let mut at = offset;
    let mut entries = Vec::new();
    for _ in 0..count {
        assert_eq!(&bytes[at..at + 4], b"PK\x01\x02");
        let variable: usize = [28, 30, 32]
            .into_iter()
            .map(|field| {
                u16::from_le_bytes(bytes[at + field..at + field + 2].try_into().unwrap()) as usize
            })
            .sum();
        let next = at + 46 + variable;
        entries.push(bytes[at..next].to_vec());
        at = next;
    }
    assert_eq!(at, end);
    (bytes[..offset].to_vec(), entries, bytes[end..].to_vec())
}

fn rebuild_zip(prefix: Vec<u8>, central: Vec<Vec<u8>>, mut end: Vec<u8>) -> Vec<u8> {
    let count: u16 = central.len().try_into().unwrap();
    let offset: u32 = prefix.len().try_into().unwrap();
    let size: u32 = central
        .iter()
        .map(Vec::len)
        .sum::<usize>()
        .try_into()
        .unwrap();
    end[8..10].copy_from_slice(&count.to_le_bytes());
    end[10..12].copy_from_slice(&count.to_le_bytes());
    end[12..16].copy_from_slice(&size.to_le_bytes());
    end[16..20].copy_from_slice(&offset.to_le_bytes());
    let mut bytes = prefix;
    for entry in central {
        bytes.extend_from_slice(&entry);
    }
    bytes.extend_from_slice(&end);
    bytes
}

#[derive(Clone, Copy)]
enum ExtraShape {
    Valid,
    Missing,
    Truncated,
    Duplicate,
}

fn required_zip64_extra(shape: ExtraShape, values: &[u64]) -> Vec<u8> {
    let mut extra = Vec::new();
    if matches!(shape, ExtraShape::Missing) {
        return extra;
    }
    let fields = if matches!(shape, ExtraShape::Truncated) {
        1
    } else {
        values.len()
    };
    extra.extend_from_slice(&1_u16.to_le_bytes());
    extra.extend_from_slice(&((values.len() * 8) as u16).to_le_bytes());
    for value in &values[..fields] {
        extra.extend_from_slice(&value.to_le_bytes());
    }
    if matches!(shape, ExtraShape::Duplicate) {
        extra.extend_from_slice(&extra.clone());
    }
    extra
}

fn zip64_member(bytes: Vec<u8>, local_shape: ExtraShape, central_shape: ExtraShape) -> Vec<u8> {
    let (mut prefix, mut central, end) = zip_parts(&bytes);
    let first = &central[0];
    let compressed = u32::from_le_bytes(first[20..24].try_into().unwrap()) as u64;
    let uncompressed = u32::from_le_bytes(first[24..28].try_into().unwrap()) as u64;
    let offset = u32::from_le_bytes(first[42..46].try_into().unwrap()) as usize;
    let name_len =
        u16::from_le_bytes(prefix[offset + 26..offset + 28].try_into().unwrap()) as usize;
    let extra_len =
        u16::from_le_bytes(prefix[offset + 28..offset + 30].try_into().unwrap()) as usize;
    let insert = offset + 30 + name_len + extra_len;
    let local_extra = required_zip64_extra(local_shape, &[uncompressed, compressed]);
    let delta = local_extra.len();
    prefix.splice(insert..insert, local_extra);
    prefix[offset + 4..offset + 6].copy_from_slice(&45_u16.to_le_bytes());
    prefix[offset + 18..offset + 26].fill(0xff);
    prefix[offset + 28..offset + 30].copy_from_slice(&((extra_len + delta) as u16).to_le_bytes());
    for (index, entry) in central.iter_mut().enumerate() {
        let old_offset = u32::from_le_bytes(entry[42..46].try_into().unwrap()) as usize;
        if index == 0 {
            let name_len = u16::from_le_bytes(entry[28..30].try_into().unwrap()) as usize;
            let extra_len = u16::from_le_bytes(entry[30..32].try_into().unwrap()) as usize;
            let extra =
                required_zip64_extra(central_shape, &[uncompressed, compressed, offset as u64]);
            let added = extra.len();
            let insert = 46 + name_len + extra_len;
            entry.splice(insert..insert, extra);
            entry[6..8].copy_from_slice(&45_u16.to_le_bytes());
            entry[20..28].fill(0xff);
            entry[42..46].fill(0xff);
            entry[30..32].copy_from_slice(&((extra_len + added) as u16).to_le_bytes());
        } else {
            assert!(old_offset >= insert);
            entry[42..46].copy_from_slice(&((old_offset + delta) as u32).to_le_bytes());
        }
    }
    rebuild_zip(prefix, central, end)
}

fn overlapping_members() -> Vec<u8> {
    let nested_path = "demo/nested.txt";
    let nested_bytes = b"nested";
    let (inner_local, inner_central, _) =
        zip_parts(&raw_zip(vec![file(nested_path, nested_bytes)]));
    let mut entries = base();
    entries.push(file("demo/outer.bin", inner_local));
    let outer = build(INFO, entries, |records| {
        records.push([
            nested_path.into(),
            format!(
                "sha256={}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(Sha256::digest(nested_bytes))
            ),
            nested_bytes.len().to_string(),
        ])
    });
    let (prefix, mut central, end) = zip_parts(&outer);
    let outer_entry = central
        .iter()
        .find(|entry| {
            let length = u16::from_le_bytes(entry[28..30].try_into().unwrap()) as usize;
            &entry[46..46 + length] == b"demo/outer.bin"
        })
        .unwrap();
    let offset = u32::from_le_bytes(outer_entry[42..46].try_into().unwrap()) as usize;
    let name_len =
        u16::from_le_bytes(prefix[offset + 26..offset + 28].try_into().unwrap()) as usize;
    let extra_len =
        u16::from_le_bytes(prefix[offset + 28..offset + 30].try_into().unwrap()) as usize;
    let nested_offset = offset + 30 + name_len + extra_len;
    assert_eq!(&prefix[nested_offset..nested_offset + 4], b"PK\x03\x04");
    let mut nested_entry = inner_central.into_iter().next().unwrap();
    nested_entry[42..46].copy_from_slice(&(nested_offset as u32).to_le_bytes());
    central.push(nested_entry);
    rebuild_zip(prefix, central, end)
}

fn zip64_footer(mut bytes: Vec<u8>) -> Vec<u8> {
    let start = bytes
        .windows(4)
        .rposition(|magic| magic == b"PK\x05\x06")
        .unwrap();
    let mut end = bytes[start..].to_vec();
    let count = u16::from_le_bytes(end[10..12].try_into().unwrap()) as u64;
    let size = u32::from_le_bytes(end[12..16].try_into().unwrap()) as u64;
    let offset = u32::from_le_bytes(end[16..20].try_into().unwrap()) as u64;
    bytes.truncate(start);
    bytes.extend_from_slice(b"PK\x06\x06");
    bytes.extend_from_slice(&44_u64.to_le_bytes());
    bytes.extend_from_slice(&45_u16.to_le_bytes());
    bytes.extend_from_slice(&45_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&offset.to_le_bytes());
    bytes.extend_from_slice(b"PK\x06\x07");
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&(start as u64).to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    end[8..12].fill(0xff);
    end[12..20].fill(0xff);
    bytes.extend_from_slice(&end);
    bytes
}

#[test]
fn valid_pure_wheel_has_complete_read_only_inventory() {
    let manifest = accepted(build(INFO, base(), |_| {}));
    assert_eq!(
        (manifest.name(), manifest.version(), manifest.dist_info()),
        ("demo", "1.0", INFO)
    );
    assert_eq!(manifest.root_scheme(), WheelScheme::Purelib);
    assert_eq!(manifest.members().len(), 4);
    let module = member(&manifest, "demo/__init__.py");
    assert_eq!(
        (
            module.archive_index(),
            module.scheme(),
            module.relative_path(),
            module.size()
        ),
        (0, WheelScheme::Purelib, "demo/__init__.py", 11)
    );
    assert_eq!(module.role(), WheelMemberRole::Payload);
    assert_eq!(
        module.record_hash().unwrap().algorithm(),
        RecordHashAlgorithm::Sha256
    );
    assert_eq!(
        module.observed_sha256().as_slice(),
        Sha256::digest(b"VALUE = 42\n").as_slice()
    );
    assert_eq!(manifest.core_metadata().metadata_version(), (2, 6));
    assert!(manifest.warnings().is_empty());
}

#[test]
fn root_is_purelib_false_selects_platlib_without_host_inference() {
    let mut entries = base();
    set(
        &mut entries,
        "/WHEEL",
        b"Wheel-Version: 1.0\nRoot-Is-Purelib: false\nTag: py3-none-any\n",
    );
    let manifest = accepted(build(INFO, entries, |_| {}));
    assert_eq!(manifest.root_scheme(), WheelScheme::Platlib);
    assert_eq!(
        member(&manifest, "demo/__init__.py").scheme(),
        WheelScheme::Platlib
    );
}

#[test]
fn five_data_schemes_have_exact_logical_relative_destinations() {
    let mut entries = base();
    for (scheme, tail) in [
        ("purelib", "extra.py"),
        ("platlib", "native.so"),
        ("headers", "demo.h"),
        ("scripts", "demo-cli"),
        ("data", "share/demo.txt"),
    ] {
        entries.push(file(format!("demo-1.0.data/{scheme}/{tail}"), b"payload"));
    }
    let manifest = accepted(build(INFO, entries, |_| {}));
    for (path, scheme, relative) in [
        (
            "demo-1.0.data/purelib/extra.py",
            WheelScheme::Purelib,
            "extra.py",
        ),
        (
            "demo-1.0.data/platlib/native.so",
            WheelScheme::Platlib,
            "native.so",
        ),
        (
            "demo-1.0.data/headers/demo.h",
            WheelScheme::Headers,
            "demo.h",
        ),
        (
            "demo-1.0.data/scripts/demo-cli",
            WheelScheme::Scripts,
            "demo-cli",
        ),
        (
            "demo-1.0.data/data/share/demo.txt",
            WheelScheme::Data,
            "share/demo.txt",
        ),
    ] {
        assert_eq!(
            (
                member(&manifest, path).scheme(),
                member(&manifest, path).relative_path()
            ),
            (scheme, relative)
        );
    }
}

#[test]
fn normalized_identity_accepts_legacy_case_dots_and_equivalent_version() {
    let info = "Demo.Pkg-1.0.0.dist-info";
    let mut entries = base();
    for entry in &mut entries {
        entry.path = entry.path.replace(INFO, info);
    }
    set(
        &mut entries,
        "/METADATA",
        b"Metadata-Version: 2.6\nName: Demo_Pkg\nVersion: 1.0.0\n",
    );
    let manifest = inspect(
        build(info, entries, |_| {}),
        "demo_pkg-1.0-py3-none-any.whl",
        "Demo-Pkg",
        "1.0",
        WheelLimits::default(),
    )
    .unwrap();
    assert_eq!(
        (manifest.name(), manifest.version(), manifest.dist_info()),
        ("demo-pkg", "1.0.0", info)
    );
}

#[test]
fn invalid_expectation_or_incompatible_filename_is_rejected_first() {
    for (filename, name, version) in [
        ("broken.whl", "demo", "1.0"),
        (FILE, "other", "1.0"),
        (FILE, "demo", "2.0"),
        (FILE, "bad name", "1.0"),
        (FILE, "demo", "opaque"),
    ] {
        let error = inspect(
            build(INFO, base(), |_| {}),
            filename,
            name,
            version,
            WheelLimits::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("expectation"), "{error}");
    }
    let error = inspect(
        build(INFO, base(), |_| {}),
        "demo-1.0-cp311-cp311-linux_aarch64.whl",
        "demo",
        "1.0",
        WheelLimits::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("compatible"), "{error}");
}

#[test]
fn invalid_or_truncated_zip_never_returns_a_manifest() {
    rejected(b"not a ZIP".to_vec(), "ZIP");
    let mut bytes = build(INFO, base(), |_| {});
    bytes.truncate(bytes.len() / 2);
    rejected(bytes, "ZIP");
    let bytes = build(INFO, base(), |_| {});
    let (prefix, central, mut end) = zip_parts(&bytes);
    end[4..6].copy_from_slice(&1_u16.to_le_bytes());
    end[6..8].copy_from_slice(&1_u16.to_le_bytes());
    rejected(rebuild_zip(prefix, central, end), "ZIP");
    let (mut prefix, mut central, end) = zip_parts(&bytes);
    for flags in [&mut prefix[6..8], &mut central[0][8..10]] {
        let value = u16::from_le_bytes((&*flags).try_into().unwrap()) | 1;
        flags.copy_from_slice(&value.to_le_bytes());
    }
    rejected(rebuild_zip(prefix, central, end), "ZIP");
}

#[test]
fn valid_small_zip64_directory_is_supported_without_large_allocations() {
    let manifest = accepted(zip64_footer(build(INFO, base(), |_| {})));
    assert_eq!(manifest.members().len(), 4);
    let valid = build(INFO, base(), |_| {});
    assert_eq!(
        accepted(zip64_member(
            valid.clone(),
            ExtraShape::Valid,
            ExtraShape::Valid
        ))
        .members()
        .len(),
        4
    );
    for shape in [
        ExtraShape::Missing,
        ExtraShape::Truncated,
        ExtraShape::Duplicate,
    ] {
        rejected(zip64_member(valid.clone(), shape, ExtraShape::Valid), "ZIP");
        rejected(zip64_member(valid.clone(), ExtraShape::Valid, shape), "ZIP");
    }
    let mut entries = base();
    let record = csv(&rows(&entries, INFO), "\n");
    entries.push(file(format!("{INFO}/RECORD"), record));
    let mut writer = zip::ZipWriter::new_stream(Vec::new());
    for entry in entries {
        writer
            .start_file(
                &entry.path,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(&entry.bytes).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    assert_ne!(u16::from_le_bytes(bytes[6..8].try_into().unwrap()) & 8, 0);
    assert_eq!(accepted(bytes).members().len(), 4);
}

#[test]
fn local_and_central_headers_must_agree_and_member_ranges_cannot_overlap() {
    let original = build(INFO, base(), |_| {});
    assert_eq!(&original[0..4], b"PK\x03\x04");
    assert_eq!(
        u16::from_le_bytes(original[6..8].try_into().unwrap()) & 8,
        0
    );
    for (offset, mask, diagnostic) in [
        (30, 1, "ZIP name"),
        (7, 8, "ZIP"),
        (8, 1, "ZIP"),
        (14, 1, "ZIP"),
        (18, 1, "ZIP"),
        (22, 1, "ZIP"),
    ] {
        let mut bytes = original.clone();
        // These files do not use descriptors. Local CRC/sizes are authoritative,
        // unlike the valid streaming-placeholder fixture in the preceding test.
        bytes[offset] ^= mask;
        rejected(bytes, diagnostic);
    }
    // Both local names/CRCs/sizes and RECORD rows are valid. The second local
    // member lives inside outer.bin's data, so only the overlap check rejects it.
    rejected(overlapping_members(), "overlap");
}

#[test]
fn dishonest_end_directory_count_fails_before_inventory_allocation() {
    let mut bytes = build(INFO, base(), |_| {});
    let start = bytes
        .windows(4)
        .rposition(|magic| magic == b"PK\x05\x06")
        .unwrap();
    bytes[start + 8..start + 12].fill(0xff);
    let error = inspect(
        bytes,
        FILE,
        "demo",
        "1.0",
        WheelLimits {
            max_members: 8,
            ..WheelLimits::default()
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("ZIP") || error.to_string().contains("limit"),
        "{error}"
    );
}

#[test]
fn absolute_and_escaping_member_paths_fail_without_destination_writes() {
    let root = tempfile::tempdir().unwrap();
    let sentinel = root.path().join("outside.txt");
    std::fs::write(&sentinel, b"untouched").unwrap();
    for path in [
        sentinel.display().to_string(),
        "../escape".into(),
        "a/../../escape".into(),
        "C:/escape".into(),
        "a\\escape".into(),
    ] {
        let mut entries = base();
        entries.push(file(path, b"bad"));
        rejected(build(INFO, entries, |_| {}), "path");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn contained_path_components_are_normalized_without_sanitizing_escapes() {
    let mut entries = base();
    entries.push(file("demo/./data/../resource.txt", b"safe"));
    let manifest = accepted(build(INFO, entries, |_| {}));
    assert_eq!(
        member(&manifest, "demo/./data/../resource.txt").relative_path(),
        "demo/resource.txt"
    );
}

#[test]
fn duplicate_normalized_names_and_file_parent_conflicts_fail() {
    let mut entries = base();
    entries.push(file("demo/./__init__.py", b"same"));
    rejected(build(INFO, entries, |_| {}), "duplicate");
    let mut entries = base();
    entries.push(file("demo", b"file cannot own a parent directory"));
    rejected(build(INFO, entries, |_| {}), "conflict");
}

#[test]
fn duplicate_zip_names_cannot_hide_behind_central_directory_lookup() {
    let mut entries = base();
    entries.push(file("demo/a.txt", b"a"));
    entries.push(file("demo/b.txt", b"b"));
    let mut bytes = build(INFO, entries, |_| {});
    rename_headers(&mut bytes, b"demo/b.txt", b"demo/a.txt");
    rejected(bytes, "duplicate");
}

#[test]
fn non_utf8_names_cannot_be_silently_lossy() {
    for invalid in [b"demo/\xff.txt".as_slice(), b"demo/\0.txt"] {
        let mut entries = base();
        entries.push(file("demo/a.txt", b"a"));
        let mut bytes = build(INFO, entries, |_| {});
        rename_headers(&mut bytes, b"demo/a.txt", invalid);
        let error = inspect(bytes, FILE, "demo", "1.0", WheelLimits::default()).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("UTF-8") || message.contains("ZIP") || message.contains("path"),
            "{error}"
        );
    }
}

#[test]
fn special_zip_entries_are_rejected_before_payload_read() {
    let mut bytes = build(INFO, base(), |_| {});
    let name = b"demo/__init__.py";
    let mut changed = false;
    for start in 0..bytes.len().saturating_sub(46 + name.len()) {
        if &bytes[start..start + 4] == b"PK\x01\x02"
            && &bytes[start + 46..start + 46 + name.len()] == name
        {
            bytes[start + 5] = 3;
            bytes[start + 38..start + 42].copy_from_slice(&(0o010644_u32 << 16).to_le_bytes());
            changed = true;
            break;
        }
    }
    assert!(changed);
    rejected(bytes, "member type");
}

#[test]
fn symlinks_and_nested_script_members_are_not_accepted_as_regular_scripts() {
    let mut entries = base();
    entries.push(Entry {
        path: "demo/link".into(),
        bytes: b"../../outside".to_vec(),
        kind: Kind::Symlink,
    });
    rejected(build(INFO, entries, |_| {}), "member type");
    let mut entries = base();
    entries.push(file("demo-1.0.data/scripts/sub/tool", b"#!/bin/sh\n"));
    rejected(build(INFO, entries, |_| {}), "scripts");
}

#[test]
fn explicit_directories_are_inventory_structure_not_record_payload() {
    let mut entries = base();
    entries.push(Entry {
        path: "demo/assets/".into(),
        bytes: Vec::new(),
        kind: Kind::Directory,
    });
    entries.push(file("demo/assets/x", b"x"));
    let manifest = accepted(build(INFO, entries, |_| {}));
    assert_eq!(manifest.members().len(), 5);
    assert!(
        !manifest
            .members()
            .iter()
            .any(|entry| entry.archive_path().ends_with('/'))
    );
}

#[test]
fn mandatory_metadata_is_present_exactly_once_in_one_distribution() {
    for suffix in ["/WHEEL", "/METADATA"] {
        let entries = base()
            .into_iter()
            .filter(|entry| !entry.path.ends_with(suffix))
            .collect();
        rejected(build(INFO, entries, |_| {}), "missing");
    }
    rejected(raw_zip(base()), "RECORD");
    let mut entries = base();
    entries.push(file(
        "other-2.0.dist-info/METADATA",
        b"Metadata-Version: 2.6\nName: other\nVersion: 2.0\n",
    ));
    rejected(build(INFO, entries, |_| {}), "dist-info");
}

#[test]
fn metadata_and_container_identities_must_match_the_expected_wheel() {
    for metadata in [
        "Metadata-Version: 2.6\nName: other\nVersion: 1.0\n",
        "Metadata-Version: 2.6\nName: demo\nVersion: 2.0\n",
    ] {
        let mut entries = base();
        set(&mut entries, "/METADATA", metadata);
        rejected(build(INFO, entries, |_| {}), "identity");
    }
    let info = "other-1.0.dist-info";
    let mut entries = base();
    for entry in &mut entries {
        entry.path = entry.path.replace(INFO, info);
    }
    rejected(build(info, entries, |_| {}), "identity");
    let mut entries = base();
    entries.push(file("other-1.0.data/data/x", b"x"));
    rejected(build(INFO, entries, |_| {}), "identity");
}

#[test]
fn higher_wheel_major_fails_and_compatible_minor_is_a_warning() {
    let mut entries = base();
    set(
        &mut entries,
        "/WHEEL",
        b"Wheel-Version: 2.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    );
    rejected(build(INFO, entries, |_| {}), "Wheel-Version");
    let mut entries = base();
    set(
        &mut entries,
        "/WHEEL",
        b"Wheel-Version: 1.1\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    );
    assert!(
        accepted(build(INFO, entries, |_| {}))
            .warnings()
            .contains(&WheelWarning::NewerWheelMinor { major: 1, minor: 1 })
    );
}

#[test]
fn wheel_singleton_headers_and_purelib_boolean_are_not_guessed() {
    for wheel in [
        "Root-Is-Purelib: true\nTag: py3-none-any\n",
        "Wheel-Version: 1.0\nTag: py3-none-any\n",
        "Wheel-Version: 1.0\nRoot-Is-Purelib: maybe\nTag: py3-none-any\n",
        "Wheel-Version: 1.0\nWheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
        "Wheel-Version: 1.0\nRoot-Is-Purelib: true\nRoot-Is-Purelib: false\nTag: py3-none-any\n",
    ] {
        let mut entries = base();
        set(&mut entries, "/WHEEL", wheel);
        rejected(build(INFO, entries, |_| {}), "WHEEL");
    }
}

#[test]
fn filename_build_and_wheel_build_use_the_existing_numeric_tuple_semantics() {
    let mut entries = base();
    set(
        &mut entries,
        "/WHEEL",
        b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\nBuild: 0002a\n",
    );
    let manifest = inspect(
        build(INFO, entries.clone(), |_| {}),
        "demo-1.0-2a-py3-none-any.whl",
        "demo",
        "1.0",
        WheelLimits::default(),
    )
    .unwrap();
    assert_eq!(manifest.build().unwrap().number(), "2");
    assert_eq!(manifest.build().unwrap().suffix(), "a");
    rejected(build(INFO, entries, |_| {}), "Build");
    let error = inspect(
        build(INFO, base(), |_| {}),
        "demo-1.0-2a-py3-none-any.whl",
        "demo",
        "1.0",
        WheelLimits::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("Build"), "{error}");
}

#[test]
fn expanded_tags_match_the_compressed_filename_as_a_set() {
    let mut entries = base();
    set(&mut entries, "/WHEEL", b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\nTag: py2-none-any\nTag: py3-none-any\n");
    let manifest = inspect(
        build(INFO, entries, |_| {}),
        "demo-1.0-py2.py3-none-any.whl",
        "demo",
        "1.0",
        WheelLimits::default(),
    )
    .unwrap();
    let tags: Vec<_> = manifest
        .tags()
        .iter()
        .map(|tag| format!("{}-{}-{}", tag.interpreter(), tag.abi(), tag.platform()))
        .collect();
    assert_eq!(tags, ["py2-none-any", "py3-none-any"]);
}

#[test]
fn absent_wrong_or_compressed_wheel_header_tags_fail() {
    for tag in [
        "",
        "Tag: cp311-cp311-linux_x86_64\n",
        "Tag: py2.py3-none-any\n",
        "Tag: broken\n",
    ] {
        let mut entries = base();
        set(
            &mut entries,
            "/WHEEL",
            format!("Wheel-Version: 1.0\nRoot-Is-Purelib: true\n{tag}"),
        );
        rejected(build(INFO, entries, |_| {}), "Tag");
    }
}

#[test]
fn utf8_folded_headers_multiuse_fields_and_description_are_preserved() {
    let mut entries = base();
    set(
        &mut entries,
        "/METADATA",
        "Metadata-Version: 2.6\r\nName: demo\r\nVersion: 1.0\r\nRequires-Dist: one\r\n (>=1)\r\nRequires-Dist: two\r\nSummary: 中文\r\n\r\nbody: not a header\r\n",
    );
    let manifest = accepted(build(INFO, entries, |_| {}));
    assert_eq!(
        manifest.core_metadata().values("requires-dist"),
        ["one (>=1)", "two"]
    );
    assert_eq!(manifest.core_metadata().values("summary"), ["中文"]);
    assert_eq!(
        manifest.core_metadata().description(),
        "body: not a header\r\n"
    );
    assert!(
        std::str::from_utf8(manifest.core_metadata().raw())
            .unwrap()
            .contains("Summary: 中文")
    );
}

#[test]
fn malformed_missing_duplicate_or_invalid_identity_headers_fail() {
    for metadata in [
        " orphan\nMetadata-Version: 2.6\nName: demo\nVersion: 1.0\n",
        "Metadata-Version: 2.6\nno colon\nName: demo\nVersion: 1.0\n",
        "Metadata-Version: 2.6\nVersion: 1.0\n",
        "Metadata-Version: 2.6\nName: demo\nName: demo\nVersion: 1.0\n",
        "Metadata-Version: 2.6\nName: demo\nVersion: 1.0\nVersion: 1.0\n",
        "Metadata-Version: 2.6\nName: bad name\nVersion: 1.0\n",
        "Metadata-Version: 2.6\nName: demo\nVersion: opaque\n",
    ] {
        let mut entries = base();
        set(&mut entries, "/METADATA", metadata);
        rejected(build(INFO, entries, |_| {}), "METADATA");
    }
}

#[test]
fn metadata_format_policy_is_current_26_with_future_minor_warning() {
    for version in ["3.0", "0.1", "1.0", "2.0", "bad"] {
        let mut entries = base();
        set(
            &mut entries,
            "/METADATA",
            format!("Metadata-Version: {version}\nName: demo\nVersion: 1.0\n"),
        );
        rejected(build(INFO, entries, |_| {}), "Metadata-Version");
    }
    let mut entries = base();
    set(
        &mut entries,
        "/METADATA",
        b"Metadata-Version: 2.7\nName: demo\nVersion: 1.0\n",
    );
    assert!(
        accepted(build(INFO, entries, |_| {}))
            .warnings()
            .contains(&WheelWarning::NewerMetadataMinor { major: 2, minor: 7 })
    );
}

#[test]
fn record_csv_handles_quoted_commas_quotes_newlines_crlf_and_utf8() {
    let mut entries = base();
    for path in [
        "demo/comma,name.txt",
        "demo/a\"quote.txt",
        "demo/line\nname.txt",
        "demo/cr\r\nlf.txt",
        "demo/中文.txt",
    ] {
        entries.push(file(path, b"resource"));
    }
    for newline in ["\n", "\r\n"] {
        let manifest = accepted(with_record(
            INFO,
            entries.clone(),
            csv(&rows(&entries, INFO), newline),
        ));
        assert_eq!(manifest.members().len(), 9);
        assert_eq!(
            member(&manifest, "demo/line\nname.txt").relative_path(),
            "demo/line\nname.txt"
        );
        assert_eq!(member(&manifest, "demo/中文.txt").size(), 8);
    }
}

#[test]
fn malformed_record_csv_and_wrong_field_count_are_errors() {
    for record in [
        b"one,two\n".as_slice(),
        b"one,two,three,four\n",
        b"\"unterminated,two,three\n",
        b"\"closed\"junk,two,three\n",
        b"one,two,three\rbroken",
        b"bad\xff,two,three\n",
    ] {
        rejected(with_record(INFO, base(), record.to_vec()), "RECORD CSV");
    }
}

#[test]
fn optional_record_sizes_are_allowed_but_present_sizes_are_exact_unsigned_decimal() {
    let entries = base();
    let manifest = accepted(build(INFO, entries.clone(), |rows| {
        for row in rows {
            row[2].clear();
        }
    }));
    assert_eq!(member(&manifest, "demo/__init__.py").size(), 11);
    accepted(build(INFO, entries.clone(), |rows| {
        rows[0][2] = "00011".into();
    }));
    for size in ["12", "-1", "+11", " 11", "18446744073709551616", "eleven"] {
        rejected(
            build(INFO, entries.clone(), |rows| {
                rows[0][2] = size.into();
            }),
            "RECORD size",
        );
    }
}

#[test]
fn all_supported_fixed_length_strong_record_hashes_use_frozen_hello_vectors() {
    for (hash, algorithm) in [
        (
            "sha256=LPJNul-wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ",
            RecordHashAlgorithm::Sha256,
        ),
        (
            "sha384=WeF0h3dEjGnea4ANejO7-5_xtGPkQ1TDVTvNucZm-pASWjx5-QOXvfX2oT3oKGhP",
            RecordHashAlgorithm::Sha384,
        ),
        (
            "sha512=m3HSJL1i83hdltRq0-o9czGb-8KJDKra4t_3JRlnPKcjI8PZm6XBHXx6zG4UuMXaDEZjR1wuXDre9G9zvN7AQw",
            RecordHashAlgorithm::Sha512,
        ),
        (
            "sha3_256=Mzi-aU9QxfM4gUmGzfBoZFOoiLhPQk15KvS5ICOY85I",
            RecordHashAlgorithm::Sha3_256,
        ),
        (
            "sha3_384=cgrqEQGe8GRA-_Bdh6okaAohU985B7I2MecXfOYg-hMw_wfA_d7lRpmkw-4O6diH",
            RecordHashAlgorithm::Sha3_384,
        ),
        (
            "sha3_512=ddUnw2jy7-hI7Pawc6NnZ4AIBenu8rGFfV-YTwNutt-JHXX3LZsVRRjBzViDUobR2po43ro96YtaU-XteKhJdg",
            RecordHashAlgorithm::Sha3_512,
        ),
        (
            "blake2b=5M-jmj03vjHFlgnoB5cHmcqmihm_qhUTXxZQheAdQaZboeGxRq62vQCStJ6sIUwQPM-jo2WVS7vlL3Sis2IMlA",
            RecordHashAlgorithm::Blake2b,
        ),
        (
            "blake2s=GSE7rMWN7m294865pHy7Mws9hvjMqJl-sAvkVvFAyiU",
            RecordHashAlgorithm::Blake2s,
        ),
    ] {
        let mut entries = base();
        entries.push(file("demo/hello.txt", b"hello"));
        let manifest = accepted(build(INFO, entries, |rows| {
            rows.iter_mut()
                .find(|row| row[0] == "demo/hello.txt")
                .unwrap()[1] = hash.into();
        }));
        assert_eq!(
            member(&manifest, "demo/hello.txt")
                .record_hash()
                .unwrap()
                .algorithm(),
            algorithm
        );
    }
}

#[test]
fn weak_unknown_malformed_or_wrong_record_hashes_are_never_skipped() {
    for (hash, diagnostic) in [
        ("md5=anything", "weak RECORD hash"),
        ("sha1=anything", "weak RECORD hash"),
        ("sha224=anything", "weak RECORD hash"),
        ("sha3_224=anything", "weak RECORD hash"),
        ("sha999=anything", "unsupported RECORD hash"),
        ("shake_256=anything", "unsupported RECORD hash"),
        ("", "RECORD hash"),
        ("sha256=AA", "RECORD hash"),
        ("sha256=not+urlsafe/", "RECORD hash"),
        (
            "sha256=LPJNul-wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=",
            "RECORD hash",
        ),
        (
            "sha256=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "RECORD hash",
        ),
    ] {
        rejected(
            build(INFO, base(), |rows| {
                rows[0][1] = hash.into();
            }),
            diagnostic,
        );
    }
}

#[test]
fn missing_extra_duplicate_and_directory_record_rows_are_rejected() {
    rejected(
        build(INFO, base(), |rows| {
            rows.remove(0);
        }),
        "RECORD member",
    );
    rejected(
        build(INFO, base(), |rows| {
            rows.push(["missing.py".into(), rows[0][1].clone(), "11".into()]);
        }),
        "RECORD member",
    );
    rejected(
        build(INFO, base(), |rows| {
            rows.push(rows[0].clone());
        }),
        "duplicate RECORD",
    );
    rejected(
        build(INFO, base(), |rows| {
            let mut duplicate = rows[0].clone();
            duplicate[0] = "demo/./__init__.py".into();
            rows.push(duplicate);
        }),
        "duplicate RECORD",
    );
    let mut entries = base();
    entries.push(Entry {
        path: "demo/assets/".into(),
        bytes: Vec::new(),
        kind: Kind::Directory,
    });
    rejected(
        build(INFO, entries, |rows| {
            rows.push(["demo/assets/".into(), String::new(), String::new()]);
        }),
        "RECORD member",
    );
}

#[test]
fn record_paths_use_archive_root_without_destination_escape() {
    for path in ["../outside", "/absolute", "C:/outside", "demo\\__init__.py"] {
        rejected(
            build(INFO, base(), |rows| rows[0][0] = path.into()),
            "RECORD path",
        );
    }
}

#[test]
fn record_self_cannot_be_hashed_but_its_present_size_is_checked() {
    rejected(
        build(INFO, base(), |rows| {
            rows.last_mut().unwrap()[1] = rows[0][1].clone();
        }),
        "RECORD self hash",
    );
    let entries = base();
    let mut record_rows = rows(&entries, INFO);
    for _ in 0..8 {
        let size = csv(&record_rows, "\n").len();
        record_rows.last_mut().unwrap()[2] = size.to_string();
    }
    let record = csv(&record_rows, "\n");
    assert_eq!(
        record_rows.last().unwrap()[2].parse::<usize>().unwrap(),
        record.len()
    );
    accepted(with_record(INFO, entries, record));
}

#[test]
fn legacy_signature_sidecars_are_preserved_with_an_explicit_warning_and_no_record_row() {
    let mut entries = base();
    for suffix in ["RECORD.jws", "RECORD.p7s"] {
        entries.push(file(
            format!("{INFO}/{suffix}"),
            b"legacy signature payload",
        ));
    }
    let manifest = accepted(build(INFO, entries, |_| {}));
    for suffix in ["RECORD.jws", "RECORD.p7s"] {
        let path = format!("{INFO}/{suffix}");
        assert_eq!(
            member(&manifest, &path).role(),
            WheelMemberRole::LegacySignature
        );
        assert!(member(&manifest, &path).record_hash().is_none());
        assert!(
            manifest
                .warnings()
                .contains(&WheelWarning::LegacySignature { path })
        );
    }
}

#[test]
fn signature_sidecar_rows_cannot_be_used_as_ordinary_payload_hash_rules() {
    let path = format!("{INFO}/RECORD.jws");
    let mut entries = base();
    entries.push(file(&path, b"signature"));
    rejected(
        build(INFO, entries, |rows| {
            rows.push([path, String::new(), String::new()]);
        }),
        "RECORD signature",
    );
}

#[test]
fn license_and_sbom_contents_are_retained_and_declared_license_files_exist() {
    let mut entries = base();
    set(
        &mut entries,
        "/METADATA",
        b"Metadata-Version: 2.4\nName: demo\nVersion: 1.0\nLicense-File: nested/LICENSE\n",
    );
    entries.push(file(
        format!("{INFO}/licenses/nested/LICENSE"),
        b"MIT license fixture",
    ));
    entries.push(file(
        format!("{INFO}/sboms/demo.spdx.json"),
        b"{\"spdxVersion\":\"SPDX-2.3\"}",
    ));
    let manifest = accepted(build(INFO, entries.clone(), |_| {}));
    assert!(
        manifest
            .members()
            .iter()
            .any(|entry| entry.archive_path().ends_with("licenses/nested/LICENSE"))
    );
    assert!(
        manifest
            .members()
            .iter()
            .any(|entry| entry.archive_path().ends_with("sboms/demo.spdx.json"))
    );
    entries.retain(|entry| !entry.path.ends_with("licenses/nested/LICENSE"));
    rejected(build(INFO, entries, |_| {}), "License-File");
    let mut entries = base();
    set(
        &mut entries,
        "/METADATA",
        b"Metadata-Version: 2.4\nName: demo\nVersion: 1.0\nLicense-File: ../outside\n",
    );
    rejected(build(INFO, entries, |_| {}), "License-File");
}

#[test]
fn unknown_data_scheme_and_same_scheme_alias_collisions_fail() {
    let mut entries = base();
    entries.push(file("demo-1.0.data/unknown/x", b"x"));
    rejected(build(INFO, entries, |_| {}), "scheme");
    let mut entries = base();
    entries.push(file(
        "demo-1.0.data/purelib/demo/__init__.py",
        b"replacement",
    ));
    rejected(build(INFO, entries, |_| {}), "destination collision");
}

#[test]
fn distinct_purelib_and_platlib_destinations_are_not_collapsed_without_a_prefix() {
    let mut entries = base();
    set(
        &mut entries,
        "/WHEEL",
        b"Wheel-Version: 1.0\nRoot-Is-Purelib: false\nTag: py3-none-any\n",
    );
    entries.push(file("demo-1.0.data/purelib/demo/__init__.py", b"other"));
    let manifest = accepted(build(INFO, entries, |_| {}));
    assert_eq!(
        member(&manifest, "demo/__init__.py").scheme(),
        WheelScheme::Platlib
    );
    assert_eq!(
        member(&manifest, "demo-1.0.data/purelib/demo/__init__.py").scheme(),
        WheelScheme::Purelib
    );
}

#[test]
fn archive_member_crc_is_verified_even_when_record_matches_expected_payload() {
    let mut bytes = build(INFO, base(), |_| {});
    let offset = bytes
        .windows(b"VALUE = 42\n".len())
        .position(|window| window == b"VALUE = 42\n")
        .unwrap();
    bytes[offset] = b'X';
    rejected(bytes, "ZIP");
}

#[test]
fn zero_or_exceeded_limits_are_errors_not_partial_manifests() {
    let limits = WheelLimits::default();
    for limits in [
        WheelLimits {
            max_archive_bytes: 0,
            ..limits
        },
        WheelLimits {
            max_directory_bytes: 0,
            ..limits
        },
        WheelLimits {
            max_members: 0,
            ..limits
        },
        WheelLimits {
            max_member_bytes: 0,
            ..limits
        },
        WheelLimits {
            max_total_uncompressed: 0,
            ..limits
        },
        WheelLimits {
            max_metadata_bytes: 0,
            ..limits
        },
        WheelLimits {
            max_record_rows: 0,
            ..limits
        },
        WheelLimits {
            max_record_field_bytes: 0,
            ..limits
        },
        WheelLimits {
            max_header_fields: 0,
            ..limits
        },
        WheelLimits {
            max_tag_triples: 0,
            ..limits
        },
        WheelLimits {
            max_archive_bytes: 1,
            ..limits
        },
        WheelLimits {
            max_directory_bytes: 1,
            ..limits
        },
        WheelLimits {
            max_members: 1,
            ..limits
        },
        WheelLimits {
            max_member_bytes: 1,
            ..limits
        },
        WheelLimits {
            max_total_uncompressed: 1,
            ..limits
        },
        WheelLimits {
            max_metadata_bytes: 1,
            ..limits
        },
        WheelLimits {
            max_record_rows: 1,
            ..limits
        },
        WheelLimits {
            max_record_field_bytes: 1,
            ..limits
        },
        WheelLimits {
            max_header_fields: 1,
            ..limits
        },
    ] {
        let error = inspect(build(INFO, base(), |_| {}), FILE, "demo", "1.0", limits).unwrap_err();
        assert!(error.to_string().contains("limit"), "{error}");
    }
    let mut entries = base();
    set(
        &mut entries,
        "/WHEEL",
        b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py2-none-any\nTag: py3-none-any\n",
    );
    let error = inspect(
        build(INFO, entries, |_| {}),
        "demo-1.0-py2.py3-none-any.whl",
        "demo",
        "1.0",
        WheelLimits {
            max_tag_triples: 1,
            ..limits
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("limit"), "{error}");
}

#[test]
fn metadata_values_and_body_are_not_disclosed_by_debug_or_error() {
    let mut entries = base();
    set(&mut entries, "/METADATA", b"Metadata-Version: 2.6\nName: demo\nVersion: 1.0\nRequires-Dist: secret @ https://user:password@private.invalid/pkg\n\nprivate-body-token\n");
    let manifest = accepted(build(INFO, entries, |_| {}));
    assert_eq!(
        manifest.core_metadata().values("requires-dist"),
        ["secret @ https://user:password@private.invalid/pkg"]
    );
    let debug = format!("{manifest:?}");
    for secret in ["password", "private.invalid", "private-body-token"] {
        assert!(!debug.contains(secret), "{debug}");
    }
}

#[test]
fn a_comment_embedded_footer_cannot_redirect_the_bounded_directory() {
    let mut bytes = build(INFO, base(), |_| {});
    let end = bytes
        .windows(4)
        .rposition(|magic| magic == b"PK\x05\x06")
        .unwrap();
    // A legitimate archive comment may contain arbitrary bytes. Zip8 otherwise
    // accepts this later footer and reads just its one claimed central member.
    let mut decoy = bytes[end..].to_vec();
    decoy[8..10].copy_from_slice(&1_u16.to_le_bytes());
    decoy[10..12].copy_from_slice(&1_u16.to_le_bytes());
    let mut comment = b"comment with a binary footer: ".to_vec();
    comment.extend_from_slice(&decoy);
    comment.extend_from_slice(b" trailing comment");
    bytes[end + 20..end + 22].copy_from_slice(&(comment.len() as u16).to_le_bytes());
    bytes.extend_from_slice(&comment);
    let original = bytes.clone();
    let tags = SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap();
    let mut reader = Cursor::new(bytes);
    let manifest = inspect_wheel(
        &mut reader,
        &WheelExpectation {
            filename: FILE,
            name: "demo",
            version: "1.0",
            supported: &tags,
        },
        WheelLimits {
            max_members: 8,
            ..WheelLimits::default()
        },
    )
    .unwrap();
    assert_eq!(manifest.members().len(), 4);
    assert_eq!(reader.into_inner(), original);
}

#[test]
fn a_self_sized_comment_footer_cannot_replace_the_real_directory() {
    let mut bytes = build(INFO, base(), |_| {});
    let end = bytes
        .windows(4)
        .rposition(|magic| magic == b"PK\x05\x06")
        .unwrap();
    let trailing = b"the rest of the legitimate archive comment";
    let mut decoy = bytes[end..].to_vec();
    decoy[8..10].copy_from_slice(&1_u16.to_le_bytes());
    decoy[10..12].copy_from_slice(&1_u16.to_le_bytes());
    decoy[20..22].copy_from_slice(&(trailing.len() as u16).to_le_bytes());
    let mut comment = b"binary comment prefix ".to_vec();
    comment.extend_from_slice(&decoy);
    comment.extend_from_slice(trailing);
    bytes[end + 20..end + 22].copy_from_slice(&(comment.len() as u16).to_le_bytes());
    bytes.extend_from_slice(&comment);
    // Both footer comment lengths end exactly at EOF. The decoy's claimed
    // directory cannot end at that footer; the real four-member directory can.
    assert_eq!(accepted(bytes).members().len(), 4);
}

fn duplicate_comment_directory(mut bytes: Vec<u8>, unsafe_name: bool) -> Vec<u8> {
    let (_, mut central, mut second_end) = zip_parts(&bytes);
    if unsafe_name {
        let size = u16::from_le_bytes(central[0][28..30].try_into().unwrap()) as usize;
        let name = format!("../{}", "x".repeat(size - 3));
        central[0][46..46 + size].copy_from_slice(name.as_bytes());
    }
    let original_end = bytes.len() - 22;
    let comment_size = central.iter().map(Vec::len).sum::<usize>() + 22;
    bytes[original_end + 20..original_end + 22]
        .copy_from_slice(&(comment_size as u16).to_le_bytes());
    second_end[16..20].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    for entry in central {
        bytes.extend_from_slice(&entry);
    }
    bytes.extend_from_slice(&second_end);
    bytes
}

#[test]
fn multiple_complete_directory_envelopes_are_rejected_as_ambiguous() {
    rejected(
        duplicate_comment_directory(build(INFO, base(), |_| {}), false),
        "ambiguous",
    );
}

#[test]
fn unsafe_candidate_policy_errors_are_not_swallowed_as_decoys() {
    rejected(
        duplicate_comment_directory(build(INFO, base(), |_| {}), true),
        "path",
    );
}

#[test]
fn directory_candidate_work_budget_is_global_across_structural_decoys() {
    let mut bytes = build(INFO, base(), |_| {});
    let original_end = bytes.len() - 22;
    let template = bytes[original_end..].to_vec();
    let directory_start = u32::from_le_bytes(template[16..20].try_into().unwrap()) as usize;
    let count = 1_000;
    bytes[original_end + 20..original_end + 22]
        .copy_from_slice(&((count * 22) as u16).to_le_bytes());
    let mut max_directory_bytes = 0;
    for index in 0..count {
        let mut decoy = template.clone();
        decoy[8..10].copy_from_slice(&5_u16.to_le_bytes());
        decoy[10..12].copy_from_slice(&5_u16.to_le_bytes());
        max_directory_bytes = bytes.len() - directory_start;
        decoy[12..16].copy_from_slice(&(max_directory_bytes as u32).to_le_bytes());
        decoy[20..22].copy_from_slice(&(((count - index - 1) * 22) as u16).to_le_bytes());
        bytes.extend_from_slice(&decoy);
    }
    // Each bounded envelope claims five members. Four real headers validate,
    // then the original EOCD cannot be a fifth central header. Per-candidate
    // limits fit, but repeated validation must exhaust one shared work budget.
    let error = inspect(
        bytes,
        FILE,
        "demo",
        "1.0",
        WheelLimits {
            max_members: 8,
            max_directory_bytes: max_directory_bytes as u64,
            ..WheelLimits::default()
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("candidate work byte limit"),
        "{error}"
    );
}

#[test]
fn impossible_comment_envelopes_are_not_mistaken_for_archive_policy() {
    let original = build(INFO, base(), |_| {});
    let end = original.len() - 22;
    for mutation in [0, 1, 2] {
        let mut bytes = original.clone();
        let mut decoy = bytes[end..].to_vec();
        match mutation {
            0 => {
                decoy[4..6].copy_from_slice(&1_u16.to_le_bytes());
                decoy[6..8].copy_from_slice(&1_u16.to_le_bytes());
            }
            1 => decoy[8..10].copy_from_slice(&1_u16.to_le_bytes()),
            2 => decoy[8..12].fill(0xff),
            _ => unreachable!(),
        }
        let prefix = b"legitimate binary archive comment: ";
        bytes[end + 20..end + 22]
            .copy_from_slice(&((prefix.len() + decoy.len()) as u16).to_le_bytes());
        bytes.extend_from_slice(prefix);
        bytes.extend_from_slice(&decoy);
        // None of these apparent footers has a directory ending at its start.
        // Their unsupported/sentinel fields cannot turn random comment bytes
        // into a real multi-disk/ZIP64 record before envelope validation.
        assert_eq!(accepted(bytes).members().len(), 4);
    }
}
