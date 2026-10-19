//! A non-collecting pass bounds strings, arrays and map fields before typed DTOs.
use super::RuntimeStartupLimits;
use eyre::{Result, ensure};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt;

struct State {
    limits: RuntimeStartupLimits,
    elements: usize,
    limited: bool,
}
struct Value<'a> {
    state: &'a mut State,
    depth: usize,
    charge: bool,
}
impl<'de> DeserializeSeed<'de> for Value<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, input: D) -> std::result::Result<(), D::Error> {
        if self.depth > 16 || (self.charge && self.state.elements >= self.state.limits.max_paths) {
            self.state.limited = true;
            return Err(de::Error::custom("startup collection limit"));
        }
        if self.charge {
            self.state.elements += 1;
        }
        input.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Value<'_> {
    type Value = ();
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded startup JSON")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_none<E: de::Error>(self) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, text: &str) -> std::result::Result<(), E> {
        if text.len() > self.state.limits.max_path_bytes {
            self.state.limited = true;
            Err(E::custom("startup text limit"))
        } else {
            Ok(())
        }
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> std::result::Result<(), A::Error> {
        while array
            .next_element_seed(Value {
                state: self.state,
                depth: self.depth + 1,
                charge: true,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        let mut count = 0;
        while map
            .next_key_seed(Key {
                state: self.state,
                count,
            })?
            .is_some()
        {
            count += 1;
            map.next_value_seed(Value {
                state: self.state,
                depth: self.depth + 1,
                charge: false,
            })?;
        }
        Ok(())
    }
}
struct Key<'a> {
    state: &'a mut State,
    count: usize,
}
impl<'de> DeserializeSeed<'de> for Key<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, input: D) -> std::result::Result<(), D::Error> {
        if self.count >= 64 {
            self.state.limited = true;
            return Err(de::Error::custom("startup field limit"));
        }
        input.deserialize_str(Value {
            state: self.state,
            depth: 0,
            charge: false,
        })
    }
}
pub(super) fn check(raw: &[u8], limits: RuntimeStartupLimits) -> Result<()> {
    ensure!(
        raw.len() <= limits.max_stdout_bytes,
        "startup limit: output bytes exceeded"
    );
    let mut state = State {
        limits,
        elements: 0,
        limited: false,
    };
    let mut input = serde_json::Deserializer::from_slice(raw);
    Value {
        state: &mut state,
        depth: 0,
        charge: false,
    }
    .deserialize(&mut input)
    .map_err(|_| {
        eyre::eyre!(if state.limited {
            "startup limit: typed collection budget exceeded"
        } else {
            "startup protocol JSON is invalid"
        })
    })?;
    input
        .end()
        .map_err(|_| eyre::eyre!("startup protocol trailing JSON"))?;
    Ok(())
}
