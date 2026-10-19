//! Content-addressed package store for pakro: package identity, disk layout,
//! deduplication across projects, and reference-counted garbage collection.
//!
//! A package's identity is `ecosystem:name@exact-version[+runtime]/platform/digest`;
//! the store lays entries out under that identity so two projects requesting the
//! same artifact share one directory.

#[cfg(unix)]
pub mod control;

mod identity;
mod store;

pub use identity::{Arch, Digest, Eco, Libc, Os, PackageIdentity, Platform, RuntimeId};
pub use store::{GcReport, Refs, Store, entry_dir, gc, hardlink_file, hardlink_tree};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("unknown ecosystem: {0}")]
    UnknownEco(String),
    #[error("invalid identity: {0}")]
    InvalidIdentity(String),
    #[error("invalid digest: {0}")]
    InvalidDigest(String),
    #[error("digest mismatch for {identity}: expected sha256-{expected}, got sha256-{actual}")]
    DigestMismatch {
        identity: String,
        expected: String,
        actual: String,
    },
    #[error("unsupported archive format (expected zip or gzip magic bytes)")]
    InvalidArchive,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;
