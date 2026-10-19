//! Bounded opaque ownership frames. These bytes grant no cleanup authority.

use super::*;

#[path = "ownership/events.rs"]
pub(crate) mod events;
use crate::packages::python_environment::journal::{JournalLimits, JournalRecord};
use lease::{GenerationIdentity, GenerationIdentityFacts};
pub(crate) use pakro_store::control::ownership::{
    FrameLimits as OwnershipStreamLimits, FrameReceipt as OwnershipAppendReceipt,
    FrameRecord as OwnershipRecord,
};

#[path = "ownership/binding.rs"]
mod binding;
#[path = "ownership/framing.rs"]
mod framing;
#[path = "ownership/read.rs"]
mod read;
#[path = "ownership/recovery.rs"]
mod recovery;

const MAGIC: &[u8] = b"pakro-ownership-v1\0";
const HEADER_BYTES: u64 = MAGIC.len() as u64 + 16 + 32 + 34 + 5 * 16 + 32;

#[derive(Clone, PartialEq, Eq)]
struct Header {
    nonce: uuid::Uuid,
    journal_sha256: [u8; 32],
    identity: GenerationIdentityFacts,
    sha256: [u8; 32],
}

struct BoundFiles {
    store: File,
    control: File,
    parent: File,
    file: File,
    identity: Option<GenerationIdentity>,
    uid: u32,
    observed: std::fs::Metadata,
}

pub(crate) struct OwnershipStream {
    binding: Option<BoundFiles>,
    header: Header,
    limits: OwnershipStreamLimits,
    next_sequence: u64,
    head: [u8; 32],
    total_bytes: u64,
    poisoned: bool,
}
impl std::fmt::Debug for OwnershipStream {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("OwnershipStream")
            .field("nonce", &self.header.nonce)
            .field("next_sequence", &self.next_sequence)
            .field("total_bytes", &self.total_bytes)
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

pub(crate) struct OwnershipLog {
    header: Header,
    records: Vec<OwnershipRecord>,
    total_bytes: u64,
    read_pin: File,
    observed: std::fs::Metadata,
}
impl OwnershipLog {
    pub(crate) fn nonce(&self) -> uuid::Uuid {
        self.header.nonce
    }
    pub(crate) fn journal_sha256(&self) -> &[u8; 32] {
        &self.header.journal_sha256
    }
    pub(crate) fn identity(&self) -> &GenerationIdentityFacts {
        &self.header.identity
    }
    pub(crate) fn records(&self) -> &[OwnershipRecord] {
        &self.records
    }
    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
    pub(crate) fn head(&self) -> &[u8; 32] {
        self.records
            .last()
            .map_or(&self.header.sha256, OwnershipRecord::chain_sha256)
    }
}
impl std::fmt::Debug for OwnershipLog {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("OwnershipLog")
            .field("nonce", &self.header.nonce)
            .field("records", &self.records.len())
            .field("total_bytes", &self.total_bytes)
            .finish_non_exhaustive()
    }
}

impl StoreControl<'_> {
    pub(in super::super) fn create_observer_stream(
        &mut self,
        journal: &JournalRecord,
        prefix: &UnpublishedPrefix,
        journal_limits: JournalLimits,
        limits: OwnershipStreamLimits,
    ) -> Result<(OwnershipStream, ControlLimits)> {
        let stream = self.create_ownership_stream(journal, prefix, journal_limits, limits)?;
        Ok((stream, self.limits))
    }

    pub(crate) fn create_ownership_stream(
        &mut self,
        journal: &JournalRecord,
        prefix: &UnpublishedPrefix,
        journal_limits: JournalLimits,
        limits: OwnershipStreamLimits,
    ) -> Result<OwnershipStream> {
        let limits = binding::limits(self, limits)?;
        let header = binding::header(self, journal, journal_limits)?;
        let identity = prefix.generation_identity()?;
        ensure!(
            identity.facts()? == header.identity,
            "ownership stream prefix and journal identity differ"
        );
        let name = filename(header.nonce);
        self.validate_name(&name)?;
        let parent = &self.namespaces[ControlNamespace::Ownership.index()];
        let mut file = File::from(openat(
            parent,
            name.as_str(),
            OFlag::O_WRONLY
                | OFlag::O_APPEND
                | OFlag::O_CREAT
                | OFlag::O_EXCL
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        fchmod(&file, Mode::from_bits_truncate(0o600))?;
        self.validate_file(&file)?;
        ensure_entry_identity(parent, name.as_str(), FileIdentity::of(&file)?)?;
        file.write_all(&framing::header_bytes(&header))?;
        file.flush()?;
        file.sync_all()?;
        parent.sync_all()?;
        self.check_binding()?;
        let observed = file.metadata()?;
        let binding = BoundFiles {
            store: self.store.root.try_clone()?,
            control: self.root.try_clone()?,
            parent: parent.try_clone()?,
            file,
            identity: Some(identity),
            uid: self.store.expected_uid,
            observed,
        };
        binding.check(prefix, &header, HEADER_BYTES, true)?;
        let head = header.sha256;
        Ok(OwnershipStream {
            binding: Some(binding),
            header,
            limits,
            next_sequence: 1,
            head,
            total_bytes: HEADER_BYTES,
            poisoned: false,
        })
    }

    pub(crate) fn read_ownership_stream(
        &self,
        journal: &JournalRecord,
        journal_limits: JournalLimits,
        limits: OwnershipStreamLimits,
    ) -> Result<Option<OwnershipLog>> {
        read::read(self, journal, journal_limits, limits)
    }
}

impl OwnershipStream {
    pub(crate) fn next_sequence(&self) -> Result<u64> {
        ensure!(!self.poisoned, "ownership stream is poisoned");
        Ok(self.next_sequence)
    }

    pub(crate) fn append(
        &mut self,
        prefix: &UnpublishedPrefix,
        payload: &[u8],
    ) -> Result<OwnershipAppendReceipt> {
        let result = self.append_inner(payload, |binding, header, total, observed| {
            binding.check(prefix, header, total, observed)
        });
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    pub(in super::super) fn append_cleanup(
        &mut self,
        witness: &super::super::observer::cleanup::CleanupWitness<'_, '_, '_>,
        payload: &[u8],
    ) -> Result<OwnershipAppendReceipt> {
        let result = self.append_inner(payload, |binding, header, total, observed| {
            witness.check_stream(header.nonce, &header.identity, &binding.store)?;
            binding.check_io(header, total, observed)
        });
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn append_inner(
        &mut self,
        payload: &[u8],
        check: impl Fn(&BoundFiles, &Header, u64, bool) -> Result<()>,
    ) -> Result<OwnershipAppendReceipt> {
        ensure!(!self.poisoned, "ownership stream is poisoned");
        let binding = self
            .binding
            .as_mut()
            .ok_or_else(|| eyre::eyre!("ownership stream has no file binding"))?;
        check(binding, &self.header, self.total_bytes, true)?;
        let receipt = pakro_store::control::ownership::append_frame(
            &mut binding.file,
            self.next_sequence,
            self.head,
            self.total_bytes,
            payload,
            self.limits,
        )?;
        check(binding, &self.header, receipt.total_bytes(), false)?;
        binding.observed = binding.file.metadata()?;
        self.next_sequence = receipt.next_sequence();
        self.head = *receipt.chain_sha256();
        self.total_bytes = receipt.total_bytes();
        Ok(receipt)
    }
}

fn filename(nonce: uuid::Uuid) -> String {
    format!("{}.stream", nonce.simple())
}

#[cfg(test)]
#[path = "ownership/tests.rs"]
mod tests;
