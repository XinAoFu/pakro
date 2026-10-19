//! Typed bounded scalar and collection seeds, with sanitized failure labels.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, DeserializeSeed, Deserializer, SeqAccess, Visitor};

use super::*;

pub(super) struct ParseState {
    pub limits: ModelLimits,
    pub references: usize,
    pub failure: Option<&'static str>,
}
impl ParseState {
    pub(super) fn fail<T, E: de::Error>(
        &mut self,
        message: &'static str,
    ) -> std::result::Result<T, E> {
        self.failure = Some(message);
        Err(E::custom("invalid catalog metadata"))
    }
    pub(super) fn once<E: de::Error>(
        &mut self,
        seen: &mut u8,
        bit: u8,
    ) -> std::result::Result<(), E> {
        if *seen & bit != 0 {
            return self.fail("duplicate catalog field");
        }
        *seen |= bit;
        Ok(())
    }
    pub(super) fn required<T, E: de::Error>(
        &mut self,
        value: Option<T>,
    ) -> std::result::Result<T, E> {
        value.ok_or_else(|| {
            self.failure = Some("catalog required field is missing");
            E::custom("invalid catalog metadata")
        })
    }
}

pub(super) struct Text<'a> {
    pub state: &'a mut ParseState,
    pub maximum: usize,
}
impl<'de> DeserializeSeed<'de> for Text<'_> {
    type Value = String;
    fn deserialize<D: Deserializer<'de>>(self, input: D) -> std::result::Result<String, D::Error> {
        input.deserialize_str(self)
    }
}
impl<'de> Visitor<'de> for Text<'_> {
    type Value = String;
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("bounded catalog string")
    }
    fn visit_str<E: de::Error>(self, text: &str) -> std::result::Result<String, E> {
        if text.len() > self.maximum {
            return self.state.fail("catalog field byte limit exceeded");
        }
        Ok(text.to_owned())
    }
    fn visit_string<E: de::Error>(self, text: String) -> std::result::Result<String, E> {
        if text.len() > self.maximum {
            return self.state.fail("catalog field byte limit exceeded");
        }
        Ok(text)
    }
}
pub(super) struct OptionalText<'a>(pub Text<'a>);
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
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("null or bounded catalog string")
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

pub(super) trait Element: Sized {
    fn parse<'de, D: Deserializer<'de>>(
        state: &mut ParseState,
        input: D,
    ) -> std::result::Result<Self, D::Error>;
}
pub(super) struct List<'a, T> {
    state: &'a mut ParseState,
    maximum: usize,
    references: bool,
    _element: PhantomData<T>,
}
impl<'a, T> List<'a, T> {
    pub(super) fn new(state: &'a mut ParseState, maximum: usize) -> Self {
        Self {
            state,
            maximum,
            references: false,
            _element: PhantomData,
        }
    }
    pub(super) fn inputs(state: &'a mut ParseState) -> Self {
        Self {
            maximum: state.limits.max_refs_per_generation,
            state,
            references: true,
            _element: PhantomData,
        }
    }
}
impl<'de, T: Element> DeserializeSeed<'de> for List<'_, T> {
    type Value = Vec<T>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        input: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_seq(self)
    }
}
impl<'de, T: Element> Visitor<'de> for List<'_, T> {
    type Value = Vec<T>;
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("bounded catalog collection")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> std::result::Result<Vec<T>, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = array.next_element_seed(Item::<T> {
            state: self.state,
            count: values.len(),
            maximum: self.maximum,
            references: self.references,
            _element: PhantomData,
        })? {
            values.push(value);
        }
        Ok(values)
    }
}
struct Item<'a, T> {
    state: &'a mut ParseState,
    count: usize,
    maximum: usize,
    references: bool,
    _element: PhantomData<T>,
}
impl<'de, T: Element> DeserializeSeed<'de> for Item<'_, T> {
    type Value = T;
    fn deserialize<D: Deserializer<'de>>(self, input: D) -> std::result::Result<T, D::Error> {
        if self.count >= self.maximum {
            return self.state.fail("catalog collection limit exceeded");
        }
        if self.references {
            let Some(total) = self
                .state
                .references
                .checked_add(1)
                .filter(|total| *total <= self.state.limits.max_total_references)
            else {
                return self.state.fail("catalog total reference limit exceeded");
            };
            self.state.references = total;
        }
        T::parse(self.state, input)
    }
}

pub(super) fn hex32(value: &str) -> Result<[u8; 32]> {
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid catalog SHA256 identity"
    );
    let mut result = [0; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| eyre::eyre!("invalid catalog SHA256 identity"))?;
    }
    Ok(result)
}
