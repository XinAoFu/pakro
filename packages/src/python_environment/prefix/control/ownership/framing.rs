//! Application-owned header encoding; opaque frame IO lives in pakro-store.

use super::*;
use pakro_store::control::ownership::hash;

pub(super) fn header_bytes(header: &Header) -> [u8; HEADER_BYTES as usize] {
    let mut bytes = [0; HEADER_BYTES as usize];
    let mut offset = 0;
    let mut push = |source: &[u8]| {
        bytes[offset..offset + source.len()].copy_from_slice(source);
        offset += source.len();
    };
    push(MAGIC);
    push(header.nonce.as_bytes());
    push(&header.journal_sha256);
    push(header.identity.allocation.as_bytes());
    for pair in [
        header.identity.store,
        header.identity.generation,
        header.identity.leases,
        header.identity.build,
        header.identity.lock,
    ] {
        for number in pair {
            push(&number.to_be_bytes());
        }
    }
    let end = bytes.len();
    let digest = hash(&bytes[..end - 32]);
    bytes[end - 32..].copy_from_slice(&digest);
    bytes
}
pub(super) fn header_hash(header: &Header) -> [u8; 32] {
    let bytes = header_bytes(header);
    bytes[bytes.len() - 32..]
        .try_into()
        .expect("fixed header digest")
}
