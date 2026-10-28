//! Catalog allocation bounds apply before source-build fields are retained.
use super::super::super::source_build::MAX_BUILD_INPUTS;
use super::*;
use super::super::read::OptionalText;

fn once<E: serde::de::Error>(
    state: &mut ParseState,
    seen: &mut u16,
    bit: u16,
) -> std::result::Result<(), E> {
    if *seen & bit != 0 {
        return state.fail("duplicate source-build field");
    }
    *seen |= bit;
    Ok(())
}

pub(super) struct OptionalBuild<'a>(pub &'a mut ParseState);
impl<'de> DeserializeSeed<'de> for OptionalBuild<'_> {
    type Value = Option<SourceBuildRecord>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_option(self)
    }
}
impl<'de> Visitor<'de> for OptionalBuild<'_> {
    type Value = Option<SourceBuildRecord>;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("null or bounded source-build record")
    }
    fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_some<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        Build(self.0).deserialize(input).map(Some)
    }
}

struct Build<'a>(&'a mut ParseState);
impl<'de> DeserializeSeed<'de> for Build<'_> {
    type Value = SourceBuildRecord;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Build<'_> {
    type Value = SourceBuildRecord;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded SourceBuildRecord")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut seen = 0u16;
        let mut source_digest = None;
        let mut request_id = None;
        let mut recipe = None;
        let mut backend = None;
        let mut inputs = None;
        let mut output_filename = None;
        let mut output_digest = None;
        let mut output_size = None;
        let mut metadata_sha256 = None;
        let mut observation = None;
        let mut native = None;
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "source_digest" => {
                    once(self.0, &mut seen, 1)?;
                    source_digest = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 135,
                    })?);
                }
                "request_id" => {
                    once(self.0, &mut seen, 2)?;
                    request_id = Some(map.next_value::<[u8; 32]>()?);
                }
                "recipe" => {
                    once(self.0, &mut seen, 4)?;
                    recipe = Some(map.next_value::<u32>()?);
                }
                "backend" => {
                    once(self.0, &mut seen, 8)?;
                    backend = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 4096,
                    })?);
                }
                "inputs" => {
                    once(self.0, &mut seen, 16)?;
                    inputs = Some({
                        let maximum = self.0.limits.max_refs_per_generation.min(MAX_BUILD_INPUTS);
                        map.next_value_seed(List::<SourceBuildInput>::new(self.0, maximum))?
                    });
                }
                "output_filename" => {
                    once(self.0, &mut seen, 32)?;
                    output_filename = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 16384,
                    })?);
                }
                "output_digest" => {
                    once(self.0, &mut seen, 64)?;
                    output_digest = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 135,
                    })?);
                }
                "output_size" => {
                    once(self.0, &mut seen, 128)?;
                    output_size = Some(map.next_value::<u64>()?);
                }
                "metadata_sha256" => {
                    once(self.0, &mut seen, 256)?;
                    metadata_sha256 = Some(map.next_value::<[u8; 32]>()?);
                }
                "observation" => {
                    once(self.0, &mut seen, 512)?;
                    observation = map.next_value_seed(OptionalObservation(self.0))?;
                }
                "native" => {
                    once(self.0, &mut seen, 1024)?;
                    let raw = map.next_value_seed(OptionalText(Text {
                        state: self.0,
                        maximum: MAX_NATIVE_JSON_BYTES,
                    }))?;
                    if let Some(raw) = raw {
                        native = Some(match NativeToolchainRecord::decode_json(&raw) {
                            Ok(record) => record,
                            Err(_) => return self.0.fail("invalid native toolchain record"),
                        });
                    }
                }
                _ => return self.0.fail("unknown source-build field"),
            }
        }
        let value = SourceBuildRecord {
            source_digest: self.0.required(source_digest)?,
            request_id: self.0.required(request_id)?,
            recipe: self.0.required(recipe)?,
            backend: self.0.required(backend)?,
            inputs: self.0.required(inputs)?,
            output_filename: self.0.required(output_filename)?,
            output_digest: self.0.required(output_digest)?,
            output_size: self.0.required(output_size)?,
            metadata_sha256: self.0.required(metadata_sha256)?,
            observation,
            native,
        };
        if value.validate().is_err() {
            return self.0.fail("invalid source-build record");
        }
        Ok(value)
    }
}

struct OptionalObservation<'a>(&'a mut ParseState);
impl<'de> DeserializeSeed<'de> for OptionalObservation<'_> {
    type Value = Option<SourceBuildObservation>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_option(self)
    }
}
impl<'de> Visitor<'de> for OptionalObservation<'_> {
    type Value = Option<SourceBuildObservation>;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("null or bounded build observation")
    }
    fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_some<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        Observation(self.0).deserialize(input).map(Some)
    }
}
struct Observation<'a>(&'a mut ParseState);
impl<'de> DeserializeSeed<'de> for Observation<'_> {
    type Value = SourceBuildObservation;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Observation<'_> {
    type Value = SourceBuildObservation;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded source build observation")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut seen = 0u16;
        let (mut initial_requirements, mut initial_inputs, mut dynamic_requirements) =
            (None, None, None);
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            let maximum = self.0.limits.max_refs_per_generation.min(MAX_BUILD_INPUTS);
            match key.as_str() {
                "initial_requirements" => {
                    once(self.0, &mut seen, 1)?;
                    initial_requirements = Some(
                        map.next_value_seed(List::<BuildRequirement>::new(self.0, maximum))?
                            .into_iter()
                            .map(|value| value.0)
                            .collect(),
                    );
                }
                "initial_inputs" => {
                    once(self.0, &mut seen, 2)?;
                    initial_inputs =
                        Some(map.next_value_seed(List::<SourceBuildInput>::new(self.0, maximum))?);
                }
                "dynamic_requirements" => {
                    once(self.0, &mut seen, 4)?;
                    dynamic_requirements = Some(
                        map.next_value_seed(List::<BuildRequirement>::new(self.0, maximum))?
                            .into_iter()
                            .map(|value| value.0)
                            .collect(),
                    );
                }
                _ => return self.0.fail("unknown source-build observation field"),
            }
        }
        Ok(SourceBuildObservation {
            initial_requirements: self.0.required(initial_requirements)?,
            initial_inputs: self.0.required(initial_inputs)?,
            dynamic_requirements: self.0.required(dynamic_requirements)?,
        })
    }
}
struct BuildRequirement(String);
impl Element for BuildRequirement {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        Text {
            state,
            maximum: 16 * 1024,
        }
        .deserialize(input)
        .map(Self)
    }
}

struct Input<'a>(&'a mut ParseState);
impl<'de> DeserializeSeed<'de> for Input<'_> {
    type Value = SourceBuildInput;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Input<'_> {
    type Value = SourceBuildInput;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded SourceBuildInput")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut seen = 0u16;
        let mut name = None;
        let mut version = None;
        let mut source_pin = None;
        let mut registry = None;
        let mut base = None;
        let mut source_filename = None;
        let mut source_digest = None;
        let mut source_size = None;
        let mut source_is_sdist = None;
        let mut advertised = None;
        let mut output_filename = None;
        let mut output_digest = None;
        let mut output_size = None;
        while let Some(key) = map.next_key_seed(Text {
            state: self.0,
            maximum: 32,
        })? {
            match key.as_str() {
                "name" => {
                    once(self.0, &mut seen, 1)?;
                    name = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 1024,
                    })?);
                }
                "version" => {
                    once(self.0, &mut seen, 2)?;
                    version = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 1024,
                    })?);
                }
                "source_pin" => {
                    once(self.0, &mut seen, 4)?;
                    source_pin = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 64,
                    })?);
                }
                "registry" => {
                    once(self.0, &mut seen, 8)?;
                    registry = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 1024,
                    })?);
                }
                "base" => {
                    once(self.0, &mut seen, 16)?;
                    base = Some(map.next_value::<[u8; 32]>()?);
                }
                "source_filename" => {
                    once(self.0, &mut seen, 32)?;
                    source_filename = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 16384,
                    })?);
                }
                "source_digest" => {
                    once(self.0, &mut seen, 64)?;
                    source_digest = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 135,
                    })?);
                }
                "source_size" => {
                    once(self.0, &mut seen, 128)?;
                    source_size = Some(map.next_value::<u64>()?);
                }
                "source_is_sdist" => {
                    once(self.0, &mut seen, 256)?;
                    source_is_sdist = Some(map.next_value::<bool>()?);
                }
                "advertised" => {
                    once(self.0, &mut seen, 512)?;
                    advertised = Some(map.next_value::<bool>()?);
                }
                "output_filename" => {
                    once(self.0, &mut seen, 1024)?;
                    output_filename = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 16384,
                    })?);
                }
                "output_digest" => {
                    once(self.0, &mut seen, 2048)?;
                    output_digest = Some(map.next_value_seed(Text {
                        state: self.0,
                        maximum: 135,
                    })?);
                }
                "output_size" => {
                    once(self.0, &mut seen, 4096)?;
                    output_size = Some(map.next_value::<u64>()?);
                }
                _ => return self.0.fail("unknown source-build field"),
            }
        }
        let value = SourceBuildInput {
            name: self.0.required(name)?,
            version: self.0.required(version)?,
            source_pin: self.0.required(source_pin)?,
            registry: self.0.required(registry)?,
            base: self.0.required(base)?,
            source_filename: self.0.required(source_filename)?,
            source_digest: self.0.required(source_digest)?,
            source_size: self.0.required(source_size)?,
            source_is_sdist: self.0.required(source_is_sdist)?,
            advertised: self.0.required(advertised)?,
            output_filename: self.0.required(output_filename)?,
            output_digest: self.0.required(output_digest)?,
            output_size: self.0.required(output_size)?,
        };
        if value.content_pin().is_err() {
            return self.0.fail("invalid source-build record");
        }
        Ok(value)
    }
}

impl Element for SourceBuildInput {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error> {
        Input(state).deserialize(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> SourceBuildRecord {
        SourceBuildRecord {
            source_digest: Digest::Sha256([7; 32]).to_string(),
            request_id: [8; 32],
            recipe: 1,
            backend: "backend".into(),
            inputs: vec![],
            output_filename: "demo-1.0-py3-none-any.whl".into(),
            output_digest: Digest::Sha256([9; 32]).to_string(),
            output_size: 100,
            metadata_sha256: [10; 32],
            observation: None,
            native: None,
        }
    }
    fn decode(raw: &str) -> (bool, Option<&'static str>) {
        let mut state = ParseState {
            limits: ModelLimits::default(),
            references: 0,
            failure: None,
        };
        let result =
            OptionalBuild(&mut state).deserialize(&mut serde_json::Deserializer::from_str(raw));
        (result.is_ok(), state.failure)
    }
    #[test]
    fn s2_source_build_codec_bounds_new_text_before_model_validation() {
        let good = record();
        assert!(decode(&serde_json::to_string(&good).unwrap()).0);
        let mut oversized = good.clone();
        oversized.backend = "x".repeat(4097);
        assert_eq!(
            decode(&serde_json::to_string(&oversized).unwrap()),
            (false, Some("catalog field byte limit exceeded"))
        );
        oversized = good;
        oversized.output_filename = "x".repeat(16 * 1024 + 1);
        assert_eq!(
            decode(&serde_json::to_string(&oversized).unwrap()),
            (false, Some("catalog field byte limit exceeded"))
        );
    }
    #[test]
    fn s2_source_build_codec_refuses_duplicate_and_unknown_fields() {
        let good = serde_json::to_string(&record()).unwrap();
        let duplicate = good.replacen(
            "\"backend\":\"backend\"",
            "\"backend\":\"backend\",\"backend\":\"other\"",
            1,
        );
        assert_eq!(
            decode(&duplicate),
            (false, Some("duplicate source-build field"))
        );
        let unknown = format!("{},\"unexpected\":true}}", &good[..good.len() - 1]);
        assert_eq!(
            decode(&unknown),
            (false, Some("unknown source-build field"))
        );
    }
    #[test]
    fn p2_build_observation_codec_bounds_fields_and_lists() {
        let mut good = record();
        good.observation = Some(SourceBuildObservation {
            initial_requirements: vec![],
            initial_inputs: vec![],
            dynamic_requirements: vec![],
        });
        assert!(decode(&serde_json::to_string(&good).unwrap()).0);
        let mut oversized = good.clone();
        oversized.observation.as_mut().unwrap().initial_requirements =
            vec!["x".repeat(16 * 1024 + 1)];
        assert_eq!(
            decode(&serde_json::to_string(&oversized).unwrap()),
            (false, Some("catalog field byte limit exceeded"))
        );
        oversized = good;
        oversized.observation.as_mut().unwrap().dynamic_requirements =
            vec!["demo".into(); MAX_BUILD_INPUTS + 1];
        assert_eq!(
            decode(&serde_json::to_string(&oversized).unwrap()),
            (false, Some("catalog collection limit exceeded"))
        );
    }
    #[test]
    fn p2_build_observation_codec_rejects_duplicate_and_unknown_fields() {
        let mut good = record();
        good.observation = Some(SourceBuildObservation {
            initial_requirements: vec![],
            initial_inputs: vec![],
            dynamic_requirements: vec![],
        });
        let raw = serde_json::to_string(&good).unwrap();
        let duplicate = raw.replacen(
            "\"initial_inputs\":[]",
            "\"initial_inputs\":[],\"initial_inputs\":[]",
            1,
        );
        assert_eq!(
            decode(&duplicate),
            (false, Some("duplicate source-build field"))
        );
        let unknown = raw.replacen(
            "\"initial_inputs\":[]",
            "\"initial_inputs\":[],\"other\":[]",
            1,
        );
        assert_eq!(
            decode(&unknown),
            (false, Some("unknown source-build observation field"))
        );
    }
}
