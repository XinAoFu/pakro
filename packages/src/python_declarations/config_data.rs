//! Explicit frontend hook settings. Values never become shell arguments here.
use super::*;
type E = PythonDeclarationError;
type L = PythonDeclarationLimit;

pub(super) fn parse(
    entries: &[(&str, PythonBuildConfigValue<'_>)],
    limits: PythonProjectLimits,
) -> PythonDeclarationResult<PythonBuildConfigData> {
    decode::validate(limits)?;
    if entries.len() > limits.max_config_entries {
        return Err(E::Limit(L::ConfigEntries));
    }
    let (mut bytes, mut count) = (0usize, 0usize);
    let mut text = |value: &str| -> PythonDeclarationResult<()> {
        if value.len() > limits.max_field_bytes {
            return Err(E::Limit(L::FieldBytes));
        }
        bytes = bytes
            .checked_add(value.len())
            .filter(|n| *n <= limits.max_total_utf8_bytes)
            .ok_or(E::Limit(L::Utf8Bytes))?;
        Ok(())
    };
    // Preflight the original entries, including duplicate keys and values,
    // before constructing the owned mapping or combining repeated entries.
    for (key, value) in entries {
        text(key)?;
        let amount = match value {
            PythonBuildConfigValue::String(value) => {
                text(value)?;
                1
            }
            PythonBuildConfigValue::Strings(values) => {
                for value in *values {
                    text(value)?;
                }
                values.len()
            }
        };
        count = count
            .checked_add(amount)
            .filter(|n| *n <= limits.max_config_values)
            .ok_or(E::Limit(L::ConfigValues))?;
    }
    let mut values = BTreeMap::new();
    for (key, value) in entries {
        let next = match value {
            PythonBuildConfigValue::String(value) => {
                OwnedBuildConfigValue::String((*value).to_owned())
            }
            PythonBuildConfigValue::Strings(values) => {
                OwnedBuildConfigValue::Strings(values.iter().map(|v| (*v).to_owned()).collect())
            }
        };
        match values.entry((*key).to_owned()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(next);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let previous = entry.get_mut();
                let mut combined =
                    match std::mem::replace(previous, OwnedBuildConfigValue::Strings(Vec::new())) {
                        OwnedBuildConfigValue::String(value) => vec![value],
                        OwnedBuildConfigValue::Strings(values) => values,
                    };
                match next {
                    OwnedBuildConfigValue::String(value) => combined.push(value),
                    OwnedBuildConfigValue::Strings(values) => combined.extend(values),
                }
                *previous = OwnedBuildConfigValue::Strings(combined);
            }
        }
    }
    Ok(PythonBuildConfigData { values })
}
