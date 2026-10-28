//! Portable native toolchain descriptions. No local paths or execution authority.

use std::collections::BTreeMap;

use eyre::{Result, ensure};
use serde::{Deserialize, Serialize};

pub(crate) const MAX_NATIVE_JSON_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeProgramRecord {
    pub role: String,
    pub sha256: [u8; 32],
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeSdkRecord {
    pub kind: String,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeToolchainRecord {
    pub schema: u32,
    pub platform: String,
    pub programs: Vec<NativeProgramRecord>,
    pub sdk_roots: Vec<NativeSdkRecord>,
    #[serde(deserialize_with = "configuration")]
    pub configuration: BTreeMap<String, String>,
}

fn identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

fn portable_text(value: &str) -> bool {
    !value.chars().any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        && !value.split(|ch: char| ch.is_ascii_whitespace() || "\"'=,[]{}();".contains(ch)).any(|token| {
            let bytes = token.as_bytes();
            token.starts_with('/')
                || ["-I", "-L", "-isystem", "-isysroot"].iter().any(|flag| {
                    token.strip_prefix(flag).is_some_and(|rest| rest.starts_with('/'))
                })
                || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\'))
        })
}

fn configuration<'de, D: serde::Deserializer<'de>>(input: D) -> std::result::Result<BTreeMap<String, String>, D::Error> {
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            out.write_str("bounded unique native configuration")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if values.len() == 64 || values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("invalid native configuration count or duplicate key"));
                }
            }
            Ok(values)
        }
    }
    input.deserialize_map(Entries)
}

impl NativeToolchainRecord {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "unsupported native toolchain schema");
        ensure!(identifier(&self.platform, 128), "invalid native target platform");
        ensure!(!self.programs.is_empty() && self.programs.len() <= 64, "native program count exceeded");
        ensure!(self.sdk_roots.len() <= 8 && self.configuration.len() <= 64, "native toolchain collection limit exceeded");
        ensure!(self.programs.windows(2).all(|pair| pair[0].role < pair[1].role), "native program roles are not sorted and unique");
        ensure!(self.sdk_roots.windows(2).all(|pair| pair[0].kind < pair[1].kind), "native SDK kinds are not sorted and unique");
        for program in &self.programs {
            ensure!(identifier(&program.role, 96) && program.sha256 != [0; 32], "invalid native program identity");
            ensure!(!program.version.trim().is_empty() && program.version.len() <= 4096 && portable_text(&program.version), "invalid or nonportable native program version");
        }
        for sdk in &self.sdk_roots {
            ensure!(identifier(&sdk.kind, 96) && sdk.sha256 != [0; 32], "invalid native SDK identity");
        }
        for (key, value) in &self.configuration {
            ensure!(identifier(key, 128) && value.len() <= 4096 && portable_text(value), "invalid or nonportable native configuration");
        }
        ensure!(serde_json::to_vec(self)?.len() <= MAX_NATIVE_JSON_BYTES, "native toolchain byte limit exceeded");
        Ok(())
    }

    pub(crate) fn decode_json(raw: &str) -> Result<Self> {
        ensure!(raw.len() <= MAX_NATIVE_JSON_BYTES, "native toolchain byte limit exceeded");
        let record: Self = serde_json::from_str(raw).map_err(|_| eyre::eyre!("invalid native toolchain JSON"))?;
        record.validate()?;
        Ok(record)
    }

    pub(crate) fn canonical_json(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_json::to_string(self)?)
    }

    pub(crate) fn fingerprint(&self) -> Result<[u8; 32]> {
        use sha2::{Digest as _, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"pakro.native-toolchain/v1\0");
        digest.update(self.canonical_json()?.as_bytes());
        Ok(digest.finalize().into())
    }

    #[cfg(test)]
    pub(crate) fn test_record() -> Self {
        Self {
            schema: 1,
            platform: "linux-x86_64-gnu".into(),
            programs: vec![NativeProgramRecord { role: "cc".into(), sha256: [13; 32], version: "fixture compiler 1.0".into() }],
            sdk_roots: vec![NativeSdkRecord { kind: "sysroot".into(), sha256: [14; 32] }],
            configuration: BTreeMap::from([("compiler-arguments".into(), "[\"-O2\",\"@sysroot\"]".into())]),
        }
    }
}

pub(super) mod optional_json {
    use super::*;

    pub(crate) fn serialize<S: serde::Serializer>(value: &Option<NativeToolchainRecord>, output: S) -> std::result::Result<S::Ok, S::Error> {
        value.as_ref().map(serde_json::to_string).transpose().map_err(serde::ser::Error::custom)?.serialize(output)
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(input: D) -> std::result::Result<Option<NativeToolchainRecord>, D::Error> {
        Option::<String>::deserialize(input)?.map(|raw| NativeToolchainRecord::decode_json(&raw).map_err(serde::de::Error::custom)).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p6_native_record_roundtrip_and_fingerprint_cover_fixed_inputs() {
        let original = NativeToolchainRecord::test_record();
        let encoded = original.canonical_json().unwrap();
        assert_eq!(NativeToolchainRecord::decode_json(&encoded).unwrap(), original);
        let first = original.fingerprint().unwrap();
        let mut changed = original.clone();
        changed.programs[0].sha256[0] ^= 1;
        assert_ne!(first, changed.fingerprint().unwrap());
        changed = original.clone();
        changed.sdk_roots[0].sha256[0] ^= 1;
        assert_ne!(first, changed.fingerprint().unwrap());
        changed = original;
        changed.configuration.insert("optimization".into(), "3".into());
        assert_ne!(first, changed.fingerprint().unwrap());
    }

    #[test]
    fn p6_native_record_rejects_unknown_duplicate_nonportable_and_oversized_input() {
        let original = NativeToolchainRecord::test_record();
        let encoded = original.canonical_json().unwrap();
        assert!(NativeToolchainRecord::decode_json(&encoded.replacen("\"schema\":1", "\"schema\":1,\"unknown\":true", 1)).is_err());
        assert!(NativeToolchainRecord::decode_json(&encoded.replacen("\"compiler-arguments\":", "\"compiler-arguments\":\"first\",\"compiler-arguments\":", 1)).is_err());
        for value in ["/private/local-sdk", "[\"-I/private/local-sdk\"]", "C:\\local\\sdk"] {
            let mut changed = original.clone();
            changed.configuration.insert("flags".into(), value.into());
            assert!(changed.validate().is_err(), "{value}");
        }
        let mut changed = original.clone();
        changed.programs.push(changed.programs[0].clone());
        assert!(changed.validate().is_err());
        changed = original;
        changed.programs[0].version = "x".repeat(4097);
        assert!(changed.validate().is_err());
        assert!(NativeToolchainRecord::decode_json(&" ".repeat(MAX_NATIVE_JSON_BYTES + 1)).is_err());
    }
}
