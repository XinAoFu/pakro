//! Closed, bounded local envelopes over a regular raw metadata file.

use super::*;
use serde::{Deserialize, Serialize};
use std::fs::{File, Metadata};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema: u32,
    entry_key: String,
    representation: String,
    content_type: String,
    canonical_document: String,
    request_document: String,
    stored_at_seconds: u64,
    stored_at_nanos: u32,
    max_age_seconds: Option<u64>,
    etag: Option<Vec<u8>>,
    body_size: u64,
    body_sha256: String,
}
fn invalid(reason: &'static str) -> SimpleClientError {
    SimpleClientError::InvalidCache(reason)
}
fn representation(value: SimpleRepresentation) -> &'static str {
    match value {
        SimpleRepresentation::JsonV1 => "json-v1",
        SimpleRepresentation::HtmlV1 => "html-v1",
        SimpleRepresentation::LegacyHtml => "legacy-html",
    }
}
fn stable_url(value: &str) -> SimpleClientResult<Url> {
    if value
        .chars()
        .any(|value| value.is_whitespace() || value.is_control())
    {
        return Err(invalid("document locator is invalid"));
    }
    let authority = value.split_once(':').map(|(_, value)| {
        value
            .trim_start_matches(['/', '\\'])
            .split(['/', '\\', '?', '#'])
            .next()
            .unwrap_or_default()
    });
    if authority.is_some_and(|value| value.contains('@')) {
        return Err(invalid("document locator has user information"));
    }
    let url = Url::parse(value).map_err(|_| invalid("document locator is invalid"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid("document locator is not stable"));
    }
    Ok(url)
}
fn media_matches(content_type: &str, kind: SimpleRepresentation) -> SimpleClientResult<()> {
    let media = decode::validate_media_type(content_type)
        .map_err(|_| invalid("stored media type is invalid"))?;
    let expected = match kind {
        SimpleRepresentation::JsonV1 => "application/vnd.pypi.simple.v1+json",
        SimpleRepresentation::HtmlV1 => "application/vnd.pypi.simple.v1+html",
        SimpleRepresentation::LegacyHtml => "text/html",
    };
    if media.essence_str() != expected {
        return Err(invalid("representation and media type disagree"));
    }
    Ok(())
}

pub(super) fn read(
    file: &mut File,
    request: &CacheRequest<'_>,
    limits: SimpleClientLimits,
) -> SimpleClientResult<CachedResponse> {
    let before = file.metadata().map_err(|_| SimpleClientError::Io)?;
    if before.len() > limits.max_cache_bytes {
        return Err(SimpleClientError::LimitExceeded(
            SimpleClientLimitKind::CacheBytes,
        ));
    }
    let mut magic = vec![0; MAGIC.len()];
    exact(file, &mut magic)?;
    if magic != MAGIC {
        return Err(invalid("entry magic is invalid"));
    }
    let mut length = [0; 8];
    exact(file, &mut length)?;
    let length = u64::from_be_bytes(length);
    if length > MAX_ENVELOPE_BYTES as u64 || length > limits.max_cache_bytes {
        return Err(invalid("entry header exceeds its bound"));
    }
    let mut encoded = vec![0; length as usize];
    exact(file, &mut encoded)?;
    let header: Header =
        serde_json::from_slice(&encoded).map_err(|_| invalid("entry header is malformed"))?;
    let kind = match header.representation.as_str() {
        "json-v1" => SimpleRepresentation::JsonV1,
        "html-v1" => SimpleRepresentation::HtmlV1,
        "legacy-html" => SimpleRepresentation::LegacyHtml,
        _ => return Err(invalid("entry representation is invalid")),
    };
    if header.schema != 1 || header.entry_key != request.key(kind).hex() {
        return Err(invalid("entry identity differs from request"));
    }
    if header.content_type.len() > limits.max_header_bytes
        || header
            .etag
            .as_ref()
            .is_some_and(|value| value.len() > limits.max_header_bytes)
    {
        return Err(invalid("stored header exceeds its field bound"));
    }
    media_matches(&header.content_type, kind)?;
    let canonical = stable_url(&header.canonical_document)?;
    let physical = stable_url(&header.request_document)?;
    if header.stored_at_nanos >= 1_000_000_000 {
        return Err(invalid("entry observation time is invalid"));
    }
    let stored_at = SystemTime::UNIX_EPOCH
        .checked_add(Duration::new(
            header.stored_at_seconds,
            header.stored_at_nanos,
        ))
        .ok_or_else(|| invalid("entry observation time overflows"))?;
    let total = (MAGIC.len() as u64)
        .checked_add(8)
        .and_then(|value| value.checked_add(length))
        .and_then(|value| value.checked_add(header.body_size))
        .ok_or_else(|| invalid("entry size overflows"))?;
    if total != before.len() {
        return Err(invalid("entry size differs from its envelope"));
    }
    if header.body_size > limits.max_wire_bytes {
        return Err(SimpleClientError::LimitExceeded(
            SimpleClientLimitKind::WireBytes,
        ));
    }
    if header.body_sha256.len() != 64
        || !header
            .body_sha256
            .bytes()
            .all(|value| value.is_ascii_hexdigit())
    {
        return Err(invalid("entry body digest is invalid"));
    }
    let mut body = Vec::new();
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut count = 0u64;
    loop {
        let request = (header.body_size - count)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let read = file
            .read(&mut buffer[..request])
            .map_err(|_| SimpleClientError::Io)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .filter(|value| *value <= header.body_size)
            .ok_or_else(|| invalid("entry has trailing body bytes"))?;
        body.try_reserve(read)
            .map_err(|_| SimpleClientError::LimitExceeded(SimpleClientLimitKind::CacheBytes))?;
        body.extend_from_slice(&buffer[..read]);
        digest.update(&buffer[..read]);
    }
    if count != header.body_size
        || pakro_store::Digest::Sha256(digest.finalize().into()).hex() != header.body_sha256
    {
        return Err(invalid("entry body digest or size differs"));
    }
    if !same_metadata(
        &before,
        &file.metadata().map_err(|_| SimpleClientError::Io)?,
    ) {
        return Err(invalid("entry changed during its read"));
    }
    let etag = header
        .etag
        .map(|value| -> SimpleClientResult<HeaderValue> {
            let mut value = HeaderValue::from_bytes(&value)
                .map_err(|_| invalid("stored validator is invalid"))?;
            value.set_sensitive(true);
            Ok(value)
        })
        .transpose()?;
    Ok(CachedResponse {
        representation: kind,
        content_type: header.content_type,
        canonical,
        physical,
        stored_at,
        max_age: header.max_age_seconds.map(Duration::from_secs),
        etag,
        body,
    })
}
fn exact(reader: &mut File, buffer: &mut [u8]) -> SimpleClientResult<()> {
    reader.read_exact(buffer).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            invalid("entry is truncated")
        } else {
            SimpleClientError::Io
        }
    })
}
fn same_metadata(before: &Metadata, after: &Metadata) -> bool {
    #[cfg(unix)]
    {
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.uid() == after.uid()
            && before.mode() == after.mode()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        before.len() == after.len()
    }
}

struct BoundedHeader {
    bytes: Vec<u8>,
}
impl Write for BoundedHeader {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_ENVELOPE_BYTES.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("cache header limit"));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| std::io::Error::other("cache allocation"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn encode_header(
    key: EntryKey,
    entry: &CachedResponse,
    limits: SimpleClientLimits,
) -> SimpleClientResult<Vec<u8>> {
    stable_url(entry.canonical.as_str())?;
    stable_url(entry.physical.as_str())?;
    media_matches(&entry.content_type, entry.representation)?;
    if entry.body.len() as u64 > limits.max_wire_bytes {
        return Err(SimpleClientError::LimitExceeded(
            SimpleClientLimitKind::WireBytes,
        ));
    }
    if entry.content_type.len() > limits.max_header_bytes
        || entry
            .etag
            .as_ref()
            .is_some_and(|value| value.as_bytes().len() > limits.max_header_bytes)
    {
        return Err(invalid("stored header exceeds its field bound"));
    }
    let stored = entry
        .stored_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| invalid("observation time precedes the cache epoch"))?;
    let header = Header {
        schema: 1,
        entry_key: key.hex(),
        representation: representation(entry.representation).into(),
        content_type: entry.content_type.clone(),
        canonical_document: entry.canonical.as_str().into(),
        request_document: entry.physical.as_str().into(),
        stored_at_seconds: stored.as_secs(),
        stored_at_nanos: stored.subsec_nanos(),
        max_age_seconds: entry.max_age.map(|value| value.as_secs()),
        etag: entry.etag.as_ref().map(|value| value.as_bytes().to_vec()),
        body_size: entry.body.len() as u64,
        body_sha256: pakro_store::Digest::from_sha256_bytes(&entry.body).hex(),
    };
    let mut writer = BoundedHeader { bytes: Vec::new() };
    serde_json::to_writer(&mut writer, &header)
        .map_err(|_| invalid("entry header exceeds its bound"))?;
    let size = (MAGIC.len() as u64)
        .checked_add(8)
        .and_then(|value| value.checked_add(writer.bytes.len() as u64))
        .and_then(|value| value.checked_add(entry.body.len() as u64))
        .ok_or(SimpleClientError::LimitExceeded(
            SimpleClientLimitKind::CacheBytes,
        ))?;
    if size > limits.max_cache_bytes {
        return Err(SimpleClientError::LimitExceeded(
            SimpleClientLimitKind::CacheBytes,
        ));
    }
    Ok(writer.bytes)
}
