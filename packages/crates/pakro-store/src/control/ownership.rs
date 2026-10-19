//! Bounded opaque frames shared by control-log callers.
//!
//! Valid frames and receipts describe bytes only. They grant no publication,
//! ownership, or cleanup authority; callers retain their actual file bindings.

use std::fs::File;
use std::io::{Read, Write};

use eyre::{Result, ensure};
use sha2::{Digest as _, Sha256};

const FRAME_BYTES: u64 = 8 + 8 + 32 + 32 + 32;
const FRAME_DOMAIN: &[u8] = b"pakro-ownership-frame/v1\0";

#[derive(Debug, Clone, Copy)]
pub struct FrameLimits {
    pub max_payload_bytes: u64,
    pub max_records: u64,
    pub max_total_bytes: u64,
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self {
            max_payload_bytes: 1024 * 1024,
            max_records: 1_048_576,
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameReceipt {
    sequence: u64,
    payload_bytes: u64,
    chain_sha256: [u8; 32],
    total_bytes: u64,
    next_sequence: u64,
}

impl FrameReceipt {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }
    pub fn chain_sha256(&self) -> &[u8; 32] {
        &self.chain_sha256
    }
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }
}

pub struct FrameRecord {
    sequence: u64,
    payload: Vec<u8>,
    chain_sha256: [u8; 32],
}

impl FrameRecord {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
    pub fn chain_sha256(&self) -> &[u8; 32] {
        &self.chain_sha256
    }
}

impl std::fmt::Debug for FrameRecord {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("FrameRecord")
            .field("sequence", &self.sequence)
            .field("payload_bytes", &self.payload.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct FrameRead {
    records: Vec<FrameRecord>,
    total_bytes: u64,
}

impl FrameRead {
    pub fn into_parts(self) -> (Vec<FrameRecord>, u64) {
        (self.records, self.total_bytes)
    }
}

pub fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn read_frames(
    reader: &mut impl Read,
    expected_header: &[u8],
    header_head: [u8; 32],
    observed_len: u64,
    limits: FrameLimits,
) -> Result<FrameRead> {
    validate_limits(limits)?;
    let header_bytes = u64::try_from(expected_header.len())
        .map_err(|_| eyre::eyre!("ownership stream header length is not representable"))?;
    ensure!(
        header_bytes != 0 && header_bytes <= limits.max_total_bytes,
        "ownership stream header byte limit exceeded"
    );
    ensure!(
        observed_len <= limits.max_total_bytes,
        "ownership stream total byte limit exceeded"
    );
    ensure!(
        observed_len >= header_bytes,
        "ownership stream header is truncated"
    );
    // The caller owns the header's meaning. Compare its bytes without allocating
    // an unbounded additional buffer or interpreting any application identity.
    let mut encoded = [0; 4096];
    for expected in expected_header.chunks(encoded.len()) {
        reader.read_exact(&mut encoded[..expected.len()])?;
        ensure!(
            encoded[..expected.len()] == *expected,
            "ownership stream header identity or digest mismatch"
        );
    }
    let mut total = header_bytes;
    let mut head = header_head;
    let mut next = 1u64;
    let mut records = Vec::new();
    loop {
        let mut first = [0; 1];
        let count = loop {
            match reader.read(&mut first) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if count == 0 {
            break;
        }
        ensure!(
            next <= limits.max_records,
            "ownership stream record limit exceeded"
        );
        let mut prefix = [0; 80];
        prefix[0] = first[0];
        reader.read_exact(&mut prefix[1..])?;
        let sequence = u64::from_be_bytes(prefix[..8].try_into().expect("fixed frame sequence"));
        let length = u64::from_be_bytes(prefix[8..16].try_into().expect("fixed frame length"));
        let end = next_total(total, length, limits)?;
        ensure!(
            sequence == next && prefix[16..48] == head,
            "ownership stream sequence or previous digest mismatch"
        );
        ensure!(end <= observed_len, "ownership stream frame is truncated");
        let length = usize::try_from(length)
            .map_err(|_| eyre::eyre!("ownership payload length limit is not representable"))?;
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(length)
            .map_err(|_| eyre::eyre!("ownership payload allocation limit failed"))?;
        payload.resize(length, 0);
        reader.read_exact(&mut payload)?;
        let mut chain = [0; 32];
        reader.read_exact(&mut chain)?;
        ensure!(
            prefix[48..80] == hash(&payload) && chain == frame_hash(&prefix),
            "ownership stream payload or chain digest mismatch"
        );
        records
            .try_reserve(1)
            .map_err(|_| eyre::eyre!("ownership record allocation limit failed"))?;
        records.push(FrameRecord {
            sequence,
            payload,
            chain_sha256: chain,
        });
        head = chain;
        total = end;
        next = next
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("ownership sequence limit overflow"))?;
    }
    ensure!(total == observed_len, "ownership stream has trailing bytes");
    Ok(FrameRead {
        records,
        total_bytes: total,
    })
}

/// Append to an already authenticated file and persist the complete frame.
///
/// The caller retains its binding, validates it before and after this call, and
/// poisons its append capability after any failure. This function does not open
/// paths or manufacture an ownership capability from the supplied bytes.
pub fn append_frame(
    file: &mut File,
    sequence: u64,
    previous: [u8; 32],
    total: u64,
    payload: &[u8],
    limits: FrameLimits,
) -> Result<FrameReceipt> {
    validate_limits(limits)?;
    let length = u64::try_from(payload.len())
        .map_err(|_| eyre::eyre!("ownership payload length limit is not representable"))?;
    ensure!(
        sequence != 0 && sequence <= limits.max_records,
        "ownership stream record limit exceeded"
    );
    let next_sequence = sequence
        .checked_add(1)
        .ok_or_else(|| eyre::eyre!("ownership sequence limit overflow"))?;
    let total_bytes = next_total(total, length, limits)?;
    let prefix = frame_prefix(sequence, length, previous, hash(payload));
    let chain_sha256 = frame_hash(&prefix);
    file.write_all(&prefix)?;
    file.write_all(payload)?;
    file.write_all(&chain_sha256)?;
    file.flush()?;
    file.sync_all()?;
    Ok(FrameReceipt {
        sequence,
        payload_bytes: length,
        chain_sha256,
        total_bytes,
        next_sequence,
    })
}

fn validate_limits(limits: FrameLimits) -> Result<()> {
    ensure!(
        limits.max_payload_bytes != 0 && limits.max_records != 0 && limits.max_total_bytes != 0,
        "ownership stream limits must be nonzero"
    );
    Ok(())
}

fn frame_prefix(
    sequence: u64,
    length: u64,
    previous: [u8; 32],
    payload_sha256: [u8; 32],
) -> [u8; 80] {
    let mut bytes = [0; 80];
    bytes[..8].copy_from_slice(&sequence.to_be_bytes());
    bytes[8..16].copy_from_slice(&length.to_be_bytes());
    bytes[16..48].copy_from_slice(&previous);
    bytes[48..80].copy_from_slice(&payload_sha256);
    bytes
}

fn frame_hash(prefix: &[u8; 80]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(FRAME_DOMAIN);
    hash.update(prefix);
    hash.finalize().into()
}

fn next_total(current: u64, payload: u64, limits: FrameLimits) -> Result<u64> {
    ensure!(
        payload <= limits.max_payload_bytes,
        "ownership stream payload byte limit exceeded"
    );
    current
        .checked_add(FRAME_BYTES)
        .and_then(|total| total.checked_add(payload))
        .filter(|total| *total <= limits.max_total_bytes)
        .ok_or_else(|| eyre::eyre!("ownership stream total byte limit exceeded"))
}

#[cfg(test)]
#[path = "ownership/tests.rs"]
mod tests;
