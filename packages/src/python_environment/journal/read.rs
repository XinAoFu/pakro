//! Closed typed seeds with input budgets charged before element decoding.

use std::fmt;

use pakro_store::Digest;
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use super::*;
use crate::packages::python_environment::model::InputKind;

struct State {
    limits: JournalLimits,
    references: usize,
    failure: Option<&'static str>,
}
impl State {
    fn fail<T, E: de::Error>(&mut self, label: &'static str) -> std::result::Result<T, E> {
        self.failure = Some(label);
        Err(E::custom("invalid journal metadata"))
    }
    fn once<E: de::Error>(&mut self, seen: &mut u16, bit: u16) -> std::result::Result<(), E> {
        if *seen & bit != 0 {
            return self.fail("duplicate journal field");
        }
        *seen |= bit;
        Ok(())
    }
    fn required<T, E: de::Error>(&mut self, field: Option<T>) -> std::result::Result<T, E> {
        field.ok_or_else(|| {
            self.failure = Some("journal required field is missing");
            E::custom("invalid journal metadata")
        })
    }
}

struct Text<'a>(&'a mut State, usize);
impl<'de> DeserializeSeed<'de> for Text<'_> {
    type Value = String;
    fn deserialize<D: Deserializer<'de>>(self, input: D) -> std::result::Result<String, D::Error> {
        input.deserialize_str(self)
    }
}
impl<'de> Visitor<'de> for Text<'_> {
    type Value = String;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded journal text")
    }
    fn visit_str<E: de::Error>(self, text: &str) -> std::result::Result<String, E> {
        if text.len() > self.1 {
            return self.0.fail("journal field byte limit exceeded");
        }
        Ok(text.to_owned())
    }
    fn visit_string<E: de::Error>(self, text: String) -> std::result::Result<String, E> {
        if text.len() > self.1 {
            return self.0.fail("journal field byte limit exceeded");
        }
        Ok(text)
    }
}
struct OptionalText<'a>(Text<'a>);
impl<'de> DeserializeSeed<'de> for OptionalText<'_> {
    type Value = Option<String>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_option(self)
    }
}
impl<'de> Visitor<'de> for OptionalText<'_> {
    type Value = Option<String>;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("null or bounded journal text")
    }
    fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_some<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        self.0.deserialize(input).map(Some)
    }
}

struct Inputs<'a>(&'a mut State);
impl<'de> DeserializeSeed<'de> for Inputs<'_> {
    type Value = Vec<InputReference>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Inputs<'_> {
    type Value = Vec<InputReference>;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded journal inputs")
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut array: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = array.next_element_seed(Input {
            state: self.0,
            count: values.len(),
        })? {
            values.push(value);
        }
        values.sort();
        values.dedup();
        Ok(values)
    }
}
struct Input<'a> {
    state: &'a mut State,
    count: usize,
}
impl<'de> DeserializeSeed<'de> for Input<'_> {
    type Value = InputReference;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        if self.count >= self.state.limits.max_inputs_per_record {
            return self
                .state
                .fail("journal per-record input reference limit exceeded");
        }
        let Some(total) = self
            .state
            .references
            .checked_add(1)
            .filter(|total| *total <= self.state.limits.max_total_inputs)
        else {
            return self
                .state
                .fail("journal total input reference limit exceeded");
        };
        self.state.references = total;
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Input<'_> {
    type Value = InputReference;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("journal input reference")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut kind, mut digest, mut seen) = (None, None, 0);
        while let Some(field) = map.next_key_seed(Text(self.state, 32))? {
            match field.as_str() {
                "kind" => {
                    self.state.once(&mut seen, 1)?;
                    kind = Some(map.next_value_seed(Text(self.state, 13))?);
                }
                "digest" => {
                    self.state.once(&mut seen, 2)?;
                    digest = Some(map.next_value_seed(Text(self.state, 135))?);
                }
                _ => return self.state.fail("unknown journal input field"),
            }
        }
        let kind = match self.state.required(kind)?.as_str() {
            "runtime" => InputKind::Runtime,
            "artifact" => InputKind::Artifact,
            "build-input" => InputKind::BuildInput,
            "language" => InputKind::Language,
            "package" => InputKind::Package,
            "build-request" => InputKind::BuildRequest,
            "build-output" => InputKind::BuildOutput,
            _ => return self.state.fail("invalid journal input kind"),
        };
        let text = self.state.required(digest)?;
        match text.parse::<Digest>() {
            Ok(digest) => {
                let reference = InputReference::new(kind, digest);
                if reference.validate().is_err() {
                    return self.state.fail("invalid journal typed input identity");
                }
                Ok(reference)
            }
            Err(_) => self.state.fail("invalid journal input digest"),
        }
    }
}

struct Key<'a>(&'a mut State);
impl<'de> DeserializeSeed<'de> for Key<'_> {
    type Value = SelectionKey;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Key<'_> {
    type Value = SelectionKey;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("journal selection identity")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut project, mut group, mut target, mut seen) = (None, None, None, 0);
        while let Some(field) = map.next_key_seed(Text(self.0, 32))? {
            match field.as_str() {
                "project" => {
                    self.0.once(&mut seen, 1)?;
                    project = Some(map.next_value_seed(Text(self.0, 36))?);
                }
                "group" => {
                    self.0.once(&mut seen, 2)?;
                    let maximum = self.0.limits.model.max_group_bytes;
                    group = Some(map.next_value_seed(Text(self.0, maximum))?);
                }
                "target" => {
                    self.0.once(&mut seen, 4)?;
                    target = Some(map.next_value_seed(Text(self.0, 64))?);
                }
                _ => return self.0.fail("unknown journal key field"),
            }
        }
        let project = self.0.required(project)?;
        let group = self.0.required(group)?;
        let target = self.0.required(target)?;
        let value = Uuid::parse_str(&project)
            .ok()
            .zip(hex32(&target).ok())
            .and_then(|(project, target)| {
                SelectionKey::new(project, &group, target, self.0.limits.model).ok()
            });
        match value {
            Some(value) => Ok(value),
            None => self.0.fail("invalid journal selection identity"),
        }
    }
}

struct Stamp<'a>(&'a mut State);
impl<'de> DeserializeSeed<'de> for Stamp<'_> {
    type Value = JournalExpectedStamp;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Stamp<'_> {
    type Value = JournalExpectedStamp;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("journal expected selection stamp")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut revision, mut generation, mut transaction, mut seen) = (None, None, None, 0);
        while let Some(field) = map.next_key_seed(Text(self.0, 32))? {
            match field.as_str() {
                "revision" => {
                    self.0.once(&mut seen, 1)?;
                    revision = Some(map.next_value::<u64>()?);
                }
                "generation" => {
                    self.0.once(&mut seen, 2)?;
                    generation = Some(map.next_value_seed(OptionalText(Text(self.0, 34)))?);
                }
                "transaction" => {
                    self.0.once(&mut seen, 4)?;
                    transaction = Some(map.next_value_seed(OptionalText(Text(self.0, 36)))?);
                }
                _ => return self.0.fail("unknown journal stamp field"),
            }
        }
        let revision = self.0.required(revision)?;
        let generation = self.0.required(generation)?;
        let transaction = match self.0.required(transaction)? {
            None => Uuid::nil(),
            Some(text) => match Uuid::parse_str(&text) {
                Ok(id) if !id.is_nil() => id,
                _ => return self.0.fail("invalid journal expected transaction"),
            },
        };
        let stamp = JournalExpectedStamp {
            revision,
            generation,
            transaction,
        };
        if validate_expected(&stamp).is_err() {
            return self.0.fail("invalid journal expected selection stamp");
        }
        Ok(stamp)
    }
}

struct Identity<'a>(&'a mut State);
impl<'de> DeserializeSeed<'de> for Identity<'_> {
    type Value = GenerationIdentityFacts;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Identity<'_> {
    type Value = GenerationIdentityFacts;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("journal generation identity facts")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut allocation, mut store, mut generation, mut leases, mut build, mut lock, mut seen) =
            (None, None, None, None, None, None, 0);
        while let Some(field) = map.next_key_seed(Text(self.0, 32))? {
            match field.as_str() {
                "allocation" => {
                    self.0.once(&mut seen, 1)?;
                    allocation = Some(map.next_value_seed(Text(self.0, 34))?);
                }
                "store" => {
                    self.0.once(&mut seen, 2)?;
                    store = Some(map.next_value::<[u64; 2]>()?);
                }
                "generation" => {
                    self.0.once(&mut seen, 4)?;
                    generation = Some(map.next_value::<[u64; 2]>()?);
                }
                "leases" => {
                    self.0.once(&mut seen, 8)?;
                    leases = Some(map.next_value::<[u64; 2]>()?);
                }
                "build" => {
                    self.0.once(&mut seen, 16)?;
                    build = Some(map.next_value::<[u64; 2]>()?);
                }
                "lock" => {
                    self.0.once(&mut seen, 32)?;
                    lock = Some(map.next_value::<[u64; 2]>()?);
                }
                _ => return self.0.fail("unknown journal identity field"),
            }
        }
        let allocation = self.0.required(allocation)?;
        if validate_allocation(&allocation).is_err() {
            return self.0.fail("invalid journal allocation identity");
        }
        Ok(GenerationIdentityFacts {
            allocation,
            store: self.0.required(store)?,
            generation: self.0.required(generation)?,
            leases: self.0.required(leases)?,
            build: self.0.required(build)?,
            lock: self.0.required(lock)?,
        })
    }
}

struct Target<'a>(&'a mut State);
impl<'de> DeserializeSeed<'de> for Target<'_> {
    type Value = RecordedTarget;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Target<'_> {
    type Value = RecordedTarget;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("journal generation or clear intent")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut kind, mut identity, mut graph, mut inputs, mut store, mut seen) =
            (None, None, None, None, None, 0);
        while let Some(field) = map.next_key_seed(Text(self.0, 32))? {
            match field.as_str() {
                "kind" => {
                    self.0.once(&mut seen, 1)?;
                    kind = Some(map.next_value_seed(Text(self.0, 10))?);
                }
                "identity" => {
                    self.0.once(&mut seen, 2)?;
                    identity = Some(map.next_value_seed(Identity(self.0))?);
                }
                "graph" => {
                    self.0.once(&mut seen, 4)?;
                    graph = Some(map.next_value_seed(Text(self.0, 64))?);
                }
                "inputs" => {
                    self.0.once(&mut seen, 8)?;
                    inputs = Some(map.next_value_seed(Inputs(self.0))?);
                }
                "store" => {
                    self.0.once(&mut seen, 16)?;
                    store = Some(map.next_value::<[u64; 2]>()?);
                }
                _ => return self.0.fail("unknown journal target field"),
            }
        }
        match self.0.required(kind)?.as_str() {
            "generation" if seen == 15 => {
                let identity = self.0.required(identity)?;
                let text = self.0.required(graph)?;
                let graph = match hex32(&text) {
                    Ok(value) => value,
                    Err(_) => return self.0.fail("invalid journal graph identity"),
                };
                Ok(RecordedTarget::Generation(JournalGenerationFacts {
                    identity,
                    graph,
                    inputs: self.0.required(inputs)?,
                }))
            }
            "clear" if seen == 17 => Ok(RecordedTarget::ClearSelection {
                store: self.0.required(store)?,
            }),
            _ => self.0.fail("invalid journal target fields or kind"),
        }
    }
}

struct Record<'a>(&'a mut State);
impl<'de> DeserializeSeed<'de> for Record<'_> {
    type Value = JournalRecord;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Record<'_> {
    type Value = JournalRecord;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("prepared journal metadata")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut schema, mut nonce, mut key, mut expected, mut target, mut seen) =
            (None, None, None, None, None, 0);
        while let Some(field) = map.next_key_seed(Text(self.0, 32))? {
            match field.as_str() {
                "schema" => {
                    self.0.once(&mut seen, 1)?;
                    schema = Some(map.next_value::<u64>()?);
                }
                "nonce" => {
                    self.0.once(&mut seen, 2)?;
                    nonce = Some(map.next_value_seed(Text(self.0, 36))?);
                }
                "key" => {
                    self.0.once(&mut seen, 4)?;
                    key = Some(map.next_value_seed(Key(self.0))?);
                }
                "expected" => {
                    self.0.once(&mut seen, 8)?;
                    expected = Some(map.next_value_seed(Stamp(self.0))?);
                }
                "target" => {
                    self.0.once(&mut seen, 16)?;
                    target = Some(map.next_value_seed(Target(self.0))?);
                }
                _ => return self.0.fail("unknown journal record field"),
            }
        }
        if self.0.required(schema)? != 1 {
            return self.0.fail("unsupported journal schema");
        }
        let text = self.0.required(nonce)?;
        let nonce = match Uuid::parse_str(&text) {
            Ok(value) if !value.is_nil() => value,
            _ => return self.0.fail("invalid journal nonce"),
        };
        Ok(JournalRecord {
            nonce,
            key: self.0.required(key)?,
            expected: self.0.required(expected)?,
            target: self.0.required(target)?,
        })
    }
}

pub(super) fn decode(
    bytes: &[u8],
    name: &str,
    store: [u64; 2],
    limits: JournalLimits,
    raw_inputs: &mut usize,
) -> Result<JournalRecord> {
    ensure!(
        bytes.len() <= limits.max_document_bytes,
        "journal document byte limit exceeded"
    );
    let mut state = State {
        limits,
        references: *raw_inputs,
        failure: None,
    };
    let mut input = serde_json::Deserializer::from_slice(bytes);
    let record = Record(&mut state)
        .deserialize(&mut input)
        .map_err(|_| eyre::eyre!(state.failure.unwrap_or("invalid journal JSON or scalar")))?;
    input
        .end()
        .map_err(|_| eyre::eyre!("invalid trailing journal JSON"))?;
    ensure!(
        filename(record.nonce) == name,
        "journal filename and nonce disagree"
    );
    ensure!(
        record.store_identity() == store,
        "journal metadata belongs to a different store"
    );
    *raw_inputs = state.references;
    Ok(record)
}

fn hex32(text: &str) -> Result<[u8; 32]> {
    ensure!(
        text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid journal SHA256 identity"
    );
    let mut result = [0; 32];
    hex::decode_to_slice(text, &mut result)
        .map_err(|_| eyre::eyre!("invalid journal SHA256 identity"))?;
    Ok(result)
}
