//! Bounded RFC4180 three-field RECORD rows and fixed-length strong hashes.

use base64::Engine;
use eyre::{Result, bail, ensure, eyre};
use sha2::Digest as Digest11;
use sha3::Digest as Digest10;

use super::usage::InspectionUsage;
use super::{RecordHashAlgorithm, WheelLimits, WheelRecordHash};

#[derive(Clone, Copy, PartialEq, Eq)]
enum CsvState {
    Start,
    Unquoted,
    Quoted,
    AfterQuote,
}

fn field_end(field: &mut Vec<u8>, raw_count: &mut usize, row: &mut Vec<String>) -> Result<()> {
    ensure!(row.len() < 3, "RECORD CSV requires exactly three fields");
    row.push(
        String::from_utf8(std::mem::take(field)).map_err(|_| eyre!("RECORD CSV invalid UTF-8"))?,
    );
    *raw_count = 0;
    Ok(())
}
fn row_end(row: &mut Vec<String>, rows: &mut Vec<[String; 3]>, limits: WheelLimits) -> Result<()> {
    ensure!(row.len() == 3, "RECORD CSV requires exactly three fields");
    ensure!(
        rows.len() < limits.max_record_rows,
        "RECORD row count limit exceeded"
    );
    rows.push(std::mem::take(row).try_into().unwrap());
    Ok(())
}

pub(super) fn parse_csv(
    bytes: &[u8],
    limits: WheelLimits,
    usage: &mut InspectionUsage,
) -> Result<Vec<[String; 3]>> {
    std::str::from_utf8(bytes).map_err(|_| eyre!("RECORD CSV invalid UTF-8"))?;
    let mut state = CsvState::Start;
    let mut field = Vec::new();
    let mut row = Vec::new();
    let mut rows = Vec::new();
    let mut raw_count = 0_usize;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        let newline = matches!(
            state,
            CsvState::Start | CsvState::Unquoted | CsvState::AfterQuote
        ) && matches!(byte, b'\r' | b'\n');
        if newline || (state != CsvState::Quoted && byte == b',') {
            if byte == b'\r' {
                ensure!(
                    bytes.get(at + 1) == Some(&b'\n'),
                    "RECORD CSV bare carriage return"
                );
                at += 1;
            }
            field_end(&mut field, &mut raw_count, &mut row)?;
            state = CsvState::Start;
            if newline {
                row_end(&mut row, &mut rows, limits)?;
            }
            at += 1;
            continue;
        }
        raw_count = raw_count
            .checked_add(1)
            .ok_or_else(|| eyre!("RECORD field byte limit overflow"))?;
        ensure!(
            raw_count <= limits.max_record_field_bytes,
            "RECORD field byte limit exceeded"
        );
        usage.record_field_bytes = usage.record_field_bytes.max(raw_count);
        let append = match state {
            CsvState::Start if byte == b'"' => {
                state = CsvState::Quoted;
                false
            }
            CsvState::Start => {
                state = CsvState::Unquoted;
                true
            }
            CsvState::Unquoted => {
                ensure!(byte != b'"', "RECORD CSV quote in unquoted field");
                true
            }
            CsvState::Quoted if byte == b'"' => {
                state = CsvState::AfterQuote;
                false
            }
            CsvState::Quoted => true,
            CsvState::AfterQuote if byte == b'"' => {
                state = CsvState::Quoted;
                true
            }
            CsvState::AfterQuote => bail!("RECORD CSV data after closing quote"),
        };
        if append {
            ensure!(
                field.len() < limits.max_record_field_bytes,
                "RECORD decoded field byte limit exceeded"
            );
            field.push(byte);
        }
        at += 1;
    }
    ensure!(
        state != CsvState::Quoted,
        "RECORD CSV unterminated quoted field"
    );
    if state != CsvState::Start || raw_count != 0 || !row.is_empty() {
        field_end(&mut field, &mut raw_count, &mut row)?;
        row_end(&mut row, &mut rows, limits)?;
    }
    usage.record_rows = usage.record_rows.max(rows.len());
    Ok(rows)
}

pub(super) fn parse_size(value: &str) -> Result<Option<u64>> {
    if value.is_empty() {
        return Ok(None);
    }
    ensure!(
        value.bytes().all(|byte| byte.is_ascii_digit()),
        "RECORD size must be unsigned ASCII decimal"
    );
    Ok(Some(
        value.parse().map_err(|_| eyre!("RECORD size overflow"))?,
    ))
}

pub(super) fn parse_hash(value: &str) -> Result<WheelRecordHash> {
    let (algorithm, text) = value
        .split_once('=')
        .ok_or_else(|| eyre!("RECORD hash is required"))?;
    let (algorithm, size) = match algorithm {
        "sha256" => (RecordHashAlgorithm::Sha256, 32),
        "sha384" => (RecordHashAlgorithm::Sha384, 48),
        "sha512" => (RecordHashAlgorithm::Sha512, 64),
        "sha3_256" => (RecordHashAlgorithm::Sha3_256, 32),
        "sha3_384" => (RecordHashAlgorithm::Sha3_384, 48),
        "sha3_512" => (RecordHashAlgorithm::Sha3_512, 64),
        "blake2b" => (RecordHashAlgorithm::Blake2b, 64),
        "blake2s" => (RecordHashAlgorithm::Blake2s, 32),
        "md5" | "sha1" | "sha224" | "sha3_224" => bail!("weak RECORD hash is forbidden"),
        _ => bail!("unsupported RECORD hash algorithm"),
    };
    ensure!(
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "RECORD hash must be URL-safe base64 without padding"
    );
    let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| eyre!("RECORD hash has invalid base64 encoding"))?;
    ensure!(
        digest.len() == size,
        "RECORD hash digest length disagrees with algorithm"
    );
    Ok(WheelRecordHash { algorithm, digest })
}

pub(super) enum MemberHasher {
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
    Sha3_256(sha3::Sha3_256),
    Sha3_384(sha3::Sha3_384),
    Sha3_512(sha3::Sha3_512),
    Blake2b(blake2::Blake2b512),
    Blake2s(blake2::Blake2s256),
}
impl MemberHasher {
    pub(super) fn new(algorithm: RecordHashAlgorithm) -> Self {
        match algorithm {
            RecordHashAlgorithm::Sha256 => Self::Sha256(sha2::Sha256::new()),
            RecordHashAlgorithm::Sha384 => Self::Sha384(sha2::Sha384::new()),
            RecordHashAlgorithm::Sha512 => Self::Sha512(sha2::Sha512::new()),
            RecordHashAlgorithm::Sha3_256 => Self::Sha3_256(sha3::Sha3_256::new()),
            RecordHashAlgorithm::Sha3_384 => Self::Sha3_384(sha3::Sha3_384::new()),
            RecordHashAlgorithm::Sha3_512 => Self::Sha3_512(sha3::Sha3_512::new()),
            RecordHashAlgorithm::Blake2b => Self::Blake2b(blake2::Blake2b512::new()),
            RecordHashAlgorithm::Blake2s => Self::Blake2s(blake2::Blake2s256::new()),
        }
    }
    pub(super) fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(state) => Digest11::update(state, bytes),
            Self::Sha384(state) => Digest11::update(state, bytes),
            Self::Sha512(state) => Digest11::update(state, bytes),
            Self::Sha3_256(state) => Digest10::update(state, bytes),
            Self::Sha3_384(state) => Digest10::update(state, bytes),
            Self::Sha3_512(state) => Digest10::update(state, bytes),
            Self::Blake2b(state) => Digest11::update(state, bytes),
            Self::Blake2s(state) => Digest11::update(state, bytes),
        }
    }
    pub(super) fn finish(self) -> Vec<u8> {
        match self {
            Self::Sha256(state) => Digest11::finalize(state).to_vec(),
            Self::Sha384(state) => Digest11::finalize(state).to_vec(),
            Self::Sha512(state) => Digest11::finalize(state).to_vec(),
            Self::Sha3_256(state) => Digest10::finalize(state).to_vec(),
            Self::Sha3_384(state) => Digest10::finalize(state).to_vec(),
            Self::Sha3_512(state) => Digest10::finalize(state).to_vec(),
            Self::Blake2b(state) => Digest11::finalize(state).to_vec(),
            Self::Blake2s(state) => Digest11::finalize(state).to_vec(),
        }
    }
}
