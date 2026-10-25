//! pakro's settings: the types generated from `settings.toml` and the
//! process-wide cache that [`Settings::get`] reads.
//!
//! Loading settings means discovering config files, checking trust, and
//! rendering templates, all of which live in pakro itself. pakro registers that
//! loader once at startup with [`set_loader`]; [`Settings::try_get`] returns the
//! cached value or runs the loader. Keeping only the types and the cache here
//! lets lower-level crates read settings without depending on pakro's config
//! system.

use confique::Config;
use confique::env::parse::{list_by_colon, list_by_comma};
use eyre::{Result, bail};
use indexmap::{IndexMap, indexmap};
use itertools::Itertools;
use serde::ser::Error;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::env::consts::{ARCH, OS};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::LazyLock as Lazy;
use std::sync::{Arc, OnceLock, RwLock};

// settings are generated from .config/settings.toml in the workspace
// make sure you run `cargo run -p pakro-settings --example render-schema` after updating settings.toml
include!(concat!(env!("OUT_DIR"), "/settings.rs"));

pub enum SettingsType {
    Bool,
    String,
    Integer,
    Duration,
    Path,
    Url,
    ListString,
    ListPath,
    SetString,
    IndexMap,
    BoolOrString,
}

pub struct SettingsMeta {
    // pub key: String,
    pub type_: SettingsType,
    pub description: &'static str,
    pub env: Option<&'static str>,
    pub deprecated: Option<&'static str>,
    pub deprecated_warn_at: Option<&'static str>,
    pub deprecated_remove_at: Option<&'static str>,
    pub global_only: bool,
    /// Consumed before config files are read, so a value in one can never apply.
    pub env_only: bool,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    Default,
    strum::EnumString,
    strum::Display,
    PartialEq,
    Eq,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SettingsStatusMissingTools {
    /// never show the warning
    Never,
    /// hide this warning if the user hasn't installed at least 1 version of the tool before
    #[default]
    IfOtherVersionsInstalled,
    /// always show the warning if tools are missing
    Always,
}

fn remove_empty_nested_settings(table: &mut toml::Table, prefix: &str) {
    table.retain(|key, value| {
        let path = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        };
        let Some(child) = value.as_table_mut() else {
            return true;
        };
        remove_empty_nested_settings(child, &path);
        !child.is_empty() || SETTINGS_META.contains_key(path.as_str())
    });
}

pub type SettingsPartial = <Settings as Config>::Layer;

impl Display for Settings {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match toml::to_string_pretty(self) {
            Ok(s) => write!(f, "{s}"),
            Err(e) => Err(std::fmt::Error::custom(e)),
        }
    }
}

impl SettingsStatus {
    pub fn missing_tools(&self) -> SettingsStatusMissingTools {
        SettingsStatusMissingTools::from_str(&self.missing_tools).unwrap()
    }
}

/// Deserialize a string to a boolean, accepting "false", "no", "0"
/// and their case-insensitive variants as `false`. Any other value (incl. "") is considered `true`.
fn bool_string<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.to_lowercase().as_str() {
        "false" | "no" | "0" => Ok(false),
        _ => Ok(true),
    }
}

fn set_by_comma<T, C>(input: &str) -> Result<C, <T as FromStr>::Err>
where
    T: FromStr + Eq + Ord,
    C: FromIterator<T>,
{
    input
        .split(',')
        // Filter out empty strings
        .filter_map(|s| {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                Some(T::from_str(trimmed))
            } else {
                None
            }
        })
        // collect into BTreeSet to remove duplicates
        .collect::<Result<BTreeSet<_>, _>>()
        .map(|set| set.into_iter().collect())
}

fn validate_setting_enum_values<'a>(
    name: &str,
    values: impl IntoIterator<Item = &'a str>,
    allowed: &[&str],
) -> Result<()> {
    if let Some(invalid) = values.into_iter().find(|value| !allowed.contains(value)) {
        bail!(
            "invalid {name} value {invalid:?}; expected one of: {}",
            allowed.join(", ")
        );
    }
    Ok(())
}

fn normalize_tool_names(tools: &BTreeSet<String>) -> BTreeSet<String> {
    tools
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse URL replacements from JSON string format
/// Expected format: {"source_domain": "replacement_domain", ...}
pub fn parse_url_replacements(input: &str) -> Result<IndexMap<String, String>, serde_json::Error> {
    serde_json::from_str(input)
}

/// Parse a path list from an environment variable using the OS-native path
/// separator (`:` on Unix, `;` on Windows). This correctly handles Windows
/// absolute paths whose drive letters contain `:` (e.g. `C:\foo`).
fn list_by_os_path_separator<C>(input: &str) -> Result<C, std::convert::Infallible>
where
    C: FromIterator<PathBuf>,
{
    Ok(std::env::split_paths(input)
        .filter(|p| !p.as_os_str().is_empty())
        .collect())
}

mod accessors;
mod cache;
pub use cache::{Loader, clear, is_loaded, last_cached, load_defaults, set_loader, store};

#[cfg(test)]
mod tests;
