//! Bounded streaming serialization over borrowed validated model fields.

use std::io::{self, Write};

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use super::*;

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
        map.serialize_entry("project", &Id(&self.0.project))?;
        map.serialize_entry("group", &self.0.group)?;
        map.serialize_entry("target", &Hex(&self.0.target))?;
        map.end()
    }
}
struct Stamp<'a>(&'a SelectionStamp);
impl Serialize for Stamp<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("revision", &self.0.revision)?;
        map.serialize_entry("generation", &self.0.generation)?;
        map.serialize_entry("language", &self.0.language)?;
        map.serialize_entry(
            "transaction",
            &(!self.0.transaction.is_nil()).then_some(Id(&self.0.transaction)),
        )?;
        map.end()
    }
}
struct Inputs<'a>(&'a [InputReference]);
struct Input<'a>(&'a InputReference);
impl Serialize for Input<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry(
            "kind",
            match self.0.kind {
                InputKind::Runtime => "runtime",
                InputKind::Artifact => "artifact",
                InputKind::BuildInput => "build-input",
                InputKind::Language => "language",
                InputKind::Package => "package",
                InputKind::BuildRequest => "build-request",
                InputKind::BuildOutput => "build-output",
            },
        )?;
        map.serialize_entry("digest", &self.0.digest)?;
        map.end()
    }
}
impl Serialize for Inputs<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for input in self.0 {
            seq.serialize_element(&Input(input))?;
        }
        seq.end()
    }
}
struct Generation<'a>(&'a GenerationRecord);
impl Serialize for Generation<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(6))?;
        map.serialize_entry("allocation", &self.0.allocation)?;
        map.serialize_entry("target", &Hex(&self.0.target))?;
        map.serialize_entry("shared_layout", &self.0.shared_layout.as_ref().map(Hex))?;
        map.serialize_entry("graph", &Hex(&self.0.graph))?;
        map.serialize_entry("receipt", &Hex(&self.0.receipt))?;
        map.serialize_entry("inputs", &Inputs(&self.0.inputs))?;
        map.end()
    }
}
struct Generations<'a>(&'a BTreeMap<String, GenerationRecord>);
impl Serialize for Generations<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0.values() {
            seq.serialize_element(&Generation(record))?;
        }
        seq.end()
    }
}
struct Selection<'a>(&'a SelectionKey, &'a SelectionStamp);
impl Serialize for Selection<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("key", &Key(self.0))?;
        map.serialize_entry("stamp", &Stamp(self.1))?;
        map.end()
    }
}
struct Selections<'a>(&'a BTreeMap<SelectionKey, SelectionStamp>);
impl Serialize for Selections<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (key, stamp) in self.0 {
            seq.serialize_element(&Selection(key, stamp))?;
        }
        seq.end()
    }
}
struct Transaction<'a>(&'a Uuid, &'a AppliedChange);
impl Serialize for Transaction<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(5))?;
        map.serialize_entry("id", &Id(self.0))?;
        map.serialize_entry("key", &Key(&self.1.key))?;
        map.serialize_entry("expected", &Stamp(&self.1.expected))?;
        map.serialize_entry("generation", &self.1.generation)?;
        map.serialize_entry("language", &self.1.language)?;
        map.end()
    }
}
struct Transactions<'a>(&'a BTreeMap<Uuid, AppliedChange>);
impl Serialize for Transactions<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (id, change) in self.0 {
            seq.serialize_element(&Transaction(id, change))?;
        }
        seq.end()
    }
}
struct Project<'a>(Uuid, &'a ProjectRegistration);
impl Serialize for Project<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("project", &Id(&self.0))?;
        map.serialize_entry("root", &self.1.root)?;
        map.serialize_entry("declaration", &self.1.declaration)?;
        map.end()
    }
}
struct Projects<'a>(&'a BTreeMap<Uuid, ProjectRegistration>);
impl Serialize for Projects<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (project, fact) in self.0 {
            seq.serialize_element(&Project(*project, fact))?;
        }
        seq.end()
    }
}
struct Catalog<'a>(&'a CatalogModel);
impl Serialize for Catalog<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(5))?;
        map.serialize_entry("schema", &3_u32)?;
        map.serialize_entry("generations", &Generations(&self.0.generations))?;
        map.serialize_entry("selections", &Selections(&self.0.selections))?;
        map.serialize_entry("transactions", &Transactions(&self.0.transactions))?;
        map.serialize_entry("projects", &Projects(&self.0.projects))?;
        map.end()
    }
}
struct BoundedWriter {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("catalog document byte limit exceeded"));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| io::Error::other("catalog bounded allocation failed"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn encode(model: &CatalogModel, limits: CatalogCodecLimits) -> Result<Vec<u8>> {
    ensure!(
        limits.max_document_bytes != 0,
        "catalog document byte limit must be nonzero"
    );
    super::state::validate(model, limits.model)?;
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        maximum: limits.max_document_bytes,
    };
    serde_json::to_writer(&mut writer, &Catalog(model))
        .map_err(|_| eyre::eyre!("catalog encoding failed or document byte limit exceeded"))?;
    Ok(writer.bytes)
}
