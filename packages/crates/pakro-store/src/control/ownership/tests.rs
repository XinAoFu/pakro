use super::*;

use std::fs::{self, OpenOptions};
use std::io::Cursor;

const HEADER: &[u8] = b"caller-owned opaque header\0";

fn digest(bytes: &[u8]) -> [u8; 32] {
    match crate::Digest::from_sha256_bytes(bytes) {
        crate::Digest::Sha256(value) => value,
        crate::Digest::Sha512(_) => unreachable!("fixed SHA256 constructor"),
    }
}

// Independent wire construction; never calls production framing helpers.
fn frame(sequence: u64, previous: [u8; 32], payload: &[u8]) -> (Vec<u8>, [u8; 32]) {
    let mut bytes = sequence.to_be_bytes().to_vec();
    bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&previous);
    bytes.extend_from_slice(&digest(payload));
    let mut signed = b"pakro-ownership-frame/v1\0".to_vec();
    signed.extend_from_slice(&bytes);
    let head = digest(&signed);
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&head);
    (bytes, head)
}

fn wire(payloads: &[&[u8]]) -> (Vec<u8>, [u8; 32]) {
    let mut bytes = HEADER.to_vec();
    let mut head = digest(HEADER);
    for (index, payload) in payloads.iter().enumerate() {
        let (encoded, next) = frame(index as u64 + 1, head, payload);
        bytes.extend_from_slice(&encoded);
        head = next;
    }
    (bytes, head)
}

fn read(bytes: &[u8], limits: FrameLimits) -> Result<FrameRead> {
    read_frames(
        &mut Cursor::new(bytes),
        HEADER,
        digest(HEADER),
        bytes.len() as u64,
        limits,
    )
}

#[test]
fn r05_frame_reader_accepts_independent_wire_and_opaque_payloads() {
    let payloads: &[&[u8]] = &[b"", b"\xff\0../opaque-secret", b"third"];
    let (bytes, head) = wire(payloads);
    let (records, total) = read(&bytes, FrameLimits::default()).unwrap().into_parts();
    assert_eq!(total, bytes.len() as u64);
    assert_eq!(records.len(), payloads.len());
    for (index, (record, payload)) in records.iter().zip(payloads).enumerate() {
        assert_eq!(record.sequence(), index as u64 + 1);
        assert_eq!(record.payload(), *payload);
    }
    assert_eq!(records.last().unwrap().chain_sha256(), &head);
    assert!(!format!("{records:?}").contains("opaque-secret"));
    let (records, total) = read(HEADER, FrameLimits::default()).unwrap().into_parts();
    assert!(records.is_empty());
    assert_eq!(total, HEADER.len() as u64);
}

#[test]
fn r05_frame_append_matches_independent_wire_and_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("frames");
    fs::write(&path, HEADER).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    let mut total = HEADER.len() as u64;
    let mut head = digest(HEADER);
    let payloads: &[&[u8]] = &[b"intent", b"", b"receipt"];
    for (index, payload) in payloads.iter().enumerate() {
        let receipt = append_frame(
            &mut file,
            index as u64 + 1,
            head,
            total,
            payload,
            FrameLimits::default(),
        )
        .unwrap();
        let (expected, expected_head) = wire(&payloads[..=index]);
        assert_eq!(receipt.sequence(), index as u64 + 1);
        assert_eq!(receipt.next_sequence(), index as u64 + 2);
        assert_eq!(receipt.payload_bytes(), payload.len() as u64);
        assert_eq!(receipt.chain_sha256(), &expected_head);
        assert_eq!(receipt.total_bytes(), expected.len() as u64);
        assert_eq!(fs::read(&path).unwrap(), expected);
        total = receipt.total_bytes();
        head = *receipt.chain_sha256();
    }
}

#[test]
fn r05_frame_reader_rejects_header_sequence_payload_and_chain_corruption() {
    let (valid, _) = wire(&[b"intent", b"receipt"]);
    read(&valid, FrameLimits::default()).unwrap();
    for offset in [
        0,
        HEADER.len(),
        HEADER.len() + 16,
        HEADER.len() + 48,
        HEADER.len() + 80,
        valid.len() - 1,
    ] {
        let mut corrupt = valid.clone();
        corrupt[offset] ^= 1;
        assert!(
            read(&corrupt, FrameLimits::default()).is_err(),
            "offset {offset}"
        );
    }
}

#[test]
fn r05_frame_reader_rejects_truncation_and_trailing_bytes() {
    let (valid, _) = wire(&[b"intent"]);
    read(&valid, FrameLimits::default()).unwrap();
    for length in [
        0,
        HEADER.len() - 1,
        HEADER.len() + 1,
        HEADER.len() + 79,
        valid.len() - 1,
    ] {
        assert!(
            read(&valid[..length], FrameLimits::default()).is_err(),
            "length {length}"
        );
    }
    let mut trailing = valid;
    trailing.push(0);
    assert!(read(&trailing, FrameLimits::default()).is_err());
}

#[test]
fn r05_frame_reader_checks_payload_budget_before_reading_payload() {
    let (bytes, _) = wire(&[b"large"]);
    let mut reader = Cursor::new(bytes.as_slice());
    let error = read_frames(
        &mut reader,
        HEADER,
        digest(HEADER),
        bytes.len() as u64,
        FrameLimits {
            max_payload_bytes: 4,
            ..FrameLimits::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("payload byte limit"));
    assert_eq!(reader.position(), HEADER.len() as u64 + 80);
}

#[test]
fn r05_frame_reader_enforces_record_total_and_nonzero_limits() {
    let (bytes, _) = wire(&[b"first", b"second"]);
    read(&bytes, FrameLimits::default()).unwrap();
    for limits in [
        FrameLimits {
            max_records: 1,
            ..FrameLimits::default()
        },
        FrameLimits {
            max_total_bytes: bytes.len() as u64 - 1,
            ..FrameLimits::default()
        },
        FrameLimits {
            max_payload_bytes: 0,
            ..FrameLimits::default()
        },
        FrameLimits {
            max_records: 0,
            ..FrameLimits::default()
        },
        FrameLimits {
            max_total_bytes: 0,
            ..FrameLimits::default()
        },
    ] {
        assert!(read(&bytes, limits).is_err());
    }
}

#[test]
fn r05_frame_append_rejects_invalid_bounds_before_writing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("frames");
    fs::write(&path, HEADER).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    append_frame(
        &mut file,
        1,
        digest(HEADER),
        HEADER.len() as u64,
        b"first",
        FrameLimits::default(),
    )
    .unwrap();
    let before = fs::read(&path).unwrap();
    for (sequence, total, limits) in [
        (0, before.len() as u64, FrameLimits::default()),
        (
            2,
            before.len() as u64,
            FrameLimits {
                max_records: 1,
                ..FrameLimits::default()
            },
        ),
        (
            u64::MAX,
            before.len() as u64,
            FrameLimits {
                max_records: u64::MAX,
                ..FrameLimits::default()
            },
        ),
        (
            2,
            u64::MAX,
            FrameLimits {
                max_total_bytes: u64::MAX,
                ..FrameLimits::default()
            },
        ),
        (
            2,
            before.len() as u64,
            FrameLimits {
                max_payload_bytes: 1,
                ..FrameLimits::default()
            },
        ),
        (
            2,
            before.len() as u64,
            FrameLimits {
                max_total_bytes: before.len() as u64,
                ..FrameLimits::default()
            },
        ),
        (
            2,
            before.len() as u64,
            FrameLimits {
                max_payload_bytes: 0,
                ..FrameLimits::default()
            },
        ),
    ] {
        assert!(
            append_frame(
                &mut file,
                sequence,
                digest(HEADER),
                total,
                b"second",
                limits
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn r05_frame_reader_retries_interrupted_reads() {
    struct InterruptedOnce {
        bytes: Cursor<Vec<u8>>,
        interrupt_at: u64,
        interrupted: bool,
    }
    impl Read for InterruptedOnce {
        fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted && self.bytes.position() == self.interrupt_at {
                self.interrupted = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            self.bytes.read(destination)
        }
    }
    let (bytes, _) = wire(&[b"intent"]);
    let length = bytes.len() as u64;
    let mut reader = InterruptedOnce {
        bytes: Cursor::new(bytes),
        interrupt_at: HEADER.len() as u64,
        interrupted: false,
    };
    let (records, total) = read_frames(
        &mut reader,
        HEADER,
        digest(HEADER),
        length,
        FrameLimits::default(),
    )
    .unwrap()
    .into_parts();
    assert!(reader.interrupted);
    assert_eq!(records[0].payload(), b"intent");
    assert_eq!(total, length);
}
