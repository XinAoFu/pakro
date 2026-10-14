//! Exact artifact sources shared by native runtime and package adapters.

use std::fmt;
use std::io::Write;
use std::path::Path;

use mise_store::Digest;
use sha2::{Digest as Sha2Digest, Sha256, Sha512};
use tempfile::NamedTempFile;

#[derive(Clone)]
pub struct SourceArtifact {
    filename: String,
    url: reqwest::Url,
    digest: Digest,
}

impl SourceArtifact {
    pub fn new(
        filename: impl Into<String>,
        url: &str,
        digest: Digest,
    ) -> Result<Self, ArtifactError> {
        let filename = filename.into();
        if filename.is_empty()
            || matches!(filename.as_str(), "." | "..")
            || filename.chars().any(|c| matches!(c, '/' | '\\' | '\0'))
        {
            return Err(ArtifactError::InvalidSource("expected a single filename"));
        }
        let url = reqwest::Url::parse(url)
            .map_err(|_| ArtifactError::InvalidSource("invalid artifact URL"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(ArtifactError::InvalidSource(
                "expected an HTTP(S) artifact URL",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ArtifactError::InvalidSource("use a credential reference"));
        }
        if url.fragment().is_some() {
            return Err(ArtifactError::InvalidSource("pass the digest separately"));
        }
        Ok(Self {
            filename,
            url,
            digest,
        })
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn url(&self) -> &reqwest::Url {
        &self.url
    }

    pub fn digest(&self) -> Digest {
        self.digest
    }
}

impl fmt::Debug for SourceArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceArtifact")
            .field("filename", &self.filename)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("invalid artifact source: {0}")]
    InvalidSource(&'static str),
    #[error("artifact download requires network access but it is disabled")]
    Offline,
    #[error("artifact HTTP request failed")]
    Http(#[source] reqwest::Error),
    #[error("artifact HTTP status is not successful: {0}")]
    HttpStatus(reqwest::StatusCode),
    #[error("artifact I/O failed")]
    Io(#[from] std::io::Error),
    #[error("artifact digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        expected: Box<Digest>,
        actual: Box<Digest>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    Allowed,
    Offline,
}

/// An HTTP transport that preserves artifact bytes regardless of feature unification.
///
/// Redirects follow the supplied builder policy. The final response must be successful,
/// and its raw body must match the source digest; automatic content decoding is disabled.
pub struct ArtifactTransport {
    client: reqwest::Client,
}

impl ArtifactTransport {
    pub fn new(builder: reqwest::ClientBuilder) -> Result<Self, ArtifactError> {
        let client = builder
            .no_gzip()
            .no_brotli()
            .no_zstd()
            .no_deflate()
            .build()
            .map_err(http_error)?;
        Ok(Self { client })
    }
}

/// A temporary artifact whose complete bytes matched the source digest when returned.
///
/// The path is valid while this handle is alive. Consumers importing it into an immutable
/// store must maintain or recheck integrity; exposing a path does not make it immutable.
#[derive(Debug)]
pub struct VerifiedDownload {
    file: NamedTempFile,
    digest: Digest,
    size: u64,
}

impl VerifiedDownload {
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    pub fn digest(&self) -> Digest {
        self.digest
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

enum Hasher {
    Sha256(Sha256),
    Sha512(Sha512),
}

impl Hasher {
    fn new(expected: Digest) -> Self {
        match expected {
            Digest::Sha256(_) => Self::Sha256(Sha256::new()),
            Digest::Sha512(_) => Self::Sha512(Sha512::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(bytes),
            Self::Sha512(hasher) => hasher.update(bytes),
        }
    }

    fn finish(self) -> Digest {
        match self {
            Self::Sha256(hasher) => Digest::Sha256(hasher.finalize().into()),
            Self::Sha512(hasher) => Digest::Sha512(hasher.finalize().into()),
        }
    }
}

fn http_error(error: reqwest::Error) -> ArtifactError {
    ArtifactError::Http(error.without_url())
}

/// Downloads an exact artifact into an existing temporary directory.
///
/// Offline mode refuses before sending a request or creating a file; this primitive
/// does not look up caches. Successful downloads are streamed to disk and verified
/// against the raw response body. Errors and dropped handles clean up temporary files.
pub async fn download_verified(
    transport: &ArtifactTransport,
    source: &SourceArtifact,
    policy: NetworkPolicy,
    temp_root: &Path,
) -> Result<VerifiedDownload, ArtifactError> {
    if policy == NetworkPolicy::Offline {
        return Err(ArtifactError::Offline);
    }
    let mut response = transport
        .client
        .get(source.url.clone())
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(http_error)?
        .error_for_status()
        .map_err(http_error)?;
    if !response.status().is_success() {
        return Err(ArtifactError::HttpStatus(response.status()));
    }
    let mut file = NamedTempFile::new_in(temp_root)?;
    let mut hasher = Hasher::new(source.digest);
    let mut size = 0_u64;
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        file.write_all(&chunk)?;
        hasher.update(&chunk);
        size += chunk.len() as u64;
    }
    let actual = hasher.finish();
    if actual != source.digest {
        return Err(ArtifactError::DigestMismatch {
            expected: Box::new(source.digest),
            actual: Box::new(actual),
        });
    }
    file.flush()?;
    Ok(VerifiedDownload {
        file,
        digest: actual,
        size,
    })
}
