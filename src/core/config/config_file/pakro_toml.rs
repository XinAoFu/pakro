use eyre::{WrapErr, eyre};
use indexmap::IndexMap;
use itertools::Itertools;
use once_cell::sync::OnceCell;
use path_absolutize::Absolutize;
use serde::Deserialize;
use serde::de::Visitor;
use serde::{Deserializer, de};
use std::fmt::{Debug, Formatter};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Mutex, MutexGuard},
};
use tera::Context as TeraContext;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Key, Value, table, value};
use versions::Versioning;

use crate::args::BackendArg;
use crate::backend::unalias_backend;
use crate::config::config_file::{
    ConfigFile, ToolConfig, config_trust_root, is_ignored, trust, trust_check,
};
use crate::config::config_file::{config_root, toml::deserialize_arr};
use crate::config::env_directive::{AgeFormat, EnvDirective, EnvDirectiveOptions, EnvValue};
use crate::config::settings::SettingsPartial;
use crate::config::{Alias, AliasMap, CommandWrapper, Config, Settings};
use crate::env_diff::EnvMap;
use crate::file::{create_dir_all, display_path};
use crate::hooks::{Hook, HookDef, Hooks};
use crate::redactions::Redactions;
use crate::registry::REGISTRY;
use crate::tera::{BASE_CONTEXT, contains_template_syntax, get_tera, render_str};
use crate::toolset::{ToolRequest, ToolRequestSet, ToolSource, ToolVersionOptions};
use crate::{env, file};

use super::diagnostic::toml_parse_error;
use super::min_version::MinVersionSpec;

const LEGACY_ENV_KEYS_DEPRECATED_WARN_AT: &str = "2026.4.17";
const LEGACY_ENV_KEYS_DEPRECATED_REMOVE_AT: &str = "2027.4.0";

const REMOTE_FRAGMENT_ALLOWED: &[&str] = &[
    "_",
    "min_version",
    "tools",
    "tool_alias",
    "alias",
    "shell_alias",
    "plugins",
    "wrappers",
    "hooks",
    "env",
    "env_path",
    "vars",
];

fn unsupported_fragment_key(table: &toml::Table) -> Option<&str> {
    table
        .keys()
        .map(String::as_str)
        .find(|key| !REMOTE_FRAGMENT_ALLOWED.contains(key))
}

/// Convert a `toml::Value` to a `toml_edit::Value` for serialization.
fn toml_value_to_edit(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::from(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => Value::from(f),
        toml::Value::Boolean(b) => Value::from(b),
        toml::Value::Datetime(dt) => {
            // Parse the datetime string back into a toml_edit datetime
            dt.to_string()
                .parse::<toml_edit::Datetime>()
                .map(Value::from)
                .unwrap_or_else(|_| Value::from(dt.to_string()))
        }
        toml::Value::Array(arr) => {
            let mut edit_arr = Array::new();
            for item in arr {
                edit_arr.push(toml_value_to_edit(item));
            }
            Value::Array(edit_arr)
        }
        toml::Value::Table(table) => {
            let mut edit_table = InlineTable::new();
            for (k, v) in table {
                edit_table.insert(k, toml_value_to_edit(v));
            }
            Value::InlineTable(edit_table)
        }
    }
}

fn normalize_option_template_value(value: toml::Value) -> toml::Value {
    match value {
        toml::Value::String(s) => toml::Value::String(s.replace("{{version}}", "{version}")),
        value => value,
    }
}

fn should_normalize_option_template(key: &str) -> bool {
    !matches!(key, "os" | "depends" | "install_env" | "lazy" | "lazy_bins")
        && !key.starts_with("install_env.")
}

fn insert_tool_option<E>(
    options: &mut ToolVersionOptions,
    key: String,
    value: toml::Value,
) -> std::result::Result<(), E>
where
    E: de::Error,
{
    let value = if should_normalize_option_template(&key) {
        normalize_option_template_value(value)
    } else {
        value
    };
    options.insert_option(key, value).map_err(de::Error::custom)
}

#[derive(Deserialize)]
#[serde(try_from = "RawToolMap")]
pub(crate) struct ParsedToolMap {
    pub request: String,
    pub options: IndexMap<String, toml::Value>,
}

#[derive(Deserialize)]
struct RawToolMap {
    version: Option<toml::Value>,
    path: Option<toml::Value>,
    prefix: Option<toml::Value>,
    #[serde(rename = "ref")]
    ref_: Option<toml::Value>,
    #[serde(flatten)]
    options: IndexMap<String, toml::Value>,
}

fn parse_tool_selector(
    key: &'static str,
    value: Option<toml::Value>,
) -> std::result::Result<Option<(&'static str, String)>, String> {
    match value {
        Some(toml::Value::String(value)) => Ok(Some((key, value))),
        Some(_) => Err(format!("tool selector `{key}` must be a string")),
        None => Ok(None),
    }
}

impl TryFrom<RawToolMap> for ParsedToolMap {
    type Error = String;

    fn try_from(raw: RawToolMap) -> std::result::Result<Self, Self::Error> {
        let selectors = [
            parse_tool_selector("version", raw.version)?,
            parse_tool_selector("path", raw.path)?,
            parse_tool_selector("prefix", raw.prefix)?,
            parse_tool_selector("ref", raw.ref_)?,
        ];
        let mut selectors = selectors.into_iter().flatten();
        let Some((key, value)) = selectors.next() else {
            return Err(
                "tool definition must include exactly one of `version`, `path`, `prefix`, or `ref`"
                    .to_string(),
            );
        };
        if let Some((other, _)) = selectors.next() {
            return Err(format!(
                "tool definition cannot specify both `{key}` and `{other}`"
            ));
        }
        let request = if key == "version" {
            value
        } else {
            format!("{key}:{value}")
        };
        Ok(Self {
            request,
            options: raw.options,
        })
    }
}

fn insert_core_options(table: &mut InlineTable, options: ToolVersionOptions) {
    let core = options.core;
    if let Some(os) = core.os
        && !os.is_empty()
    {
        let mut arr = Array::new();
        for o in os {
            arr.push(Value::from(o));
        }
        table.insert("os", Value::Array(arr));
    }
    if let Some(depends) = core.depends
        && !depends.is_empty()
    {
        let mut arr = Array::new();
        for dep in depends {
            arr.push(Value::from(dep));
        }
        table.insert("depends", Value::Array(arr));
    }
    if !core.install_env.is_empty() {
        let mut env = InlineTable::new();
        for (k, v) in core.install_env {
            env.insert(k, v.into());
        }
        table.insert("install_env", env.into());
    }
    if let Some(lazy) = core.lazy {
        table.insert("lazy", Value::from(lazy));
    }
    if !core.lazy_bins.is_empty() {
        let mut bins = Array::new();
        for bin in core.lazy_bins {
            bins.push(Value::from(bin));
        }
        table.insert("lazy_bins", Value::Array(bins));
    }
}

const TOOL_SELECTOR_KEYS: [&str; 4] = ["version", "prefix", "ref", "path"];

fn tool_request_selector(request: &ToolRequest) -> (&'static str, String) {
    match request {
        ToolRequest::Version { version, .. } => ("version", version.clone()),
        ToolRequest::Prefix { prefix, .. } => ("prefix", prefix.clone()),
        ToolRequest::Ref { ref_, ref_type, .. } if ref_type == "ref" => ("ref", ref_.clone()),
        ToolRequest::Path { .. } => (
            "path",
            request.version().strip_prefix("path:").unwrap().to_string(),
        ),
        // `tag:`, `branch:`, `rev:`, `sub-N:` and `system` have no dedicated
        // key in table syntax, so preserve their complete request string in
        // the general `version` selector.
        _ => ("version", request.version()),
    }
}

fn insert_table_item_preserving_decor(table: &mut toml_edit::Table, key: &str, mut item: Item) {
    let key = get_key_with_decor(table, key);
    let value_decor = get_value_decor(table, key.get());
    set_value_decor(&mut item, &value_decor);
    table.insert_formatted(&key, item);
}

fn insert_inline_value_preserving_decor(table: &mut InlineTable, key: &str, mut value: Value) {
    let key = table
        .get_key_value(key)
        .map(|(key, _)| key.clone())
        .unwrap_or_else(|| Key::from(key));
    if let Some(existing) = table.get(key.get()) {
        *value.decor_mut() = existing.decor().clone();
    }
    table.insert_formatted(&key, value);
}

fn update_install_env_table(table: &mut toml_edit::Table, options: &ToolVersionOptions) {
    if options.install_env.is_empty() {
        return;
    }

    match table.get_mut("install_env") {
        Some(Item::Table(env)) => {
            for (key, env_value) in &options.install_env {
                insert_table_item_preserving_decor(env, key, value(env_value.clone()));
            }
        }
        Some(Item::Value(Value::InlineTable(env))) => {
            for (key, env_value) in &options.install_env {
                insert_inline_value_preserving_decor(env, key, env_value.clone().into());
            }
        }
        _ => {
            let mut env = InlineTable::new();
            for (key, env_value) in &options.install_env {
                env.insert(key, env_value.clone().into());
            }
            insert_table_item_preserving_decor(
                table,
                "install_env",
                Item::Value(Value::InlineTable(env)),
            );
        }
    }
}

fn update_explicit_tool_options(table: &mut toml_edit::Table, options: &ToolVersionOptions) {
    for (key, value) in &options.opts {
        insert_table_item_preserving_decor(
            table,
            key,
            Item::Value(toml_value_to_edit(value.clone())),
        );
    }
    if let Some(os) = options.os.as_ref().filter(|os| !os.is_empty()) {
        let mut arr = Array::new();
        for os in os {
            arr.push(os.as_str());
        }
        insert_table_item_preserving_decor(table, "os", Item::Value(Value::Array(arr)));
    }
    if let Some(depends) = options
        .depends
        .as_ref()
        .filter(|depends| !depends.is_empty())
    {
        let mut arr = Array::new();
        for dependency in depends {
            arr.push(dependency.as_str());
        }
        insert_table_item_preserving_decor(table, "depends", Item::Value(Value::Array(arr)));
    }
    if let Some(lazy) = options.lazy {
        insert_table_item_preserving_decor(table, "lazy", value(lazy));
    }
    if !options.lazy_bins.is_empty() {
        let mut bins = Array::new();
        for bin in &options.lazy_bins {
            bins.push(bin.as_str());
        }
        insert_table_item_preserving_decor(table, "lazy_bins", Item::Value(Value::Array(bins)));
    }
    update_install_env_table(table, options);
}

fn update_standard_tool_table(
    table: &mut toml_edit::Table,
    request: &ToolRequest,
    explicit_options: Option<&ToolVersionOptions>,
) {
    let (selector, selector_value) = tool_request_selector(request);
    let existing_selector = TOOL_SELECTOR_KEYS
        .iter()
        .find(|key| table.contains_key(key))
        .copied();
    let selector_key = existing_selector
        .map(|existing| get_key_with_decor_from(table, selector, existing))
        .unwrap_or_else(|| Key::from(selector));
    let selector_decor = existing_selector.and_then(|key| get_value_decor(table, key));
    let mut selector_item = value(selector_value);
    set_value_decor(&mut selector_item, &selector_decor);

    if let Some(existing) = existing_selector {
        let names = table.iter().map(|(key, _)| key.to_string()).collect_vec();
        let entries = names
            .into_iter()
            .filter_map(|key| table.remove_entry(&key).map(|entry| (key, entry)))
            .collect_vec();
        for (key, (formatted_key, item)) in entries {
            if key == existing {
                table.insert_formatted(&selector_key, selector_item.clone());
            } else if !TOOL_SELECTOR_KEYS.contains(&key.as_str()) {
                table.insert_formatted(&formatted_key, item);
            }
        }
    } else {
        table.insert_formatted(&selector_key, selector_item);
    }

    // Options omitted from the CLI are inherited from this table, not removed. Only rewrite
    // options the user supplied explicitly so their existing representation and decor survive.
    if let Some(options) = explicit_options {
        update_explicit_tool_options(table, options);
    }
}

fn replace_tool_entries_preserving_position(
    tools: &mut toml_edit::Table,
    keys: &[String],
    key: Key,
    item: Item,
) {
    let Some(first) = keys.first() else {
        tools.insert_formatted(&key, item);
        return;
    };
    let names = tools.iter().map(|(key, _)| key.to_string()).collect_vec();
    let entries = names
        .into_iter()
        .filter_map(|name| tools.remove_entry(&name).map(|entry| (name, entry)))
        .collect_vec();
    for (name, (formatted_key, existing_item)) in entries {
        if &name == first {
            tools.insert_formatted(&key, item.clone());
        } else if !keys.contains(&name) {
            tools.insert_formatted(&formatted_key, existing_item);
        }
    }
}

#[derive(Default, Deserialize)]
pub struct PakroToml {
    #[serde(rename = "_")]
    custom: Option<toml::Value>,
    #[serde(default, deserialize_with = "deserialize_min_version")]
    min_version: Option<MinVersionSpec>,
    #[serde(skip)]
    context: TeraContext,
    #[serde(skip)]
    path: PathBuf,
    #[serde(default, deserialize_with = "deserialize_arr")]
    env_file: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_arr")]
    dotenv: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_root_env")]
    env: EnvList,
    #[serde(default, deserialize_with = "deserialize_arr")]
    env_path: Vec<String>,
    #[serde(default)]
    alias: AliasMap,
    #[serde(default)]
    tool_alias: AliasMap,
    #[serde(default)]
    shell_alias: IndexMap<String, String>,
    #[serde(default)]
    wrappers: IndexMap<String, CommandWrapper>,
    #[serde(skip)]
    doc: Mutex<OnceCell<DocumentMut>>,
    #[serde(default)]
    hooks: IndexMap<Hooks, HookDef>,
    #[serde(default)]
    tools: Mutex<IndexMap<BackendArg, PakroTomlToolList>>,
    #[serde(default)]
    plugins: HashMap<String, String>,
    #[serde(default)]
    redactions: Redactions,
    #[serde(default)]
    tool_config: ToolConfig,
    #[serde(default)]
    doctor: crate::config::doctor::DoctorConfig,
    #[serde(default, deserialize_with = "deserialize_vars")]
    vars: EnvList,
    #[serde(default)]
    settings: SettingsPartial,
    /// Marks this config as a monorepo root for lockfile discovery.
    #[serde(default)]
    monorepo_root: Option<bool>,
    /// Legacy name for monorepo_root, retained during its deprecation period
    #[serde(default)]
    experimental_monorepo_root: Option<bool>,
    /// Configuration for monorepo and workspace discovery
    #[serde(default)]
    monorepo: Option<MonorepoConfig>,
    /// Complete package declarations grouped by runtime identity. String pins
    /// and strict option tables share the ordinary and static readers.
    #[serde(default)]
    pub(crate) packages: std::sync::Mutex<crate::packages::request::PackageConfigTable>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct PakroTomlToolList(Vec<PakroTomlTool>);

#[derive(Debug, Clone)]
pub(crate) struct PakroTomlTool {
    /// The version request exactly as written, still un-rendered.
    ///
    /// Deliberately not parsed into a [`ToolVersionType`] here: a `:` inside a template belongs to
    /// the template, not to a version selector, and templates are not rendered until
    /// [`PakroToml::to_tool_request_set`]. Parsing at deserialize time made
    /// `{{ exec(command='echo VER: 1.2.3') | split(pat=': ') | last }}` fail as
    /// `invalid prefix: {{ exec(command='echo VER`. `ToolRequest::new` parses the rendered string,
    /// so selectors like `prefix:` and `ref:` are still honoured — just later. This matches what
    /// `[tasks.*.tools]` and `.tool-versions` already do.
    ///
    /// See: <https://github.com/jdx/mise/discussions/5531>
    pub request: String,
    pub options: Option<ToolVersionOptions>,
}

fn parse_pakro_toml_tool_map<E>(parsed: ParsedToolMap) -> std::result::Result<PakroTomlTool, E>
where
    E: de::Error,
{
    let mut options = ToolVersionOptions::default();
    for (key, value) in parsed.options {
        insert_tool_option(&mut options, key, value)?;
    }
    Ok(PakroTomlTool {
        request: parsed.request,
        options: Some(options),
    })
}

#[derive(Debug, Default, Clone)]
pub struct EnvList(pub Vec<EnvDirective>);

/// Configuration for the [monorepo] section in pakro.toml.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct MonorepoConfig {
    /// Explicit list of config roots for monorepo lockfile discovery.
    /// Supports single-level glob patterns (*).
    pub config_roots: Option<Vec<String>>,
    /// Short names for configured monorepo roots.
    #[serde(default)]
    pub path_aliases: BTreeMap<String, String>,
    /// Use a single lockfile at the monorepo root for descendant config roots.
    /// None follows the rollout default; true opts in, false keeps colocated locks.
    pub lockfile: Option<bool>,
}

impl EnvList {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl PakroToml {
    fn enforce_min_version_fallback(body: &str) -> eyre::Result<()> {
        if let Ok(val) = toml::from_str::<toml::Value>(body)
            && let Some(min_val) = val.get("min_version")
        {
            let mut hard_req: Option<versions::Versioning> = None;
            let mut soft_req: Option<versions::Versioning> = None;
            match min_val {
                toml::Value::String(s) => {
                    hard_req = versions::Versioning::new(s);
                }
                toml::Value::Table(t) => {
                    if let Some(toml::Value::String(s)) = t.get("hard") {
                        hard_req = versions::Versioning::new(s);
                    }
                    if let Some(toml::Value::String(s)) = t.get("soft") {
                        soft_req = versions::Versioning::new(s);
                    }
                }
                _ => {}
            }
            if let Some(spec) =
                crate::config::config_file::min_version::MinVersionSpec::new(hard_req, soft_req)
            {
                crate::config::Config::enforce_min_version_spec(&spec)?;
            }
        }
        Ok(())
    }
    pub fn init(path: &Path) -> Self {
        let mut context = BASE_CONTEXT.clone();
        context.insert(
            "config_root",
            config_root::config_root(path).to_str().unwrap(),
        );
        context.insert("config_source", &config_root::config_source(path));
        let mut rf = Self {
            path: path.to_path_buf(),
            context,
            ..Default::default()
        };
        rf.update_context_env(env::PRISTINE_ENV.clone());
        rf
    }

    pub fn from_file(path: &Path) -> eyre::Result<Self> {
        let body = file::read_to_string(path)?;
        Self::from_str(&body, path)
    }

    pub(super) fn validate_static_include(body: &str) -> Result<(), super::StaticConfigError> {
        let table: toml::Table =
            toml::from_str(body).map_err(|_| super::StaticConfigError::InvalidDocument)?;
        if unsupported_fragment_key(&table).is_some() {
            return Err(super::StaticConfigError::UnsupportedInclude);
        }
        Ok(())
    }

    /// Reuse the native declaration decoders while keeping backend names raw.
    /// Nothing here constructs Config, BackendArg, Tera, trust or task contexts.
    pub(super) fn static_declarations(
        body: &str,
    ) -> Result<super::StaticConfigDeclarations, super::StaticConfigError> {
        use super::{
            StaticConfigDeclarations, StaticConfigError as Error, StaticInputKind as Input,
            StaticToolDeclaration,
        };
        let table: toml::Table = toml::from_str(body).map_err(|_| Error::InvalidDocument)?;
        let minimum_version = table
            .get("min_version")
            .map(|value| deserialize_min_version(value.clone()).map_err(|_| Error::InvalidDocument))
            .transpose()?
            .flatten();
        if minimum_version
            .as_ref()
            .is_some_and(|spec| spec.hard_violation(&crate::version::V).is_some())
        {
            return Err(Error::MinimumVersionNotMet);
        }
        let minimum_version_recommended = minimum_version
            .as_ref()
            .is_some_and(|spec| spec.soft_violation(&crate::version::V).is_some());
        let mut tools = vec![];
        if let Some(value) = table.get("tools") {
            if toml_value_has_template(value) {
                return Err(Error::MissingStaticInput(Input::ToolTemplate));
            }
            let entries = value.as_table().ok_or(Error::InvalidDocument)?;
            for (name, value) in entries {
                // This is the existing native tool grammar and option decoder.
                PakroTomlToolList::deserialize(value.clone())
                    .map_err(|_| Error::InvalidDocument)?;
                let entries = match value {
                    toml::Value::Array(values) => values.as_slice(),
                    value => std::slice::from_ref(value),
                };
                for entry in entries {
                    let (selector, version, options) = match entry {
                        toml::Value::String(version) => {
                            ("version", version.clone(), toml::Table::new())
                        }
                        toml::Value::Table(options) => {
                            let selector = ["version", "path", "prefix", "ref"]
                                .into_iter()
                                .find(|selector| options.contains_key(*selector))
                                .ok_or(Error::InvalidDocument)?;
                            let version = options[selector]
                                .as_str()
                                .ok_or(Error::InvalidDocument)?
                                .to_owned();
                            let mut options = options.clone();
                            options.remove(selector);
                            (selector, version, options)
                        }
                        _ => return Err(Error::InvalidDocument),
                    };
                    tools.push(StaticToolDeclaration {
                        name: name.clone(),
                        selector: selector.to_owned(),
                        version,
                        options,
                    });
                }
            }
        }
        let packages = table
            .get("packages")
            .map(|value| {
                let packages =
                    crate::packages::request::PackageConfigTable::deserialize(value.clone())
                        .map_err(|error| Error::InvalidPackages(error.to_string()))?;
                crate::packages::requests_from_config(&packages).map_err(Error::InvalidPackages)
            })
            .transpose()?
            .unwrap_or_default();
        let includes: Vec<String> = table
            .get("include")
            .map(|value| {
                if toml_value_has_template(value) {
                    return Err(Error::MissingStaticInput(Input::IncludeTemplate));
                }
                deserialize_arr(value.clone()).map_err(|_| Error::InvalidDocument)
            })
            .transpose()?
            .unwrap_or_default();
        let read_env = |key: &str, kind| -> Result<Vec<EnvDirective>, Error> {
            let Some(value) = table.get(key) else {
                return Ok(vec![]);
            };
            if toml_value_has_template(value) {
                return Err(Error::MissingStaticInput(kind));
            }
            let list = if key == "vars" {
                deserialize_vars(value.clone())
            } else {
                deserialize_root_env(value.clone())
            }
            .map_err(|_| Error::InvalidDocument)?;
            for directive in &list.0 {
                if directive.options().expand
                    || !matches!(
                        directive,
                        EnvDirective::Val(..)
                            | EnvDirective::Default(..)
                            | EnvDirective::Rm(..)
                            | EnvDirective::Path(..)
                    )
                {
                    return Err(Error::MissingStaticInput(kind));
                }
            }
            Ok(list.0)
        };
        if ["env_file", "dotenv"]
            .iter()
            .any(|key| table.contains_key(*key))
        {
            return Err(Error::MissingStaticInput(Input::Environment));
        }
        let mut environment = read_env("env", Input::Environment)?;
        if let Some(value) = table.get("env_path") {
            if toml_value_has_template(value) {
                return Err(Error::MissingStaticInput(Input::Environment));
            }
            let paths: Vec<String> =
                deserialize_arr(value.clone()).map_err(|_| Error::InvalidDocument)?;
            environment.splice(
                0..0,
                paths
                    .into_iter()
                    .map(|path| EnvDirective::Path(path, Default::default())),
            );
        }
        let variables = read_env("vars", Input::Variables)?;
        let fields = table.keys().cloned().collect();
        let mut unprojected_fields = table;
        for projected in ["tools", "packages", "include", "env", "vars", "env_path"] {
            unprojected_fields.remove(projected);
        }
        Ok(StaticConfigDeclarations {
            tools,
            packages,
            includes,
            fields,
            environment,
            variables,
            unprojected_fields,
            minimum_version_recommended,
        })
    }

    /// Decode only the static monorepo declarations of a config, without
    /// trusting or evaluating it, applying the same legacy
    /// `experimental_monorepo_root` alias as normal loading. The deprecation
    /// warning is left to normal loading.
    pub(crate) fn for_monorepo_inspection(body: &str, path: &Path) -> eyre::Result<Self> {
        let mut parsed: Self = toml::from_str(body)?;
        parsed.path = path.to_path_buf();
        if let Some(legacy_monorepo_root) = parsed.experimental_monorepo_root.take() {
            parsed.monorepo_root.get_or_insert(legacy_monorepo_root);
        }
        Ok(parsed)
    }

    pub fn from_str(body: &str, path: &Path) -> eyre::Result<Self> {
        Self::parse_body(body, path, true)
    }

    /// Read only the held declarations, preserving invalid-package errors.
    /// Unlike the legacy ConfigFile projection, this never treats errors as
    /// an empty dependency set and never rereads or edits the document.
    pub(super) fn checked_package_requests(
        &self,
    ) -> Result<Vec<crate::packages::PackageInstallRequest>, String> {
        let packages = self
            .packages
            .lock()
            .map_err(|_| "package declarations are unavailable".to_owned())?;
        crate::packages::requests_from_config(&packages)
    }

    fn parse_body(body: &str, path: &Path, check_trust: bool) -> eyre::Result<Self> {
        if check_trust && !Self::is_trust_exempt(body, path) {
            trust_check(path)?;
        }
        trace!("parsing: {}", display_path(path));
        let des = toml::Deserializer::parse(body).map_err(|e| toml_parse_error(&e, body, path))?;
        let de_res = serde_ignored::deserialize(des, |p| {
            warn!("unknown field in {}: {p}", display_path(path));
        });
        let mut rf: PakroToml = match de_res {
            Ok(rf) => rf,
            Err(err) => {
                Self::enforce_min_version_fallback(body)?;
                return Err(toml_parse_error(&err, body, path));
            }
        };
        rf.checked_package_requests()
            .map_err(|error| eyre!(error))?;
        if let Some(legacy_monorepo_root) = rf.experimental_monorepo_root.take() {
            deprecated_at!(
                "2026.7.7",
                "2027.12.0",
                "config.experimental_monorepo_root",
                "`experimental_monorepo_root` in {} is deprecated. Use `monorepo_root` instead.",
                display_path(path)
            );
            rf.monorepo_root.get_or_insert(legacy_monorepo_root);
        }
        rf.context = BASE_CONTEXT.clone();
        rf.context.insert(
            "config_root",
            config_root::config_root(path).to_str().unwrap(),
        );
        rf.context
            .insert("config_source", &config_root::config_source(path));
        rf.update_context_env(env::PRISTINE_ENV.clone());
        rf.path = path.to_path_buf();
        // trace!("{}", rf.dump()?);
        Ok(rf)
    }

    /// Whether the config file at `path` loads without trust (see
    /// [`Self::is_trust_exempt`]). Returns false for unreadable files and for
    /// non-pakro.toml files (e.g. `.tool-versions`), which have their own flow.
    pub(crate) fn path_is_trust_exempt(path: &Path) -> bool {
        file::read_to_string(path).is_ok_and(|body| Self::is_trust_exempt(&body, path))
    }

    /// Whether this config body can be loaded without trusting the file.
    ///
    /// Safe configs cannot execute code or change pakro's behavior beyond
    /// requesting tool versions, so there is nothing to
    /// gate behind a trust prompt. Anything else — env vars, hooks, settings,
    /// aliases, templates, tool options like `postinstall`/`install_env` —
    /// still requires trust.
    fn is_trust_exempt(body: &str, path: &Path) -> bool {
        if Settings::try_get().is_ok_and(|settings| settings.paranoid) {
            return false;
        }
        // configs the user chose to ignore should stay unloaded rather than
        // becoming loadable because their content happens to be safe
        if is_ignored(&config_trust_root(path)) || is_ignored(path) {
            return false;
        }
        is_safe_config_body(body)
    }

    fn doc(&self) -> eyre::Result<DocumentMut> {
        self.doc
            .lock()
            .unwrap()
            .get_or_try_init(|| {
                let body = file::read_to_string(&self.path).unwrap_or_default();
                Ok(body.parse()?)
            })
            .cloned()
    }

    fn warn_deprecated_env_keys(&self) {
        if !self.env_file.is_empty() {
            deprecated_at!(
                LEGACY_ENV_KEYS_DEPRECATED_WARN_AT,
                LEGACY_ENV_KEYS_DEPRECATED_REMOVE_AT,
                "config.env_file",
                "`env_file` in {} is deprecated. Use `env._.file` instead.",
                display_path(&self.path)
            );
        }
        if !self.dotenv.is_empty() {
            deprecated_at!(
                LEGACY_ENV_KEYS_DEPRECATED_WARN_AT,
                LEGACY_ENV_KEYS_DEPRECATED_REMOVE_AT,
                "config.dotenv",
                "`dotenv` in {} is deprecated. Use `env._.file` instead.",
                display_path(&self.path)
            );
        }
        if !self.env_path.is_empty() {
            deprecated_at!(
                LEGACY_ENV_KEYS_DEPRECATED_WARN_AT,
                LEGACY_ENV_KEYS_DEPRECATED_REMOVE_AT,
                "config.env_path",
                "`env_path` in {} is deprecated. Use `env._.path` instead.",
                display_path(&self.path)
            );
        }
    }

    fn doc_mut(&self) -> eyre::Result<MutexGuard<'_, OnceCell<DocumentMut>>> {
        self.doc()?;
        Ok(self.doc.lock().unwrap())
    }

    pub fn set_backend_alias(&mut self, fa: &BackendArg, to: &str) -> eyre::Result<()> {
        self.doc_mut()?
            .get_mut()
            .unwrap()
            .entry("tool_alias")
            .or_insert_with(table)
            .as_table_like_mut()
            .unwrap()
            .insert(&fa.short, value(to));
        Ok(())
    }

    pub fn set_alias(&mut self, fa: &BackendArg, from: &str, to: &str) -> eyre::Result<()> {
        self.tool_alias
            .entry(fa.short.to_string())
            .or_default()
            .versions
            .insert(from.into(), to.into());
        let mut doc = self.doc_mut()?;
        let versions = doc
            .get_mut()
            .unwrap()
            .entry("tool_alias")
            .or_insert_with(table)
            .as_table_like_mut()
            .unwrap()
            .entry(&fa.to_string())
            .or_insert_with(table)
            .as_table_like_mut()
            .unwrap()
            .entry("versions")
            .or_insert_with(table);
        insert_preserving_decor(versions, from, value(to));
        Ok(())
    }

    pub fn remove_backend_alias(&mut self, fa: &BackendArg) -> eyre::Result<()> {
        let mut doc = self.doc_mut()?;
        let doc = doc.get_mut().unwrap();
        // Remove from both tool_alias and deprecated alias sections
        for section in ["tool_alias", "alias"] {
            if let Some(aliases) = doc.get_mut(section).and_then(|v| v.as_table_mut()) {
                aliases.remove(&fa.short);
                if aliases.is_empty() {
                    doc.as_table_mut().remove(section);
                }
            }
        }
        Ok(())
    }

    pub fn remove_alias(&mut self, fa: &BackendArg, from: &str) -> eyre::Result<()> {
        // Remove from both tool_alias and deprecated alias in memory
        for alias_map in [&mut self.tool_alias, &mut self.alias] {
            if let Some(aliases) = alias_map.get_mut(&fa.short) {
                aliases.versions.shift_remove(from);
                if aliases.versions.is_empty() && aliases.backend.is_none() {
                    alias_map.shift_remove(&fa.short);
                }
            }
        }
        let mut doc = self.doc_mut()?;
        let doc = doc.get_mut().unwrap();
        // Remove from both tool_alias and deprecated alias sections in doc
        for section in ["tool_alias", "alias"] {
            if let Some(aliases) = doc.get_mut(section).and_then(|v| v.as_table_mut()) {
                if let Some(alias) = aliases
                    .get_mut(&fa.to_string())
                    .and_then(|v| v.as_table_mut())
                {
                    if let Some(versions) = alias.get_mut("versions").and_then(|v| v.as_table_mut())
                    {
                        versions.remove(from);
                        if versions.is_empty() {
                            alias.remove("versions");
                        }
                    }
                    if alias.is_empty() {
                        aliases.remove(&fa.to_string());
                    }
                }
                if aliases.is_empty() {
                    doc.as_table_mut().remove(section);
                }
            }
        }
        Ok(())
    }

    pub fn set_shell_alias(&mut self, name: &str, command: &str) -> eyre::Result<()> {
        self.shell_alias.insert(name.into(), command.into());
        let mut doc = self.doc_mut()?;
        let shell_alias = doc
            .get_mut()
            .unwrap()
            .entry("shell_alias")
            .or_insert_with(table);
        insert_preserving_decor(shell_alias, name, value(command));
        Ok(())
    }

    pub fn remove_shell_alias(&mut self, name: &str) -> eyre::Result<()> {
        self.shell_alias.shift_remove(name);
        let mut doc = self.doc_mut()?;
        let doc = doc.get_mut().unwrap();
        if let Some(shell_alias) = doc.get_mut("shell_alias").and_then(|v| v.as_table_mut()) {
            shell_alias.remove(name);
            if shell_alias.is_empty() {
                doc.as_table_mut().remove("shell_alias");
            }
        }
        Ok(())
    }

    pub fn update_env<V: Into<Value>>(&mut self, key: &str, value: V) -> eyre::Result<()> {
        let mut doc = self.doc_mut()?;
        let mut env_tbl = doc
            .get_mut()
            .unwrap()
            .entry("env")
            .or_insert_with(table)
            .as_table_mut()
            .unwrap();
        let key_parts = key.split('.').collect_vec();
        for (i, k) in key_parts.iter().enumerate() {
            if i == key_parts.len() - 1 {
                let value_decor = get_value_decor(env_tbl, k);
                let k = get_key_with_decor(env_tbl, k);
                let mut item = toml_edit::value(value);
                set_value_decor(&mut item, &value_decor);
                env_tbl.insert_formatted(&k, item);
                break;
            } else if !env_tbl.contains_key(k) {
                env_tbl.insert_formatted(&Key::from(*k), toml_edit::table());
            }
            env_tbl = env_tbl.get_mut(k).unwrap().as_table_mut().unwrap();
        }
        Ok(())
    }

    pub fn update_env_age(
        &mut self,
        key: &str,
        value: &str,
        format: Option<AgeFormat>,
    ) -> eyre::Result<()> {
        let mut doc = self.doc_mut()?;
        let mut env_tbl = doc
            .get_mut()
            .unwrap()
            .entry("env")
            .or_insert_with(table)
            .as_table_mut()
            .unwrap();

        // Create the age inline table
        let mut outer_table = InlineTable::new();

        // Check if we need the complex format or can use simplified form
        match format {
            Some(AgeFormat::Zstd) => {
                // Non-default format, use full form: {age = {value = "...", format = "zstd"}}
                let mut age_table = InlineTable::new();
                age_table.insert("value", value.into());
                age_table.insert("format", "zstd".into());
                outer_table.insert("age", Value::InlineTable(age_table));
            }
            Some(AgeFormat::Raw) | None => {
                // Default format or no format, use simplified form: {age = "..."}
                outer_table.insert("age", value.into());
            }
        }

        let key_parts = key.split('.').collect_vec();
        for (i, k) in key_parts.iter().enumerate() {
            if i == key_parts.len() - 1 {
                let value_decor = get_value_decor(env_tbl, k);
                let k = get_key_with_decor(env_tbl, k);
                let mut item = toml_edit::Item::Value(Value::InlineTable(outer_table));
                set_value_decor(&mut item, &value_decor);
                env_tbl.insert_formatted(&k, item);
                break;
            } else if !env_tbl.contains_key(k) {
                env_tbl.insert_formatted(&Key::from(*k), toml_edit::table());
            }
            env_tbl = env_tbl.get_mut(k).unwrap().as_table_mut().unwrap();
        }
        Ok(())
    }

    pub fn remove_env(&mut self, key: &str) -> eyre::Result<()> {
        let mut doc = self.doc_mut()?;
        let env_tbl = doc
            .get_mut()
            .unwrap()
            .entry("env")
            .or_insert_with(table)
            .as_table_mut()
            .unwrap();
        env_tbl.remove(key);
        Ok(())
    }

    // Merge base OS env vars with env sections from this file,
    // so they are available for templating.
    // Note this only merges regular key-value variables; referenced files are not resolved.
    fn update_context_env(&mut self, mut base_env: EnvMap) {
        for e in &self.env.0 {
            match e {
                EnvDirective::Val(key, value, _) => {
                    base_env.insert(key.clone(), value.clone());
                }
                EnvDirective::Default(key, value, _)
                    if base_env.get(key).is_none_or(|v| v.is_empty()) =>
                {
                    base_env.insert(key.clone(), value.clone());
                }
                _ => {}
            }
        }
        self.context.insert("env", &base_env);
    }

    fn parse_template(&self, input: &str) -> eyre::Result<String> {
        self.parse_template_with_context(&self.template_context(), input)
    }

    /// Context for a version request that only became invalid after its template was rendered.
    ///
    /// Deserialization no longer validates the version, so this is the first place such a failure
    /// can surface — and it is far from the file that caused it. Name the file, and show the
    /// template alongside what it produced, since neither alone explains the error.
    fn tool_request_error_context(
        &self,
        short: &str,
        tool: &PakroTomlTool,
        rendered: &str,
    ) -> String {
        let where_ = format!("{short} in {}", display_path(&self.path));
        if tool.request == rendered {
            format!("invalid version for {where_}")
        } else {
            format!(
                "invalid version for {where_}: {} rendered to {rendered:?}",
                tool.request
            )
        }
    }

    fn parse_template_with_context(
        &self,
        context: &TeraContext,
        input: &str,
    ) -> eyre::Result<String> {
        if !contains_template_syntax(input) {
            return Ok(input.to_string());
        }
        let dir = self.path.parent();
        let mut tera = get_tera(dir);
        let output = render_str(&mut tera, input, context).wrap_err_with(|| {
            let p = display_path(&self.path);
            eyre!("failed to parse template {input} in {p}")
        })?;
        Ok(output)
    }

    fn template_context(&self) -> TeraContext {
        let mut context = self.context.clone();
        Self::insert_resolved_vars(&mut context);
        context
    }

    fn insert_resolved_vars(context: &mut TeraContext) {
        if context.get("vars").is_some() {
            return;
        }
        let Some(config) = Config::maybe_get() else {
            return;
        };
        if let Some(vars_results) = config.vars_results_cached() {
            let vars = vars_results
                .vars
                .iter()
                .map(|(k, (v, _))| (k.clone(), v.clone()))
                .collect::<IndexMap<_, _>>();
            context.insert("vars", &vars);
        } else if !config.vars.is_empty() {
            context.insert("vars", &config.vars);
        }
    }

    /// Render a tool-option template at config-load time, resolving env/vars but
    /// deferring `os()`/`arch()` (re-emitted as `{{ os() }}`/`{{ arch() }}`) so
    /// backends can render them for the host at install time or for an arbitrary
    /// target during cross-platform `pakro lock`.
    fn parse_tool_option_template(
        &self,
        context: &TeraContext,
        input: &str,
    ) -> eyre::Result<String> {
        if !contains_template_syntax(input) {
            return Ok(input.to_string());
        }
        let dir = self.path.parent();
        let mut tera = crate::tera::get_tera_preserving_os_arch(dir);
        let output = render_str(&mut tera, input, context).wrap_err_with(|| {
            let p = display_path(&self.path);
            eyre!("failed to parse template {input} in {p}")
        })?;
        Ok(output)
    }

    fn parse_tool_option_value_template(
        &self,
        context: &TeraContext,
        key: Option<&str>,
        value: &mut toml::Value,
        defer_os_arch: bool,
    ) -> eyre::Result<()> {
        match value {
            toml::Value::String(s) => {
                let preserve_os_arch = defer_os_arch && matches!(key, Some("url" | "checksum_url"));
                *s = if preserve_os_arch {
                    self.parse_tool_option_template(context, s)?
                } else {
                    self.parse_template_with_context(context, s)?
                };
            }
            toml::Value::Array(values) => {
                for value in values {
                    self.parse_tool_option_value_template(context, key, value, defer_os_arch)?;
                }
            }
            toml::Value::Table(table) => {
                for (key, value) in table.iter_mut() {
                    self.parse_tool_option_value_template(
                        context,
                        Some(key),
                        value,
                        defer_os_arch,
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl ConfigFile for PakroToml {
    fn get_path(&self) -> &Path {
        self.path.as_path()
    }

    fn min_version(&self) -> Option<&MinVersionSpec> {
        self.min_version.as_ref()
    }

    fn settings(&self) -> Option<&SettingsPartial> {
        Some(&self.settings)
    }

    fn plugins(&self) -> eyre::Result<HashMap<String, String>> {
        self.plugins
            .clone()
            .into_iter()
            .map(|(k, v)| {
                crate::plugins::PluginType::from_plugin_config(&k)?;
                if v.starts_with("packslip:") {
                    eyre::bail!("Packslip plugin archives have been removed: {k}");
                }
                let v = self.parse_template(&v)?;
                Ok((k, resolve_plugin_source_path(&self.path, v)?))
            })
            .collect()
    }

    fn env_entries(&self) -> eyre::Result<Vec<EnvDirective>> {
        self.warn_deprecated_env_keys();
        let env_entries = self.env.0.iter().cloned();
        let path_entries = self
            .env_path
            .iter()
            .map(|p| EnvDirective::Path(p.clone(), Default::default()))
            .collect_vec();
        let env_files = self
            .env_file
            .iter()
            .chain(&self.dotenv)
            .map(|p| EnvDirective::File(p.clone(), Default::default()))
            .collect_vec();
        let all = path_entries
            .into_iter()
            .chain(env_files)
            .chain(env_entries)
            .collect::<Vec<_>>();
        Ok(all)
    }

    fn vars_entries(&self) -> eyre::Result<Vec<EnvDirective>> {
        Ok(self.vars.0.clone())
    }

    fn remove_tool(&self, fa: &BackendArg) -> eyre::Result<()> {
        let mut tools = self.tools.lock().unwrap();
        tools.shift_remove(fa);
        let mut doc = self.doc_mut()?;
        let doc = doc.get_mut().unwrap();
        if let Some(tools) = doc.get_mut("tools")
            && let Some(tools) = tools.as_table_like_mut()
        {
            // the tool may be written as an alias ("nodejs"), a qualified name ("core:node") or the
            // fully-qualified backend rather than as fa.short; removing only the short name leaves
            // the entry in the document and save() writes it straight back out
            let keys = tool_keys_for(&*tools, fa);
            for key in &keys {
                tools.remove(key);
            }
            if tools.is_empty() {
                doc.as_table_mut().remove("tools");
            }
        }
        Ok(())
    }

    fn replace_versions(&self, ba: &BackendArg, versions: Vec<ToolRequest>) -> eyre::Result<()> {
        trace!("replacing versions {ba:?} {versions:?}");
        let mut tools = self.tools.lock().unwrap();
        let is_tools_sorted = is_tools_sorted(&tools); // was it previously sorted (if so we'll keep it sorted)
        let existing = tools.entry(ba.clone()).or_default();
        let output_empty_opts = |opts: &ToolVersionOptions| {
            if opts.os.as_ref().is_some_and(|o| !o.is_empty())
                || opts.depends.as_ref().is_some_and(|d| !d.is_empty())
                || !opts.install_env.is_empty()
                || opts.lazy.is_some()
                || !opts.lazy_bins.is_empty()
            {
                return false;
            }
            if let Some(reg_ba) = REGISTRY.get(ba.short.as_str()).and_then(|b| b.ba())
                && reg_ba.opts.as_ref().is_some_and(|o| o == opts)
            {
                // in this case the options specified are the same as in the registry so output no options and rely on the defaults
                return true;
            }
            opts.is_empty()
        };
        existing.0 = versions
            .iter()
            .map(|tr| PakroTomlTool::from(tr.clone()))
            .collect();
        if is_tools_sorted {
            // Keep the parsed representation in sync with the document ordering. Sorting only the
            // document leaves this map in insertion order, so a later replacement in the same
            // `pakro use` command can mistake an originally sorted table for an unsorted one.
            tools.sort_keys();
        }
        trace!("done replacing versions");
        let mut doc = self.doc_mut()?;
        trace!("got doc");
        let tools = doc
            .get_mut()
            .unwrap()
            .entry("tools")
            .or_insert_with(table)
            .as_table_mut()
            .unwrap();

        // the entry may be written under any spelling of this tool — an alias like "nodejs", a
        // qualified "core:node", or the fully-qualified backend — so collect every key in the
        // document that refers to it. The first one is the entry the file appears to define, so it
        // supplies the decorations; the last one is the effective entry after deserialization.
        // The rest are duplicates and are dropped below.
        let keys = tool_keys_for(&*tools, ba);
        let existing = keys.first().cloned().unwrap_or_else(|| ba.short.clone());
        let effective = keys.last().cloned().unwrap_or_else(|| ba.short.clone());
        if keys.len() > 1 {
            let dupes = keys.iter().map(|k| format!("`{k}`")).join(", ");
            warn!(
                "{}: {dupes} are the same tool; collapsing them into a single `{}` entry",
                display_path(&self.path),
                ba.short
            );
        }
        // create a key from the short name preserving any decorations like prefix/suffix if the key already exists
        let key = get_key_with_decor_from(tools, ba.short.as_str(), &existing);
        // and the same for the value, so a comment after the version survives the replacement
        let value_decor = get_value_decor(tools, &existing);
        // keep the array as it was written so its layout — multi-line shape, trailing comma and any
        // comments between the elements — can be reused when only the versions change. Read before
        // the removal below, which drops the entry when it is stored under a long name.
        let existing_arr = tools
            .get(&existing)
            .and_then(|i| i.as_value())
            .and_then(|v| v.as_array())
            .cloned();

        if versions.len() == 1
            && let Some(mut table) = tools.get(&effective).and_then(Item::as_table).cloned()
        {
            if let Some(first_table) = tools.get(&existing).and_then(Item::as_table) {
                let decor = first_table.decor().clone();
                let position = first_table.position();
                *table.decor_mut() = decor;
                table.set_position(position);
            }
            let explicit_options = versions[0].explicit_options();
            update_standard_tool_table(&mut table, &versions[0], Some(&explicit_options));
            replace_tool_entries_preserving_position(tools, &keys, key, Item::Table(table));
            if is_tools_sorted {
                tools.sort_values();
            }
            return Ok(());
        }

        // drop the other spellings: they all deserialize to this one entry, so leaving one behind
        // means the file has two keys for one tool and the later one silently wins on read-back.
        // ba.short itself is kept so insert_formatted overwrites it in place instead of appending.
        for k in &keys {
            if k != &ba.short {
                tools.remove(k);
            }
        }

        if versions.len() == 1 {
            let options = versions[0].options();
            let mut item = if output_empty_opts(&options) {
                value(versions[0].version())
            } else {
                let mut table = InlineTable::new();
                table.insert("version", versions[0].version().into());
                for (k, v) in &options.opts {
                    table.insert(k, toml_value_to_edit(v.clone()));
                }
                insert_core_options(&mut table, options);
                Item::Value(Value::InlineTable(table))
            };
            set_value_decor(&mut item, &value_decor);
            tools.insert_formatted(&key, item);
        } else {
            // Reuse the existing array when the version count is unchanged: swapping the values in
            // place keeps the layout, including comments written between the elements. A comment
            // after an element belongs to the decor of the element that follows it, so an array
            // built from scratch always drops it. When the count changes there is no way to line
            // the old decor up with the new elements, so build a fresh array as before.
            let reused = existing_arr.filter(|a| a.len() == versions.len());
            let reusing = reused.is_some();
            let mut arr = reused.unwrap_or_else(Array::new);
            for (i, tr) in versions.into_iter().enumerate() {
                let v = tr.version();
                let val: Value = if output_empty_opts(&tr.options()) {
                    v.to_string().into()
                } else {
                    let mut table = InlineTable::new();
                    table.insert("version", v.to_string().into());
                    let options = tr.options();
                    for (k, v) in &options.opts {
                        table.insert(k, toml_value_to_edit(v.clone()));
                    }
                    insert_core_options(&mut table, options);
                    table.into()
                };
                match reusing.then(|| arr.get_mut(i)).flatten() {
                    Some(slot) => {
                        let mut val = val;
                        *val.decor_mut() = slot.decor().clone();
                        *slot = val;
                    }
                    // `push` applies the default separators, exactly as before
                    None => arr.push(val),
                }
            }
            let mut item = Item::Value(Value::Array(arr));
            set_value_decor(&mut item, &value_decor);
            tools.insert_formatted(&key, item);
        }

        if is_tools_sorted {
            tools.sort_values();
        }

        Ok(())
    }

    fn save(&self) -> eyre::Result<()> {
        let contents = self.dump()?;
        if let Some(parent) = self.path.parent() {
            create_dir_all(parent)?;
        }
        file::write_atomic(&self.path, contents)?;
        trust(&config_trust_root(&self.path))?;
        Ok(())
    }

    fn dump(&self) -> eyre::Result<String> {
        Ok(self.doc()?.to_string())
    }

    fn source(&self) -> ToolSource {
        ToolSource::PakroToml(self.path.clone())
    }

    fn to_tool_request_set(&self) -> eyre::Result<ToolRequestSet> {
        let source = ToolSource::PakroToml(self.path.clone());
        let mut trs = ToolRequestSet::new();
        let tools = self.tools.lock().unwrap();
        let mut context = self.context.clone();
        if let Some(config) = Config::maybe_get()
            && let Some(env_results) = config.env_results_cached()
        {
            let mut env_vars: EnvMap =
                if let Some(existing_env) = context.get("env").and_then(|v| v.as_map()) {
                    existing_env
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.to_string(), s.to_string())))
                        .collect()
                } else {
                    env::PRISTINE_ENV.clone()
                };
            for key in &env_results.env_remove {
                env_vars.remove(key);
            }
            env_vars.extend(
                env_results
                    .env
                    .iter()
                    .map(|(k, (v, _))| (k.clone(), v.clone())),
            );
            context.insert("env", &env_vars);
        }
        Self::insert_resolved_vars(&mut context);
        for (ba, tvp) in tools.iter() {
            for tool in &tvp.0 {
                let version = self.parse_template_with_context(&context, &tool.request)?;
                // taken before `ba` is consumed below
                let short = ba.short.clone();
                let tvr = if let Some(mut options) = tool.options.clone() {
                    // Add placeholder for version since it's not available at config load time
                    // This preserves {{ version }} in the output for install-time rendering
                    let mut opts_context = context.clone();
                    opts_context.insert("version", "{{ version }}");
                    // The http and s3 backends re-render their url/checksum_url per
                    // target platform (host at install, any target during `pakro
                    // lock`), so only those two options defer os()/arch() instead of
                    // resolving them now. Every other option (here and for other
                    // backends) is consumed verbatim, so it keeps host resolution at
                    // config load — deferring it would leak raw `{{ os() }}`
                    // fragments into consumers that never render again (e.g.
                    // checksum_expr).
                    let defer_os_arch = matches!(
                        ba.backend_type(),
                        crate::backend::backend_type::BackendType::Http
                            | crate::backend::backend_type::BackendType::S3
                    );
                    // `install_env` is a typed core option, so it never reaches the
                    // `opts` loop below and its values went to the installer
                    // unrendered — `{{ env.HOME }}` arrived literally, unlike every
                    // other tool option (#13306). Render it here, with `version`
                    // bound to itself so the placeholder still round-trips: core
                    // options are deliberately exempt from the backend `{version}`
                    // normalization, and nothing renders `install_env` again later.
                    let mut install_env_context = context.clone();
                    install_env_context.insert("version", "{{version}}");
                    for value in options.core.install_env.values_mut() {
                        if let EnvValue::String(s) = value {
                            *s = self.parse_template_with_context(&install_env_context, s)?;
                        }
                    }
                    for (k, v) in options.opts.iter_mut() {
                        self.parse_tool_option_value_template(
                            &opts_context,
                            Some(k),
                            v,
                            defer_os_arch,
                        )?;
                    }
                    let mut ba = ba.clone();
                    // Start with cached options but filter out install-time-only options
                    // when config provides its own options. This allows:
                    // - Changing url/asset_pattern/checksum without reinstall issues
                    // - Replacing stale layout options like bin_path with current config values
                    let mut ba_opts = ba.opts().clone();
                    let backend_type = ba.backend_type();
                    ba_opts.opts.retain(|k, _| {
                        !crate::backend::is_install_time_option_key_for_type(&backend_type, k)
                    });
                    ba_opts.apply_overrides(&options);
                    // Re-apply registry defaults for install-time keys not overridden by user.
                    // The filtering above strips both stale install-state cache AND registry
                    // defaults. We want to keep registry defaults while discarding stale cache.
                    if let Some(rt) = crate::registry::REGISTRY.get(ba.short.as_str()) {
                        let full = ba.full();
                        // Get structured options from registry (table-format backends)
                        let mut registry_opts = rt.backend_options(&full);
                        // Also parse inline options from [key=val,...] in the full string
                        if let Some(start) = full.rfind('[')
                            && full.ends_with(']')
                        {
                            let inline = crate::toolset::parse_tool_options(
                                &full[start + 1..full.len() - 1],
                            );
                            for (k, v) in inline.opts {
                                registry_opts.opts.entry(k).or_insert(v);
                            }
                        }
                        for (k, v) in registry_opts.opts {
                            ba_opts.opts.entry(k).or_insert(v);
                        }
                    }
                    // Replace config-owned fields rather than merging them with cached values.
                    // This intentionally supersedes apply_overrides above so omitted values clear
                    // stale cache and install_env does not retain cached entries.
                    ba_opts.os = options.os.clone();
                    ba_opts.depends = options.depends.clone();
                    ba_opts.install_env = options.install_env.clone();
                    ba.set_opts(Some(ba_opts.clone()));
                    ToolRequest::new_with_options(ba.into(), &version, ba_opts, source.clone())
                        .wrap_err_with(|| self.tool_request_error_context(&short, tool, &version))?
                } else {
                    ToolRequest::new(ba.clone().into(), &version, source.clone())
                        .wrap_err_with(|| self.tool_request_error_context(&short, tool, &version))?
                };
                trs.add_version(tvr, &source);
            }
        }
        Ok(trs)
    }

    fn aliases(&self) -> eyre::Result<AliasMap> {
        // Emit deprecation warning if [alias] is used
        if !self.alias.is_empty() {
            deprecated!(
                "alias",
                "[alias] is deprecated, use [tool_alias] instead in {}",
                display_path(&self.path)
            );
        }

        // Merge alias and tool_alias, with tool_alias taking precedence
        let mut combined: AliasMap = self.alias.clone();
        for (k, v) in &self.tool_alias {
            combined.insert(k.clone(), v.clone());
        }

        combined
            .iter()
            .map(|(k, v)| {
                let versions = v
                    .clone()
                    .versions
                    .into_iter()
                    .map(|(k, v)| {
                        let v = self.parse_template(&v)?;
                        Ok::<(String, String), eyre::Report>((k, v))
                    })
                    .collect::<eyre::Result<IndexMap<_, _>>>()?;
                Ok((
                    k.clone(),
                    Alias {
                        backend: v.backend.clone(),
                        versions,
                    },
                ))
            })
            .collect()
    }

    fn shell_aliases(&self) -> eyre::Result<IndexMap<String, String>> {
        self.shell_alias
            .iter()
            .map(|(k, v)| {
                let v = self.parse_template(v)?;
                Ok((k.clone(), v))
            })
            .collect()
    }

    fn command_wrappers(&self) -> eyre::Result<IndexMap<String, CommandWrapper>> {
        Ok(self.wrappers.clone())
    }

    fn tool_config(&self) -> &ToolConfig {
        &self.tool_config
    }

    fn monorepo_root(&self) -> Option<bool> {
        self.monorepo_root
    }

    fn monorepo(&self) -> Option<&MonorepoConfig> {
        self.monorepo.as_ref()
    }

    fn redactions(&self) -> &Redactions {
        &self.redactions
    }

    fn hooks(&self) -> eyre::Result<Vec<Hook>> {
        Ok(self
            .hooks
            .iter()
            .map(|(hook_type, def)| {
                let mut hooks = def.clone().into_hooks(*hook_type);
                for hook in hooks.iter_mut() {
                    hook.render_templates(|s| self.parse_template(s))?;
                }
                eyre::Ok(hooks)
            })
            .collect::<eyre::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    fn doctor_config(&self) -> crate::config::doctor::DoctorConfig {
        self.doctor.clone()
    }
}

fn resolve_plugin_source_path(config_path: &Path, source: String) -> eyre::Result<String> {
    let source_path = Path::new(&source);
    let is_explicit_relative = matches!(
        source_path.components().next(),
        Some(Component::CurDir | Component::ParentDir)
    );
    let expanded = file::replace_path(source_path);
    let path = if expanded.is_absolute() {
        Some(expanded)
    } else if is_explicit_relative {
        Some(config_root::config_root(config_path).join(expanded))
    } else {
        None
    };

    match path {
        Some(path) => Ok(path.absolutize()?.to_string_lossy().into_owned()),
        None => Ok(source),
    }
}

/// Returns a [`toml_edit::Key`] from the given `key`.
/// Preserves any surrounding whitespace (e.g. comments) if the key already exists in the provided [`toml_edit::Table`].
fn get_key_with_decor(table: &toml_edit::Table, key: &str) -> Key {
    get_key_with_decor_from(table, key, key)
}

/// Same as [`get_key_with_decor`], but takes the decor from `existing` rather than from `key`.
/// The entry being replaced may be stored under a different key than the one written back, e.g. a
/// fully-qualified `"core:node"` that `pakro use node` rewrites to `node`.
fn get_key_with_decor_from(table: &toml_edit::Table, key: &str, existing: &str) -> Key {
    let mut key = Key::from(key);
    if let Some((k, _)) = table.get_key_value(existing) {
        if let Some(prefix) = k.leaf_decor().prefix() {
            key.leaf_decor_mut().set_prefix(prefix.clone());
        }
        if let Some(suffix) = k.leaf_decor().suffix() {
            key.leaf_decor_mut().set_suffix(suffix.clone());
        }
    }
    key
}

/// Every key in a `[tools]` table that refers to `ba`, in document order.
///
/// The same tool can be written under several spellings: its short name (`node`), one of the
/// hardcoded aliases (`nodejs`, `golang`, `dotnet-core`), a `core:`-qualified name (`core:node`),
/// or the fully-qualified backend the short name resolves to. [`unalias_backend`] folds the first
/// three onto the short name, and that is what happens when the file is deserialized — so pakro's
/// own view of `[tools]` holds a single entry no matter how many of those keys the document has,
/// and when there is more than one the later one silently wins. A writer therefore has to find all
/// of them and not just the one it would write itself, or it leaves a second key behind that
/// outvotes the one it just wrote.
///
/// Registry aliases (`rg` for `ripgrep`) are a different mechanism: they resolve to *different*
/// short names, pakro keeps them as separate entries and writes each back as the user spelled it,
/// so they are deliberately not matched here. Keys carrying inline options
/// (`"go:example.com/x[tags=y]"`) are likewise left alone — collapsing those would have to decide
/// what happens to the options.
///
/// The keys are returned owned so the caller can go on to mutate the table.
fn tool_keys_for(tools: &dyn toml_edit::TableLike, ba: &BackendArg) -> Vec<String> {
    let full = ba.full();
    tools
        .iter()
        .filter(|&(k, _)| unalias_backend(k) == ba.short.as_str() || k == full.as_str())
        .map(|(k, _)| k.to_string())
        .collect()
}

/// Captures the decor of the value `key` currently holds, if any.
///
/// A comment written after the value on the same line lives in that decor, so replacing the value
/// without carrying it over drops the comment. The comment *above* the line belongs to the key
/// instead and is handled by [`get_key_with_decor`].
fn get_value_decor(table: &toml_edit::Table, key: &str) -> Option<toml_edit::Decor> {
    let value = table.get(key)?.as_value()?;
    Some(value.decor().clone())
}

/// Inserts `item` under `key` in `target`, carrying over the decor of the entry being replaced:
/// the comment above the line lives on the key, the one after the value on the value.
///
/// Inline tables have no `insert_formatted`, so they fall back to a plain insert — which is what
/// every caller here did unconditionally before, so nothing regresses for them.
pub(crate) fn insert_preserving_decor(target: &mut Item, key: &str, mut item: Item) {
    if let Some(tbl) = target.as_table_mut() {
        let k = get_key_with_decor(tbl, key);
        let value_decor = get_value_decor(tbl, key);
        set_value_decor(&mut item, &value_decor);
        tbl.insert_formatted(&k, item);
    } else if let Some(tbl) = target.as_table_like_mut() {
        tbl.insert(key, item);
    }
}

/// Applies decor captured by [`get_value_decor`] to the value that replaces it.
fn set_value_decor(item: &mut Item, decor: &Option<toml_edit::Decor>) {
    if let (Some(decor), Some(value)) = (decor, item.as_value_mut()) {
        if let Some(prefix) = decor.prefix() {
            value.decor_mut().set_prefix(prefix.clone());
        }
        if let Some(suffix) = decor.suffix() {
            value.decor_mut().set_suffix(suffix.clone());
        }
    }
}

impl Debug for PakroToml {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let tools = self.to_tool_request_set().unwrap().to_string();
        let title = format!("PakroToml({}): {tools}", display_path(&self.path));
        let mut d = f.debug_struct(&title);
        if let Some(min_version) = &self.min_version {
            d.field("min_version", min_version);
        }
        if !self.env_file.is_empty() {
            d.field("env_file", &self.env_file);
        }
        if !self.dotenv.is_empty() {
            d.field("dotenv", &self.dotenv);
        }
        if let Ok(env) = self.env_entries()
            && !env.is_empty()
        {
            d.field("env", &env);
        }
        if !self.alias.is_empty() {
            d.field("alias", &self.alias);
        }
        if !self.plugins.is_empty() {
            d.field("plugins", &self.plugins);
        }
        d.finish()
    }
}

impl Clone for PakroToml {
    fn clone(&self) -> Self {
        Self {
            custom: self.custom.clone(),
            min_version: self.min_version.clone(),
            context: self.context.clone(),
            path: self.path.clone(),
            env_file: self.env_file.clone(),
            dotenv: self.dotenv.clone(),
            env: self.env.clone(),
            env_path: self.env_path.clone(),
            alias: self.alias.clone(),
            tool_alias: self.tool_alias.clone(),
            shell_alias: self.shell_alias.clone(),
            wrappers: self.wrappers.clone(),
            doc: Mutex::new(self.doc.lock().unwrap().clone()),
            hooks: self.hooks.clone(),
            tools: Mutex::new(self.tools.lock().unwrap().clone()),
            redactions: self.redactions.clone(),
            plugins: self.plugins.clone(),
            tool_config: self.tool_config.clone(),
            settings: self.settings.clone(),
            doctor: self.doctor.clone(),
            vars: self.vars.clone(),
            monorepo_root: self.monorepo_root,
            experimental_monorepo_root: self.experimental_monorepo_root,
            monorepo: self.monorepo.clone(),
            packages: std::sync::Mutex::new(self.packages.lock().unwrap().clone()),
        }
    }
}

impl From<ToolRequest> for PakroTomlTool {
    fn from(tr: ToolRequest) -> Self {
        // `ToolRequest::version()` re-emits the selector prefix (`prefix:`, `ref:`, `path:`,
        // `sub-N:`, `system`), which is the same string this would have been written from, so the
        // per-variant reconstruction that used to live here is redundant.
        let options = tr.options();
        Self {
            request: tr.version(),
            options: if options.is_empty() {
                None
            } else {
                Some(options)
            },
        }
    }
}

fn deserialize_min_version<'de, D>(deserializer: D) -> Result<Option<MinVersionSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    struct MinVersionVisitor;

    impl<'de> Visitor<'de> for MinVersionVisitor {
        type Value = Option<MinVersionSpec>;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("string or table for min_version")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_any(self)
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            let version = Versioning::new(v)
                .ok_or_else(|| versions::Error::IllegalVersioning(v.to_string()))
                .map_err(E::custom)?;
            Ok(MinVersionSpec::new(Some(version), None))
        }

        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            self.visit_str(&v)
        }

        fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            let mut hard: Option<Versioning> = None;
            let mut soft: Option<Versioning> = None;
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "hard" => {
                        if hard.is_some() {
                            return Err(de::Error::duplicate_field("hard"));
                        }
                        let value: String = map.next_value()?;
                        let version = Versioning::new(&value)
                            .ok_or_else(|| versions::Error::IllegalVersioning(value.clone()))
                            .map_err(de::Error::custom)?;
                        hard = Some(version);
                    }
                    "soft" => {
                        if soft.is_some() {
                            return Err(de::Error::duplicate_field("soft"));
                        }
                        let value: String = map.next_value()?;
                        let version = Versioning::new(&value)
                            .ok_or_else(|| versions::Error::IllegalVersioning(value.clone()))
                            .map_err(de::Error::custom)?;
                        soft = Some(version);
                    }
                    other => {
                        return Err(de::Error::unknown_field(other, &["hard", "soft"]));
                    }
                }
            }
            Ok(MinVersionSpec::new(hard, soft))
        }
    }

    deserializer.deserialize_option(MinVersionVisitor)
}

impl<'de> de::Deserialize<'de> for EnvList {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct EnvManVisitor;

        impl<'de> Visitor<'de> for EnvManVisitor {
            type Value = EnvList;
            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("environment variable table")
            }

            fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                let mut env = vec![];
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "_" | "pakro" => {
                            if key == "pakro" {
                                deprecated_at!(
                                    "2026.7.0",
                                    "2026.12.0",
                                    "config.env.pakro",
                                    "`env.pakro` is deprecated. Use `env._` instead."
                                );
                            }
                            #[derive(Deserialize)]
                            #[serde(untagged)]
                            enum PakroTomlEnvDirectiveValue {
                                Single {
                                    #[serde(alias = "value")]
                                    path: String,
                                    #[serde(flatten)]
                                    options: EnvDirectiveOptions,
                                },
                                Multiple {
                                    #[serde(alias = "value", alias = "values", alias = "paths")]
                                    path: Vec<String>,
                                    #[serde(flatten)]
                                    options: EnvDirectiveOptions,
                                },
                            }

                            struct PakroTomlEnvDirective(PakroTomlEnvDirectiveValue);

                            impl<'de> Deserialize<'de> for PakroTomlEnvDirective {
                                fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
                                where
                                    D: Deserializer<'de>,
                                {
                                    let value = toml::Value::deserialize(deserializer)?;
                                    let (uses_value, uses_values) = value
                                        .as_table()
                                        .map(|table| {
                                            (
                                                table.contains_key("value"),
                                                table.contains_key("values"),
                                            )
                                        })
                                        .unwrap_or_default();
                                    let directive = PakroTomlEnvDirectiveValue::deserialize(value)
                                        .map_err(de::Error::custom)?;

                                    if uses_value {
                                        deprecated_at!(
                                            "2026.7.0",
                                            "2026.12.0",
                                            "config.directive.value",
                                            "`value` in built-in `file`, `path`, and `source` directive objects is deprecated. Use `path` instead."
                                        );
                                    }
                                    if uses_values {
                                        deprecated_at!(
                                            "2026.7.0",
                                            "2026.12.0",
                                            "config.directive.values",
                                            "`values` in built-in `file`, `path`, and `source` directive objects is deprecated. Use `path` instead."
                                        );
                                    }

                                    Ok(PakroTomlEnvDirective(directive))
                                }
                            }

                            impl FromStr for PakroTomlEnvDirective {
                                type Err = String;
                                fn from_str(s: &str) -> Result<Self, Self::Err> {
                                    Ok(PakroTomlEnvDirective(PakroTomlEnvDirectiveValue::Single {
                                        path: s.to_string(),
                                        options: Default::default(),
                                    }))
                                }
                            }

                            // Reuses `deserialize_arr` so each of `path`/`file`/`source`
                            // accepts either a single value or a list, while
                            // `ParsedEnvBlock` below iterates the `_` table in the order
                            // the keys were written.
                            #[derive(Deserialize)]
                            struct DirectiveArr(
                                #[serde(deserialize_with = "deserialize_arr")]
                                Vec<PakroTomlEnvDirective>,
                            );

                            fn flatten_directives<F>(
                                directives: Vec<PakroTomlEnvDirective>,
                                constructor: F,
                            ) -> impl Iterator<Item = EnvDirective>
                            where
                                F: Fn(String, EnvDirectiveOptions) -> EnvDirective + 'static,
                            {
                                directives.into_iter().flat_map(move |d| match d.0 {
                                    PakroTomlEnvDirectiveValue::Single { path, options } => {
                                        vec![constructor(path, options)]
                                    }
                                    PakroTomlEnvDirectiveValue::Multiple { path, options } => path
                                        .into_iter()
                                        .map(|v| constructor(v, options.clone()))
                                        .collect(),
                                })
                            }

                            // Parse the `_` table preserving the written order of its
                            // sub-keys (`path`/`file`/`source`/modules) so that a later
                            // directive's template can reference a variable exported by
                            // an earlier one — e.g. `_.path` using a var from `_.source`
                            // (discussion #3783).
                            struct ParsedEnvBlock {
                                directives: Vec<EnvDirective>,
                            }

                            impl<'de> Deserialize<'de> for ParsedEnvBlock {
                                fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
                                where
                                    D: Deserializer<'de>,
                                {
                                    struct ParsedEnvBlockVisitor;
                                    impl<'de> Visitor<'de> for ParsedEnvBlockVisitor {
                                        type Value = ParsedEnvBlock;
                                        fn expecting(
                                            &self,
                                            formatter: &mut Formatter,
                                        ) -> std::fmt::Result
                                        {
                                            formatter.write_str("the env `_` directive table")
                                        }
                                        fn visit_map<M>(
                                            self,
                                            mut map: M,
                                        ) -> Result<Self::Value, M::Error>
                                        where
                                            M: de::MapAccess<'de>,
                                        {
                                            let mut directives = vec![];
                                            while let Some(key) = map.next_key::<String>()? {
                                                match key.as_str() {
                                                    "path" => {
                                                        directives.extend(flatten_directives(
                                                            map.next_value::<DirectiveArr>()?.0,
                                                            EnvDirective::Path,
                                                        ));
                                                    }
                                                    "file" => {
                                                        directives.extend(flatten_directives(
                                                            map.next_value::<DirectiveArr>()?.0,
                                                            EnvDirective::File,
                                                        ));
                                                    }
                                                    "source" => {
                                                        directives.extend(flatten_directives(
                                                            map.next_value::<DirectiveArr>()?.0,
                                                            EnvDirective::Source,
                                                        ));
                                                    }

                                                    _ => {
                                                        let mut value =
                                                            map.next_value::<toml::Value>()?;
                                                        let mut opts =
                                                            EnvDirectiveOptions::default();
                                                        if let Some(table) = value.as_table_mut()
                                                            && let Some(tools) =
                                                                table.remove("tools")
                                                        {
                                                            opts.tools =
                                                                tools.as_bool().unwrap_or(false);
                                                        }
                                                        // The user's explicit `redact` overrides the plugin's own preference.
                                                        if let Some(table) = value.as_table_mut()
                                                            && let Some(redact) =
                                                                table.remove("redact")
                                                        {
                                                            opts.redact = Some(redact.as_bool().ok_or_else(|| {
                                                                de::Error::custom(format!(
                                                                    "_.{key}.redact must be a boolean"
                                                                ))
                                                            })?);
                                                        }
                                                        directives.push(EnvDirective::Module(
                                                            key, value, opts,
                                                        ));
                                                    }
                                                }
                                            }
                                            Ok(ParsedEnvBlock { directives })
                                        }
                                    }
                                    deserializer.deserialize_map(ParsedEnvBlockVisitor)
                                }
                            }

                            let block = map.next_value::<ParsedEnvBlock>()?;
                            env.extend(block.directives);
                        }
                        _ => {
                            #[derive(Deserialize)]
                            #[serde(untagged)]
                            enum Val {
                                AgeComplex {
                                    age: AgeComplexVal,
                                },
                                AgeWithOptions {
                                    age: String,
                                    #[serde(flatten)]
                                    options: EnvDirectiveOptions,
                                },
                                Map {
                                    value: EnvValue,
                                    #[serde(flatten)]
                                    options: EnvDirectiveOptions,
                                },
                                DefaultMap {
                                    default: EnvValue,
                                    #[serde(flatten)]
                                    options: EnvDirectiveOptions,
                                },
                                OptionsOnly {
                                    #[serde(flatten)]
                                    options: EnvDirectiveOptions,
                                },
                                Primitive(EnvValue),
                            }

                            #[derive(Deserialize)]
                            struct AgeComplexVal {
                                value: String,
                                #[serde(default)]
                                format: Option<AgeFormat>,
                                #[serde(flatten)]
                                options: EnvDirectiveOptions,
                            }
                            let raw_value = map.next_value::<toml::Value>()?;
                            if let Some(table) = raw_value.as_table() {
                                let has_default = table.contains_key("default");
                                if has_default && table.contains_key("value") {
                                    return Err(serde::de::Error::custom(format!(
                                        "Environment variable '{}' cannot have both 'value' and 'default'. The 'value' field always overwrites, while 'default' only applies when the variable is unset or empty. Remove either the 'value' field or the 'default' field.",
                                        key
                                    )));
                                }
                                if has_default && table.contains_key("required") {
                                    return Err(serde::de::Error::custom(format!(
                                        "Environment variable '{}' cannot have both 'default' and 'required'. The 'required' flag means the variable must be defined elsewhere, while 'default' provides a fallback value. Remove either the 'default' field or the 'required' flag.",
                                        key
                                    )));
                                }
                                if has_default && table.contains_key("age") {
                                    return Err(serde::de::Error::custom(format!(
                                        "Environment variable '{}' cannot have both 'age' and 'default'. Remove either the 'age' field or the 'default' field.",
                                        key
                                    )));
                                }
                            }
                            let val_result = raw_value.try_into::<Val>().map_err(|e| {
                                serde::de::Error::custom(format!(
                                    "failed to parse environment variable '{}': {}",
                                    key, e
                                ))
                            })?;

                            // Handle Age variants separately since they create different directive types
                            match &val_result {
                                Val::AgeComplex { age } => {
                                    let directive = EnvDirective::Age {
                                        key: key.clone(),
                                        value: age.value.clone(),
                                        format: age.format.clone(),
                                        options: age.options.clone(),
                                    };
                                    env.push(directive);
                                    continue;
                                }
                                Val::AgeWithOptions { age, options } => {
                                    let directive = EnvDirective::Age {
                                        key: key.clone(),
                                        value: age.clone(),
                                        format: None, // Default format for simplified syntax with options
                                        options: options.clone(),
                                    };
                                    env.push(directive);
                                    continue;
                                }
                                _ => {}
                            }

                            let directive = match val_result {
                                Val::Primitive(value) => match value.into_string() {
                                    Some(s) => {
                                        EnvDirective::Val(key, s, EnvDirectiveOptions::default())
                                    }
                                    None => EnvDirective::Rm(key, EnvDirectiveOptions::default()),
                                },
                                Val::Map { value, options } => {
                                    // Validate that required cannot be used with any value
                                    if options.required.is_required() {
                                        return Err(serde::de::Error::custom(format!(
                                            "Environment variable '{}' cannot have both 'value' and 'required'. The 'required' flag means the variable must be defined elsewhere (in the environment or a later config file). Remove either the 'value' field or the 'required' flag.",
                                            key
                                        )));
                                    }
                                    match value.into_string() {
                                        Some(s) => EnvDirective::Val(key, s, options),
                                        None => EnvDirective::Rm(key, options),
                                    }
                                }
                                Val::DefaultMap { default, options } => {
                                    let Some(default) = default.into_default_string() else {
                                        return Err(serde::de::Error::custom(format!(
                                            "Environment variable '{}' default cannot be a boolean. Use a string or integer fallback instead.",
                                            key
                                        )));
                                    };
                                    EnvDirective::Default(key, default, options)
                                }
                                Val::OptionsOnly { options } => {
                                    // No value provided - this creates a required variable that must be defined elsewhere
                                    if !options.required.is_required() {
                                        return Err(serde::de::Error::custom(format!(
                                            "Environment variable '{}' has no value. Either provide a value or set required=true to indicate it must be defined elsewhere.",
                                            key
                                        )));
                                    }
                                    // For required variables without a value, we create a Required directive
                                    EnvDirective::Required(key, options)
                                }
                                Val::AgeComplex { .. } | Val::AgeWithOptions { .. } => {
                                    unreachable!() // Already handled above
                                }
                            };
                            env.push(directive);
                        }
                    }
                }
                Ok(EnvList(env))
            }
        }

        deserializer.deserialize_map(EnvManVisitor)
    }
}

pub(crate) fn deserialize_vars<'de, D>(deserializer: D) -> std::result::Result<EnvList, D::Error>
where
    D: de::Deserializer<'de>,
{
    let value = toml::Value::deserialize(deserializer)?;
    fn contains_pakro(value: &toml::Value) -> bool {
        match value {
            toml::Value::Table(table) => table.contains_key("pakro"),
            toml::Value::Array(values) => values.iter().any(contains_pakro),
            _ => false,
        }
    }
    if contains_pakro(&value) {
        return Err(de::Error::custom("`vars.pakro` is not supported"));
    }
    EnvList::deserialize(value).map_err(de::Error::custom)
}

fn deserialize_root_env<'de, D>(deserializer: D) -> std::result::Result<EnvList, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct RootEnvVisitor;

    impl<'de> Visitor<'de> for RootEnvVisitor {
        type Value = EnvList;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("env table or flat array of env tables")
        }

        fn visit_map<M>(self, map: M) -> std::result::Result<Self::Value, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            EnvList::deserialize(de::value::MapAccessDeserializer::new(map))
        }

        fn visit_seq<S>(self, mut seq: S) -> std::result::Result<Self::Value, S::Error>
        where
            S: de::SeqAccess<'de>,
        {
            let mut env = vec![];
            while let Some(list) = seq.next_element::<EnvList>()? {
                env.extend(list.0);
            }
            Ok(EnvList(env))
        }
    }

    deserializer.deserialize_any(RootEnvVisitor)
}

impl<'de> de::Deserialize<'de> for PakroTomlToolList {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct PakroTomlToolListVisitor;

        impl<'de> Visitor<'de> for PakroTomlToolListVisitor {
            type Value = PakroTomlToolList;
            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("tool list")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(PakroTomlToolList(vec![PakroTomlTool {
                    request: v.to_string(),
                    options: None,
                }]))
            }

            fn visit_seq<S>(self, mut seq: S) -> std::result::Result<Self::Value, S::Error>
            where
                S: de::SeqAccess<'de>,
            {
                let mut tools = vec![];
                while let Some(tool) = seq.next_element::<PakroTomlTool>()? {
                    tools.push(tool);
                }
                Ok(PakroTomlToolList(tools))
            }

            fn visit_map<M>(self, map: M) -> std::result::Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                let parsed =
                    ParsedToolMap::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(PakroTomlToolList(vec![parse_pakro_toml_tool_map::<
                    M::Error,
                >(parsed)?]))
            }
        }

        deserializer.deserialize_any(PakroTomlToolListVisitor)
    }
}

impl<'de> de::Deserialize<'de> for PakroTomlTool {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct PakroTomlToolVisitor;

        impl<'de> Visitor<'de> for PakroTomlToolVisitor {
            type Value = PakroTomlTool;
            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("tool definition")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(PakroTomlTool {
                    request: v.to_string(),
                    options: None,
                })
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                let parsed =
                    ParsedToolMap::deserialize(de::value::MapAccessDeserializer::new(map))?;
                parse_pakro_toml_tool_map::<M::Error>(parsed)
            }
        }

        deserializer.deserialize_any(PakroTomlToolVisitor)
    }
}

impl<'de> de::Deserialize<'de> for BackendArg {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct BackendArgVisitor;

        impl Visitor<'_> for BackendArgVisitor {
            type Value = BackendArg;
            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("backend argument")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(v.into())
            }
        }

        deserializer.deserialize_any(BackendArgVisitor)
    }
}

impl<'de> de::Deserialize<'de> for Alias {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct AliasVisitor;

        impl<'de> Visitor<'de> for AliasVisitor {
            type Value = Alias;
            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("alias")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Alias {
                    backend: Some(v.to_string()),
                    ..Default::default()
                })
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                let mut backend = None;
                let mut versions = IndexMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "backend" => {
                            backend = Some(map.next_value()?);
                        }
                        "versions" => {
                            versions = map.next_value()?;
                        }
                        _ => {
                            deprecated!(
                                "TOOL_VERSION_ALIASES",
                                "tool version aliases should be `alias.<TOOL>.versions.<FROM> = <TO>`, not `alias.<TOOL>.<FROM> = <TO>`"
                            );
                            versions.insert(key, map.next_value()?);
                        }
                    }
                }
                Ok(Alias { backend, versions })
            }
        }

        deserializer.deserialize_any(AliasVisitor)
    }
}

/// A config body is safe to load without trust when nothing in it can execute
/// code at load time or change pakro's behavior without an explicit user
/// action:
/// - `min_version` is inert
/// - `[tools]` entries with plain version strings only matter when the user
///   runs something like `pakro install`. Entries with options (tables) are
///   excluded because options like `postinstall` and `install_env` run code
///   or alter the install environment. Options can also ride in the key
///   (`"tool[postinstall=...]" = "1"`), so keys with inline options are
///   excluded too.
/// - no Tera template syntax anywhere — templates render while config and
///   values load and can run arbitrary commands via exec()
fn is_safe_config_body(body: &str) -> bool {
    // Fast reject: literal Tera delimiters in the raw text.
    if contains_template_syntax(body) {
        return false;
    }
    let Ok(toml::Value::Table(table)) = toml::from_str::<toml::Value>(body) else {
        // let the normal trust + parse flow handle invalid TOML
        return false;
    };
    // The raw-body check above misses escaped delimiters that TOML decodes,
    // e.g. `"{{ exec(...) }}"` becomes `{{ exec(...) }}`
    // after parsing and would still render via Tera. Re-check every decoded
    // string (keys and values, at any depth) so no exec()-capable template
    // can slip through into tool versions.
    if toml_table_has_template(&table) {
        return false;
    }
    table.iter().all(|(key, value)| match key.as_str() {
        "min_version" => true,
        "tools" => value.as_table().is_some_and(|tools| {
            tools.iter().all(|(tool, version)| {
                !tool.contains('[')
                    && match version {
                        toml::Value::String(_) => true,
                        toml::Value::Array(versions) => {
                            versions.iter().all(|v| matches!(v, toml::Value::String(_)))
                        }
                        _ => false,
                    }
            })
        }),
        _ => false,
    })
}

/// Whether any decoded string (table key or value, at any depth) contains
/// Tera template syntax. Used to catch escaped delimiters (e.g. `{{`)
/// that a raw-text scan misses but that still render after TOML parsing.
pub(crate) fn toml_value_has_template(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(s) => contains_template_syntax(s),
        toml::Value::Array(arr) => arr.iter().any(toml_value_has_template),
        toml::Value::Table(t) => toml_table_has_template(t),
        _ => false,
    }
}

fn toml_table_has_template(table: &toml::Table) -> bool {
    table
        .iter()
        .any(|(k, v)| contains_template_syntax(k) || toml_value_has_template(v))
}

fn is_tools_sorted(tools: &IndexMap<BackendArg, PakroTomlToolList>) -> bool {
    let mut last = None;
    for k in tools.keys() {
        if let Some(last) = last
            && k < last
        {
            return false;
        }
        last = Some(k);
    }
    true
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use indoc::{formatdoc, indoc};
    use insta::{assert_debug_snapshot, assert_snapshot};
    use test_log::test;

    use crate::dirs;
    use crate::file;
    use crate::test::replace_path;
    use crate::toolset::{CoreToolOptions, ToolRequest};
    use crate::{config::Config, dirs::CWD};

    use super::*;

    #[test]
    fn test_resolve_plugin_source_path() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("project/pakro.toml");
        let absolute_plugin = temp.path().join("absolute/plugin");
        let home_plugin = dirs::HOME.join("plugins/example");

        for (source, expected) in [
            (
                absolute_plugin.to_string_lossy().into_owned(),
                absolute_plugin,
            ),
            (
                "./plugins/example".to_string(),
                temp.path().join("project/plugins/example"),
            ),
            ("../example".to_string(), temp.path().join("example")),
            ("~/plugins/example".to_string(), home_plugin),
        ] {
            assert_eq!(
                resolve_plugin_source_path(&config_path, source).unwrap(),
                expected.to_string_lossy()
            );
        }

        for source in [
            "https://github.com/example/plugin.git",
            "file:///tmp/plugin",
            "example/plugin",
        ] {
            assert_eq!(
                resolve_plugin_source_path(&config_path, source.to_string()).unwrap(),
                source
            );
        }
    }

    #[tokio::test]
    async fn test_fixture() {
        let _config = Config::get().await.unwrap();
        let cf = PakroToml::from_file(&dirs::HOME.join("fixtures/.pakro.toml")).unwrap();

        assert_debug_snapshot!(cf.env_entries().unwrap());
        assert_debug_snapshot!(cf.plugins().unwrap());
        assert_snapshot!(replace_path(&format!(
            "{:#?}",
            cf.to_tool_request_set().unwrap()
        )));
        assert_debug_snapshot!(cf.alias);

        assert_snapshot!(replace_path(&format!("{:#?}", cf)));
    }

    /// `save()` has to replace the config file, not rewrite it in place.
    ///
    /// An in-place `O_TRUNC` write is not atomic. A second pakro process can read the
    /// file while it is truncated and conclude no tools are configured, and because
    /// `O_TRUNC` only shortens the file at open time, a shorter write landing on a
    /// longer one leaves the previous tail attached — which is invalid TOML and breaks
    /// every later pakro command. Replacing through a rename makes the update
    /// all-or-nothing for concurrent readers, so assert the file is genuinely swapped.
    #[test]
    fn save_replaces_the_config_file_rather_than_rewriting_in_place() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let p = temp.path().join("config.toml");
        file::write(&p, "[tools]\nclaude = \"latest\"\n").unwrap();
        let before = std::fs::metadata(&p).unwrap().ino();

        let cf = PakroToml::from_file(&p).unwrap();
        cf.save().unwrap();

        let after = std::fs::metadata(&p).unwrap().ino();
        assert_ne!(
            before, after,
            "save() rewrote the config in place, so a concurrent reader can observe a torn file"
        );
    }

    #[tokio::test]
    async fn test_env() {
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
        min_version = "2024.1.1"
        [env]
        foo="bar"
        foo2='qux\nquux'
        foo3="qux\nquux"
        "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dump = cf.dump().unwrap();
        let env = parse_env(file::read_to_string(&p).unwrap());

        assert_debug_snapshot!(env, @r#""foo=bar\nfoo2=qux\\nquux\nfoo3=qux\nquux""#);
        let cf: Box<dyn ConfigFile> = Box::new(cf);
        with_settings!({
            assert_snapshot!(dump);
            assert_snapshot!(cf);
            assert_debug_snapshot!(cf);
        });
    }

    #[tokio::test]
    async fn test_env_directive_written_order() {
        // Directives inside `[env]._` are emitted in the order they are written,
        // rather than the previous fixed path -> file -> source order, so a later
        // directive's template can reference a variable set by an earlier one.
        // https://github.com/jdx/mise/discussions/3783
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [env]
            _.source = "a.sh"
            _.path = "b"
            _.file = "c.env"
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let kinds: Vec<&str> = cf
            .env
            .0
            .iter()
            .map(|d| match d {
                EnvDirective::Source(..) => "source",
                EnvDirective::Path(..) => "path",
                EnvDirective::File(..) => "file",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["source", "path", "file"]);
    }

    #[tokio::test]
    async fn test_env_var_in_tool() {
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(
            &p,
            r#"
        [env]
        TERRAFORM_VERSION = '1.0.0'
        JQ_PREFIX = '1.6'

        [tools]
        terraform = "{{env.TERRAFORM_VERSION}}"
        jq = { prefix = "{{ env.JQ_PREFIX }}" }
        "#,
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        assert_snapshot!(replace_path(&format!(
            "{:#?}",
            cf.to_tool_request_set().unwrap().tools
        )));
    }

    /// A `:` inside a template belongs to the template, not to a version selector, and the
    /// template has not been rendered yet when the config is deserialized. Selectors must still
    /// come out the other side, so this pins the rendered `ToolRequest`, not the raw string.
    ///
    /// See: <https://github.com/jdx/mise/discussions/5531>
    #[tokio::test]
    async fn test_colon_in_templated_tool_version() {
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(
            &p,
            r#"
        [env]
        BRANCH = 'main'

        [tools]
        terraform = "{{ exec(command='echo VER: 1.0.0') | split(pat=': ') | last | trim }}"
        jq = { ref = "{{ env.BRANCH }}" }
        node = "{% if true %}20.0.0{% else %}18.0.0{% endif %}"
        "#,
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let trs = cf.to_tool_request_set().unwrap();
        let version_of = |short: &str| {
            trs.tools
                .iter()
                .find(|(ba, _)| ba.short == short)
                .unwrap_or_else(|| panic!("{short} missing"))
                .1[0]
                .version()
        };
        // the colon belonged to the template, not to a selector
        assert_eq!(version_of("terraform"), "1.0.0");
        // ...and a real selector still survives being rendered late
        assert_eq!(version_of("jq"), "ref:main");
        assert_eq!(version_of("node"), "20.0.0");
    }

    /// The version is no longer validated at deserialize time, so a template that renders to an
    /// unknown selector fails later and further from the config. Pin that the error still names
    /// the file, the template, and what it became.
    #[tokio::test]
    async fn test_templated_tool_version_rendering_to_bad_selector() {
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(
            &p,
            "[tools]\nterraform = \"{{ exec(command='echo bogus:1.0') | trim }}\"\n",
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let err = cf.to_tool_request_set().unwrap_err().to_string();
        assert!(err.contains("invalid version for terraform"), "{err}");
        assert!(err.contains(".test.pakro.toml"), "{err}");
        assert!(err.contains("rendered to \"bogus:1.0\""), "{err}");
    }

    #[test]
    fn test_tool_selectors() {
        #[derive(Deserialize)]
        struct ToolConfig {
            #[allow(dead_code)]
            tools: IndexMap<BackendArg, PakroTomlToolList>,
        }

        let valid = indoc! {r#"
        [tools]
        node = { version = "20" }
        go = { prefix = "1.22" }
        python = { ref = "main" }
        shellcheck = { path = "/opt/shellcheck" }
        ruby = [{ prefix = "3.3", os = "linux-x64" }]
        "#};
        assert!(toml::from_str::<ToolConfig>(valid).is_ok());

        for (config, expected) in [
            (
                "[tools]\nnode = { version = \"20\", prefix = \"20\" }\n",
                "tool definition cannot specify both `version` and `prefix`",
            ),
            (
                "[tools]\nnode = [{ path = \"/opt/node\", ref = \"main\" }]\n",
                "tool definition cannot specify both `path` and `ref`",
            ),
            (
                "[tools]\nnode = [{ os = \"linux-x64\" }]\n",
                "tool definition must include exactly one of `version`, `path`, `prefix`, or `ref`",
            ),
            (
                "[tools]\nnode = { prefix = 20 }\n",
                "tool selector `prefix` must be a string",
            ),
        ] {
            let err = match toml::from_str::<ToolConfig>(config) {
                Ok(_) => panic!("expected tool selector validation to fail"),
                Err(err) => err,
            };
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    #[tokio::test]
    async fn test_core_options_do_not_normalize_version_placeholder() {
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(
            &p,
            r#"
        [tools]
        node = { version = "1.0.0", depends = ["{{version}}"], install_env = { FOO = "{{version}}" }, url = "https://example.com/{{version}}" }
        "#,
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let trs = cf.to_tool_request_set().unwrap();
        let node_req = trs
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "node")
            .and_then(|(_, reqs)| reqs.first())
            .unwrap();
        let opts = node_req.options();

        assert_eq!(opts.depends, Some(vec!["{{version}}".to_string()]));
        assert_eq!(
            opts.install_env.get("FOO"),
            Some(&EnvValue::String("{{version}}".to_string()))
        );
        assert_eq!(opts.get("url"), Some("https://example.com/{version}"));
        file::remove_file(&p).unwrap();
    }

    /// `install_env` is the documented way to give a tool an environment for its
    /// download, install, and verification steps, but as a core option it skipped
    /// the tool-option template pass and its values reached the installer raw.
    /// See: <https://github.com/jdx/mise/discussions/13306>
    #[tokio::test]
    async fn test_install_env_renders_templates() {
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(
            &p,
            r#"
        [env]
        COMPAT_LIB_DIR = "/opt/curses-narrow-compat"

        [tools]
        node = { version = "1.0.0", install_env = { LD_LIBRARY_PATH = "{{env.COMPAT_LIB_DIR}}", KEEP = "{{version}}" } }
        "#,
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let trs = cf.to_tool_request_set().unwrap();
        let opts = trs
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "node")
            .and_then(|(_, reqs)| reqs.first())
            .unwrap()
            .options();

        assert_eq!(
            opts.install_env.get("LD_LIBRARY_PATH"),
            Some(&EnvValue::String("/opt/curses-narrow-compat".to_string()))
        );
        // Nothing renders install_env again, so the version placeholder is left
        // as written rather than normalized the way backend options are.
        assert_eq!(
            opts.install_env.get("KEEP"),
            Some(&EnvValue::String("{{version}}".to_string()))
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_tool_options_preserve_quoted_literal_dotted_keys() {
        crate::toolset::install_state::init().await.unwrap();
        let cf = PakroToml::from_str(
            r#"
            [tools."aqua:example/vars-tool"]
            version = "1.0.0"
            fixture_version = "2.0.0"
            "vars.fixture_version" = "1.0.0"
            "#,
            std::path::Path::new("pakro.toml"),
        )
        .unwrap();
        let trs = cf.to_tool_request_set().unwrap();
        let (_, requests, _) = trs.iter().next().unwrap();
        let opts = requests[0].options();

        assert_eq!(opts.get("fixture_version"), Some("2.0.0"));
        assert_eq!(opts.get("vars.fixture_version"), Some("1.0.0"));
    }

    #[tokio::test]
    async fn test_env_source_var_in_tool() {
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let cwd = CWD.as_ref().unwrap();
        let script = cwd.join("set-go-version.sh");
        let config_file = cwd.join(".test.pakro.toml");

        file::write(&script, "export MY_GO_VERSION=\"1.2.3\"\n").unwrap();
        file::write(
            &config_file,
            r#"
        [env]
        _.source = "./set-go-version.sh"

        [tools]
        go = "{{env.MY_GO_VERSION}}"
        "#,
        )
        .unwrap();

        let config = Config::reset().await.unwrap();
        let trs = config.get_tool_request_set().await.unwrap();
        let go_req = trs
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "go")
            .and_then(|(_, reqs)| reqs.first())
            .unwrap();

        assert_eq!(go_req.version(), "1.2.3");

        file::remove_file(&config_file).unwrap();
        file::remove_file(&script).unwrap();
    }

    #[tokio::test]
    async fn test_env_array_valid() {
        let _config = Config::get().await.unwrap();
        let env = parse_env(formatdoc! {r#"
        [[env]]
        foo="bar"

        [[env]]
        bar="baz"

        [[env]]
        foo2='qux\nquux'
        bar2="qux\nquux"
        "#});

        assert_snapshot!(env, @r"
        foo=bar
        bar=baz
        foo2=qux\nquux
        bar2=qux
        quux
        ");
    }

    #[test]
    fn test_env_and_vars_array_boundaries() {
        assert!(toml::from_str::<PakroToml>(r#"env = [{ FOO = "one" }, { BAR = "two" }]"#).is_ok());
        assert!(toml::from_str::<PakroToml>("env = []").is_ok());
        assert!(
            toml::from_str::<PakroToml>(
                r#"
                [env]
                _.source = ["./first.sh", "./second.sh"]

                [vars]
                _.file = ["./first.env", "./second.env"]
                "#,
            )
            .is_ok()
        );

        for invalid in [
            r#"env = [[{ FOO = "bar" }]]"#,
            r#"env = [{ FOO = "bar" }, [{ BAR = "baz" }]]"#,
            r#"vars = []"#,
            r#"vars = [{ FOO = "bar" }]"#,
            r#"vars = [[{ FOO = "bar" }]]"#,
        ] {
            assert!(
                toml::from_str::<PakroToml>(invalid).is_err(),
                "expected config to be rejected: {invalid}"
            );
        }
    }

    #[tokio::test]
    async fn test_path_dirs() {
        let _config = Config::get().await.unwrap();
        let env = parse_env(formatdoc! {r#"
            env_path=["/foo", "./bar"]
            [env]
            foo="bar"
            "#});

        assert_snapshot!(env, @r#"
        _.path = "/foo"
        _.path = "./bar"
        foo=bar
        "#);

        let env = parse_env(formatdoc! {r#"
            env_path="./bar"
            "#});
        assert_snapshot!(env, @r#"_.path = "./bar""#);

        let env = parse_env(formatdoc! {r#"
            [env]
            _.path = "./bar"
            "#});
        assert_debug_snapshot!(env, @r#""_.path = \"./bar\"""#);

        let env = parse_env(formatdoc! {r#"
            [env]
            _.path = ["/foo", "./bar"]
            "#});
        assert_snapshot!(env, @r#"
        _.path = "/foo"
        _.path = "./bar"
        "#);

        let env = parse_env(formatdoc! {r#"
            [[env]]
            _.path = "/foo"
            [[env]]
            _.path = "./bar"
            "#});
        assert_snapshot!(env, @r#"
        _.path = "/foo"
        _.path = "./bar"
        "#);

        let env = parse_env(formatdoc! {r#"
            env_path = "/foo"
            [env]
            _.path = "./bar"
            "#});
        assert_snapshot!(env, @r#"
        _.path = "/foo"
        _.path = "./bar"
        "#);
    }

    #[tokio::test]
    async fn test_env_file() {
        let _config = Config::get().await.unwrap();
        let env = parse_env(formatdoc! {r#"
            env_file = ".env"
            "#});

        assert_debug_snapshot!(env, @r#""_.file = \".env\"""#);

        let env = parse_env(formatdoc! {r#"
            env_file=[".env", ".env2"]
            "#});
        assert_debug_snapshot!(env, @r#""_.file = \".env\"\n_.file = \".env2\"""#);

        let env = parse_env(formatdoc! {r#"
            [env]
            _.file = ".env"
            "#});
        assert_debug_snapshot!(env, @r#""_.file = \".env\"""#);

        let env = parse_env(formatdoc! {r#"
            [env]
            _.file = [".env", ".env2"]
            "#});
        assert_debug_snapshot!(env, @r#""_.file = \".env\"\n_.file = \".env2\"""#);

        let env = parse_env(formatdoc! {r#"
            dotenv = ".env"
            [env]
            _.file = ".env2"
        "#});
        assert_debug_snapshot!(env, @r#""_.file = \".env\"\n_.file = \".env2\"""#);

        let env = parse_env(formatdoc! {r#"
            env_file = ".env"
            dotenv = ".env2"
            "#});
        assert_debug_snapshot!(env, @r#""_.file = \".env\"\n_.file = \".env2\"""#);
    }

    #[tokio::test]
    async fn test_set_alias() {
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tool_alias.node.versions]
            16 = "16.0.0"
            18 = "18.0.0"
        "#},
        )
        .unwrap();
        let mut cf = PakroToml::from_file(&p).unwrap();
        let node = "node".into();
        let python = "python".into();
        cf.set_alias(&node, "18", "18.0.1").unwrap();
        cf.set_alias(&node, "20", "20.0.0").unwrap();
        cf.set_alias(&python, "3.10", "3.10.0").unwrap();

        assert_debug_snapshot!(cf.tool_alias);
        let cf: Box<dyn ConfigFile> = Box::new(cf);
        assert_snapshot!(cf);
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_remove_alias() {
        let _cwd_config = crate::test::CwdConfigGuard::lock();
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [alias.node.versions]
            16 = "16.0.0"
            18 = "18.0.0"

            [alias.python.versions]
            "3.10" = "3.10.0"
            "#},
        )
        .unwrap();
        let mut cf = PakroToml::from_file(&p).unwrap();
        let node = "node".into();
        let python = "python".into();
        cf.remove_alias(&node, "16").unwrap();
        cf.remove_alias(&python, "3.10").unwrap();

        assert_debug_snapshot!(cf.alias);
        let cf: Box<dyn ConfigFile> = Box::new(cf);
        assert_snapshot!(cf.dump().unwrap());
        assert_snapshot!(cf);
        assert_debug_snapshot!(cf);
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions() {
        let _config = Config::get().await.unwrap();
        let p = PathBuf::from("/tmp/.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            node = ["16.0.0", "18.0.0"]
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let node = "node".into();
        cf.replace_versions(
            &node,
            vec![
                ToolRequest::new(Arc::new("node".into()), "16.0.1", ToolSource::Unknown).unwrap(),
                ToolRequest::new(Arc::new("node".into()), "18.0.1", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        assert_debug_snapshot!(cf.to_toolset().unwrap());
        let cf: Box<dyn ConfigFile> = Box::new(cf);
        assert_snapshot!(cf.dump().unwrap());
        assert_snapshot!(cf);
        assert_debug_snapshot!(cf);
        file::remove_all(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_preserves_standard_tool_table() {
        let _config = Config::get().await.unwrap();
        let p = CWD
            .as_ref()
            .unwrap()
            .join(".standard-tool-table.pakro.toml");
        let input = indoc! {r#"
            # tool header comment
            [tools.dummy]
            # version explanation
            version = "1.0.0" # selector comment
            foo = "bar" # option comment

            [tools.dummy.install_env]
            # nested explanation
            FOO = "bar" # nested comment
        "#};
        file::write(&p, input).unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dummy = BackendArg::from("dummy");
        let options = cf.tools.lock().unwrap().get(&dummy).unwrap().0[0]
            .options
            .clone()
            .unwrap();

        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new_with_options(
                    Arc::new(dummy.clone()),
                    "2.0.0",
                    options,
                    ToolSource::Unknown,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(
            cf.dump().unwrap(),
            input.replace("version = \"1.0.0\"", "version = \"2.0.0\"")
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_updates_explicit_standard_table_options() {
        let _config = Config::get().await.unwrap();
        let p = CWD
            .as_ref()
            .unwrap()
            .join(".standard-tool-table-explicit-options.pakro.toml");
        file::write(
            &p,
            indoc! {r#"
                [tools.dummy]
                version = "1.0.0"
                foo = "bar" # option comment
                os = ["macos"] # os comment
                depends = ["old"] # depends comment

                [tools.dummy.install_env]
                FOO = "bar" # install env comment
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dummy =
            BackendArg::from("dummy[foo=baz,os=linux,depends=tiny,install_env.FOO=updated]");
        let options = dummy.explicit_opts().unwrap().clone();

        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new_with_options(
                    Arc::new(dummy.clone()),
                    "2.0.0",
                    options,
                    ToolSource::Unknown,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(
            cf.dump().unwrap(),
            indoc! {r#"
                [tools.dummy]
                version = "2.0.0"
                foo = "baz" # option comment
                os = ["linux"] # os comment
                depends = ["tiny"] # depends comment

                [tools.dummy.install_env]
                FOO = "updated" # install env comment
            "#}
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_omits_empty_explicit_standard_table_options() {
        let _config = Config::get().await.unwrap();
        let p = CWD
            .as_ref()
            .unwrap()
            .join(".standard-tool-table-empty-options.pakro.toml");
        file::write(&p, "[tools.dummy]\nversion = \"1.0.0\"\n").unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dummy = BackendArg::from("dummy[os=[],depends=[]]");
        let options = dummy.explicit_opts().unwrap().clone();

        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new_with_options(
                    Arc::new(dummy.clone()),
                    "2.0.0",
                    options,
                    ToolSource::Unknown,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(cf.dump().unwrap(), "[tools.dummy]\nversion = \"2.0.0\"\n");
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_changes_standard_table_selector_in_place() {
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".standard-selector.pakro.toml");
        file::write(
            &p,
            indoc! {r#"
                [tools.dummy]
                version = "1.0.0" # keep selector comment
                foo = "bar"
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dummy = BackendArg::from("dummy");
        let options = cf.tools.lock().unwrap().get(&dummy).unwrap().0[0]
            .options
            .clone()
            .unwrap();

        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new_with_options(
                    Arc::new(dummy.clone()),
                    "prefix:2",
                    options,
                    ToolSource::Unknown,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(
            cf.dump().unwrap(),
            indoc! {r#"
                [tools.dummy]
                prefix = "2" # keep selector comment
                foo = "bar"
            "#}
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_preserves_standard_table_when_renaming_alias() {
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".standard-alias.pakro.toml");
        file::write(
            &p,
            indoc! {r#"
                # node table comment
                [tools.nodejs]
                version = "20.0.0" # keep me

                [tools.dummy]
                version = "1.0.0"
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let node = BackendArg::from("node");

        cf.replace_versions(
            &node,
            vec![ToolRequest::new(Arc::new(node.clone()), "22.0.0", ToolSource::Unknown).unwrap()],
        )
        .unwrap();

        assert_eq!(
            cf.dump().unwrap(),
            indoc! {r#"
                # node table comment
                [tools.node]
                version = "22.0.0" # keep me

                [tools.dummy]
                version = "1.0.0"
            "#}
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_preserves_effective_duplicate_standard_table() {
        let _config = Config::get().await.unwrap();
        let p = CWD
            .as_ref()
            .unwrap()
            .join(".duplicate-standard-tool-table.pakro.toml");
        file::write(
            &p,
            indoc! {r#"
                # first spelling supplies the table position
                [tools.nodejs]
                version = "20.0.0"
                flavor = "shadowed"

                [tools.dummy]
                version = "1.0.0"

                [tools.node]
                version = "22.0.0" # effective selector
                flavor = "effective" # effective option
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let node = BackendArg::from("node");

        cf.replace_versions(
            &node,
            vec![ToolRequest::new(Arc::new(node.clone()), "24.0.0", ToolSource::Unknown).unwrap()],
        )
        .unwrap();

        assert_eq!(
            cf.dump().unwrap(),
            indoc! {r#"
                # first spelling supplies the table position
                [tools.node]
                version = "24.0.0" # effective selector
                flavor = "effective" # effective option

                [tools.dummy]
                version = "1.0.0"
            "#}
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_converts_standard_table_for_multiple_versions() {
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".standard-to-array.pakro.toml");
        file::write(&p, "[tools.dummy]\nversion = \"1.0.0\"\n").unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dummy = BackendArg::from("dummy");

        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new(Arc::new(dummy.clone()), "1.0.0", ToolSource::Unknown).unwrap(),
                ToolRequest::new(Arc::new(dummy.clone()), "2.0.0", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains("[\"1.0.0\", \"2.0.0\"]"),
            "both versions should be preserved: {dump}"
        );
        assert!(
            !dump.contains("[tools.dummy]"),
            "multiple versions should use array syntax: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_keeps_sorted_across_multiple_updates() {
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".multiple-tool-sort.pakro.toml");
        file::write(&p, "[tools]\ntiny-ref = \"1\"\n").unwrap();
        let cf = PakroToml::from_file(&p).unwrap();

        for tool in ["dummy", "tiny-local", "tiny"] {
            let ba = BackendArg::from(tool);
            cf.replace_versions(
                &ba,
                vec![ToolRequest::new(Arc::new(ba.clone()), "1", ToolSource::Unknown).unwrap()],
            )
            .unwrap();
        }

        assert_eq!(
            cf.dump().unwrap(),
            "[tools]\ndummy = \"1\"\ntiny = \"1\"\ntiny-local = \"1\"\ntiny-ref = \"1\"\n"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_remove_plugin() {
        let _config = Config::get().await.unwrap();
        let p = PathBuf::from("/tmp/.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            node = ["16.0.0", "18.0.0"]
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        cf.remove_tool(&"node".into()).unwrap();

        assert_debug_snapshot!(cf.to_toolset().unwrap());
        let cf: Box<dyn ConfigFile> = Box::new(cf);
        assert_snapshot!(cf.dump().unwrap());
        assert_snapshot!(cf);
        assert_debug_snapshot!(cf);
    }

    #[test]
    fn test_env_entries() {
        let toml = formatdoc! {r#"
        [env]
        foo1="1"
        rm=false
        _.path="/foo"
        foo2="2"
        _.file=".env"
        foo3="3"
        "#};
        assert_snapshot!(parse_env(toml), @r#"
        foo1=1
        unset rm
        _.path = "/foo"
        _.file = ".env"
        foo2=2
        foo3=3
        "#);
    }

    #[test]
    fn test_vars_pakro_is_not_a_directive_namespace() {
        let err = toml::from_str::<PakroToml>(indoc! {r#"
            [vars.pakro]
            file = ".env"
        "#})
        .unwrap_err()
        .to_string();
        assert!(err.contains("`vars.pakro` is not supported"), "{err}");

        let err = toml::from_str::<PakroToml>(r#"vars = [[{ pakro = { file = ".env" } }]]"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`vars.pakro` is not supported"), "{err}");
    }

    #[test]
    fn test_env_default_entries() {
        let toml = indoc! {r#"
        [env]
        foo1 = { default = "fallback" }
        foo2 = { default = 2, tools = true }
        "#}
        .to_string();
        assert_snapshot!(parse_env(toml), @r#"
        foo1 default=fallback
        foo2 default=2
        "#);
    }

    #[test]
    fn test_env_module_redact_option() {
        let toml = indoc! {r#"
        [env]
        _.secret-plugin = { redact = false, vault = "main" }
        _.other-plugin = { vault = "main" }
        "#}
        .to_string();
        let entries = parse(toml).env_entries().unwrap();
        let module = |name: &str| {
            entries
                .iter()
                .find_map(|d| match d {
                    EnvDirective::Module(n, value, opts) if n == name => Some((value, opts)),
                    _ => None,
                })
                .unwrap()
        };
        let (value, opts) = module("secret-plugin");
        assert_eq!(opts.redact, Some(false));
        // `redact` is a pakro directive control, not an option for the plugin
        assert!(value.get("redact").is_none());
        assert_eq!(value.get("vault").and_then(|v| v.as_str()), Some("main"));
        assert_eq!(module("other-plugin").1.redact, None);
    }

    #[test]
    fn test_env_module_redact_must_be_boolean() {
        let err = parse_error("[env]\n_.my-plugin = { redact = \"true\" }\n");
        assert!(
            err.contains("_.my-plugin.redact must be a boolean"),
            "{err}"
        );
    }

    #[test]
    fn test_env_default_invalid_combinations() {
        let err = parse_error("[env]\nFOO = { value = \"x\", default = \"y\" }\n");
        assert!(
            err.contains("cannot have both 'value' and 'default'"),
            "{err}"
        );
        let err = parse_error("[env]\nFOO = { required = true, default = \"y\" }\n");
        assert!(
            err.contains("cannot have both 'default' and 'required'"),
            "{err}"
        );
        let err = parse_error("[env]\nFOO = { age = \"AGE-SECRET\", default = \"y\" }\n");
        assert!(
            err.contains("cannot have both 'age' and 'default'"),
            "{err}"
        );
        let err = parse_error("[env]\nFOO = { default = false }\n");
        assert!(err.contains("default cannot be a boolean"), "{err}");
    }

    #[test]
    fn test_env_arr() {
        let toml = formatdoc! {r#"
        [[env]]
        foo1="1"
        rm=false
        _.path="/foo"
        foo2="2"
        _.file=".env"
        foo3="3"
        _.source="/baz1"

        [[env]]
        foo4="4"
        rm=false
        _.file=".env2"
        foo5="5"
        _.path="/bar"
        foo6="6"
        _.source="/baz2"
        "#};
        assert_snapshot!(parse_env(toml), @r#"
        foo1=1
        unset rm
        _.path = "/foo"
        _.file = ".env"
        _.source = "/baz1"
        foo2=2
        foo3=3
        foo4=4
        unset rm
        _.file = ".env2"
        _.path = "/bar"
        _.source = "/baz2"
        foo5=5
        foo6=6
        "#);
    }

    fn parse(s: String) -> PakroToml {
        let p = CWD.as_ref().unwrap().join(".test.pakro.toml");
        file::write(&p, s).unwrap();
        let cfg = PakroToml::from_file(&p).unwrap();
        file::remove_file(&p).unwrap();

        cfg
    }

    fn parse_env(toml: String) -> String {
        parse(toml).env_entries().unwrap().into_iter().join("\n")
    }

    fn parse_error(toml: &str) -> String {
        #[derive(Debug, Deserialize)]
        struct TestConfig {
            #[allow(dead_code)]
            env: EnvList,
        }

        toml::from_str::<TestConfig>(toml).unwrap_err().to_string()
    }

    #[test]
    fn test_is_safe_config_body() {
        assert!(is_safe_config_body(""));
        // Includes are not an inert tool-version declaration.
        assert!(!is_safe_config_body("include = [\"oci::r/x@sha256:0\"]\n"));
        assert!(is_safe_config_body(indoc! {r#"
        min_version = "2024.1.1"
        [tools]
        node = "20"
        python = ["3.11", "3.12"]
        "cargo:eza" = "latest"
        "#}));
        // templates can execute commands
        assert!(!is_safe_config_body(indoc! {r#"
        [tools]
        node = "{{ exec(command='echo 20') }}"
        "#}));
        // tool options like postinstall/install_env run code
        assert!(!is_safe_config_body(indoc! {r#"
        [tools]
        node = { version = "20", postinstall = "corepack enable" }
        "#}));
        assert!(!is_safe_config_body(indoc! {r#"
        [tools]
        node = [{ version = "20" }]
        "#}));
        // options can also be written inline in the key
        assert!(!is_safe_config_body(indoc! {r#"
        [tools]
        "http:probe[url=https://example.com/x.tar.gz,postinstall=touch PWNED]" = "1"
        "#}));
        assert!(!is_safe_config_body(indoc! {r#"
        [tools]
        "github:owner/repo[api_url=http://example.com/api/v3]" = "latest"
        "#}));
        // escaped Tera delimiters ({ == '{', } == '}') decode to
        // `{{ exec(...) }}` after TOML parsing and must not bypass the check
        assert!(!is_safe_config_body(
            "[tools]\nnode = \"\\u007b\\u007b exec(command='echo 20') \\u007d\\u007d\"\n"
        ));
        // an escaped delimiter in a key must also be caught
        // Anything beyond min_version/tools requires trust.
        for body in [
            "[env]\nFOO = \"bar\"",
            "[hooks]\nenter = \"echo hi\"",
            "[settings]\nparanoid = false",
            "[alias]\nnode = \"asdf:foo/bar\"",
            "[plugins]\nfoo = \"https://example.com/foo.git\"",
            "env_file = \".env\"",
            "[doctor.checks.probe]\nrun = \"echo hi\"",
        ] {
            assert!(!is_safe_config_body(body), "should require trust: {body}");
        }
        // invalid toml falls back to the normal trust + parse flow
        assert!(!is_safe_config_body("[tools"));
    }

    #[tokio::test]
    async fn test_table_syntax_preserves_registry_defaults() {
        // Test for #8039: table syntax like `codeql = { version = "latest" }`
        // should preserve registry defaults (e.g. bin_path="codeql").
        let _config = Config::get().await.unwrap();
        let cf = parse(formatdoc! {r#"
            [tools]
            codeql = {{ version = "latest" }}
        "#});
        let trs = cf.to_tool_request_set().unwrap();
        let tools = trs.tools;
        // Find the codeql tool request
        let codeql_requests = tools
            .iter()
            .find(|(ba, _)| ba.short == "codeql")
            .map(|(_, reqs)| reqs)
            .expect("codeql should be in tool request set");
        let opts = codeql_requests[0].options();
        assert_eq!(
            opts.opts.get("bin_path").and_then(toml::Value::as_str),
            Some("codeql"),
            "registry default bin_path should be preserved with table syntax"
        );

        // Also verify that user-provided options override registry defaults
        let cf2 = parse(formatdoc! {r#"
            [tools]
            codeql = {{ version = "latest", bin_path = "custom-codeql" }}
        "#});
        let trs2 = cf2.to_tool_request_set().unwrap();
        let codeql2 = trs2
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "codeql")
            .map(|(_, reqs)| reqs)
            .expect("codeql should be in tool request set");
        let opts2 = codeql2[0].options();
        assert_eq!(
            opts2.opts.get("bin_path").and_then(toml::Value::as_str),
            Some("custom-codeql"),
            "user-provided bin_path should override the registry default"
        );

        let cf3 = parse(formatdoc! {r#"
            [tools]
            codeql = {{ version = "latest", bin_path = "" }}
        "#});
        let trs3 = cf3.to_tool_request_set().unwrap();
        let codeql3 = trs3
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "codeql")
            .map(|(_, reqs)| reqs)
            .expect("codeql should be in tool request set");
        let opts3 = codeql3[0].options();
        assert_eq!(
            opts3.opts.get("bin_path").and_then(toml::Value::as_str),
            Some(""),
            "an empty user-provided bin_path should clear the registry default"
        );
    }

    #[tokio::test]
    async fn test_table_syntax_user_opts_override_registry_defaults() {
        let _config = Config::get().await.unwrap();
        let cf = parse(formatdoc! {r#"
            [tools]
            podman = {{ version = "latest", rename_exe = "podman-remote" }}
        "#});
        let trs = cf.to_tool_request_set().unwrap();
        let podman = trs
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "podman")
            .map(|(_, reqs)| reqs)
            .expect("podman should be in tool request set");

        assert_eq!(
            podman[0].options().get("rename_exe"),
            Some("podman-remote"),
            "user-provided rename_exe should override the registry default"
        );
    }

    #[tokio::test]
    async fn test_npm_table_syntax_preserves_aube_install_options() {
        let _config = Config::get().await.unwrap();
        let cf = parse(formatdoc! {r#"
            [tools]
            "npm:gws-axi" = {{
                version = "0.17.0",
                allow_low_downloads = true,
                trust_policy_excludes = ["undici"]
            }}
        "#});
        let trs = cf.to_tool_request_set().unwrap();
        let request = trs
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "npm:gws-axi")
            .map(|(_, requests)| &requests[0])
            .expect("npm request should be in tool request set");
        let options = request.options();

        // Backend scalar options are normalized to strings by ToolOptions.
        assert_eq!(
            options.opts.get("allow_low_downloads"),
            Some(&toml::Value::String("true".to_string()))
        );
        assert_eq!(
            options.opts.get("trust_policy_excludes"),
            Some(&toml::Value::Array(vec![toml::Value::String(
                "undici".to_string()
            )]))
        );
    }

    #[tokio::test]
    async fn test_depends_field_parsing() {
        let _config = Config::get().await.unwrap();
        let cf = parse(formatdoc! {r#"
            [tools]
            dummy = {{ version = "latest", depends = ["tiny"] }}
        "#});
        let trs = cf.to_tool_request_set().unwrap();
        let dummy = trs
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "dummy")
            .map(|(_, reqs)| reqs)
            .expect("dummy should be in tool request set");
        let opts = dummy[0].options();
        assert_eq!(
            opts.depends,
            Some(vec!["tiny".to_string()]),
            "depends should be parsed as a named field"
        );
        assert!(
            !opts.opts.contains_key("depends"),
            "depends should not leak into opts"
        );
    }

    #[tokio::test]
    async fn test_depends_field_single_string() {
        let _config = Config::get().await.unwrap();
        let cf = parse(formatdoc! {r#"
            [tools]
            dummy = {{ version = "latest", depends = "tiny" }}
        "#});
        let trs = cf.to_tool_request_set().unwrap();
        let dummy = trs
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "dummy")
            .map(|(_, reqs)| reqs)
            .expect("dummy should be in tool request set");
        let opts = dummy[0].options();
        assert_eq!(
            opts.depends,
            Some(vec!["tiny".to_string()]),
            "single string depends should be wrapped in a vec"
        );
    }

    #[tokio::test]
    async fn test_os_field_single_string() {
        let _config = Config::get().await.unwrap();
        let cf = parse(formatdoc! {r#"
            [tools]
            dummy = {{ version = "latest", os = "linux" }}
        "#});
        let trs = cf.to_tool_request_set().unwrap();
        let dummy = trs
            .tools
            .iter()
            .find(|(ba, _)| ba.short == "dummy")
            .map(|(_, reqs)| reqs)
            .expect("dummy should be in tool request set");
        let opts = dummy[0].options();
        assert_eq!(
            opts.os,
            Some(vec!["linux".to_string()]),
            "single string os should be wrapped in a vec"
        );
        assert!(
            !opts.opts.contains_key("os"),
            "os should not leak into opts"
        );
    }

    #[tokio::test]
    async fn test_replace_versions_preserves_named_core_options() {
        let _config = Config::get().await.unwrap();
        let p = CWD
            .as_ref()
            .unwrap()
            .join(".replace-core-options.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            needs-dummy = "1.0.0"
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let needs_dummy = "needs-dummy".into();
        let mut options = ToolVersionOptions {
            core: CoreToolOptions {
                os: Some(vec!["linux".to_string()]),
                depends: Some(vec!["dummy".to_string()]),
                ..Default::default()
            },
            ..Default::default()
        };
        options
            .install_env
            .insert("FOO".to_string(), EnvValue::from("bar"));

        cf.replace_versions(
            &needs_dummy,
            vec![
                ToolRequest::new_with_options(
                    Arc::new("needs-dummy".into()),
                    "1.0.1",
                    options,
                    ToolSource::Unknown,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(dump.contains("depends"), "depends should be written back");
        assert!(
            dump.contains("dummy"),
            "depends value should be written back"
        );
        assert!(dump.contains("os"), "os should be written back");
        assert!(
            dump.contains("install_env"),
            "install_env should be written back"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_preserves_comments() {
        // https://github.com/jdx/mise/discussions/4797
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".replace-comments.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            # renovate: datasource=github-releases depName=node
            node = "16.0.0" # keep me
            dummy = ["1.0.0"] # keep me too
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let node = "node".into();
        cf.replace_versions(
            &node,
            vec![ToolRequest::new(Arc::new("node".into()), "18.0.0", ToolSource::Unknown).unwrap()],
        )
        .unwrap();
        let dummy = "dummy".into();
        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new(Arc::new("dummy".into()), "1.0.1", ToolSource::Unknown).unwrap(),
                ToolRequest::new(Arc::new("dummy".into()), "2.0.0", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains("# renovate: datasource=github-releases depName=node"),
            "comment above the tool should survive: {dump}"
        );
        assert!(
            dump.contains(r#"node = "18.0.0" # keep me"#),
            "comment after the version should survive: {dump}"
        );
        assert!(
            dump.contains(r#"dummy = ["1.0.1", "2.0.0"] # keep me too"#),
            "comment after a multi-version tool should survive: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_preserves_array_element_comments() {
        // https://github.com/jdx/mise/discussions/4797
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".array-comments.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            dummy = [
              "1.0.0", # first
              "2.0.0", # second
            ]
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dummy = "dummy".into();
        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new(Arc::new("dummy".into()), "1.0.1", ToolSource::Unknown).unwrap(),
                ToolRequest::new(Arc::new("dummy".into()), "2.0.1", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains(indoc! {r#"
                dummy = [
                  "1.0.1", # first
                  "2.0.1", # second
                ]"#}),
            "the array should keep its layout with each comment on its own element: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_array_comments_stay_positional() {
        // A comment written after an array element belongs to the decor of the element that
        // follows it, so it is tied to a position rather than to a version. Reordering the
        // versions therefore leaves the comments where the user put them, which is the same rule
        // the scalar case follows: a comment survives its value changing.
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".array-reorder.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            dummy = [
              "1.0.0", # first
              "2.0.0", # second
            ]
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dummy = "dummy".into();
        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new(Arc::new("dummy".into()), "2.0.0", ToolSource::Unknown).unwrap(),
                ToolRequest::new(Arc::new("dummy".into()), "1.0.0", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains(indoc! {r#"
                dummy = [
                  "2.0.0", # first
                  "1.0.0", # second
                ]"#}),
            "comments should stay at their position rather than follow a version: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_array_count_change_still_writes() {
        // a different number of versions cannot reuse the old array's layout, so it falls back to
        // building a fresh one — the versions themselves still have to be written
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".array-count.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            dummy = ["1.0.0", "2.0.0"]
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dummy = "dummy".into();
        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new(Arc::new("dummy".into()), "1.0.1", ToolSource::Unknown).unwrap(),
                ToolRequest::new(Arc::new("dummy".into()), "2.0.1", ToolSource::Unknown).unwrap(),
                ToolRequest::new(Arc::new("dummy".into()), "3.0.0", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains(r#"dummy = ["1.0.1", "2.0.1", "3.0.0"]"#),
            "all versions should be written: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_array_element_comments_survive_alias_rename() {
        // an aliased entry is renamed to the short name, and the element comments have to come
        // along. Nothing in the reuse itself states why this works: the array is looked up under
        // the spelling the file actually uses, which the alias-aware key list supplies. Reading it
        // from the short name instead would drop these comments again while every other test here
        // still passed, so pin the combination.
        let _config = Config::get().await.unwrap();
        let p = CWD
            .as_ref()
            .unwrap()
            .join(".aliased-array-comments.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            nodejs = [
              "20.11.0", # first
              "22.0.0", # second
            ]
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let node: BackendArg = "node".into();
        cf.replace_versions(
            &node,
            vec![
                ToolRequest::new(Arc::new("node".into()), "20.11.1", ToolSource::Unknown).unwrap(),
                ToolRequest::new(Arc::new("node".into()), "22.1.0", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains(indoc! {r#"
                node = [
                  "20.11.1", # first
                  "22.1.0", # second
                ]"#}),
            "the renamed key should keep the comments between its elements: {dump}"
        );
        assert!(
            !dump.contains("nodejs"),
            "the alias must not be left behind as a second key: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_preserves_comments_on_qualified_key() {
        // a fully-qualified entry is rewritten to its short name, and the comments have to move
        // with it: https://github.com/jdx/mise/discussions/4797
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".qualified.pakro.toml");
        let node: BackendArg = "node".into();
        let contents = formatdoc! {r#"
            [tools]
            # renovate: datasource=github-releases depName=node
            "{}" = "16.0.0" # keep me
            "#, node.full()};
        file::write(&p, contents).unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        cf.replace_versions(
            &node,
            vec![ToolRequest::new(Arc::new("node".into()), "18.0.0", ToolSource::Unknown).unwrap()],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains("# renovate: datasource=github-releases depName=node"),
            "comment above should survive the rename: {dump}"
        );
        assert!(
            dump.contains(r#"node = "18.0.0" # keep me"#),
            "comment after the version should survive the rename: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_renames_aliased_key() {
        // `nodejs` is an alias for `node`, so the file defines one tool and not two: the entry has
        // to be renamed in place rather than a second key added beside it, or the file ends up
        // with two keys for one tool and the later one silently wins when it is read back
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".aliased-key.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            # renovate: datasource=github-releases depName=node
            nodejs = "20.11.0" # keep me
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let node: BackendArg = "node".into();
        cf.replace_versions(
            &node,
            vec![
                ToolRequest::new(Arc::new("node".into()), "24.16.0", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains(r#"node = "24.16.0" # keep me"#),
            "the aliased key should be renamed and keep its comment: {dump}"
        );
        assert!(
            dump.contains("# renovate: datasource=github-releases depName=node"),
            "the comment above should survive the rename: {dump}"
        );
        assert!(
            !dump.contains("nodejs"),
            "the alias must not be left behind as a second key: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_renames_aliased_key_with_array() {
        // same for a multi-version entry: the array moves to the short name rather than being
        // written out a second time
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".aliased-array.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            nodejs = ["20.11.0", "22.0.0"] # keep me
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let node: BackendArg = "node".into();
        cf.replace_versions(
            &node,
            vec![
                ToolRequest::new(Arc::new("node".into()), "20.11.1", ToolSource::Unknown).unwrap(),
                ToolRequest::new(Arc::new("node".into()), "22.1.0", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains(r#"node = ["20.11.1", "22.1.0"] # keep me"#),
            "the array should move to the short name: {dump}"
        );
        assert!(
            !dump.contains("nodejs"),
            "the alias must not be left behind as a second key: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_collapses_duplicate_spellings() {
        // a file damaged by the old behavior has both keys; they deserialize to one entry and the
        // later one silently wins, so the write has to leave exactly one key behind. The first key
        // in the file is the entry the file appears to define, so it supplies the decorations.
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".dupe-spellings.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            nodejs = "20.11.0" # keep me
            node = "24.16.0"
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let node: BackendArg = "node".into();
        cf.replace_versions(
            &node,
            vec![
                ToolRequest::new(Arc::new("node".into()), "24.17.0", ToolSource::Unknown).unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains(r#"node = "24.17.0" # keep me"#),
            "the first key in the file supplies the decor: {dump}"
        );
        assert!(
            !dump.contains("nodejs"),
            "the duplicate spelling should be removed: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_keeps_registry_alias_key() {
        // `rg` is a registry alias for `ripgrep`, not one of the hardcoded backend aliases: the two
        // resolve to different short names, pakro treats them as separate entries, and each is
        // written back under the name it was asked for
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".registry-alias.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            ripgrep = "14.1.0"
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let rg: BackendArg = "rg".into();
        cf.replace_versions(
            &rg,
            vec![ToolRequest::new(Arc::new("rg".into()), "14.1.1", ToolSource::Unknown).unwrap()],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains(r#"ripgrep = "14.1.0""#),
            "a registry alias is a different entry and must be left as written: {dump}"
        );
        assert!(
            dump.contains(r#"rg = "14.1.1""#),
            "the tool should be written under the name it was asked for: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_remove_tool_removes_aliased_key() {
        // `pakro unuse nodejs` used to report success and prune the install while leaving the
        // `nodejs` key in the file, because it only ever looked for the short name
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".remove-aliased.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            dummy = "1.0.0"
            nodejs = "20.11.0"
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        cf.remove_tool(&"nodejs".into()).unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            !dump.contains("nodejs"),
            "the aliased key should be removed: {dump}"
        );
        assert!(
            dump.contains(r#"dummy = "1.0.0""#),
            "other tools should be left alone: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_remove_tool_removes_qualified_key() {
        // matching is done on the key as written, so this holds whatever the registry currently
        // reports as node's backend
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".remove-qualified.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            "core:node" = "20.11.0"
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        cf.remove_tool(&"node".into()).unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            !dump.contains("node"),
            "the qualified key should be removed: {dump}"
        );
        assert!(
            !dump.contains("[tools]"),
            "the now-empty tools table should be dropped: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_update_env_preserves_comments() {
        // https://github.com/jdx/mise/discussions/4797
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".env-comments.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [env]
            # keep this comment
            FOO = "bar" # keep me
            "#},
        )
        .unwrap();
        let mut cf = PakroToml::from_file(&p).unwrap();
        cf.update_env("FOO", "baz").unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains("# keep this comment"),
            "comment above the variable should survive: {dump}"
        );
        assert!(
            dump.contains(r#"FOO = "baz" # keep me"#),
            "comment after the value should survive: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_set_alias_preserves_comments() {
        // https://github.com/jdx/mise/discussions/4797
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".alias-comments.pakro.toml");
        let node: BackendArg = "node".into();
        let contents = formatdoc! {r#"
            [tool_alias.{node}.versions]
            # keep this comment
            lts = "20" # keep me
            "#};
        file::write(&p, contents).unwrap();
        let mut cf = PakroToml::from_file(&p).unwrap();
        cf.set_alias(&node, "lts", "22").unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains("# keep this comment"),
            "comment above the alias should survive: {dump}"
        );
        assert!(
            dump.contains(r#"lts = "22" # keep me"#),
            "comment after the alias should survive: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_set_shell_alias_preserves_comments() {
        // https://github.com/jdx/mise/discussions/4797
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".shell-alias.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [shell_alias]
            # keep this comment
            ll = "ls -l" # keep me
            "#},
        )
        .unwrap();
        let mut cf = PakroToml::from_file(&p).unwrap();
        cf.set_shell_alias("ll", "ls -la").unwrap();

        let dump = cf.dump().unwrap();
        assert!(
            dump.contains("# keep this comment"),
            "comment above the shell alias should survive: {dump}"
        );
        assert!(
            dump.contains(r#"ll = "ls -la" # keep me"#),
            "comment after the shell alias should survive: {dump}"
        );
        file::remove_file(&p).unwrap();
    }

    #[tokio::test]
    async fn test_replace_versions_omits_empty_os() {
        let _config = Config::get().await.unwrap();
        let p = CWD.as_ref().unwrap().join(".replace-empty-os.pakro.toml");
        file::write(
            &p,
            formatdoc! {r#"
            [tools]
            dummy = "1.0.0"
            "#},
        )
        .unwrap();
        let cf = PakroToml::from_file(&p).unwrap();
        let dummy = "dummy".into();
        let options = ToolVersionOptions {
            core: CoreToolOptions {
                os: Some(vec![]),
                ..Default::default()
            },
            ..Default::default()
        };

        cf.replace_versions(
            &dummy,
            vec![
                ToolRequest::new_with_options(
                    Arc::new("dummy".into()),
                    "1.0.1",
                    options,
                    ToolSource::Unknown,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        let dump = cf.dump().unwrap();
        assert!(dump.contains(r#"dummy = "1.0.1""#));
        assert!(!dump.contains("os"), "empty os should not be written back");
        file::remove_file(&p).unwrap();
    }
}
