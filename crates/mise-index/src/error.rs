use thiserror::Error;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid registry response for {package}: {reason}")]
    BadResponse { package: String, reason: String },
    #[error("invalid requirement string {0:?}")]
    BadRequirement(String, #[source] pep508_rs::Pep508Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, IndexError>;
