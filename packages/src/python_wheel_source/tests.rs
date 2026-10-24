use super::*;

use base64::Engine;
use pakro_index::{SupportedWheelTags, WheelTag};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Cursor, Seek};
use std::path::Path;

use crate::artifact_snapshot::spool_verified_reader;
use crate::packages::python_wheel::inspect_wheel;

const FILE: &str = "demo-1.0-py3-none-any.whl";
const INFO: &str = "demo-1.0.dist-info";
const MODULE: &str = "demo/__init__.py";
const PAYLOAD: &[u8] = b"VALUE = 42\n";

type Entries = Vec<(String, Vec<u8>)>;
fn base() -> Entries {
    vec![
        (MODULE.into(), PAYLOAD.to_vec()),
        (
            format!("{INFO}/WHEEL"),
            b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_vec(),
        ),
        (
            format!("{INFO}/METADATA"),
            b"Metadata-Version: 2.6\nName: demo\nVersion: 1.0\n".to_vec(),
        ),
    ]
}
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.into()
    }
}
fn with_record(mut entries: Entries) -> Entries {
    let mut record = String::new();
    for (path, bytes) in &entries {
        if path.ends_with("/RECORD.jws") || path.ends_with("/RECORD.p7s") {
            continue;
        }
        record.push_str(&format!(
            "{},sha256={},{}\n",
            csv_field(path),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
            bytes.len()
        ));
    }
    record.push_str(&format!("{INFO}/RECORD,,\n"));
    entries.push((format!("{INFO}/RECORD"), record.into_bytes()));
    entries
}
fn write_zip<W: Write + Seek>(
    mut writer: zip::ZipWriter<W>,
    entries: &Entries,
    directories: bool,
    method: zip::CompressionMethod,
) -> W {
    let options = zip::write::SimpleFileOptions::default().compression_method(method);
    if directories {
        writer.add_directory("demo/assets/", options).unwrap();
    }
    for (name, bytes) in entries {
        writer.start_file(name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap()
}
fn wheel(entries: Entries) -> Vec<u8> {
    write_zip(
        zip::ZipWriter::new(Cursor::new(Vec::new())),
        &with_record(entries),
        false,
        zip::CompressionMethod::Stored,
    )
    .into_inner()
}
fn streamed(entries: Entries) -> Vec<u8> {
    write_zip(
        zip::ZipWriter::new_stream(Vec::new()),
        &with_record(entries),
        false,
        zip::CompressionMethod::Deflated,
    )
    .into_inner()
}
fn tags() -> SupportedWheelTags {
    SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap()
}
fn expectation(supported: &SupportedWheelTags) -> WheelExpectation<'_> {
    WheelExpectation {
        filename: FILE,
        name: "demo",
        version: "1.0",
        supported,
    }
}
fn snapshot(bytes: &[u8], filename: &str, digest: Digest, scratch: &Path) -> VerifiedArtifact {
    let mut artifact = spool_verified_reader(
        Cursor::new(bytes),
        filename,
        digest,
        bytes.len() as u64,
        scratch,
    )
    .expect("shared snapshot prerequisite must be independently GREEN");
    // The fixed ZIP is accepted by the already-GREEN A API before C1 is called.
    // A setup failures must never be described as C1 constructor/session RED.
    inspect_wheel(&mut artifact, &expectation(&tags()), WheelLimits::default())
        .expect("A fixture prerequisite must be independently GREEN");
    artifact.rewind().unwrap();
    artifact
}
fn inspected(bytes: &[u8]) -> (tempfile::TempDir, InspectedWheelArchive) {
    let root = tempfile::tempdir().unwrap();
    let digest = Digest::from_sha256_bytes(bytes);
    let source = snapshot(bytes, FILE, digest, root.path());
    let archive = inspect_owned_wheel(
        source,
        digest,
        &expectation(&tags()),
        WheelLimits::default(),
    )
    .unwrap();
    (root, archive)
}
fn index(archive: &InspectedWheelArchive, path: &str) -> usize {
    archive
        .manifest()
        .members()
        .iter()
        .find(|member| member.archive_path() == path)
        .unwrap()
        .archive_index()
}
fn assert_error<T: fmt::Debug>(result: Result<T>, diagnostic: &str) {
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains(diagnostic),
        "expected {diagnostic:?}, got {error}"
    );
}

#[test]
fn owned_constructor_retains_exact_source_key_and_read_only_manifest() {
    let bytes = wheel(base());
    let (_root, archive) = inspected(&bytes);
    assert_eq!(archive.filename(), FILE);
    assert_eq!(archive.digest(), Digest::from_sha256_bytes(&bytes));
    assert_eq!(archive.size(), bytes.len() as u64);
    assert_eq!(
        (
            archive.manifest().name(),
            archive.manifest().version(),
            archive.manifest().dist_info()
        ),
        ("demo", "1.0", INFO)
    );
}

#[test]
fn constructor_rejects_a_different_expected_source_digest_or_algorithm() {
    let bytes = wheel(base());
    let root = tempfile::tempdir().unwrap();
    for expected in [
        Digest::from_sha256_bytes(b"another artifact"),
        Digest::from_sha512_bytes(&bytes),
    ] {
        let source = snapshot(&bytes, FILE, Digest::from_sha256_bytes(&bytes), root.path());
        assert_error(
            inspect_owned_wheel(
                source,
                expected,
                &expectation(&tags()),
                WheelLimits::default(),
            ),
            "digest",
        );
    }
}

#[test]
fn constructor_rejects_another_exact_filename_even_for_the_same_bytes_and_identity() {
    let bytes = wheel(base());
    let root = tempfile::tempdir().unwrap();
    let digest = Digest::from_sha256_bytes(&bytes);
    for filename in ["demo-1.0-1-py3-none-any.whl", "other-1.0-py3-none-any.whl"] {
        let source = snapshot(&bytes, filename, digest, root.path());
        assert_error(
            inspect_owned_wheel(
                source,
                digest,
                &expectation(&tags()),
                WheelLimits::default(),
            ),
            "filename",
        );
    }
}

#[test]
fn constructor_accepts_sha512_outer_identity_without_confusing_member_sha256() {
    let bytes = wheel(base());
    let root = tempfile::tempdir().unwrap();
    let digest = Digest::from_sha512_bytes(&bytes);
    let source = snapshot(&bytes, FILE, digest, root.path());
    let mut archive = inspect_owned_wheel(
        source,
        digest,
        &expectation(&tags()),
        WheelLimits::default(),
    )
    .unwrap();
    assert_eq!(archive.digest(), digest);
    let module = index(&archive, MODULE);
    let bytes = archive
        .with_members(|session| session.read_member(module, 64))
        .unwrap();
    assert_eq!(bytes, PAYLOAD);
}

#[test]
fn constructor_keeps_archive_and_expectation_limits_from_the_real_inspector() {
    let bytes = wheel(base());
    let root = tempfile::tempdir().unwrap();
    let digest = Digest::from_sha256_bytes(&bytes);
    let source = snapshot(&bytes, FILE, digest, root.path());
    assert_error(
        inspect_owned_wheel(
            source,
            digest,
            &expectation(&tags()),
            WheelLimits {
                max_archive_bytes: bytes.len() as u64 - 1,
                ..WheelLimits::default()
            },
        ),
        "limit",
    );
    let source = snapshot(&bytes, FILE, digest, root.path());
    let supported = tags();
    assert_error(
        inspect_owned_wheel(
            source,
            digest,
            &WheelExpectation {
                filename: FILE,
                name: "another",
                version: "1.0",
                supported: &supported,
            },
            WheelLimits::default(),
        ),
        "expectation",
    );
}

#[test]
fn one_session_reads_and_copies_members_with_actual_statistics() {
    let bytes = wheel(base());
    let (_root, mut archive) = inspected(&bytes);
    let module = index(&archive, MODULE);
    archive
        .with_members(|session| {
            assert_eq!(session.read_member(module, PAYLOAD.len() as u64)?, PAYLOAD);
            let mut output = Vec::new();
            let receipt = session.copy_member(module, &mut output)?;
            assert_eq!(output, PAYLOAD);
            assert_eq!(receipt.archive_index(), module);
            assert_eq!(receipt.size(), PAYLOAD.len() as u64);
            assert_eq!(
                receipt.sha256().as_slice(),
                Sha256::digest(PAYLOAD).as_slice()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn repeated_sessions_do_not_depend_on_the_previous_read_cursor() {
    let bytes = wheel(base());
    let (_root, mut archive) = inspected(&bytes);
    let module = index(&archive, MODULE);
    for _ in 0..3 {
        assert_eq!(
            archive
                .with_members(|session| session.read_member(module, 64))
                .unwrap(),
            PAYLOAD
        );
    }
}

#[test]
fn caller_read_bounds_are_exact_and_zero_or_small_limits_do_not_truncate_successfully() {
    let bytes = wheel(base());
    let (_root, mut archive) = inspected(&bytes);
    let module = index(&archive, MODULE);
    for limit in [0, PAYLOAD.len() as u64 - 1] {
        assert_error(
            archive.with_members(|session| session.read_member(module, limit)),
            "limit",
        );
    }
    assert_eq!(
        archive
            .with_members(|session| session.read_member(module, PAYLOAD.len() as u64))
            .unwrap(),
        PAYLOAD
    );
}

#[test]
fn vector_reads_cannot_override_the_saved_metadata_budget_with_an_unlimited_request() {
    let mut entries = base();
    entries.push(("demo/large.bin".into(), vec![b'x'; 4096]));
    let bytes = wheel(entries);
    let root = tempfile::tempdir().unwrap();
    let digest = Digest::from_sha256_bytes(&bytes);
    let source = snapshot(&bytes, FILE, digest, root.path());
    let mut archive = inspect_owned_wheel(
        source,
        digest,
        &expectation(&tags()),
        WheelLimits {
            max_metadata_bytes: 1024,
            ..WheelLimits::default()
        },
    )
    .unwrap();
    let member = index(&archive, "demo/large.bin");
    assert_error(
        archive.with_members(|session| session.read_member(member, u64::MAX)),
        "limit",
    );
    let mut output = Vec::new();
    let receipt = archive
        .with_members(|session| session.copy_member(member, &mut output))
        .unwrap();
    assert_eq!(output, vec![b'x'; 4096]);
    assert_eq!(receipt.size(), 4096);
}

#[test]
fn out_of_range_indexes_are_errors_in_both_session_operations() {
    let bytes = wheel(base());
    let (_root, mut archive) = inspected(&bytes);
    for member in [archive.manifest().members().len() + 100, usize::MAX] {
        assert_error(
            archive.with_members(|session| session.read_member(member, 64)),
            "member",
        );
        assert_error(
            archive.with_members(|session| session.copy_member(member, &mut Vec::new())),
            "member",
        );
    }
}

#[test]
fn zip_directory_indexes_are_not_reinterpreted_as_file_inventory_positions() {
    let bytes = write_zip(
        zip::ZipWriter::new(Cursor::new(Vec::new())),
        &with_record(base()),
        true,
        zip::CompressionMethod::Stored,
    )
    .into_inner();
    let (_root, mut archive) = inspected(&bytes);
    let module = index(&archive, MODULE);
    assert_eq!(module, 1);
    assert_error(
        archive.with_members(|session| session.read_member(0, 64)),
        "member",
    );
    assert_error(
        archive.with_members(|session| session.copy_member(0, &mut Vec::new())),
        "member",
    );
    assert_eq!(
        archive
            .with_members(|session| session.read_member(module, 64))
            .unwrap(),
        PAYLOAD
    );
}

#[test]
fn unicode_quoted_alias_and_empty_files_keep_original_member_indexes_and_bytes() {
    let mut entries = base();
    entries.extend([
        ("demo/comma,\"line\n中文.txt".into(), b"resource".to_vec()),
        ("demo/./sub/../alias.txt".into(), b"alias".to_vec()),
        ("demo/empty".into(), Vec::new()),
    ]);
    let bytes = wheel(entries.clone());
    let (_root, mut archive) = inspected(&bytes);
    let expected: Vec<_> = entries
        .iter()
        .map(|(name, bytes)| (index(&archive, name), bytes.clone()))
        .collect();
    archive
        .with_members(|session| {
            for (member, bytes) in &expected {
                assert_eq!(session.read_member(*member, 128)?, *bytes);
                let mut copied = Vec::new();
                let receipt = session.copy_member(*member, &mut copied)?;
                assert_eq!(&copied, bytes);
                assert_eq!(receipt.size(), bytes.len() as u64);
                assert_eq!(
                    receipt.sha256().as_slice(),
                    Sha256::digest(bytes).as_slice()
                );
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn all_files_including_record_and_legacy_sidecars_are_bounded_verified_members() {
    let mut entries = base();
    entries.push((
        format!("{INFO}/RECORD.jws"),
        b"legacy signature bytes".to_vec(),
    ));
    let expected: BTreeMap<_, _> = with_record(entries.clone()).into_iter().collect();
    let bytes = wheel(entries);
    let (_root, mut archive) = inspected(&bytes);
    let members: Vec<_> = archive
        .manifest()
        .members()
        .iter()
        .map(|member| (member.archive_index(), member.archive_path().to_owned()))
        .collect();
    archive
        .with_members(|session| {
            for (member, path) in &members {
                let bytes = session.read_member(*member, 16 * 1024)?;
                assert_eq!(&bytes, expected.get(path).unwrap());
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_batch_session_reads_many_members_without_losing_directory_binding() {
    let mut entries = base();
    for member in 0..64 {
        entries.push((
            format!("demo/resources/{member}"),
            vec![member as u8; member + 1],
        ));
    }
    let expected = with_record(entries.clone());
    let bytes = wheel(entries);
    let (_root, mut archive) = inspected(&bytes);
    let members: Vec<_> = expected
        .iter()
        .map(|(name, bytes)| (index(&archive, name), bytes.clone()))
        .collect();
    archive
        .with_members(|session| {
            for (index, expected) in &members {
                assert_eq!(session.read_member(*index, 32 * 1024)?, *expected);
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn immutable_snapshot_survives_source_path_replacement_and_deletion() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mutable-cache.whl");
    let bytes = wheel(base());
    std::fs::write(&path, &bytes).unwrap();
    let digest = Digest::from_sha256_bytes(&bytes);
    let mut snapshot = spool_verified_reader(
        File::open(&path).unwrap(),
        FILE,
        digest,
        bytes.len() as u64,
        root.path(),
    )
    .unwrap();
    inspect_wheel(&mut snapshot, &expectation(&tags()), WheelLimits::default())
        .expect("A prerequisite");
    let mut archive = inspect_owned_wheel(
        snapshot,
        digest,
        &expectation(&tags()),
        WheelLimits::default(),
    )
    .unwrap();
    std::fs::write(&path, b"replaced cache content").unwrap();
    std::fs::remove_file(&path).unwrap();
    let module = index(&archive, MODULE);
    assert_eq!(
        archive
            .with_members(|session| session.read_member(module, 64))
            .unwrap(),
        PAYLOAD
    );
    assert_eq!(archive.digest(), digest);
}

#[test]
fn anonymous_snapshot_members_remain_usable_after_scratch_directory_is_removed() {
    let bytes = wheel(base());
    let (root, mut archive) = inspected(&bytes);
    let module = index(&archive, MODULE);
    drop(root);
    assert_eq!(
        archive
            .with_members(|session| session.read_member(module, 64))
            .unwrap(),
        PAYLOAD
    );
}

struct ShortWrite(Vec<u8>);
impl Write for ShortWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len().min(1);
        self.0.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct FailedWrite;
impl Write for FailedWrite {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("private output error body"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn member_copy_completes_short_writes_and_reports_failures_without_a_receipt() {
    let bytes = wheel(base());
    let (_root, mut archive) = inspected(&bytes);
    let module = index(&archive, MODULE);
    let mut output = ShortWrite(Vec::new());
    let receipt = archive
        .with_members(|session| session.copy_member(module, &mut output))
        .unwrap();
    assert_eq!(output.0, PAYLOAD);
    assert_eq!(receipt.size(), PAYLOAD.len() as u64);
    let error = archive
        .with_members(|session| session.copy_member(module, &mut FailedWrite))
        .unwrap_err();
    assert!(error.to_string().contains("write"), "{error}");
    assert!(!error.to_string().contains("private output error body"));
}

#[test]
fn archive_and_member_debug_do_not_disclose_core_metadata_values_or_body() {
    let mut entries = base();
    entries[2].1 = b"Metadata-Version: 2.6\nName: demo\nVersion: 1.0\nRequires-Dist: private @ https://user:password@private.invalid/pkg\n\nprivate-body-token\n".to_vec();
    let bytes = wheel(entries);
    let (_root, archive) = inspected(&bytes);
    assert_eq!(
        archive.manifest().core_metadata().values("requires-dist"),
        ["private @ https://user:password@private.invalid/pkg"]
    );
    let debug = format!("{archive:?}");
    for secret in ["password", "private.invalid", "private-body-token"] {
        assert!(!debug.contains(secret), "{debug}");
    }
}

fn comment_decoy(mut bytes: Vec<u8>, self_sized: bool) -> Vec<u8> {
    let end = bytes.len() - 22;
    let trailing = b"trailing comment bytes";
    let mut decoy = bytes[end..].to_vec();
    decoy[8..10].copy_from_slice(&1_u16.to_le_bytes());
    decoy[10..12].copy_from_slice(&1_u16.to_le_bytes());
    if self_sized {
        decoy[20..22].copy_from_slice(&(trailing.len() as u16).to_le_bytes());
    }
    let mut comment = b"binary prefix ".to_vec();
    comment.extend_from_slice(&decoy);
    comment.extend_from_slice(trailing);
    bytes[end + 20..end + 22].copy_from_slice(&(comment.len() as u16).to_le_bytes());
    bytes.extend_from_slice(&comment);
    bytes
}
#[test]
fn sessions_retain_both_validated_comment_footer_boundaries() {
    for self_sized in [false, true] {
        let bytes = comment_decoy(wheel(base()), self_sized);
        let (_root, mut archive) = inspected(&bytes);
        let module = index(&archive, MODULE);
        assert_eq!(
            archive
                .with_members(|session| session.read_member(module, 64))
                .unwrap(),
            PAYLOAD
        );
    }
}

#[test]
fn s3_inspected_wheel_duplicate_preserves_actual_comment_and_footer_bytes() {
    for self_sized in [false, true] {
        let bytes = comment_decoy(wheel(base()), self_sized);
        let expected_digest = Digest::from_sha256_bytes(&bytes);
        let (root, archive) = inspected(&bytes);
        let (mut retained, mut copy) = archive
            .duplicate_for_attempt(
                &tags(),
                WheelLimits::default(),
                root.path(),
                std::time::Instant::now() + std::time::Duration::from_secs(5),
                bytes.len() as u64,
            )
            .unwrap();
        for archive in [&mut retained, &mut copy] {
            assert_eq!(archive.digest(), expected_digest);
            assert_eq!(archive.size(), bytes.len() as u64);
            let module = index(archive, MODULE);
            assert_eq!(
                archive
                    .with_members(|session| session.read_member(module, 64))
                    .unwrap(),
                PAYLOAD
            );
        }
    }
}

#[test]
fn s3_inspected_wheel_duplicate_refuses_budget_and_deadline_before_writing() {
    let bytes = wheel(base());
    for expired in [false, true] {
        let (root, archive) = inspected(&bytes);
        let absent = root.path().join("absent");
        let deadline = if expired {
            std::time::Instant::now() - std::time::Duration::from_secs(1)
        } else {
            std::time::Instant::now() + std::time::Duration::from_secs(5)
        };
        let error = archive
            .duplicate_for_attempt(
                &tags(),
                WheelLimits::default(),
                &absent,
                deadline,
                if expired {
                    bytes.len() as u64
                } else {
                    bytes.len() as u64 - 1
                },
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(if expired { "deadline" } else { "byte limit" }),
            "{error}"
        );
        assert!(!absent.exists());
    }
}

fn zip64(mut bytes: Vec<u8>) -> Vec<u8> {
    let start = bytes.len() - 22;
    let mut end = bytes[start..].to_vec();
    let count = u16::from_le_bytes(end[10..12].try_into().unwrap()) as u64;
    let size = u32::from_le_bytes(end[12..16].try_into().unwrap()) as u64;
    let offset = u32::from_le_bytes(end[16..20].try_into().unwrap()) as u64;
    bytes.truncate(start);
    bytes.extend_from_slice(b"PK\x06\x06");
    bytes.extend_from_slice(&44_u64.to_le_bytes());
    bytes.extend_from_slice(&45_u16.to_le_bytes());
    bytes.extend_from_slice(&45_u16.to_le_bytes());
    bytes.extend_from_slice(&[0; 8]);
    for value in [count, count, size, offset] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(b"PK\x06\x07");
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(start as u64).to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    end[8..20].fill(0xff);
    bytes.extend_from_slice(&end);
    bytes
}
#[test]
fn sessions_read_zip64_and_deflate_stream_descriptor_archives() {
    for bytes in [zip64(wheel(base())), streamed(base())] {
        let (_root, mut archive) = inspected(&bytes);
        let module = index(&archive, MODULE);
        let mut copied = Vec::new();
        let receipt = archive
            .with_members(|session| session.copy_member(module, &mut copied))
            .unwrap();
        assert_eq!(copied, PAYLOAD);
        assert_eq!(
            receipt.sha256().as_slice(),
            Sha256::digest(PAYLOAD).as_slice()
        );
    }
}

fn source_for_usage(bytes: &[u8], filename: &str) -> (tempfile::TempDir, InspectedWheelArchive) {
    let root = tempfile::tempdir().unwrap();
    let digest = Digest::from_sha256_bytes(bytes);
    let cap = spool_verified_reader(
        Cursor::new(bytes),
        filename,
        digest,
        bytes.len() as u64,
        root.path(),
    )
    .expect("shared snapshot prerequisite must already be GREEN");
    let supported = tags();
    let source = inspect_owned_wheel(
        cap,
        digest,
        &WheelExpectation {
            filename,
            name: "demo",
            version: "1.0",
            supported: &supported,
        },
        WheelLimits::default(),
    )
    .expect("A/C1 prerequisite must be GREEN before usage-check RED");
    (root, source)
}

fn exact_usage(bytes: &[u8], entries: &Entries) -> WheelLimits {
    let footer = bytes.len() - 22;
    assert_eq!(&bytes[footer..footer + 4], b"PK\x05\x06");
    let max_field = entries
        .iter()
        .map(|(path, data)| {
            csv_field(path)
                .len()
                .max(if path == &format!("{INFO}/RECORD") {
                    0
                } else {
                    format!(
                        "sha256={}",
                        base64::engine::general_purpose::URL_SAFE_NO_PAD
                            .encode(Sha256::digest(data))
                    )
                    .len()
                })
                .max(data.len().to_string().len())
        })
        .max()
        .unwrap();
    let metadata: Vec<_> = entries
        .iter()
        .filter(|(path, _)| {
            [
                format!("{INFO}/WHEEL"),
                format!("{INFO}/METADATA"),
                format!("{INFO}/RECORD"),
            ]
            .contains(path)
        })
        .collect();
    let header_fields = metadata
        .iter()
        .filter(|(path, _)| !path.ends_with("/RECORD"))
        .map(|(_, bytes)| {
            std::str::from_utf8(bytes)
                .unwrap()
                .lines()
                .take_while(|line| !line.is_empty())
                .filter(|line| !line.starts_with([' ', '\t']))
                .count()
        })
        .max()
        .unwrap();
    WheelLimits {
        max_archive_bytes: bytes.len() as u64,
        max_directory_bytes: u64::from(u32::from_le_bytes(
            bytes[footer + 12..footer + 16].try_into().unwrap(),
        )),
        max_members: usize::from(u16::from_le_bytes(
            bytes[footer + 10..footer + 12].try_into().unwrap(),
        )),
        max_member_bytes: entries
            .iter()
            .map(|(_, bytes)| bytes.len() as u64)
            .max()
            .unwrap(),
        max_total_uncompressed: entries.iter().map(|(_, bytes)| bytes.len() as u64).sum(),
        max_metadata_bytes: metadata.iter().map(|(_, bytes)| bytes.len()).max().unwrap(),
        max_record_rows: entries.len(),
        max_record_field_bytes: max_field,
        max_header_fields: header_fields,
        max_tag_triples: 1,
    }
}

fn usage_fixture(
    entries: Entries,
    directories: bool,
) -> (tempfile::TempDir, InspectedWheelArchive, WheelLimits) {
    let entries = with_record(entries);
    let bytes = write_zip(
        zip::ZipWriter::new(Cursor::new(Vec::new())),
        &entries,
        directories,
        zip::CompressionMethod::Stored,
    )
    .into_inner();
    let exact = exact_usage(&bytes, &entries);
    let (root, source) = source_for_usage(&bytes, FILE);
    (root, source, exact)
}

#[test]
fn actual_usage_accepts_exact_limits_and_rejects_each_lower_budget_without_reinspection() {
    let (_root, source, exact) = usage_fixture(base(), false);
    source.check_limits(exact).unwrap();
    let mut cases = vec![];
    let mut lower = exact;
    lower.max_archive_bytes -= 1;
    cases.push(lower);
    let mut lower = exact;
    lower.max_directory_bytes -= 1;
    cases.push(lower);
    let mut lower = exact;
    lower.max_members -= 1;
    cases.push(lower);
    let mut lower = exact;
    lower.max_member_bytes -= 1;
    cases.push(lower);
    let mut lower = exact;
    lower.max_total_uncompressed -= 1;
    cases.push(lower);
    let mut lower = exact;
    lower.max_metadata_bytes -= 1;
    cases.push(lower);
    let mut lower = exact;
    lower.max_record_rows -= 1;
    cases.push(lower);
    let mut lower = exact;
    lower.max_record_field_bytes -= 1;
    cases.push(lower);
    let mut lower = exact;
    lower.max_header_fields -= 1;
    cases.push(lower);
    let mut lower = exact;
    lower.max_tag_triples = 0;
    cases.push(lower);
    for (index, lower) in cases.into_iter().enumerate() {
        let error = source.check_limits(lower).unwrap_err();
        assert!(
            error.to_string().contains("limit"),
            "dimension {index}: {error:#}"
        );
    }
    source.check_limits(WheelLimits::default()).unwrap();
}

#[test]
fn actual_usage_member_count_includes_directory_entries_omitted_from_manifest() {
    let (_root, source, exact) = usage_fixture(base(), true);
    assert_eq!(exact.max_members, source.manifest().members().len() + 1);
    source.check_limits(exact).unwrap();
    assert_error(
        source.check_limits(WheelLimits {
            max_members: exact.max_members - 1,
            ..exact
        }),
        "limit",
    );
}

#[test]
fn actual_usage_record_field_counts_raw_quotes_as_well_as_decoded_utf8_bytes() {
    let mut entries = base();
    let path = format!("demo/{}资源.txt", "\"".repeat(80));
    entries.push((path.clone(), b"payload".to_vec()));
    let (_root, source, exact) = usage_fixture(entries, false);
    assert_eq!(exact.max_record_field_bytes, csv_field(&path).len());
    assert!(exact.max_record_field_bytes > path.len());
    source.check_limits(exact).unwrap();
    assert_error(
        source.check_limits(WheelLimits {
            max_record_field_bytes: exact.max_record_field_bytes - 1,
            ..exact
        }),
        "limit",
    );
}

#[test]
fn actual_usage_header_fields_counts_duplicate_headers_but_not_continuation_lines() {
    let mut entries = base();
    entries
        .iter_mut()
        .find(|(path, _)| path.ends_with("/METADATA"))
        .unwrap()
        .1
        .extend_from_slice(
            b"Requires-Dist: one\n continuation\nRequires-Dist: two\nRequires-Dist: three\n",
        );
    let (_root, source, exact) = usage_fixture(entries, false);
    assert_eq!(exact.max_header_fields, 6);
    source.check_limits(exact).unwrap();
    assert_error(
        source.check_limits(WheelLimits {
            max_header_fields: 5,
            ..exact
        }),
        "limit",
    );
}

#[test]
fn actual_usage_records_expanded_filename_tag_product() {
    let mut entries = base();
    entries
        .iter_mut()
        .find(|(path, _)| path.ends_with("/WHEEL"))
        .unwrap()
        .1
        .extend_from_slice(b"Tag: py2-none-any\n");
    let entries = with_record(entries);
    let bytes = write_zip(
        zip::ZipWriter::new(Cursor::new(Vec::new())),
        &entries,
        false,
        zip::CompressionMethod::Stored,
    )
    .into_inner();
    let mut exact = exact_usage(&bytes, &entries);
    exact.max_tag_triples = 2;
    let (_root, source) = source_for_usage(&bytes, "demo-1.0-py2.py3-none-any.whl");
    source.check_limits(exact).unwrap();
    assert_error(
        source.check_limits(WheelLimits {
            max_tag_triples: 1,
            ..exact
        }),
        "limit",
    );
}

#[test]
fn actual_usage_directory_budget_includes_a_larger_zip64_extensible_record() {
    let entries = with_record(base());
    let classic = write_zip(
        zip::ZipWriter::new(Cursor::new(Vec::new())),
        &entries,
        false,
        zip::CompressionMethod::Stored,
    )
    .into_inner();
    let mut exact = exact_usage(&classic, &entries);
    let mut bytes = zip64(classic);
    let record = bytes
        .windows(4)
        .position(|part| part == b"PK\x06\x06")
        .unwrap();
    let locator = record + 56;
    let record_size = 1024_u64;
    bytes[record + 4..record + 12].copy_from_slice(&record_size.to_le_bytes());
    bytes.splice(locator..locator, vec![0; record_size as usize - 44]);
    exact.max_archive_bytes = bytes.len() as u64;
    exact.max_directory_bytes = record_size;
    let (_root, source) = source_for_usage(&bytes, FILE);
    source.check_limits(exact).unwrap();
    assert_error(
        source.check_limits(WheelLimits {
            max_directory_bytes: record_size - 1,
            ..exact
        }),
        "limit",
    );
}

#[test]
fn actual_usage_retains_shared_candidate_work_not_just_the_winning_directory_size() {
    let entries = with_record(base());
    let mut bytes = write_zip(
        zip::ZipWriter::new(Cursor::new(Vec::new())),
        &entries,
        false,
        zip::CompressionMethod::Stored,
    )
    .into_inner();
    let mut exact = exact_usage(&bytes, &entries);
    let footer = bytes.len() - 22;
    let count = 1000_usize;
    let comment_len = count * 42;
    bytes[footer + 20..footer + 22].copy_from_slice(&(comment_len as u16).to_le_bytes());
    for index in 0..count {
        // Every fake EOCD is self-sized to EOF. Its preceding locator causes
        // an additional bounded record probe at offset zero, which is a local
        // header and therefore makes this candidate structurally impossible.
        bytes.extend_from_slice(b"PK\x06\x07");
        bytes.extend_from_slice(&[0; 12]);
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        let mut decoy = [0; 22];
        decoy[..4].copy_from_slice(b"PK\x05\x06");
        decoy[20..22].copy_from_slice(&(((count - index - 1) * 42) as u16).to_le_bytes());
        bytes.extend_from_slice(&decoy);
    }
    exact.max_archive_bytes = bytes.len() as u64;
    let supported = tags();
    let oracle = inspect_wheel(Cursor::new(&bytes), &expectation(&supported), exact).unwrap_err();
    assert!(oracle.to_string().contains("candidate work"), "{oracle:#}");
    let (_root, source) = source_for_usage(&bytes, FILE);
    assert_error(source.check_limits(exact), "limit");
    source
        .check_limits(WheelLimits {
            max_directory_bytes: 100_000,
            ..exact
        })
        .unwrap();
}
