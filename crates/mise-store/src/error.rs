use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("unknown ecosystem: {0}")]
    UnknownEco(String),
    #[error("invalid identity: {0}")]
    InvalidIdentity(String),
    #[error("invalid digest: {0}")]
    InvalidDigest(String),
    #[error("unsupported archive format (expected zip or gzip magic bytes)")]
    InvalidArchive,
    #[error("digest mismatch for {identity}: expected sha256-{expected}, got sha256-{actual}")]
    DigestMismatch {
        identity: String,
        expected: String,
        actual: String,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;
