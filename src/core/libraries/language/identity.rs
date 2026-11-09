//! Portable input identities. A key describes content, never an opened object.

use crate::libraries::package::identity::{ArtifactId, BuildRequestId};
use pakro_resolver::solve::{Pep440, VersionAdapter};
use pakro_store::Digest as ContentDigest;
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, Copy)]
pub(crate) struct IdentityLimits {
    pub max_field_bytes: usize,
    pub max_collection_items: usize,
    pub max_total_utf8_bytes: usize,
}
impl Default for IdentityLimits {
    fn default() -> Self {
        Self {
            max_field_bytes: 16 * 1024,
            max_collection_items: 65_536,
            max_total_utf8_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum IdentityError {
    #[error("shared identity limits must be positive")]
    InvalidLimits,
    #[error("invalid shared identity field: {0}")]
    InvalidField(&'static str),
    #[error("shared identity resource limit exceeded")]
    Limit,
    #[error("shared identity target differs from its inputs")]
    TargetMismatch,
}

#[derive(Clone, Copy)]
pub(crate) struct TargetIdentityInput<'a> {
    pub os: &'a str,
    pub arch: &'a str,
    /// Explicit libc family/version, or `none` for a target without libc.
    pub libc: &'a str,
    pub abi: &'a str,
    /// Fixed compatibility conditions, supplied without host inference.
    pub compatibility: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TargetIdentity([u8; 32]);
impl TargetIdentity {
    pub(crate) fn new(
        input: TargetIdentityInput<'_>,
        limits: IdentityLimits,
    ) -> Result<Self, IdentityError> {
        let mut hash = IdentityHash::new(b"pakro.shared-target", limits)?;
        if !matches!(input.os, "linux" | "macos") {
            return Err(IdentityError::InvalidField("os"));
        }
        for (field, value) in [
            ("os", input.os),
            ("arch", input.arch),
            ("libc", input.libc),
            ("abi", input.abi),
        ] {
            hash.atom(field, value)?;
        }
        hash.field(&input.compatibility);
        Ok(Self(hash.finish()))
    }
    pub(crate) fn fingerprint(self) -> [u8; 32] {
        self.0
    }
    pub(crate) fn filename(self) -> String {
        format!("target-{}", hex::encode(self.0))
    }
}
impl AsRef<[u8; 32]> for TargetIdentity {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LanguageInput {
    Artifact(ArtifactId),
    CondaArtifact {
        artifact: ArtifactId,
        channel: [u8; 32],
    },
    Build(BuildRequestId),
}

#[derive(Clone, Copy)]
pub(crate) struct LanguageIdentityInput<'a> {
    pub implementation: &'a str,
    pub distribution: &'a str,
    pub exact_version: &'a str,
    pub target: &'a TargetIdentity,
    pub input: LanguageInput,
    pub recipe: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LanguageKey {
    fingerprint: [u8; 32],
    target: [u8; 32],
}
impl LanguageKey {
    /// Only the current Python implementations are admitted. Other ecosystems
    /// must define their exact-version semantics in their own implementation.
    pub(crate) fn new(
        input: LanguageIdentityInput<'_>,
        limits: IdentityLimits,
    ) -> Result<Self, IdentityError> {
        let mut hash = IdentityHash::new(b"pakro.shared-language", limits)?;
        if !matches!(input.implementation, "cpython" | "pypy") {
            return Err(IdentityError::InvalidField("implementation"));
        }
        hash.atom("implementation", input.implementation)?;
        hash.atom("distribution", input.distribution)?;
        let version = hash.python_version(input.exact_version, true)?;
        hash.field(version.as_bytes());
        hash.field(input.target.as_ref());
        match input.input {
            LanguageInput::Artifact(artifact) => {
                hash.field(b"artifact");
                hash.digest(artifact.digest());
            }
            LanguageInput::CondaArtifact { artifact, channel } => {
                hash.field(b"conda-artifact");
                hash.digest(artifact.digest());
                hash.field(&channel);
            }
            LanguageInput::Build(build) => {
                if build.target() != input.target.fingerprint() {
                    return Err(IdentityError::TargetMismatch);
                }
                hash.field(b"build-request");
                hash.field(build.as_ref());
            }
        }
        hash.recipe(input.recipe)?;
        Ok(Self {
            fingerprint: hash.finish(),
            target: input.target.fingerprint(),
        })
    }
    pub(crate) fn fingerprint(self) -> [u8; 32] {
        self.fingerprint
    }
    pub(crate) fn target(self) -> [u8; 32] {
        self.target
    }
    pub(crate) fn filename(self) -> String {
        format!("language-{}", hex::encode(self.fingerprint))
    }
}
impl AsRef<[u8; 32]> for LanguageKey {
    fn as_ref(&self) -> &[u8; 32] {
        &self.fingerprint
    }
}

/// One framing implementation for these pure identity descriptions. Every
/// field is length-prefixed; sets supply their canonical count separately.
/// This is input hashing, not an additional artifact-verification kernel.
pub(crate) struct IdentityHash {
    hash: Sha256,
    limits: IdentityLimits,
    text_bytes: usize,
    collection_items: usize,
}
impl IdentityHash {
    pub(crate) fn new(
        domain: &'static [u8],
        limits: IdentityLimits,
    ) -> Result<Self, IdentityError> {
        if limits.max_field_bytes == 0
            || limits.max_collection_items == 0
            || limits.max_total_utf8_bytes == 0
        {
            return Err(IdentityError::InvalidLimits);
        }
        let mut value = Self {
            hash: Sha256::new(),
            limits,
            text_bytes: 0,
            collection_items: 0,
        };
        value.field(domain);
        value.field(&1_u32.to_le_bytes());
        Ok(value)
    }
    pub(crate) fn field(&mut self, bytes: &[u8]) {
        self.hash.update((bytes.len() as u64).to_le_bytes());
        self.hash.update(bytes);
    }
    pub(crate) fn check_text(
        &mut self,
        field: &'static str,
        value: &str,
        allow_empty: bool,
    ) -> Result<(), IdentityError> {
        if value.len() > self.limits.max_field_bytes {
            return Err(IdentityError::Limit);
        }
        self.text_bytes = self
            .text_bytes
            .checked_add(value.len())
            .filter(|total| *total <= self.limits.max_total_utf8_bytes)
            .ok_or(IdentityError::Limit)?;
        if (!allow_empty && value.is_empty()) || value.chars().any(char::is_control) {
            return Err(IdentityError::InvalidField(field));
        }
        Ok(())
    }
    pub(crate) fn atom(&mut self, field: &'static str, value: &str) -> Result<(), IdentityError> {
        self.check_text(field, value, false)?;
        if !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            || !value
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(IdentityError::InvalidField(field));
        }
        self.field(value.as_bytes());
        Ok(())
    }
    pub(crate) fn python_version(
        &mut self,
        value: &str,
        language: bool,
    ) -> Result<String, IdentityError> {
        self.check_text("exact_version", value, false)?;
        if value.chars().any(char::is_whitespace) {
            return Err(IdentityError::InvalidField("exact_version"));
        }
        let mut version =
            Pep440::parse_version(value).ok_or(IdentityError::InvalidField("exact_version"))?;
        if language && version.release().len() < 3 {
            return Err(IdentityError::InvalidField("exact_version"));
        }
        // PEP 440 considers trailing release zeros equal. Preserve at least
        // three components for a language version and one for a package.
        let minimum = if language { 3 } else { 1 };
        let mut release = version.release().to_vec();
        while release.len() > minimum && release.last() == Some(&0) {
            release.pop();
        }
        version = version.with_release(release);
        let canonical = version.to_string();
        // Normalization may lengthen a spelling; charge the larger spelling.
        if canonical.len() > self.limits.max_field_bytes {
            return Err(IdentityError::Limit);
        }
        self.text_bytes = self
            .text_bytes
            .checked_add(canonical.len().saturating_sub(value.len()))
            .filter(|total| *total <= self.limits.max_total_utf8_bytes)
            .ok_or(IdentityError::Limit)?;
        Ok(canonical)
    }
    pub(crate) fn collection(&mut self, submitted: usize) -> Result<(), IdentityError> {
        self.collection_items = self
            .collection_items
            .checked_add(submitted)
            .filter(|total| *total <= self.limits.max_collection_items)
            .ok_or(IdentityError::Limit)?;
        Ok(())
    }
    pub(crate) fn count(&mut self, count: usize) {
        self.field(&(count as u64).to_le_bytes());
    }
    pub(crate) fn recipe(&mut self, recipe: u32) -> Result<(), IdentityError> {
        if recipe == 0 {
            return Err(IdentityError::InvalidField("recipe"));
        }
        self.field(&recipe.to_le_bytes());
        Ok(())
    }
    pub(crate) fn digest(&mut self, digest: ContentDigest) {
        match digest {
            ContentDigest::Sha256(bytes) => {
                self.field(b"sha256");
                self.field(&bytes);
            }
            ContentDigest::Sha512(bytes) => {
                self.field(b"sha512");
                self.field(&bytes);
            }
        }
    }
    pub(crate) fn finish(self) -> [u8; 32] {
        self.hash.finalize().into()
    }
}

#[cfg(test)]
#[path = "identity/tests.rs"]
mod tests;
