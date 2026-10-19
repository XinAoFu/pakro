//! Raw bytes, package instances, build requests, and layout candidates stay distinct.

use std::collections::BTreeMap;

use pakro_index::python_source::{NormalizedPythonName, PythonRegistryId};
use pakro_store::Digest;

use crate::libraries::language::identity::{
    IdentityError, IdentityHash, IdentityLimits, LanguageKey, TargetIdentity,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ArtifactId(Digest);
impl ArtifactId {
    pub(crate) fn new(digest: Digest) -> Self {
        Self(digest)
    }
    pub(crate) fn digest(self) -> Digest {
        self.0
    }
    pub(crate) fn filename(self) -> String {
        self.0.to_string()
    }
}

/// Actual output evidence, separate from the inputs used to request a build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct OutputDigest(Digest);
impl OutputDigest {
    pub(crate) fn new(digest: Digest) -> Self {
        Self(digest)
    }
    pub(crate) fn digest(self) -> Digest {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SourceIdentity([u8; 32]);
impl SourceIdentity {
    /// The fingerprint describes all fixed portable source facts (including
    /// revisions, subdirectories, and trust requirements where applicable).
    /// Neither this value nor an ArtifactId proves access authorization.
    pub(crate) fn new(
        logical_id: &str,
        source_fingerprint: [u8; 32],
        limits: IdentityLimits,
    ) -> Result<Self, IdentityError> {
        let mut hash = IdentityHash::new(b"pakro.shared-source", limits)?;
        hash.check_text("logical_source", logical_id, false)?;
        let logical_id = PythonRegistryId::new(logical_id)
            .map_err(|_| IdentityError::InvalidField("logical_source"))?;
        hash.field(logical_id.as_str().as_bytes());
        hash.field(&source_fingerprint);
        Ok(Self(hash.finish()))
    }
    pub(crate) fn fingerprint(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackageRole {
    Runtime,
    Build,
}

#[derive(Clone, Copy)]
pub(crate) struct PackageIdentityInput<'a> {
    pub name: &'a str,
    pub exact_version: &'a str,
    pub source: &'a SourceIdentity,
    pub role: PackageRole,
    pub target: &'a TargetIdentity,
    pub dependency_context: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PackageInstanceId {
    fingerprint: [u8; 32],
    target: [u8; 32],
}
impl PackageInstanceId {
    pub(crate) fn python(
        input: PackageIdentityInput<'_>,
        limits: IdentityLimits,
    ) -> Result<Self, IdentityError> {
        let mut hash = IdentityHash::new(b"pakro.shared-package-instance", limits)?;
        hash.field(b"python");
        hash.check_text("package_name", input.name, false)?;
        let name = NormalizedPythonName::new(input.name)
            .map_err(|_| IdentityError::InvalidField("package_name"))?;
        let version = hash.python_version(input.exact_version, false)?;
        hash.field(name.as_str().as_bytes());
        hash.field(version.as_bytes());
        hash.field(&input.source.fingerprint());
        hash.field(match input.role {
            PackageRole::Runtime => b"runtime",
            PackageRole::Build => b"build",
        });
        hash.field(input.target.as_ref());
        hash.field(&input.dependency_context);
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
        format!("package-{}", hex::encode(self.fingerprint))
    }
}
impl AsRef<[u8; 32]> for PackageInstanceId {
    fn as_ref(&self) -> &[u8; 32] {
        &self.fingerprint
    }
}

#[derive(Clone, Copy)]
pub(crate) struct BuildRequestInput<'a> {
    pub source: ArtifactId,
    pub host: &'a TargetIdentity,
    pub target: &'a TargetIdentity,
    pub build_dependencies: &'a [PackageInstanceId],
    pub toolchain: ArtifactId,
    pub configuration: &'a BTreeMap<String, String>,
    pub recipe: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BuildRequestId {
    fingerprint: [u8; 32],
    target: [u8; 32],
}
impl BuildRequestId {
    pub(crate) fn new(
        input: BuildRequestInput<'_>,
        limits: IdentityLimits,
    ) -> Result<Self, IdentityError> {
        let mut hash = IdentityHash::new(b"pakro.shared-build-request", limits)?;
        hash.collection(input.build_dependencies.len())?;
        hash.collection(input.configuration.len())?;
        hash.digest(input.source.digest());
        hash.field(input.host.as_ref());
        hash.field(input.target.as_ref());
        // Bound the submitted list before sorting or collapsing duplicates.
        let mut dependencies = input.build_dependencies.to_vec();
        dependencies.sort_unstable();
        dependencies.dedup();
        hash.count(dependencies.len());
        for dependency in dependencies {
            hash.field(dependency.as_ref());
        }
        hash.digest(input.toolchain.digest());
        hash.count(input.configuration.len());
        for (key, value) in input.configuration {
            hash.atom("configuration_key", key)?;
            hash.check_text("configuration_value", value, true)?;
            // Build configuration is portable, fixed data. Prefix allocation
            // and local paths belong to receipts/bindings, not this identity.
            if value.contains(['/', '\\']) || value.as_bytes().get(1) == Some(&b':') {
                return Err(IdentityError::InvalidField("configuration_value"));
            }
            hash.field(value.as_bytes());
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
        format!("build-{}", hex::encode(self.fingerprint))
    }
}
impl AsRef<[u8; 32]> for BuildRequestId {
    fn as_ref(&self) -> &[u8; 32] {
        &self.fingerprint
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SharedLayoutInput<'a> {
    pub language: LanguageKey,
    pub graph: [u8; 32],
    pub build_requests: &'a [BuildRequestId],
    pub target: &'a TargetIdentity,
    pub recipe: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SharedLayoutKey {
    fingerprint: [u8; 32],
    target: [u8; 32],
}
impl SharedLayoutKey {
    pub(crate) fn new(
        input: SharedLayoutInput<'_>,
        limits: IdentityLimits,
    ) -> Result<Self, IdentityError> {
        let mut hash = IdentityHash::new(b"pakro.shared-layout", limits)?;
        hash.collection(input.build_requests.len())?;
        let target = input.target.fingerprint();
        if input.language.target() != target
            || input
                .build_requests
                .iter()
                .any(|build| build.target() != target)
        {
            return Err(IdentityError::TargetMismatch);
        }
        hash.field(input.language.as_ref());
        hash.field(&input.graph);
        let mut builds = input.build_requests.to_vec();
        builds.sort_unstable();
        builds.dedup();
        hash.count(builds.len());
        for build in builds {
            hash.field(build.as_ref());
        }
        hash.field(input.target.as_ref());
        hash.recipe(input.recipe)?;
        Ok(Self {
            fingerprint: hash.finish(),
            target,
        })
    }
    pub(crate) fn fingerprint(self) -> [u8; 32] {
        self.fingerprint
    }
    pub(crate) fn target(self) -> [u8; 32] {
        self.target
    }
    pub(crate) fn filename(self) -> String {
        format!("layout-{}", hex::encode(self.fingerprint))
    }
}
impl AsRef<[u8; 32]> for SharedLayoutKey {
    fn as_ref(&self) -> &[u8; 32] {
        &self.fingerprint
    }
}

#[cfg(test)]
#[path = "identity/tests.rs"]
mod tests;
