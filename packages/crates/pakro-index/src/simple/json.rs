//! Bounded JSON Simple API facts, independent of target compatibility.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::facts::*;
use super::*;

pub fn parse_simple_json(
    body: &[u8],
    context: SimpleParseContext<'_>,
) -> SimpleResult<SimpleProject> {
    parse_inner(body, context, None)
}

pub(super) fn parse_for_document(
    body: &[u8],
    context: SimpleParseContext<'_>,
    document: &super::client::http::EffectiveSimpleDocument,
) -> SimpleResult<SimpleProject> {
    parse_inner(body, context, Some(document))
}

fn parse_inner(
    body: &[u8],
    context: SimpleParseContext<'_>,
    document: Option<&super::client::http::EffectiveSimpleDocument>,
) -> SimpleResult<SimpleProject> {
    validate_limits(context.limits)?;
    if body.len() as u64 > context.limits.max_response_bytes {
        return Err(SimpleError::LimitExceeded(SimpleLimitKind::ResponseBytes));
    }
    validate_context(&context)?;

    // Validate every raw structure, including ignored extensions, without a DOM.
    let mut budget = RawBudget {
        limits: context.limits,
        values: 0,
        error: None,
    };
    let mut input = serde_json::Deserializer::from_slice(body);
    RawSeed {
        budget: &mut budget,
        depth: 0,
    }
    .deserialize(&mut input)
    .and_then(|()| input.end())
    .map_err(|error| {
        budget.error.take().unwrap_or_else(|| {
            if error.to_string().starts_with("recursion limit exceeded") {
                SimpleError::LimitExceeded(SimpleLimitKind::JsonDepth)
            } else {
                SimpleError::InvalidJson
            }
        })
    })?;

    let mut state = CollectState {
        limits: context.limits,
        relations: 0,
        error: None,
    };
    let mut input = serde_json::Deserializer::from_slice(body);
    let raw = ProjectSeed(&mut state)
        .deserialize(&mut input)
        .and_then(|value| {
            input.end()?;
            Ok(value)
        })
        .map_err(|_| {
            state
                .error
                .take()
                .unwrap_or(SimpleError::InvalidResponse("invalid field type"))
        })?;
    match document {
        Some(document) => finish_project_for_document(raw, &context, document.canonical()),
        None => finish_project(raw, &context),
    }
}

struct RawBudget {
    limits: SimpleLimits,
    values: usize,
    error: Option<SimpleError>,
}
impl RawBudget {
    fn fail<T, E: de::Error>(&mut self, kind: SimpleLimitKind) -> Result<T, E> {
        self.error = Some(SimpleError::LimitExceeded(kind));
        Err(E::custom("Simple API raw budget exceeded"))
    }
    fn value<E: de::Error>(&mut self) -> Result<(), E> {
        match self.values.checked_add(1) {
            Some(value) if value <= self.limits.max_json_values => {
                self.values = value;
                Ok(())
            }
            _ => self.fail(SimpleLimitKind::JsonValues),
        }
    }
}
struct RawSeed<'a> {
    budget: &'a mut RawBudget,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for RawSeed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<(), D::Error> {
        self.budget.value()?;
        input.deserialize_any(RawVisitor(self))
    }
}
struct RawVisitor<'a>(RawSeed<'a>);
impl<'de> Visitor<'de> for RawVisitor<'_> {
    type Value = ();
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded JSON")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_string<E: de::Error>(self, _: String) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> Result<(), A::Error> {
        let depth = self.0.depth + 1;
        if depth > self.0.budget.limits.max_json_depth {
            return self.0.budget.fail(SimpleLimitKind::JsonDepth);
        }
        while array
            .next_element_seed(RawSeed {
                budget: self.0.budget,
                depth,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<(), A::Error> {
        let depth = self.0.depth + 1;
        if depth > self.0.budget.limits.max_json_depth {
            return self.0.budget.fail(SimpleLimitKind::JsonDepth);
        }
        let mut count = 0usize;
        while object
            .next_key_seed(RawSeed {
                budget: self.0.budget,
                depth,
            })?
            .is_some()
        {
            count = count
                .checked_add(1)
                .ok_or_else(|| de::Error::custom("field count overflow"))?;
            if count > self.0.budget.limits.max_object_fields {
                return self.0.budget.fail(SimpleLimitKind::ObjectFields);
            }
            object.next_value_seed(RawSeed {
                budget: self.0.budget,
                depth,
            })?;
        }
        Ok(())
    }
}

struct CollectState {
    limits: SimpleLimits,
    relations: usize,
    error: Option<SimpleError>,
}
impl CollectState {
    fn fail<T, E: de::Error>(&mut self, error: SimpleError) -> Result<T, E> {
        self.error = Some(error);
        Err(E::custom("Simple API fact rejected"))
    }
    fn string<E: de::Error>(&mut self, value: &str) -> Result<(), E> {
        if value.len() > self.limits.max_field_bytes {
            return self.fail(SimpleError::LimitExceeded(SimpleLimitKind::FieldBytes));
        }
        Ok(())
    }
    fn key<E: de::Error>(
        &mut self,
        seen: &mut BTreeSet<String>,
        key: &str,
        known: &[&str],
    ) -> Result<(), E> {
        if known.contains(&key) && !seen.insert(key.to_owned()) {
            return self.fail(SimpleError::InvalidResponse("duplicate JSON field"));
        }
        Ok(())
    }
}
struct StringSeed<'a>(&'a mut CollectState);
impl<'de> DeserializeSeed<'de> for StringSeed<'_> {
    type Value = String;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<String, D::Error> {
        input.deserialize_string(self)
    }
}
impl<'de> Visitor<'de> for StringSeed<'_> {
    type Value = String;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded string")
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        self.0.string(value)?;
        Ok(value.to_owned())
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<String, E> {
        self.0.string(&value)?;
        Ok(value)
    }
}
struct OptionalStringSeed<'a>(&'a mut CollectState);
impl<'de> DeserializeSeed<'de> for OptionalStringSeed<'_> {
    type Value = Option<String>;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_option(self)
    }
}
impl<'de> Visitor<'de> for OptionalStringSeed<'_> {
    type Value = Option<String>;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("string or null")
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_some<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        StringSeed(self.0).deserialize(input).map(Some)
    }
}

struct StringListSeed<'a> {
    state: &'a mut CollectState,
    kind: SimpleLimitKind,
}
impl<'de> DeserializeSeed<'de> for StringListSeed<'_> {
    type Value = Vec<String>;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for StringListSeed<'_> {
    type Value = Vec<String>;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded string array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = array.next_element_seed(StringElementSeed {
            state: self.state,
            kind: self.kind,
            count: values.len(),
        })? {
            values.push(value);
        }
        Ok(values)
    }
}
struct StringElementSeed<'a> {
    state: &'a mut CollectState,
    kind: SimpleLimitKind,
    count: usize,
}
impl<'de> DeserializeSeed<'de> for StringElementSeed<'_> {
    type Value = String;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<String, D::Error> {
        let used = if self.kind == SimpleLimitKind::Relations {
            self.state.relations
        } else {
            self.count
        };
        let limit = if self.kind == SimpleLimitKind::Relations {
            self.state.limits.max_relations
        } else {
            self.state.limits.max_versions
        };
        if used >= limit {
            return self.state.fail(SimpleError::LimitExceeded(self.kind));
        }
        if self.kind == SimpleLimitKind::Relations {
            self.state.relations += 1;
        }
        StringSeed(self.state).deserialize(input)
    }
}

struct MetaSeed<'a>(&'a mut CollectState);
impl<'de> DeserializeSeed<'de> for MetaSeed<'_> {
    type Value = RawMeta;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for MetaSeed<'_> {
    type Value = RawMeta;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("Simple API meta object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self::Value, A::Error> {
        let mut value = RawMeta::default();
        let mut seen = BTreeSet::new();
        while let Some(key) = object.next_key_seed(StringSeed(self.0))? {
            self.0.key(&mut seen, &key, &["api-version", "tracks"])?;
            match key.as_str() {
                "api-version" => value.api = Some(object.next_value_seed(StringSeed(self.0))?),
                "tracks" => {
                    value.tracks = object.next_value_seed(StringListSeed {
                        state: self.0,
                        kind: SimpleLimitKind::Relations,
                    })?
                }
                _ => {
                    object.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(value)
    }
}
struct StatusSeed<'a>(&'a mut CollectState);
impl<'de> DeserializeSeed<'de> for StatusSeed<'_> {
    type Value = RawStatus;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for StatusSeed<'_> {
    type Value = RawStatus;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("project status object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self::Value, A::Error> {
        let mut value = RawStatus::default();
        let mut seen = BTreeSet::new();
        while let Some(key) = object.next_key_seed(StringSeed(self.0))? {
            self.0.key(&mut seen, &key, &["status", "reason"])?;
            match key.as_str() {
                "status" => value.status = Some(object.next_value_seed(StringSeed(self.0))?),
                "reason" => value.reason = Some(object.next_value_seed(StringSeed(self.0))?),
                _ => {
                    object.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(value)
    }
}
struct ProjectSeed<'a>(&'a mut CollectState);
impl<'de> DeserializeSeed<'de> for ProjectSeed<'_> {
    type Value = RawProject;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for ProjectSeed<'_> {
    type Value = RawProject;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("Simple API project object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self::Value, A::Error> {
        let mut value = RawProject::default();
        let mut seen = BTreeSet::new();
        while let Some(key) = object.next_key_seed(StringSeed(self.0))? {
            self.0.key(
                &mut seen,
                &key,
                &[
                    "meta",
                    "name",
                    "versions",
                    "files",
                    "alternate-locations",
                    "project-status",
                ],
            )?;
            match key.as_str() {
                "meta" => value.meta = Some(object.next_value_seed(MetaSeed(self.0))?),
                "name" => value.name = Some(object.next_value_seed(StringSeed(self.0))?),
                "versions" => {
                    value.versions = Some(object.next_value_seed(StringListSeed {
                        state: self.0,
                        kind: SimpleLimitKind::Versions,
                    })?)
                }
                "files" => value.files = Some(object.next_value_seed(FilesSeed(self.0))?),
                "alternate-locations" => {
                    value.alternate = object.next_value_seed(StringListSeed {
                        state: self.0,
                        kind: SimpleLimitKind::Relations,
                    })?
                }
                "project-status" => {
                    value.status = Some(object.next_value_seed(StatusSeed(self.0))?)
                }
                _ => {
                    object.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(value)
    }
}
struct FilesSeed<'a>(&'a mut CollectState);
impl<'de> DeserializeSeed<'de> for FilesSeed<'_> {
    type Value = Vec<RawFile>;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for FilesSeed<'_> {
    type Value = Vec<RawFile>;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded file array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> Result<Self::Value, A::Error> {
        let mut files = Vec::new();
        while let Some(file) = array.next_element_seed(FileSeed {
            state: self.0,
            count: files.len(),
        })? {
            files.push(file);
        }
        Ok(files)
    }
}
struct FileSeed<'a> {
    state: &'a mut CollectState,
    count: usize,
}
impl<'de> DeserializeSeed<'de> for FileSeed<'_> {
    type Value = RawFile;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        if self.count >= self.state.limits.max_files {
            return self
                .state
                .fail(SimpleError::LimitExceeded(SimpleLimitKind::Files));
        }
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for FileSeed<'_> {
    type Value = RawFile;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("distribution file object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self::Value, A::Error> {
        let mut file = RawFile::default();
        let mut seen = BTreeSet::new();
        while let Some(key) = object.next_key_seed(StringSeed(self.state))? {
            self.state.key(
                &mut seen,
                &key,
                &[
                    "filename",
                    "url",
                    "hashes",
                    "requires-python",
                    "yanked",
                    "core-metadata",
                    "dist-info-metadata",
                    "size",
                    "upload-time",
                    "provenance",
                ],
            )?;
            match key.as_str() {
                "filename" => file.filename = Some(object.next_value_seed(StringSeed(self.state))?),
                "url" => file.url = Some(object.next_value_seed(StringSeed(self.state))?),
                "hashes" => file.hashes = Some(object.next_value_seed(HashSeed(self.state))?),
                "requires-python" => {
                    file.requires_python = object.next_value_seed(OptionalStringSeed(self.state))?
                }
                "yanked" => file.yanked = Some(object.next_value_seed(YankSeed(self.state))?),
                "core-metadata" => file.core = Some(object.next_value_seed(CoreSeed(self.state))?),
                "dist-info-metadata" => {
                    file.legacy_core = Some(object.next_value_seed(CoreSeed(self.state))?)
                }
                "size" => file.size = Some(object.next_value::<u64>()?),
                "upload-time" => {
                    file.upload_time = Some(object.next_value_seed(StringSeed(self.state))?)
                }
                "provenance" => {
                    file.provenance = object.next_value_seed(OptionalStringSeed(self.state))?
                }
                _ => {
                    object.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(file)
    }
}
struct HashSeed<'a>(&'a mut CollectState);
impl<'de> DeserializeSeed<'de> for HashSeed<'_> {
    type Value = BTreeMap<String, String>;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for HashSeed<'_> {
    type Value = BTreeMap<String, String>;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded hash object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self::Value, A::Error> {
        let mut hashes = BTreeMap::new();
        while let Some(key) = object.next_key_seed(StringSeed(self.0))? {
            if hashes.len() >= self.0.limits.max_hashes_per_file {
                return self
                    .0
                    .fail(SimpleError::LimitExceeded(SimpleLimitKind::Hashes));
            }
            if hashes.contains_key(&key) {
                return self
                    .0
                    .fail(SimpleError::InvalidResponse("duplicate hash field"));
            }
            hashes.insert(key, object.next_value_seed(StringSeed(self.0))?);
        }
        Ok(hashes)
    }
}
struct CoreSeed<'a>(&'a mut CollectState);
impl<'de> DeserializeSeed<'de> for CoreSeed<'_> {
    type Value = RawCore;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for CoreSeed<'_> {
    type Value = RawCore;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("metadata boolean or hash object")
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(if value {
            RawCore::Available(BTreeMap::new())
        } else {
            RawCore::Absent
        })
    }
    fn visit_map<A: MapAccess<'de>>(self, object: A) -> Result<Self::Value, A::Error> {
        let hashes = HashSeed(self.0).visit_map(object)?;
        Ok(if hashes.is_empty() {
            RawCore::Absent
        } else {
            RawCore::Available(hashes)
        })
    }
}
struct YankSeed<'a>(&'a mut CollectState);
impl<'de> DeserializeSeed<'de> for YankSeed<'_> {
    type Value = Yanked;
    fn deserialize<D: de::Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
        input.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for YankSeed<'_> {
    type Value = Yanked;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("yanked boolean or nonempty reason")
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(if value {
            Yanked::Yes { reason: None }
        } else {
            Yanked::No
        })
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        self.0.string(value)?;
        if value.is_empty() {
            return self
                .0
                .fail(SimpleError::InvalidResponse("empty JSON yanked reason"));
        }
        Ok(Yanked::Yes {
            reason: Some(value.to_owned()),
        })
    }
}
