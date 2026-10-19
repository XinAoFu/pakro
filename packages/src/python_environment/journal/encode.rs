//! Borrowed serialization into a bounded writer, without a cloned wire DOM.

use std::io::{self, Write};

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use super::*;
use crate::packages::python_environment::model::InputKind;

struct Hex<'a>(&'a [u8; 32]);
impl Serialize for Hex<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut bytes = [0; 64];
        for (index, byte) in self.0.iter().enumerate() {
            bytes[index * 2] = DIGITS[usize::from(byte >> 4)];
            bytes[index * 2 + 1] = DIGITS[usize::from(byte & 15)];
        }
        serializer.serialize_str(std::str::from_utf8(&bytes).expect("ASCII hexadecimal"))
    }
}
struct Id<'a>(&'a Uuid);
impl Serialize for Id<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.hyphenated().encode_lower(&mut [0; 36]))
    }
}
struct Key<'a>(&'a SelectionKey);
impl Serialize for Key<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("project", &Id(&self.0.project()))?;
        map.serialize_entry("group", self.0.group())?;
        map.serialize_entry("target", &Hex(self.0.target()))?;
        map.end()
    }
}
struct Stamp<'a>(&'a JournalExpectedStamp);
impl Serialize for Stamp<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("revision", &self.0.revision)?;
        map.serialize_entry("generation", &self.0.generation)?;
        map.serialize_entry(
            "transaction",
            &(!self.0.transaction.is_nil()).then_some(Id(&self.0.transaction)),
        )?;
        map.end()
    }
}
struct Identity<'a>(&'a GenerationIdentityFacts);
impl Serialize for Identity<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(6))?;
        map.serialize_entry("allocation", &self.0.allocation)?;
        map.serialize_entry("store", &self.0.store)?;
        map.serialize_entry("generation", &self.0.generation)?;
        map.serialize_entry("leases", &self.0.leases)?;
        map.serialize_entry("build", &self.0.build)?;
        map.serialize_entry("lock", &self.0.lock)?;
        map.end()
    }
}
struct Inputs<'a>(&'a [InputReference]);
struct Input<'a>(&'a InputReference);
impl Serialize for Inputs<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for reference in self.0 {
            sequence.serialize_element(&Input(reference))?;
        }
        sequence.end()
    }
}
impl Serialize for Input<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        let kind = match self.0.kind() {
            InputKind::Runtime => "runtime",
            InputKind::Artifact => "artifact",
            InputKind::BuildInput => "build-input",
            InputKind::Language => "language",
            InputKind::Package => "package",
            InputKind::BuildRequest => "build-request",
            InputKind::BuildOutput => "build-output",
        };
        map.serialize_entry("kind", kind)?;
        map.serialize_entry("digest", self.0.digest())?;
        map.end()
    }
}
struct Target<'a>(&'a RecordedTarget);
impl Serialize for Target<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            RecordedTarget::Generation(facts) => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("kind", "generation")?;
                map.serialize_entry("identity", &Identity(&facts.identity))?;
                map.serialize_entry("graph", &Hex(&facts.graph))?;
                map.serialize_entry("inputs", &Inputs(&facts.inputs))?;
                map.end()
            }
            RecordedTarget::ClearSelection { store } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("kind", "clear")?;
                map.serialize_entry("store", store)?;
                map.end()
            }
        }
    }
}
struct Record<'a>(&'a JournalRecord);
impl Serialize for Record<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(5))?;
        map.serialize_entry("schema", &1u8)?;
        map.serialize_entry("nonce", &Id(&self.0.nonce))?;
        map.serialize_entry("key", &Key(&self.0.key))?;
        map.serialize_entry("expected", &Stamp(&self.0.expected))?;
        map.serialize_entry("target", &Target(&self.0.target))?;
        map.end()
    }
}

struct BoundedWriter {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes
            .len()
            .checked_add(bytes.len())
            .filter(|total| *total <= self.maximum)
            .ok_or_else(|| io::Error::other("journal encoded document byte limit exceeded"))?;
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| io::Error::other("journal output allocation failed"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn encode(record: &JournalRecord, maximum: usize) -> Result<Vec<u8>> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        maximum,
    };
    serde_json::to_writer(&mut writer, &Record(record))
        .map_err(|_| eyre::eyre!("journal encoding or document byte limit failed"))?;
    Ok(writer.bytes)
}
