//! Closed schema object seeds. No intermediate serde_json::Value or wire DOM.

use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, Visitor};

use super::read::{Element, List, OptionalText, ParseState, Text, hex32};
use super::*;

struct Key<'a>(&'a mut ParseState);
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
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("catalog selection key")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut project, mut group, mut target, mut seen) = (None, None, None, 0);
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "project" => {
                    self.0.once(&mut seen, 1)?;
                    project = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 36,
                    })?);
                }
                "group" => {
                    self.0.once(&mut seen, 2)?;
                    let maximum = self.0.limits.max_group_bytes;
                    group = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum,
                    })?);
                }
                "target" => {
                    self.0.once(&mut seen, 4)?;
                    target = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 64,
                    })?);
                }
                _ => return self.0.fail("unknown catalog key field"),
            }
        }
        let project = self.0.required(project)?;
        let group = self.0.required(group)?;
        let target = self.0.required(target)?;
        let value = Uuid::parse_str(&project)
            .ok()
            .zip(hex32(&target).ok())
            .and_then(|(project, target)| {
                SelectionKey::new(project, &group, target, self.0.limits).ok()
            });
        match value {
            Some(value) => Ok(value),
            None => self.0.fail("invalid catalog selection identity"),
        }
    }
}
struct Stamp<'a>(&'a mut ParseState);
impl<'de> DeserializeSeed<'de> for Stamp<'_> {
    type Value = SelectionStamp;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Stamp<'_> {
    type Value = SelectionStamp;
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("catalog selection stamp")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut revision, mut generation, mut language, mut transaction, mut seen) =
            (None, None, None, None, 0);
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "revision" => {
                    self.0.once(&mut seen, 1)?;
                    revision = Some(map.next_value::<u64>()?);
                }
                "generation" => {
                    self.0.once(&mut seen, 2)?;
                    generation = Some(map.next_value_seed(OptionalText(Text {
                        state: self.0,
                        maximum: 34,
                    }))?);
                }
                // Absent in schema 2 documents; a language-only selection
                // always writes it, including explicit null.
                "language" => {
                    self.0.once(&mut seen, 4)?;
                    language = map.next_value_seed(OptionalText(Text {
                        state: self.0,
                        maximum: 135,
                    }))?;
                }
                "transaction" => {
                    self.0.once(&mut seen, 8)?;
                    transaction = Some(map.next_value_seed(OptionalText(Text {
                        state: self.0,
                        maximum: 36,
                    }))?);
                }
                _ => return self.0.fail("unknown catalog stamp field"),
            }
        }
        let revision = self.0.required(revision)?;
        let generation = self.0.required(generation)?;
        let transaction = match self.0.required(transaction)? {
            None => Uuid::nil(),
            Some(value) => match Uuid::parse_str(&value) {
                Ok(value) if !value.is_nil() => value,
                _ => return self.0.fail("invalid catalog stamp transaction"),
            },
        };
        let stamp = SelectionStamp {
            revision,
            generation,
            language,
            transaction,
        };
        if super::state::validate_stamp(&stamp).is_err() {
            return self.0.fail("invalid catalog selection stamp");
        }
        Ok(stamp)
    }
}

struct Generation<'a>(&'a mut ParseState);
impl Element for GenerationRecord {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        input.deserialize_map(Generation(state))
    }
}
impl<'de> Visitor<'de> for Generation<'_> {
    type Value = GenerationRecord;
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("catalog generation record")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (
            mut allocation,
            mut target,
            mut shared_layout,
            mut graph,
            mut receipt,
            mut inputs,
            mut package_graph,
            mut seen,
        ) = (None, None, None, None, None, None, None, 0);
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "allocation" => {
                    self.0.once(&mut seen, 1)?;
                    allocation = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 34,
                    })?);
                }
                "target" => {
                    self.0.once(&mut seen, 2)?;
                    target = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 64,
                    })?);
                }
                "shared_layout" => {
                    self.0.once(&mut seen, 32)?;
                    shared_layout = Some(map.next_value_seed(OptionalText(Text {
                        state: self.0,
                        maximum: 64,
                    }))?);
                }
                "graph" => {
                    self.0.once(&mut seen, 4)?;
                    graph = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 64,
                    })?);
                }
                "receipt" => {
                    self.0.once(&mut seen, 8)?;
                    receipt = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 64,
                    })?);
                }
                "inputs" => {
                    self.0.once(&mut seen, 16)?;
                    inputs = Some(map.next_value_seed(List::<InputReference>::inputs(self.0))?);
                }
                "package_graph" => {
                    self.0.once(&mut seen, 64)?;
                    package_graph = Some(map.next_value_seed(super::graph::Graph(self.0))?);
                }
                _ => return self.0.fail("unknown catalog generation field"),
            }
        }
        let allocation = self.0.required(allocation)?;
        let target = self.0.required(target)?;
        let shared_layout: Option<String> = self.0.required(shared_layout)?;
        let graph = self.0.required(graph)?;
        let receipt = self.0.required(receipt)?;
        let inputs = self.0.required(inputs)?;
        let value = (|| -> Result<GenerationRecord> {
            let mut record = GenerationRecord::new(
                &allocation,
                hex32(&target)?,
                hex32(&graph)?,
                hex32(&receipt)?,
                inputs,
                self.0.limits,
            )?;
            record.shared_layout = shared_layout.as_deref().map(hex32).transpose()?;
            record.package_graph = package_graph;
            record.validate(self.0.limits)?;
            Ok(record)
        })()
        .ok();
        match value {
            Some(value) => Ok(value),
            None => self.0.fail("invalid catalog generation values"),
        }
    }
}

struct Input<'a>(&'a mut ParseState);
impl Element for InputReference {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        input.deserialize_map(Input(state))
    }
}
impl<'de> Visitor<'de> for Input<'_> {
    type Value = InputReference;
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("catalog input reference")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut kind, mut digest, mut seen) = (None, None, 0);
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "kind" => {
                    self.0.once(&mut seen, 1)?;
                    kind = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 13,
                    })?);
                }
                "digest" => {
                    self.0.once(&mut seen, 2)?;
                    digest = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 135,
                    })?);
                }
                _ => return self.0.fail("unknown catalog input field"),
            }
        }
        let kind = match self.0.required(kind)?.as_str() {
            "runtime" => InputKind::Runtime,
            "artifact" => InputKind::Artifact,
            "build-input" => InputKind::BuildInput,
            "language" => InputKind::Language,
            "package" => InputKind::Package,
            "build-request" => InputKind::BuildRequest,
            "build-output" => InputKind::BuildOutput,
            _ => return self.0.fail("invalid catalog input kind"),
        };
        let digest = self.0.required(digest)?;
        match digest.parse::<Digest>() {
            Ok(digest) => Ok(InputReference::new(kind, digest)),
            Err(_) => self.0.fail("invalid catalog input digest"),
        }
    }
}

struct Selection<'a>(&'a mut ParseState);
impl Element for (SelectionKey, SelectionStamp) {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        input.deserialize_map(Selection(state))
    }
}
impl<'de> Visitor<'de> for Selection<'_> {
    type Value = (SelectionKey, SelectionStamp);
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("catalog active selection")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut key, mut stamp, mut seen) = (None, None, 0);
        while let Some(field) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match field.as_str() {
                "key" => {
                    self.0.once(&mut seen, 1)?;
                    key = Some(map.next_value_seed(Key(self.0))?);
                }
                "stamp" => {
                    self.0.once(&mut seen, 2)?;
                    stamp = Some(map.next_value_seed(Stamp(self.0))?);
                }
                _ => return self.0.fail("unknown catalog selection field"),
            }
        }
        Ok((self.0.required(key)?, self.0.required(stamp)?))
    }
}
struct Transaction<'a>(&'a mut ParseState);
impl Element for (Uuid, AppliedChange) {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        input.deserialize_map(Transaction(state))
    }
}
impl<'de> Visitor<'de> for Transaction<'_> {
    type Value = (Uuid, AppliedChange);
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("catalog applied transaction")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut id, mut key, mut expected, mut generation, mut language, mut seen) =
            (None, None, None, None, None, 0);
        while let Some(field) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match field.as_str() {
                "id" => {
                    self.0.once(&mut seen, 1)?;
                    id = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 36,
                    })?);
                }
                "key" => {
                    self.0.once(&mut seen, 2)?;
                    key = Some(map.next_value_seed(Key(self.0))?);
                }
                "expected" => {
                    self.0.once(&mut seen, 4)?;
                    expected = Some(map.next_value_seed(Stamp(self.0))?);
                }
                "generation" => {
                    self.0.once(&mut seen, 8)?;
                    generation = Some(map.next_value_seed(OptionalText(Text {
                        state: self.0,
                        maximum: 34,
                    }))?);
                }
                // Absent in schema 2 transaction history.
                "language" => {
                    self.0.once(&mut seen, 16)?;
                    language = Some(map.next_value_seed(OptionalText(Text {
                        state: self.0,
                        maximum: 135,
                    }))?);
                }
                _ => return self.0.fail("unknown catalog transaction field"),
            }
        }
        let raw = self.0.required(id)?;
        let id = match Uuid::parse_str(&raw) {
            Ok(id) if !id.is_nil() => id,
            _ => return self.0.fail("invalid catalog transaction identity"),
        };
        Ok((
            id,
            AppliedChange {
                key: self.0.required(key)?,
                expected: self.0.required(expected)?,
                generation: self.0.required(generation)?,
                language: language.flatten(),
            },
        ))
    }
}

struct Project<'a>(&'a mut ParseState);
impl Element for (Uuid, ProjectRegistration) {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        input.deserialize_map(Project(state))
    }
}
impl<'de> Visitor<'de> for Project<'_> {
    type Value = (Uuid, ProjectRegistration);
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("catalog project registration")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut project, mut root, mut declaration, mut seen) = (None, None, None, 0);
        while let Some(field) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match field.as_str() {
                "project" => {
                    self.0.once(&mut seen, 1)?;
                    project = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 36,
                    })?);
                }
                "root" => {
                    self.0.once(&mut seen, 2)?;
                    root = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 4096,
                    })?);
                }
                "declaration" => {
                    self.0.once(&mut seen, 4)?;
                    declaration = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 64,
                    })?);
                }
                _ => return self.0.fail("unknown catalog project field"),
            }
        }
        let raw = self.0.required(project)?;
        let id = match Uuid::parse_str(&raw) {
            Ok(id) if !id.is_nil() => id,
            _ => return self.0.fail("invalid catalog project identity"),
        };
        let root = self.0.required(root)?;
        let declaration = self.0.required(declaration)?;
        match ProjectRegistration::from_facts(&root, &declaration) {
            Some(value) => Ok((id, value)),
            None => self.0.fail("invalid catalog project registration"),
        }
    }
}

struct Catalog<'a>(&'a mut ParseState);
impl<'de> DeserializeSeed<'de> for Catalog<'_> {
    type Value = CatalogModel;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Catalog<'_> {
    type Value = CatalogModel;
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("versioned catalog document")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let (mut schema, mut generations, mut selections, mut transactions, mut projects, mut seen) =
            (None, None, None, None, None, 0);
        while let Some(field) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match field.as_str() {
                "schema" => {
                    self.0.once(&mut seen, 1)?;
                    schema = Some(map.next_value::<u32>()?);
                    // Schema 3 adds language-only selections and project
                    // registrations; schema 2 documents decode as their
                    // pre-extension equivalents. Nothing else is accepted.
                    if !matches!(schema, Some(2 | 3)) {
                        return self.0.fail("unsupported catalog schema");
                    }
                }
                "generations" => {
                    self.0.once(&mut seen, 2)?;
                    let maximum = self.0.limits.max_generation_records;
                    generations =
                        Some(map.next_value_seed(List::<GenerationRecord>::new(self.0, maximum))?);
                }
                "selections" => {
                    self.0.once(&mut seen, 4)?;
                    let maximum = self.0.limits.max_selection_slots;
                    selections = Some(map.next_value_seed(
                        List::<(SelectionKey, SelectionStamp)>::new(self.0, maximum),
                    )?);
                }
                "transactions" => {
                    self.0.once(&mut seen, 8)?;
                    let maximum = self.0.limits.max_transactions;
                    transactions = Some(
                        map.next_value_seed(List::<(Uuid, AppliedChange)>::new(self.0, maximum))?,
                    );
                }
                // Absent in schema 2 documents.
                "projects" => {
                    self.0.once(&mut seen, 16)?;
                    let maximum = self.0.limits.max_selection_slots;
                    projects = Some(map.next_value_seed(
                        List::<(Uuid, ProjectRegistration)>::new(self.0, maximum),
                    )?);
                }
                _ => return self.0.fail("unknown catalog document field"),
            }
        }
        self.0.required(schema)?;
        let generations = self.0.required(generations)?;
        let selections = self.0.required(selections)?;
        let transactions = self.0.required(transactions)?;
        let projects = projects.unwrap_or_default();
        let mut model = CatalogModel::new(self.0.limits)
            .map_err(|_| de::Error::custom("invalid catalog limits"))?;
        for record in generations {
            if model.generations.contains_key(record.allocation()) {
                return self.0.fail("duplicate catalog generation identity");
            }
            if model.register_generation(record).is_err() {
                return self.0.fail("invalid catalog generation record");
            }
        }
        for (key, stamp) in selections {
            if model.selections.insert(key, stamp).is_some() {
                return self.0.fail("duplicate catalog selection key");
            }
        }
        for (id, change) in transactions {
            if model.transactions.insert(id, change).is_some() {
                return self.0.fail("duplicate catalog transaction identity");
            }
        }
        for (project, fact) in projects {
            if model.projects.contains_key(&project) {
                return self.0.fail("duplicate catalog project registration");
            }
            if model
                .register_project(project, fact.root(), fact.declaration())
                .is_err()
            {
                return self.0.fail("invalid catalog project registration");
            }
        }
        if super::state::validate(&model, self.0.limits).is_err() {
            return self
                .0
                .fail("inconsistent catalog identities or revision history");
        }
        Ok(model)
    }
}

pub(super) fn decode(bytes: &[u8], limits: CatalogCodecLimits) -> Result<CatalogModel> {
    limits.model.validate()?;
    ensure!(
        limits.max_document_bytes != 0,
        "catalog document byte limit must be nonzero"
    );
    ensure!(
        bytes.len() <= limits.max_document_bytes,
        "catalog document byte limit exceeded"
    );
    let mut state = ParseState {
        limits: limits.model,
        references: 0,
        failure: None,
    };
    let mut input = serde_json::Deserializer::from_slice(bytes);
    Catalog(&mut state)
        .deserialize(&mut input)
        .and_then(|model| {
            input.end()?;
            Ok(model)
        })
        .map_err(|_| eyre::eyre!(state.failure.unwrap_or("invalid catalog metadata document")))
}
