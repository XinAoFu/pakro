//! Actual unified-lock snapshots and scoped file-pin lookup.
//!
//! Data decoded from a lock is not a resolved graph or source observation.

use std::fmt;
use std::path::Path;
use std::time::Instant;

use eyre::{Result, bail, eyre};
use pakro_resolver::solve::{Pep440, VersionAdapter};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};

use super::lock_model::{
    ContentPin, LockModelLimits, PinId, PythonLockModel, PythonLockSeed, RegistryFileRef,
    RegistryFileRefInput,
};
use crate::lockfile::transaction::{LockImage, ProjectLockSession};
pub(crate) mod document;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolutionIntent {
    Locked,
    Resolve,
}

pub(crate) struct BoundPythonLock {
    image: LockImage,
    model: Option<PythonLockModel>,
    session: ProjectLockSession,
    limits: LockModelLimits,
}
impl BoundPythonLock {
    pub(crate) fn acquire(path: &Path, limits: LockModelLimits, deadline: Instant) -> Result<Self> {
        limits.validate()?;
        let session = ProjectLockSession::acquire(path, limits.max_document_bytes, deadline)?;
        let image = session.snapshot()?;
        let model = match image.bytes() {
            None => None,
            Some(bytes) => decode_root(bytes, limits)?,
        };
        session.recheck(&image)?;
        Ok(Self {
            image,
            model,
            session,
            limits,
        })
    }

    pub(crate) fn source_build(
        &self,
        pin: PinId,
    ) -> Option<&crate::packages::python_environment::model::SourceBuildRecord> {
        self.model
            .as_ref()
            .and_then(|model| model.source_build(pin))
    }

    pub(crate) fn model(&self) -> Option<&PythonLockModel> {
        self.model.as_ref()
    }

    pub(crate) fn recheck(&self) -> Result<()> {
        self.session.recheck(&self.image)
    }

    pub(crate) fn existing_pin(&self, id: PinId) -> Result<Option<ExistingFileSlot<'_>>> {
        self.recheck()?;
        let Some(pin) = self
            .model
            .as_ref()
            .and_then(|model| model.pins().iter().find(|pin| pin.id() == id))
        else {
            return Ok(None);
        };
        // Reuse the ordinary lookup's complete semantic-file conflict check,
        // then retain the specifically requested actual pin, not its first
        // equivalent version spelling in portable-ID sort order.
        self.expected_pin(pin.file())?;
        Ok(Some(ExistingFileSlot { owner: self, pin }))
    }

    pub(crate) fn check_limits(&self, limits: LockModelLimits) -> Result<()> {
        limits.validate()?;
        self.recheck()?;
        if self
            .image
            .bytes()
            .is_some_and(|bytes| bytes.len() > limits.max_document_bytes)
        {
            bail!("Python lock document exceeds the current byte budget");
        }
        if let Some(model) = &self.model {
            model.check_limits(limits)?;
        }
        self.recheck()
    }

    pub(crate) fn lookup<'a>(
        &'a self,
        file: &RegistryFileRef,
        intent: ResolutionIntent,
    ) -> Result<FilePinLookup<'a>> {
        let file = RegistryFileRef::new(
            RegistryFileRefInput {
                registry_id: file.registry_id(),
                canonical_base_fingerprint: *file.canonical_base_fingerprint(),
                project: file.project(),
                version: file.version(),
                filename: file.filename(),
                kind: file.kind(),
            },
            self.limits,
        )?;
        self.session.recheck(&self.image)?;
        let expected = self.expected_pin(&file)?;
        match (expected, intent) {
            (Some(pin), _) => Ok(FilePinLookup::Existing(ExistingFileSlot {
                owner: self,
                pin,
            })),
            (None, ResolutionIntent::Locked) => Ok(FilePinLookup::Missing),
            (None, ResolutionIntent::Resolve) => {
                Ok(FilePinLookup::Fresh(FreshFileSlot { owner: self, file }))
            }
        }
    }
    fn expected_pin(&self, file: &RegistryFileRef) -> Result<Option<&ContentPin>> {
        expected_pin(self.model.as_ref(), file)
    }
}

/// The same semantic-file conflict kernel serves bound source sessions and
/// pure readonly project observations. It does not grant source authority.
pub(crate) fn expected_pin<'a>(
    model: Option<&'a PythonLockModel>,
    file: &RegistryFileRef,
) -> Result<Option<&'a ContentPin>> {
    let mut expected: Option<&ContentPin> = None;
    if let Some(model) = model {
        for pin in model
            .pins()
            .iter()
            .filter(|pin| same_file(pin.file(), file))
        {
            if let Some(previous) = expected {
                if previous.digest() != pin.digest() || previous.size() != pin.size() {
                    bail!("Python lock has conflicting pins for the selected file");
                }
            } else {
                expected = Some(pin);
            }
        }
    }
    Ok(expected)
}

pub(crate) fn same_file(left: &RegistryFileRef, right: &RegistryFileRef) -> bool {
    left.registry_id() == right.registry_id()
        && left.canonical_base_fingerprint() == right.canonical_base_fingerprint()
        && left.project() == right.project()
        && left.filename() == right.filename()
        && left.kind() == right.kind()
        && match (
            Pep440::parse_version(left.version()),
            Pep440::parse_version(right.version()),
        ) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
}

struct RootSeed(LockModelLimits);
impl<'de> DeserializeSeed<'de> for RootSeed {
    type Value = Option<PythonLockModel>;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        struct RootVisitor(LockModelLimits);
        impl<'de> Visitor<'de> for RootVisitor {
            type Value = Option<PythonLockModel>;
            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a unified pakro lock document")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                use serde::de::Error as _;
                let mut revision = None;
                let mut model = None;
                let mut seen_python = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "lockfile_version" => {
                            if revision.is_some() {
                                return Err(M::Error::custom("duplicate lock revision"));
                            }
                            revision = Some(map.next_value::<u32>()?);
                        }
                        "python" => {
                            if seen_python {
                                return Err(M::Error::custom("duplicate Python section"));
                            }
                            seen_python = true;
                            model = Some(map.next_value_seed(PythonLockSeed::new(self.0))?);
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                let revision = revision.unwrap_or(0);
                if revision > crate::lockfile::CURRENT_LOCKFILE_VERSION
                    || (seen_python && revision < 4)
                {
                    return Err(M::Error::custom("unsupported unified Python lock revision"));
                }
                Ok(model)
            }
        }
        deserializer.deserialize_map(RootVisitor(self.0))
    }
}
/// Pure bounded decode for an already observed readonly lock document.
/// This does not create a lock session or authorize source acquisition.
pub(crate) fn decode_root(
    bytes: &[u8],
    limits: LockModelLimits,
) -> Result<Option<PythonLockModel>> {
    limits.validate()?;
    eyre::ensure!(
        bytes.len() <= limits.max_document_bytes,
        "Python lock document byte limit exceeded"
    );
    let raw = std::str::from_utf8(bytes).map_err(|_| eyre!("invalid unified lock encoding"))?;
    let deserializer =
        toml::de::Deserializer::parse(raw).map_err(|_| eyre!("invalid unified lock document"))?;
    // TOML errors retain source text. Never propagate that third-party error.
    RootSeed(limits)
        .deserialize(deserializer)
        .map_err(|_| eyre!("invalid unified Python lock projection"))
}
impl fmt::Debug for BoundPythonLock {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("BoundPythonLock")
            .field("has_python", &self.model.is_some())
            .finish_non_exhaustive()
    }
}

pub(crate) enum FilePinLookup<'a> {
    Existing(ExistingFileSlot<'a>),
    Fresh(FreshFileSlot<'a>),
    Missing,
}

/// Existing expected bytes are authorized by the actual pinned lock image.
/// A pure ContentPin decoded elsewhere cannot construct this owner.
pub(crate) struct ExistingFileSlot<'a> {
    owner: &'a BoundPythonLock,
    pin: &'a ContentPin,
}
impl ExistingFileSlot<'_> {
    pub(crate) fn pin(&self) -> &ContentPin {
        self.pin
    }
    pub(crate) fn recheck(&self) -> Result<()> {
        self.owner.session.recheck(&self.owner.image)
    }
    pub(crate) fn recheck_for(&self, owner: &BoundPythonLock) -> Result<()> {
        if !std::ptr::eq(self.owner, owner) {
            bail!("Python file slot belongs to a different project lock owner");
        }
        self.recheck()
    }
}
impl fmt::Debug for ExistingFileSlot<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("ExistingFileSlot(<bound expected pin>)")
    }
}
impl fmt::Debug for FilePinLookup<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match self {
            Self::Existing(_) => "FilePinLookup::Existing",
            Self::Fresh(_) => "FilePinLookup::Fresh",
            Self::Missing => "FilePinLookup::Missing",
        })
    }
}

/// A borrow of a live, actually read project lock with no existing exact pin.
pub(crate) struct FreshFileSlot<'a> {
    owner: &'a BoundPythonLock,
    file: RegistryFileRef,
}
impl FreshFileSlot<'_> {
    pub(crate) fn file(&self) -> &RegistryFileRef {
        &self.file
    }

    pub(crate) fn recheck(&self) -> Result<()> {
        self.owner.session.recheck(&self.owner.image)
    }
    pub(crate) fn recheck_for(&self, owner: &BoundPythonLock) -> Result<()> {
        if !std::ptr::eq(self.owner, owner) {
            bail!("Python file slot belongs to a different project lock owner");
        }
        self.recheck()
    }
}
impl fmt::Debug for FreshFileSlot<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("FreshFileSlot(<bound project snapshot>)")
    }
}

#[cfg(test)]
mod tests;
