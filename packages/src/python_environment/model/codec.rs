//! Versioned pure catalog metadata. This grants no filesystem authority.

use super::*;

#[path = "codec/graph.rs"]
mod graph;

#[path = "codec/decode.rs"]
mod decode;
#[path = "codec/encode.rs"]
mod encode;
#[path = "codec/read.rs"]
mod read;
#[path = "codec/state.rs"]
mod state;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CatalogCodecLimits {
    pub model: ModelLimits,
    pub max_document_bytes: usize,
}
impl Default for CatalogCodecLimits {
    fn default() -> Self {
        Self {
            model: ModelLimits::default(),
            max_document_bytes: 16 * 1024 * 1024,
        }
    }
}

impl CatalogModel {
    pub(crate) fn encode_catalog(&self, limits: CatalogCodecLimits) -> Result<Vec<u8>> {
        encode::encode(self, limits)
    }

    pub(crate) fn decode_catalog(bytes: &[u8], limits: CatalogCodecLimits) -> Result<Self> {
        decode::decode(bytes, limits)
    }
}

#[cfg(test)]
#[path = "codec/tests.rs"]
mod tests;
