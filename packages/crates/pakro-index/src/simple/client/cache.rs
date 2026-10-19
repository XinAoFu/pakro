//! Private, bounded, single-file raw metadata cache entries.
//! The local envelope is not a portable lock or an upstream integrity proof.

use super::*;
use crate::python_source::SimpleRepresentation;
use sha2::{Digest as _, Sha256};

mod codec;
mod store;

pub(super) const MAGIC: &[u8] = b"pakro-simple-cache-v1\0";
const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;

pub(super) struct CacheRequest<'a> {
    pub(super) registry: &'a PythonRegistry,
    pub(super) auth_scope: &'a AuthScopeKey,
    pub(super) transport: TransportRulesFingerprint,
    pub(super) project: &'a NormalizedPythonName,
    pub(super) canonical: &'a Url,
    pub(super) physical: &'a Url,
}
#[derive(Clone, Copy)]
pub(super) struct EntryKey([u8; 32]);
impl EntryKey {
    pub(super) fn hex(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
/// One atomic publication slot for this fixed Accept negotiation. The
/// envelope's EntryKey separately binds the actual returned representation.
#[derive(Clone, Copy)]
pub(super) struct SlotKey([u8; 32]);
impl SlotKey {
    pub(super) fn hex(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
impl CacheRequest<'_> {
    pub(super) fn slot_key(&self) -> SlotKey {
        let mut digest = Sha256::new();
        let keys = [
            self.key(SimpleRepresentation::JsonV1),
            self.key(SimpleRepresentation::HtmlV1),
            self.key(SimpleRepresentation::LegacyHtml),
        ];
        for value in [
            b"pakro.python-simple-negotiation-slot/v1".as_slice(),
            keys[0].0.as_slice(),
            keys[1].0.as_slice(),
            keys[2].0.as_slice(),
            http::ACCEPT_VALUE.as_bytes(),
        ] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        SlotKey(digest.finalize().into())
    }
    pub(super) fn key(&self, representation: SimpleRepresentation) -> EntryKey {
        let mut digest = Sha256::new();
        let scope = self
            .registry
            .cache_key(self.auth_scope, &self.transport, representation);
        for value in [
            b"pakro.python-simple-entry/v1".as_slice(),
            scope.as_bytes().as_slice(),
            self.project.as_str().as_bytes(),
            self.canonical.as_str().as_bytes(),
            self.physical.as_str().as_bytes(),
            b"simple-http-codec/v1".as_slice(),
        ] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        EntryKey(digest.finalize().into())
    }
}

pub(super) struct CachedResponse {
    pub(super) representation: SimpleRepresentation,
    pub(super) content_type: String,
    pub(super) canonical: Url,
    pub(super) physical: Url,
    pub(super) stored_at: SystemTime,
    pub(super) max_age: Option<Duration>,
    pub(super) etag: Option<HeaderValue>,
    pub(super) body: Vec<u8>,
}
impl fmt::Debug for CachedResponse {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("CachedResponse")
            .field("representation", &self.representation)
            .field("body_bytes", &self.body.len())
            .field("has_etag", &self.etag.is_some())
            .finish_non_exhaustive()
    }
}
#[cfg(test)]
pub(super) fn path(root: &Path, key: SlotKey) -> PathBuf {
    root.join("simple-v1").join(format!("{}.page", key.hex()))
}

pub(super) fn read(
    root: &Path,
    request: &CacheRequest<'_>,
    limits: SimpleClientLimits,
) -> SimpleClientResult<Option<CachedResponse>> {
    let Some(store) = store::Store::open(root, false)? else {
        return Ok(None);
    };
    let Some(mut file) = store.read(request.slot_key())? else {
        return Ok(None);
    };
    codec::read(&mut file, request, limits).map(Some)
}

pub(super) fn publish(
    root: &Path,
    request: &CacheRequest<'_>,
    entry: &CachedResponse,
    response_headers: &reqwest::header::HeaderMap,
    limits: SimpleClientLimits,
) -> SimpleClientResult<()> {
    if entry.canonical.query().is_some()
        || entry.physical.query().is_some()
        || !cacheable(response_headers)
    {
        if let Some(store) = store::Store::open(root, false)? {
            store.remove(request.slot_key())?;
        }
        return Ok(());
    }
    let key = request.key(entry.representation);
    let header = codec::encode_header(key, entry, limits)?;
    let store = store::Store::open(root, true)?.ok_or(SimpleClientError::Io)?;
    store.write(request.slot_key(), &header, &entry.body)
}

pub(super) fn is_fresh(entry: &CachedResponse, policy: SimpleFetchPolicy) -> bool {
    if policy.refresh {
        return false;
    }
    let Ok(age) = policy.now.duration_since(entry.stored_at) else {
        return false;
    };
    let maximum = entry
        .max_age
        .map_or(policy.fresh_for, |value| value.min(policy.fresh_for));
    age < maximum
}

fn cacheable(headers: &reqwest::header::HeaderMap) -> bool {
    for value in headers.get_all(reqwest::header::CACHE_CONTROL) {
        let Ok(value) = value.to_str() else {
            return false;
        };
        if value.split(',').any(|directive| {
            directive
                .trim()
                .split('=')
                .next()
                .is_some_and(|name| name.trim().eq_ignore_ascii_case("no-store"))
        }) {
            return false;
        }
    }
    for value in headers.get_all(reqwest::header::VARY) {
        let Ok(value) = value.to_str() else {
            return false;
        };
        if value.split(',').any(|name| {
            !matches!(
                name.trim().to_ascii_lowercase().as_str(),
                "accept" | "accept-encoding" | "authorization"
            )
        }) {
            return false;
        }
    }
    true
}
pub(super) fn max_age(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let mut result = None;
    for value in headers.get_all(reqwest::header::CACHE_CONTROL) {
        let value = value.to_str().ok()?;
        for directive in value.split(',') {
            if directive.trim().eq_ignore_ascii_case("no-cache") {
                return Some(Duration::ZERO);
            }
            let Some((name, value)) = directive.split_once('=') else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case("max-age") {
                let value = value.trim().trim_matches('"').parse::<u64>().ok()?;
                result = Some(result.map_or(value, |old: u64| old.min(value)));
            }
        }
    }
    result.map(Duration::from_secs)
}
