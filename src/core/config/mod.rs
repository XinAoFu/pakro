use eyre::{Context, Result, bail, eyre};
use indexmap::{IndexMap, IndexSet};
use itertools::Itertools;
use path_absolutize::Absolutize;
pub use settings::{CompilePurpose, Settings, SettingsExt};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env::join_paths;
use std::fmt::{Debug, Formatter};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock as Lazy;
use std::sync::atomic::{AtomicBool, Ordering};

// Startup admission and local queries may decode configuration but must never
// turn that observation into tracking, migration, or installation metadata.
static READONLY_INVOCATION: AtomicBool = AtomicBool::new(false);

pub fn set_readonly_invocation(readonly: bool) {
    READONLY_INVOCATION.store(readonly, Ordering::Relaxed);
}

pub fn readonly_invocation() -> bool {
    READONLY_INVOCATION.load(Ordering::Relaxed)
}
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};
use tokio::{sync::OnceCell, task::JoinSet};

use crate::args::{BackendArg, split_bracketed_opts};
use crate::backend::ABackend;
use crate::config::config_file::idiomatic_version::IdiomaticVersionFile;
use crate::config::config_file::min_version::MinVersionSpec;
use crate::config::config_file::pakro_toml::{MonorepoConfig, PakroToml};
use crate::config::config_file::{ConfigFile, config_trust_root};
use crate::config::env_directive::{EnvResolveOptions, EnvResults, ToolsFilter};
use crate::config::tracking::Tracker;
use crate::env::{PAKRO_DEFAULT_CONFIG_FILENAME, PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME};
use crate::file::display_path;
use crate::shorthands::{Shorthands, get_shorthands};
use crate::tera::take_tera_accessed_files;
use crate::toolset::env_cache::{CachedNonToolEnv, compute_settings_hash, get_file_mtime};
use crate::toolset::{
    ResolveOptions, ResolvedToolOptions, ToolOptions, ToolRequestSet, ToolRequestSetBuilder,
    ToolSource, ToolVersion, ToolVersionOptions, Toolset, install_state,
};
use crate::ui::style;
use crate::version;
use crate::{backend, dirs, env, file, lockfile, registry, runtime_symlinks, shims, timeout};

pub(crate) mod command_wrapper;
pub mod config_file;
pub mod doctor;
pub mod env_directive;
pub mod pakrorc;
pub(crate) mod provenance;
pub mod settings;
pub mod tracking;

use crate::env_diff::EnvMap;
use crate::hook_env::WatchFilePattern;
use crate::hooks::Hook;
use crate::plugins::PluginType;
use crate::tera::BASE_CONTEXT;
use crate::wildcard::Wildcard;
pub use command_wrapper::CommandWrapper;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigDiscoveryCandidate {
    pub(crate) directory: PathBuf,
    pub(crate) pattern: String,
}

/// Every discovery choice belongs to the invocation, rather than process globals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrozenConfigDiscovery {
    pub(crate) local_patterns: Vec<String>,
    pub(crate) tool_versions_patterns: Vec<String>,
    pub(crate) ceiling_paths: Vec<PathBuf>,
    pub(crate) global_candidates: Vec<ConfigDiscoveryCandidate>,
    pub(crate) system_candidates: Vec<ConfigDiscoveryCandidate>,
    pub(crate) environments: Vec<String>,
    pub(crate) env_conf_d: bool,
    pub(crate) ignored_paths: Vec<PathBuf>,
    pub(crate) no_config: bool,
}
impl FrozenConfigDiscovery {
    /// Capture the same invocation choices as Config without loading plugins,
    /// evaluating configuration or touching the tracker. Discovery itself then
    /// records both present and missing paths in the project observation.
    pub(crate) fn from_process() -> Self {
        let tool_overrides = env::PAKRO_OVERRIDE_TOOL_VERSIONS_FILENAMES
            .as_ref()
            .map(|patterns| patterns.iter().cloned().collect::<Vec<_>>());
        let config_overrides = env::PAKRO_OVERRIDE_CONFIG_FILENAMES
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut frozen = Self::from_explicit_options(
            &env::PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME,
            &env::PAKRO_DEFAULT_CONFIG_FILENAME,
            tool_overrides.as_deref(),
            &config_overrides,
        );
        frozen.environments = env::PAKRO_ENV_WITH_AUTO.clone();
        frozen.env_conf_d = env::env_conf_d();
        frozen.ceiling_paths = env::PAKRO_CEILING_PATHS.iter().cloned().collect();
        frozen.ignored_paths = env::PAKRO_IGNORED_CONFIG_PATHS.clone();
        frozen.no_config = Settings::no_config();
        frozen.global_candidates = Self::external_candidates(
            &dirs::CONFIG,
            env::PAKRO_GLOBAL_CONFIG_FILE.as_deref(),
            &frozen.environments,
            frozen.env_conf_d,
        );
        frozen.system_candidates = Self::external_candidates(
            &dirs::SYSTEM_CONFIG,
            env::PAKRO_SYSTEM_CONFIG_FILE.as_deref(),
            &frozen.environments,
            frozen.env_conf_d,
        );
        if env::PAKRO_GLOBAL_CONFIG_FILE.is_none() && !*env::PAKRO_USE_TOML {
            frozen.global_candidates.push(ConfigDiscoveryCandidate {
                directory: dirs::HOME.to_path_buf(),
                pattern: env::PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME.to_string(),
            });
        }
        frozen
    }

    fn external_candidates(
        directory: &Path,
        explicit: Option<&Path>,
        environments: &[String],
        env_conf_d: bool,
    ) -> Vec<ConfigDiscoveryCandidate> {
        if let Some(path) = explicit {
            return vec![ConfigDiscoveryCandidate {
                directory: path.parent().unwrap_or(Path::new("/")).to_owned(),
                pattern: glob::Pattern::escape(
                    &path.file_name().unwrap_or_default().to_string_lossy(),
                ),
            }];
        }
        let mut patterns = vec![
            "conf.d/*.toml".to_owned(),
            "conf.d/*/pakro.toml".to_owned(),
            "conf.d/*/pakro.local.toml".to_owned(),
            "config.toml".to_owned(),
            "pakro.toml".to_owned(),
        ];
        for environment in environments {
            let environment = glob::Pattern::escape(environment);
            if env_conf_d {
                patterns.push(format!("conf.d/*.{}.toml", environment));
            }
            patterns.extend([
                format!("conf.d/*/pakro.{environment}.toml"),
                format!("config.{environment}.toml"),
                format!("pakro.{environment}.toml"),
            ]);
        }
        patterns.extend([
            "config.local.toml".to_owned(),
            "pakro.local.toml".to_owned(),
        ]);
        for environment in environments {
            let environment = glob::Pattern::escape(environment);
            if env_conf_d {
                patterns.push(format!("conf.d/*.{}.local.toml", environment));
            }
            patterns.extend([
                format!("conf.d/*/pakro.{environment}.local.toml"),
                format!("config.{environment}.local.toml"),
                format!("pakro.{environment}.local.toml"),
            ]);
        }
        patterns
            .into_iter()
            .map(|pattern| ConfigDiscoveryCandidate {
                directory: directory.to_owned(),
                pattern,
            })
            .collect()
    }

    pub(crate) fn new(local_patterns: Vec<String>) -> Self {
        Self {
            local_patterns,
            tool_versions_patterns: vec![".tool-versions".to_owned()],
            ceiling_paths: vec![],
            global_candidates: vec![],
            system_candidates: vec![],
            environments: vec![],
            env_conf_d: false,
            ignored_paths: vec![],
            no_config: false,
        }
    }

    pub(crate) fn default_local_patterns(
        tool_versions_filename: &str,
        config_filename: &str,
        tool_versions_overrides: Option<&[String]>,
        config_overrides: &[String],
    ) -> Vec<String> {
        let tool_overrides = tool_versions_overrides
            .map(|values| values.iter().map(String::as_str).collect::<Vec<_>>());
        let config_overrides = config_overrides
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        local_config_patterns(
            tool_versions_filename,
            config_filename,
            tool_overrides.as_deref(),
            &config_overrides,
        )
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    pub(crate) fn from_explicit_options(
        tool_versions_filename: &str,
        config_filename: &str,
        tool_versions_overrides: Option<&[String]>,
        config_overrides: &[String],
    ) -> Self {
        let mut options = Self::new(Self::default_local_patterns(
            tool_versions_filename,
            config_filename,
            tool_versions_overrides,
            config_overrides,
        ));
        options.tool_versions_patterns = tool_versions_overrides.map_or_else(
            || {
                [".tool-versions", tool_versions_filename]
                    .into_iter()
                    .unique()
                    .map(str::to_owned)
                    .collect()
            },
            |patterns| patterns.to_vec(),
        );
        options
    }

    pub(crate) fn format_for(
        &self,
        path: &Path,
    ) -> std::result::Result<config_file::ConfigDocumentFormat, ConfigDiscoveryError> {
        if !path.is_absolute() {
            return Err(ConfigDiscoveryError::RelativePath);
        }
        for pattern in &self.tool_versions_patterns {
            if Path::new(pattern).is_absolute() {
                return Err(ConfigDiscoveryError::InvalidPattern);
            }
            let pattern =
                glob::Pattern::new(pattern).map_err(|_| ConfigDiscoveryError::InvalidPattern)?;
            let matches = glob::MatchOptions {
                require_literal_separator: true,
                ..Default::default()
            };
            for parent in path.ancestors().skip(1) {
                let relative = path
                    .strip_prefix(parent)
                    .map_err(|_| ConfigDiscoveryError::InvalidPattern)?;
                if pattern.matches_path_with(relative, matches) {
                    return Ok(config_file::ConfigDocumentFormat::ToolVersions);
                }
            }
        }
        Ok(config_file::ConfigDocumentFormat::PakroToml)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConfigDiscoveryLimits {
    pub(crate) max_files: usize,
    pub(crate) max_candidates: usize,
    pub(crate) max_path_bytes: usize,
    pub(crate) max_total_path_bytes: usize,
}
impl Default for ConfigDiscoveryLimits {
    fn default() -> Self {
        Self {
            max_files: 1024,
            max_candidates: 16_384,
            max_path_bytes: 16 * 1024,
            max_total_path_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigGlobEvidence {
    pub(crate) directory: PathBuf,
    pub(crate) pattern: String,
    pub(crate) matches: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DiscoveredConfigInputs {
    /// Highest precedence first, including each alias spelling for observation.
    pub(crate) files: Vec<PathBuf>,
    pub(crate) missing_candidates: Vec<PathBuf>,
    pub(crate) glob_scans: Vec<ConfigGlobEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ConfigDiscoveryError {
    #[error("configuration discovery needs absolute frozen paths")]
    RelativePath,
    #[error("configuration discovery has an invalid pattern")]
    InvalidPattern,
    #[error("configuration discovery could not inspect a candidate")]
    Unavailable,
    #[error("configuration discovery exceeded its input budget")]
    Budget,
    #[error("configuration discovery deadline expired")]
    Expired,
}

pub(crate) fn discover_project_configs(
    root: &Path,
    options: &FrozenConfigDiscovery,
    limits: ConfigDiscoveryLimits,
    deadline: std::time::Instant,
) -> std::result::Result<DiscoveredConfigInputs, ConfigDiscoveryError> {
    if std::time::Instant::now() >= deadline {
        return Err(ConfigDiscoveryError::Expired);
    }
    if !root.is_absolute()
        || options
            .ceiling_paths
            .iter()
            .chain(&options.ignored_paths)
            .any(|path| !path.is_absolute())
        || options
            .global_candidates
            .iter()
            .chain(&options.system_candidates)
            .any(|candidate| !candidate.directory.is_absolute())
    {
        return Err(ConfigDiscoveryError::RelativePath);
    }
    if options.no_config {
        return Ok(DiscoveredConfigInputs::default());
    }
    let mut collector = ConfigDiscoveryCollector {
        limits,
        deadline,
        options,
        candidates: 0,
        path_bytes: 0,
        output: DiscoveredConfigInputs::default(),
        files_seen: std::collections::HashSet::new(),
        missing_seen: std::collections::HashSet::new(),
        ignored: config_file::FrozenIgnoredConfigPaths::default(),
    };
    collector.charge(root.as_os_str().len())?;
    for path in &options.ignored_paths {
        collector.charge(path.as_os_str().len())?;
    }
    for pattern in &options.tool_versions_patterns {
        collector.charge(pattern.len())?;
        if Path::new(pattern).is_absolute() {
            return Err(ConfigDiscoveryError::InvalidPattern);
        }
        glob::Pattern::new(pattern).map_err(|_| ConfigDiscoveryError::InvalidPattern)?;
    }
    collector.ignored = config_file::FrozenIgnoredConfigPaths::new(&options.ignored_paths)?;
    // Both existing loading and this observation use the same filename order:
    // low-to-high patterns, environment overlays last, nearest directory first.
    for directory in root.ancestors().take_while(|path| {
        !options
            .ceiling_paths
            .iter()
            .any(|ceiling| *path == ceiling.as_path())
    }) {
        collector.check_clock()?;
        if collector.ignored(directory)? {
            continue;
        }
        for environment in options.environments.iter().rev() {
            collector.charge(environment.len())?;
            for pattern in env_config_patterns_with_conf_d(environment, options.env_conf_d)
                .iter()
                .rev()
            {
                collector.scan(directory, pattern, false)?;
            }
        }
        for pattern in options.local_patterns.iter().rev() {
            collector.scan(directory, pattern, false)?;
        }
    }
    for candidate in options
        .global_candidates
        .iter()
        .rev()
        .chain(options.system_candidates.iter().rev())
    {
        collector.scan(&candidate.directory, &candidate.pattern, true)?;
    }
    Ok(collector.output)
}

struct ConfigDiscoveryCollector<'a> {
    options: &'a FrozenConfigDiscovery,
    limits: ConfigDiscoveryLimits,
    deadline: std::time::Instant,
    candidates: usize,
    path_bytes: usize,
    output: DiscoveredConfigInputs,
    files_seen: std::collections::HashSet<PathBuf>,
    missing_seen: std::collections::HashSet<PathBuf>,
    ignored: config_file::FrozenIgnoredConfigPaths,
}

impl ConfigDiscoveryCollector<'_> {
    fn check_clock(&self) -> std::result::Result<(), ConfigDiscoveryError> {
        if std::time::Instant::now() >= self.deadline {
            Err(ConfigDiscoveryError::Expired)
        } else {
            Ok(())
        }
    }

    /// Charge before allocating/copying candidate paths or collecting glob entries.
    fn charge(&mut self, bytes: usize) -> std::result::Result<(), ConfigDiscoveryError> {
        self.check_clock()?;
        if bytes > self.limits.max_path_bytes || self.candidates >= self.limits.max_candidates {
            return Err(ConfigDiscoveryError::Budget);
        }
        self.path_bytes = self
            .path_bytes
            .checked_add(bytes)
            .ok_or(ConfigDiscoveryError::Budget)?;
        if self.path_bytes > self.limits.max_total_path_bytes {
            return Err(ConfigDiscoveryError::Budget);
        }
        self.candidates += 1;
        Ok(())
    }

    fn ignored(&self, path: &Path) -> std::result::Result<bool, ConfigDiscoveryError> {
        self.check_clock()?;
        let result = self.ignored.is_match(path)?;
        self.check_clock()?;
        Ok(result)
    }

    fn scan(
        &mut self,
        directory: &Path,
        pattern: &str,
        external: bool,
    ) -> std::result::Result<(), ConfigDiscoveryError> {
        if Path::new(pattern).is_absolute() {
            return Err(ConfigDiscoveryError::InvalidPattern);
        }
        let path_bytes = directory
            .as_os_str()
            .len()
            .checked_add(pattern.len())
            .and_then(|length| length.checked_add(1))
            .ok_or(ConfigDiscoveryError::Budget)?;
        self.charge(path_bytes)?;
        let path = directory.join(pattern);
        if self.ignored(directory)? {
            return Ok(());
        }
        if !is_glob_pattern(pattern) {
            if self.ignored(&path)? {
                return Ok(());
            }
            return self.record_path(path, true);
        }
        // Deliberately avoid GLOB_RESULTS, global/system caches and cached desymlinking.
        // Escaping the root keeps literal bracket characters in a project path literal.
        let root = directory
            .to_str()
            .ok_or(ConfigDiscoveryError::InvalidPattern)?;
        let escaped = glob::Pattern::escape(root);
        let glob_pattern = Path::new(&escaped).join(pattern);
        let glob_pattern = glob_pattern
            .to_str()
            .ok_or(ConfigDiscoveryError::InvalidPattern)?;
        let entries = glob::glob(glob_pattern).map_err(|_| ConfigDiscoveryError::InvalidPattern)?;
        let mut matches = Vec::new();
        for entry in entries {
            self.check_clock()?;
            let entry = entry.map_err(|_| ConfigDiscoveryError::Unavailable)?;
            self.charge(entry.as_os_str().len())?;
            let roots: &[&Path] = if external {
                std::slice::from_ref(&directory)
            } else {
                &[]
            };
            if self.ignored(&entry)?
                || !config_glob_entry_is_visible(&entry, roots)
                || !conf_d_path_is_active(
                    &entry,
                    pattern,
                    self.options.env_conf_d,
                    &self.options.environments,
                )
            {
                continue;
            }
            matches.push(entry);
        }
        matches.sort();
        for path in matches.iter().rev() {
            self.record_path(path.clone(), false)?;
        }
        self.output.glob_scans.push(ConfigGlobEvidence {
            directory: directory.to_path_buf(),
            pattern: pattern.to_owned(),
            matches,
        });
        Ok(())
    }

    fn record_path(
        &mut self,
        path: PathBuf,
        literal: bool,
    ) -> std::result::Result<(), ConfigDiscoveryError> {
        self.check_clock()?;
        match std::fs::symlink_metadata(&path) {
            Ok(entry) => {
                let metadata = if entry.file_type().is_symlink() {
                    std::fs::metadata(&path).map_err(|_| ConfigDiscoveryError::Unavailable)?
                } else {
                    entry
                };
                if metadata.is_dir() {
                    return Ok(());
                }
                if !metadata.is_file() {
                    return Err(ConfigDiscoveryError::Unavailable);
                }
                if self.files_seen.contains(&path) {
                    return Ok(());
                }
                if self.output.files.len() >= self.limits.max_files {
                    return Err(ConfigDiscoveryError::Budget);
                }
                self.files_seen.insert(path.clone());
                self.output.files.push(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !literal {
                    return Err(ConfigDiscoveryError::Unavailable);
                }
                if self.missing_seen.insert(path.clone()) {
                    self.output.missing_candidates.push(path);
                }
            }
            Err(_) => return Err(ConfigDiscoveryError::Unavailable),
        }
        Ok(())
    }
}

#[cfg(test)]
mod r03_discovery_tests {
    use super::*;
    use std::time::Instant;

    fn discover(root: &Path, options: &FrozenConfigDiscovery) -> DiscoveredConfigInputs {
        discover_project_configs(
            root,
            options,
            Default::default(),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn r03_discovery_ignored_alias_uses_the_same_identity_as_the_candidate() {
        let temporary = tempfile::tempdir().unwrap();
        let actual = temporary.path().join("actual");
        let alias = temporary.path().join("ignored");
        std::fs::create_dir(&actual).unwrap();
        std::fs::write(actual.join("pakro.toml"), "").unwrap();
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        let mut options = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
        options.ceiling_paths.push(temporary.path().to_path_buf());
        options.ignored_paths.push(alias.clone());
        assert!(discover(&actual, &options).files.is_empty());
        options.ignored_paths = vec![actual];
        assert!(discover(&alias, &options).files.is_empty());
    }

    #[test]
    fn r03_discovery_ignored_globs_and_invalid_patterns_are_explicit() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        std::fs::create_dir_all(root.join(".pakro/conf.d")).unwrap();
        std::fs::write(root.join("pakro.toml"), "").unwrap();
        std::fs::write(root.join(".pakro/conf.d/ignored.toml"), "").unwrap();
        let mut options =
            FrozenConfigDiscovery::new(vec![".pakro/conf.d/*.toml".into(), "pakro.toml".into()]);
        options.ceiling_paths.push(temporary.path().to_path_buf());
        options
            .ignored_paths
            .push(root.join(".pakro/conf.d/*.toml"));
        assert_eq!(
            discover(&root, &options).files,
            vec![root.join("pakro.toml")]
        );
        options.ignored_paths = vec![root.join("[")];
        assert_eq!(
            discover_project_configs(
                &root,
                &options,
                Default::default(),
                Instant::now() + Duration::from_secs(10)
            )
            .unwrap_err(),
            ConfigDiscoveryError::InvalidPattern
        );
    }

    #[test]
    fn p1_config_read_receipt_rejects_changed_source_before_shared_observation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let path = root.join("pakro.toml");
        std::fs::write(&path, "[tools]\npython='3.15.0'\n[env]\nEPOCH='old'\n").unwrap();
        let mut discovery =
            FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
        discovery
            .ceiling_paths
            .push(root.parent().unwrap().to_owned());
        let deadline = Instant::now() + Duration::from_secs(10);
        let receipt = ConfigReadReceipt::capture(
            &root,
            discovery,
            std::slice::from_ref(&path),
            deadline,
            deadline,
        )
        .unwrap();
        // The legacy parser has consumed the old source; a later Python
        // observation must not merge a new declaration into those old envs.
        let _parsed = PakroToml::from_file(&path).unwrap();
        receipt.recheck(deadline).unwrap();
        std::fs::write(&path, "[tools]\npython='3.14.0'\n[env]\nEPOCH='new'\n").unwrap();
        assert!(receipt.recheck(deadline).is_err());
    }

    #[test]
    fn p1_config_read_receipt_rejects_new_higher_priority_files() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let path = root.join("pakro.toml");
        std::fs::write(&path, "[tools]\npython='3.15.0'\n").unwrap();
        let mut discovery =
            FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
        discovery
            .ceiling_paths
            .push(root.parent().unwrap().to_owned());
        let deadline = Instant::now() + Duration::from_secs(10);
        let receipt = ConfigReadReceipt::capture(
            &root,
            discovery.clone(),
            std::slice::from_ref(&path),
            deadline,
            deadline,
        )
        .unwrap();
        std::fs::write(root.join("pakro.local.toml"), "[tools]\npython='3.14.0'\n").unwrap();
        assert!(receipt.recheck(deadline).is_err());
        // Also reject a file created after legacy discovery but before the
        // receipt capture, instead of accepting the stale legacy fileset.
        assert!(ConfigReadReceipt::capture(&root, discovery, &[path], deadline, deadline).is_err());
    }

    #[test]
    fn p1_config_read_receipt_keeps_legacy_inputs_without_python_parsing() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let path = root.join(".nvmrc");
        std::fs::write(&path, "v22.0.0\n").unwrap();
        let mut discovery = FrozenConfigDiscovery::new(vec![".nvmrc".into()]);
        discovery
            .ceiling_paths
            .push(root.parent().unwrap().to_owned());
        let deadline = Instant::now() + Duration::from_secs(10);
        let receipt = ConfigReadReceipt::capture(
            &root,
            discovery,
            std::slice::from_ref(&path),
            deadline,
            deadline,
        )
        .unwrap();
        receipt.recheck(deadline).unwrap();
        std::fs::write(&path, "v23.0.0\n").unwrap();
        assert!(receipt.recheck(deadline).is_err());
    }

    #[test]
    fn p1_frozen_alias_backends_match_existing_cross_file_override_order() {
        use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
        use crate::programs::query::snapshot::python_tool_name;
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().canonicalize().unwrap();
        let child = parent.join("child");
        std::fs::create_dir(&child).unwrap();
        for (near, far) in [("core:python", "asdf:node"), ("asdf:node", "core:python")] {
            let near_path = child.join("pakro.toml");
            let far_path = parent.join("pakro.toml");
            let near_body = format!("[tool_alias.py]\nbackend='{near}'\n[tools]\npy='3.15.0'\n");
            let far_body = format!("[tool_alias.py]\nbackend='{far}'\n");
            std::fs::write(&near_path, &near_body).unwrap();
            std::fs::write(&far_path, &far_body).unwrap();
            let mut config_files = ConfigMap::new();
            config_files.insert(
                near_path.clone(),
                Arc::new(PakroToml::from_str(&near_body, &near_path).unwrap()),
            );
            config_files.insert(
                far_path.clone(),
                Arc::new(PakroToml::from_str(&far_body, &far_path).unwrap()),
            );
            let loaded = load_aliases(&config_files).unwrap();
            assert_eq!(loaded["py"].backend.as_deref(), Some(far));
            let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
            discovery
                .ceiling_paths
                .push(parent.parent().unwrap().to_owned());
            let observed = observe_project(
                &child,
                ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(10)),
            )
            .unwrap();
            assert_eq!(
                python_tool_name(&observed, "py"),
                loaded["py"].backend.as_deref() == Some("core:python")
            );
        }
    }

    #[test]
    fn p1_discovery_literal_global_and_system_files_escape_glob_syntax() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        let global = temporary.path().join("global");
        let system = temporary.path().join("system");
        for directory in [&root, &global, &system] {
            std::fs::create_dir(directory).unwrap();
        }
        let global_file = global.join("config[dev].toml");
        let system_file = system.join("settings?.toml");
        for path in [
            &global_file,
            &global.join("configd.toml"),
            &system_file,
            &system.join("settings1.toml"),
        ] {
            std::fs::write(path, "").unwrap();
        }
        let mut options = FrozenConfigDiscovery::new(vec![]);
        options.ceiling_paths.push(root.clone());
        options.global_candidates =
            FrozenConfigDiscovery::external_candidates(&global, Some(&global_file), &[], false);
        options.system_candidates =
            FrozenConfigDiscovery::external_candidates(&system, Some(&system_file), &[], false);
        assert_eq!(
            discover(&root, &options).files,
            vec![global_file, system_file]
        );
    }

    #[test]
    fn p1_discovery_external_profiles_escape_glob_syntax() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        let external = temporary.path().join("external");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir_all(external.join("conf.d/fragment")).unwrap();
        let expected = [
            "config.dev[one].toml",
            "pakro.dev[one].local.toml",
            "conf.d/extras.dev[one].toml",
            "conf.d/fragment/pakro.dev[one].toml",
        ]
        .map(|name| external.join(name));
        for path in &expected {
            std::fs::write(path, "").unwrap();
        }
        for decoy in [
            "config.devo.toml",
            "pakro.devo.local.toml",
            "conf.d/extras.devo.toml",
            "conf.d/fragment/pakro.devo.toml",
        ] {
            std::fs::write(external.join(decoy), "").unwrap();
        }
        for system in [false, true] {
            let mut options = FrozenConfigDiscovery::new(vec![]);
            options.ceiling_paths.push(root.clone());
            options.environments = vec!["dev[one]".into()];
            options.env_conf_d = true;
            let candidates = FrozenConfigDiscovery::external_candidates(
                &external,
                None,
                &options.environments,
                true,
            );
            if system {
                options.system_candidates = candidates;
            } else {
                options.global_candidates = candidates;
            }
            let actual = discover(&root, &options)
                .files
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(actual, expected.iter().cloned().collect());
        }
    }

    #[test]
    fn r03_discovery_explicit_builder_freezes_custom_format_patterns() {
        let options =
            FrozenConfigDiscovery::from_explicit_options("runtime-pins", "Pakrofile", None, &[]);
        assert!(
            options
                .local_patterns
                .iter()
                .any(|pattern| pattern == "Pakrofile")
        );
        assert!(
            options
                .local_patterns
                .iter()
                .any(|pattern| pattern == "runtime-pins")
        );
        assert_eq!(
            options
                .format_for(Path::new("/project/runtime-pins"))
                .unwrap(),
            config_file::ConfigDocumentFormat::ToolVersions
        );
        assert_eq!(
            options.format_for(Path::new("/project/Pakrofile")).unwrap(),
            config_file::ConfigDocumentFormat::PakroToml
        );
        let options = FrozenConfigDiscovery::from_explicit_options(
            ".tool-versions",
            "pakro.toml",
            Some(&["config/*.versions".to_owned()]),
            &["project.native".to_owned()],
        );
        assert_eq!(
            options
                .format_for(Path::new("/project/config/lang.versions"))
                .unwrap(),
            config_file::ConfigDocumentFormat::ToolVersions
        );
        assert_eq!(
            options
                .format_for(Path::new("/project/.tool-versions"))
                .unwrap(),
            config_file::ConfigDocumentFormat::PakroToml
        );
    }

    #[test]
    fn r03_discovery_freezes_priority_and_rediscovers_literal_and_glob_additions() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        std::fs::create_dir_all(root.join(".pakro/conf.d")).unwrap();
        std::fs::write(root.join("pakro.toml"), "").unwrap();
        std::fs::write(root.join(".pakro/conf.d/a.toml"), "").unwrap();
        let mut options = FrozenConfigDiscovery::new(vec![
            ".pakro/conf.d/*.toml".into(),
            "pakro.toml".into(),
            "pakro.local.toml".into(),
        ]);
        options.ceiling_paths.push(temporary.path().to_path_buf());
        let before = discover(&root, &options);
        assert_eq!(
            before.files,
            vec![root.join("pakro.toml"), root.join(".pakro/conf.d/a.toml")]
        );
        assert!(
            before
                .missing_candidates
                .contains(&root.join("pakro.local.toml"))
        );
        std::fs::write(root.join("pakro.local.toml"), "").unwrap();
        std::fs::write(root.join(".pakro/conf.d/z.toml"), "").unwrap();
        let after = discover(&root, &options);
        assert_ne!(before, after);
        assert_eq!(after.files[0], root.join("pakro.local.toml"));
        assert!(
            after
                .glob_scans
                .iter()
                .any(|scan| scan.matches.contains(&root.join(".pakro/conf.d/z.toml")))
        );
    }

    #[test]
    fn r03_discovery_orders_local_ancestors_globals_and_explicit_profiles() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        std::fs::create_dir(&root).unwrap();
        for path in [
            root.join("pakro.toml"),
            root.join("pakro.dev.toml"),
            temporary.path().join("pakro.toml"),
            temporary.path().join("global.toml"),
            temporary.path().join("system.toml"),
        ] {
            std::fs::write(path, "").unwrap();
        }
        let mut options = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
        options
            .ceiling_paths
            .push(temporary.path().parent().unwrap().to_path_buf());
        options.environments.push("dev".into());
        options.global_candidates.push(ConfigDiscoveryCandidate {
            directory: temporary.path().to_path_buf(),
            pattern: "global.toml".into(),
        });
        options.system_candidates.push(ConfigDiscoveryCandidate {
            directory: temporary.path().to_path_buf(),
            pattern: "system.toml".into(),
        });
        assert_eq!(
            discover(&root, &options).files,
            vec![
                root.join("pakro.dev.toml"),
                root.join("pakro.toml"),
                temporary.path().join("pakro.toml"),
                temporary.path().join("global.toml"),
                temporary.path().join("system.toml")
            ]
        );
    }

    #[test]
    fn r03_discovery_default_patterns_and_explicit_filters_match_legacy_order() {
        let patterns = FrozenConfigDiscovery::default_local_patterns(
            ".tool-versions",
            "pakro.toml",
            None,
            &[],
        );
        assert_eq!(patterns.first().map(String::as_str), Some(".tool-versions"));
        assert_eq!(
            patterns.last().map(String::as_str),
            Some(".pakro.local.toml")
        );
        assert!(
            patterns
                .iter()
                .position(|path| path == ".pakro/conf.d/*.toml")
                < patterns.iter().position(|path| path == "pakro.toml")
        );
        let temporary = tempfile::tempdir().unwrap();
        std::fs::write(temporary.path().join("pakro.toml"), "").unwrap();
        let mut options = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
        options.ignored_paths.push(temporary.path().to_path_buf());
        options
            .ceiling_paths
            .push(temporary.path().parent().unwrap().to_path_buf());
        assert!(discover(temporary.path(), &options).files.is_empty());
        options.no_config = true;
        assert_eq!(
            discover(temporary.path(), &options),
            DiscoveredConfigInputs::default()
        );
    }

    #[test]
    fn r03_discovery_reports_invalid_inputs_and_budgets() {
        let temporary = tempfile::tempdir().unwrap();
        let mut options = FrozenConfigDiscovery::new(vec!["[".into()]);
        options
            .ceiling_paths
            .push(temporary.path().parent().unwrap().to_path_buf());
        let deadline = Instant::now() + Duration::from_secs(10);
        assert_eq!(
            discover_project_configs(temporary.path(), &options, Default::default(), deadline)
                .unwrap_err(),
            ConfigDiscoveryError::InvalidPattern
        );
        options.local_patterns = vec!["pakro.toml".into()];
        let limits = ConfigDiscoveryLimits {
            max_candidates: 0,
            ..Default::default()
        };
        assert_eq!(
            discover_project_configs(temporary.path(), &options, limits, deadline).unwrap_err(),
            ConfigDiscoveryError::Budget
        );
        assert_eq!(
            discover_project_configs(
                Path::new("relative"),
                &options,
                Default::default(),
                deadline
            )
            .unwrap_err(),
            ConfigDiscoveryError::RelativePath
        );
        assert_eq!(
            discover_project_configs(
                temporary.path(),
                &options,
                Default::default(),
                Instant::now()
            )
            .unwrap_err(),
            ConfigDiscoveryError::Expired
        );
    }
}

type AliasMap = IndexMap<String, Alias>;
pub type ConfigMap = IndexMap<PathBuf, Arc<dyn ConfigFile>>;
pub(crate) type EnvWithSources = IndexMap<String, (String, PathBuf)>;
pub struct Config {
    pub config_files: ConfigMap,

    pub project_root: Option<PathBuf>,
    pub all_aliases: AliasMap,
    pub repo_urls: HashMap<String, String>,
    pub vars: IndexMap<String, String>,
    pub tera_ctx: tera::Context,
    pub shorthands: Shorthands,
    pub shell_aliases: EnvWithSources,
    /// Files accessed by tera template functions (read_file, hash_file, etc.)
    /// during shell alias template rendering, used to watch for changes in hook-env.
    pub tera_files: Vec<PathBuf>,
    aliases: AliasMap,
    env: OnceCell<EnvResults>,
    env_with_sources: OnceCell<EnvWithSources>,
    hooks: OnceCell<Vec<(PathBuf, Option<PathBuf>, Hook)>>,

    tool_request_set: OnceCell<ToolRequestSet>,
    toolset: OnceCell<Toolset>,
    vars_results: OnceCell<EnvResults>,
    /// The lockfile paths this config resolves to (which key the shared
    /// lockfile cache) plus the monorepo discovery behind them. Deriving them
    /// walks every config path and dedups the results, and backend-identity
    /// lookups ask for them hundreds of times per invocation, so derive once
    /// per config.
    lockfile_discovery: std::sync::OnceLock<Arc<crate::lockfile::LockfileDiscovery>>,
    readonly_receipt: Option<Arc<ConfigReadReceipt>>,
}

/// Raw configuration input receipt. It reuses existing readonly file/path
/// owners and discovery; it does not parse Python or grant mutation authority.
struct ConfigReadReceipt {
    root: PathBuf,
    discovery: FrozenConfigDiscovery,
    discovered: DiscoveredConfigInputs,
    root_owner: crate::lockfile::transaction::ReadOnlyPathObservation,
    files: Vec<crate::lockfile::transaction::ReadOnlyFileObservation>,
    observation_deadline: std::time::Instant,
    query_deadline: std::time::Instant,
}
impl ConfigReadReceipt {
    fn capture(
        root: &Path,
        discovery: FrozenConfigDiscovery,
        legacy_paths: &[PathBuf],
        observation_deadline: std::time::Instant,
        query_deadline: std::time::Instant,
    ) -> Result<Self> {
        use crate::lockfile::transaction::{ReadOnlyFileObservation, ReadOnlyPathObservation};
        let discovered =
            discover_project_configs(root, &discovery, Default::default(), observation_deadline)?;
        // The legacy parser and the shared observation must consume the same
        // files. Alias spellings may differ, while their real source is equal.
        let identities = |paths: &[PathBuf]| -> Result<BTreeSet<PathBuf>> {
            paths
                .iter()
                .map(|path| path.canonicalize().map_err(Into::into))
                .collect()
        };
        eyre::ensure!(
            identities(&discovered.files)? == identities(legacy_paths)?,
            "configuration discovery changed between reads; observe the project again"
        );
        let paths: BTreeSet<_> = discovered
            .files
            .iter()
            .chain(legacy_paths)
            .cloned()
            .collect();
        eyre::ensure!(
            paths.len() <= 1024,
            "configuration observation file budget exceeded"
        );
        let mut files = Vec::new();
        let mut bytes = 0usize;
        for path in paths {
            let owner =
                ReadOnlyFileObservation::open(&path, Default::default(), observation_deadline)?;
            bytes = bytes
                .checked_add(owner.bytes().len())
                .ok_or_else(|| eyre!("configuration observation byte budget exceeded"))?;
            eyre::ensure!(
                bytes <= 16 * 1024 * 1024,
                "configuration observation byte budget exceeded"
            );
            files.push(owner);
        }
        let root_owner =
            ReadOnlyPathObservation::open(root, Default::default(), observation_deadline)?;
        eyre::ensure!(
            root_owner.exists(),
            "configuration observation root is unavailable"
        );
        let receipt = Self {
            root: root.to_owned(),
            discovery,
            discovered,
            root_owner,
            files,
            observation_deadline,
            query_deadline,
        };
        receipt.recheck(observation_deadline)?;
        Ok(receipt)
    }
    fn recheck(&self, deadline: std::time::Instant) -> Result<()> {
        let deadline = deadline.min(self.observation_deadline);
        eyre::ensure!(
            std::time::Instant::now() < deadline,
            "configuration observation deadline expired"
        );
        self.root_owner.recheck()?;
        let current =
            discover_project_configs(&self.root, &self.discovery, Default::default(), deadline)?;
        eyre::ensure!(
            current == self.discovered,
            "configuration inputs changed; observe the project again"
        );
        for file in &self.files {
            eyre::ensure!(
                std::time::Instant::now() < deadline,
                "configuration observation deadline expired"
            );
            file.recheck()?;
        }
        eyre::ensure!(
            std::time::Instant::now() < deadline,
            "configuration observation deadline expired"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Alias {
    pub backend: Option<String>,
    pub versions: IndexMap<String, String>,
}

static _CONFIG: RwLock<Option<Arc<Config>>> = RwLock::new(None);
use pakro_util::redactions::GLOBAL_REDACTOR as _REDACTOR;
const MONOREPO_LOCKFILE_WARN_AT: &str = "2026.12.0";
const MONOREPO_LOCKFILE_DEFAULT_AT: &str = "2027.6.0";

pub(crate) fn is_loaded() -> bool {
    _CONFIG.read().unwrap().is_some()
}

impl Config {
    /// Whether invocation-wide locked mode applies to a tool's config scope.
    /// Sources outside configuration files remain locked because they have no
    /// scope that can be excluded.
    pub(crate) fn invocation_locked_for(
        &self,
        source: &ToolSource,
        invocation_locked: bool,
    ) -> bool {
        if !invocation_locked {
            return false;
        }
        let Some(path) = source.path() else {
            return true;
        };
        let scope = match provenance::ConfigProvenance::from_path(path).scope() {
            provenance::ConfigFileScope::System => "system",
            provenance::ConfigFileScope::User => "global",
            provenance::ConfigFileScope::Project => "project",
        };
        Settings::get().locked_scopes.contains(scope)
    }

    /// Whether the config root that owns this tool request opted it into strict
    /// lockfile enforcement. Non-config sources remain governed only by the
    /// invocation-wide `locked` setting/flag.
    pub(crate) fn tool_config_locked(&self, source: &ToolSource) -> bool {
        let ToolSource::PakroToml(path) = source else {
            return false;
        };
        let Some(root) = self.config_files.get(path).map(|cf| cf.config_root()) else {
            return false;
        };
        // The global config root defaults to $HOME, which can also be the
        // config_root of a local config stored directly in $HOME. Keep those
        // conceptual roots separate even when their path values coincide.
        let is_global = is_global_config(path);
        self.config_files.values().any(|cf| {
            is_global_config(cf.get_path()) == is_global
                && cf.config_root() == root
                && cf.tool_config().locked
        })
    }

    pub async fn get() -> Result<Arc<Self>> {
        if let Some(config) = &*_CONFIG.read().unwrap() {
            return Ok(config.clone());
        }
        measure!("load config", { Self::load().await })
    }
    /// Validate the original source receipt without exposing its file owners.
    /// Query consumers reuse its original 300-second boundary; installation
    /// keeps its own HTTP operation budget.
    pub(crate) fn recheck_readonly_inputs(&self, deadline: std::time::Instant) -> Result<()> {
        if let Some(receipt) = &self.readonly_receipt {
            receipt.recheck(deadline)?;
        }
        Ok(())
    }
    pub(crate) fn readonly_query_deadline(&self) -> Option<std::time::Instant> {
        self.readonly_receipt
            .as_ref()
            .map(|receipt| receipt.query_deadline)
    }
    pub(crate) fn maybe_get() -> Option<Arc<Self>> {
        _CONFIG.read().unwrap().as_ref().cloned()
    }
    pub(crate) fn get_() -> Arc<Self> {
        (*_CONFIG.read().unwrap()).clone().unwrap()
    }
    pub async fn reset() -> Result<Arc<Self>> {
        backend::reset().await?;
        timeout::run_with_timeout_async(
            async || {
                _CONFIG.write().unwrap().take();
                *GLOBAL_CONFIG_FILES.lock().unwrap() = None;
                *SYSTEM_CONFIG_FILES.lock().unwrap() = None;
                // Global-config membership follows symlinks, which may have
                // been retargeted since the last load.
                file::clear_desymlink_cache();
                GLOB_RESULTS.lock().unwrap().clear();
                crate::lockfile::invalidate_caches();
                Ok(())
            },
            Duration::from_secs(5),
        )
        .await?;
        Settings::reload()?;
        Config::load().await
    }

    pub fn with_config_files(&self, config_files: ConfigMap) -> Result<Arc<Self>> {
        let project_root = get_project_root(&config_files).or_else(|| self.project_root.clone());
        let repo_urls = load_plugins(&config_files)?;
        Ok(Arc::new(Self {
            tera_ctx: self.tera_ctx.clone(),
            config_files,

            env: OnceCell::new(),
            env_with_sources: OnceCell::new(),
            shorthands: self.shorthands.clone(),
            hooks: OnceCell::new(),

            tool_request_set: OnceCell::new(),
            toolset: OnceCell::new(),
            all_aliases: self.all_aliases.clone(),
            aliases: self.aliases.clone(),
            project_root,
            repo_urls,
            shell_aliases: self.shell_aliases.clone(),
            tera_files: self.tera_files.clone(),
            vars: self.vars.clone(),
            vars_results: OnceCell::new(),
            lockfile_discovery: Default::default(),
            readonly_receipt: self.readonly_receipt.clone(),
        }))
    }

    /// Lockfile paths and monorepo discovery this config resolves to, derived
    /// once. See [`Self::lockfile_discovery`]'s field docs.
    pub(crate) fn lockfile_discovery(
        &self,
        derive: impl FnOnce() -> crate::lockfile::LockfileDiscovery,
    ) -> Arc<crate::lockfile::LockfileDiscovery> {
        self.lockfile_discovery
            .get_or_init(|| Arc::new(derive()))
            .clone()
    }

    pub fn with_tool_request_set(&self, tool_request_set: ToolRequestSet) -> Result<Arc<Self>> {
        let config = self.with_config_files(self.config_files.clone())?;
        config.tool_request_set.set(tool_request_set).unwrap();
        Ok(config)
    }

    #[async_backtrace::framed]
    pub async fn load() -> Result<Arc<Self>> {
        let started = std::time::Instant::now();
        backend::load_tools().await?;
        let idiomatic_files = measure!("config::load idiomatic_files", {
            load_idiomatic_filenames().await
        });
        let config_filenames = idiomatic_files
            .keys()
            .chain(DEFAULT_CONFIG_FILENAMES.iter())
            .cloned()
            .collect_vec();
        let config_paths = measure!("config::load config_paths", {
            load_config_paths(&config_filenames, false)
        });
        trace!("config_paths: {config_paths:?}");
        let readonly_receipt = if cfg!(unix) && readonly_invocation() {
            let mut discovery = FrozenConfigDiscovery::from_process();
            // Idiomatic non-Python files still use their original parser; the
            // receipt only keeps their bytes and the original discovered set.
            discovery
                .local_patterns
                .splice(0..0, idiomatic_files.keys().cloned());
            let query_deadline = started + Duration::from_secs(300);
            let observation_deadline = started
                + Settings::get()
                    .http_download_timeout()
                    .max(Duration::from_secs(300));
            Some(Arc::new(ConfigReadReceipt::capture(
                &std::env::current_dir()?,
                discovery,
                &config_paths,
                observation_deadline,
                query_deadline,
            )?))
        } else {
            None
        };
        let config_files = measure!("config::load config_files", {
            load_all_config_files(&config_paths, &idiomatic_files).await?
        });

        let mut config = Self::load_from_config_files(config_files, false).await?;
        if let Some(receipt) = &readonly_receipt {
            receipt.recheck(receipt.observation_deadline)?;
        }
        Arc::get_mut(&mut config)
            .ok_or_else(|| eyre!("new configuration unexpectedly shared before publication"))?
            .readonly_receipt = readonly_receipt;
        *_CONFIG.write().unwrap() = Some(config.clone());
        Ok(config)
    }

    pub(crate) async fn load_from_config_files(
        config_files: ConfigMap,
        global_only: bool,
    ) -> Result<Arc<Self>> {
        let mut config = Self {
            tera_ctx: BASE_CONTEXT.clone(),
            config_files,

            env: OnceCell::new(),
            env_with_sources: OnceCell::new(),
            shorthands: get_shorthands(&Settings::get()),
            hooks: OnceCell::new(),

            tool_request_set: OnceCell::new(),
            toolset: OnceCell::new(),
            all_aliases: Default::default(),
            aliases: Default::default(),
            project_root: Default::default(),
            repo_urls: Default::default(),
            shell_aliases: Default::default(),
            tera_files: Default::default(),
            vars: Default::default(),
            vars_results: OnceCell::new(),
            lockfile_discovery: Default::default(),
            readonly_receipt: None,
        };
        let vars_config = Arc::new(Self {
            tera_ctx: config.tera_ctx.clone(),
            config_files: config.config_files.clone(),

            env: OnceCell::new(),
            env_with_sources: OnceCell::new(),
            shorthands: config.shorthands.clone(),
            hooks: OnceCell::new(),

            tool_request_set: OnceCell::new(),
            toolset: OnceCell::new(),
            all_aliases: config.all_aliases.clone(),
            aliases: config.aliases.clone(),
            project_root: config.project_root.clone(),
            repo_urls: config.repo_urls.clone(),
            shell_aliases: config.shell_aliases.clone(),
            tera_files: config.tera_files.clone(),
            vars: config.vars.clone(),
            vars_results: OnceCell::new(),
            lockfile_discovery: Default::default(),
            readonly_receipt: None,
        });
        let vars_results = measure!("config::load vars_results", {
            let results = load_vars(&vars_config).await?;
            config.vars_results.set(results.clone()).ok();
            results
        });
        let vars: IndexMap<String, String> = vars_results
            .vars
            .iter()
            .map(|(k, (v, _))| (k.clone(), v.clone()))
            .collect();
        config.tera_ctx.insert("vars", &vars);

        config.vars = vars;
        config.aliases = load_aliases(&config.config_files)?;
        // Clear any previously tracked files before loading shell aliases
        let _ = take_tera_accessed_files();
        config.shell_aliases = load_shell_aliases(&config.config_files)?;
        config.tera_files = take_tera_accessed_files();
        config.project_root = get_project_root(&config.config_files);
        config.repo_urls = load_plugins(&config.config_files)?;
        measure!("config::load validate", {
            config.validate()?;
        });

        config.all_aliases = measure!("config::load all_aliases", { config.load_all_aliases() });

        measure!("config::load redactions", {
            config.add_redactions_excluding(
                config
                    .redaction_keys()
                    .into_iter()
                    .chain(vars_results.redactions.iter().cloned()),
                &config.vars.clone().into_iter().collect(),
                &vars_results.redaction_exclusions,
            );
        });

        if log::log_enabled!(log::Level::Trace) {
            trace!("config: {config:#?}");
        } else if log::log_enabled!(log::Level::Debug) {
            for p in config.config_files.keys() {
                debug!("config: {}", display_path(p));
            }
        }

        if !global_only {
            warn_if_auto_env_files_exist();
            warn_if_monorepo_lockfile_default_changes(&config);
        }

        time!("load done");

        measure!("config::load install_state", {
            for plugin in config.repo_urls.keys() {
                let (plugin_type, plugin) = PluginType::from_plugin_config(plugin)?;
                install_state::add_plugin(plugin, plugin_type).await?;
            }
        });

        measure!("config::load remove_aliased_tools", {
            for short in config
                .all_aliases
                .iter()
                .filter(|(_, a)| a.backend.is_some())
                .map(|(s, _)| s)
                .chain(config.repo_urls.keys())
            {
                // we need to remove aliased tools so they get re-added with updated "full" values
                backend::remove(short);
            }
        });

        let config = Arc::new(config);
        config.env_results().await?;
        Ok(config)
    }

    pub(crate) fn env_maybe(&self) -> Option<IndexMap<String, String>> {
        self.env_with_sources.get().map(|env| {
            env.iter()
                .map(|(k, (v, _))| (k.clone(), v.clone()))
                .collect()
        })
    }

    pub async fn env(self: &Arc<Self>) -> eyre::Result<IndexMap<String, String>> {
        Ok(self
            .env_with_sources()
            .await?
            .iter()
            .map(|(k, (v, _))| (k.clone(), v.clone()))
            .collect())
    }
    /// The tools-independent config env resolved now, bypassing every cache.
    ///
    /// [`Self::env`] memoizes into a process-wide `OnceCell`, which is right for
    /// a command that resolves the env once and then runs. It is wrong for a
    /// tool's `postinstall` hook: hooks run one after another inside a single
    /// install batch, an `[env]` value can read a file or command output that an
    /// earlier hook just changed, and the memo would hand every later hook the
    /// first one's snapshot. Which hook fills that memo is not even fixed —
    /// installs run in parallel, so it comes down to a race.
    ///
    /// So a hook resolves its own env and leaves the shared caches alone: it
    /// neither reads nor writes the memo or the on-disk `CachedNonToolEnv`, and
    /// a hook that changes an env input is visible to the hooks ordered after
    /// it. The cost is one resolution per tool that declares a hook.
    pub(crate) async fn env_uncached(self: &Arc<Self>) -> eyre::Result<IndexMap<String, String>> {
        Ok(self
            .load_env(false)
            .await?
            .env
            .into_iter()
            .map(|(k, (v, _))| (k, v))
            .collect())
    }

    pub async fn env_with_sources(self: &Arc<Self>) -> eyre::Result<&EnvWithSources> {
        self.env_with_sources
            .get_or_try_init(async || Ok(self.env_results().await?.env.clone()))
            .await
    }
    pub async fn env_results(self: &Arc<Self>) -> Result<&EnvResults> {
        self.env
            .get_or_try_init(|| async { self.load_env(true).await })
            .await
    }

    pub(crate) fn env_results_cached(&self) -> Option<&EnvResults> {
        self.env.get()
    }
    pub(crate) fn vars_results_cached(&self) -> Option<&EnvResults> {
        self.vars_results.get()
    }
    pub async fn path_dirs(self: &Arc<Self>) -> eyre::Result<&Vec<PathBuf>> {
        Ok(&self.env_results().await?.env_paths)
    }

    pub async fn get_tool_request_set(self: &Arc<Self>) -> eyre::Result<&ToolRequestSet> {
        self.tool_request_set
            .get_or_try_init(async || ToolRequestSetBuilder::new().build(self).await)
            .await
    }

    pub async fn get_toolset(self: &Arc<Self>) -> Result<&Toolset> {
        self.get_toolset_with_opts(&ResolveOptions::default()).await
    }

    pub async fn get_toolset_with_opts(
        self: &Arc<Self>,
        opts: &ResolveOptions,
    ) -> Result<&Toolset> {
        let opts = opts.clone();
        self.toolset
            .get_or_try_init(|| async {
                let mut ts = Toolset::from(self.get_tool_request_set().await?.clone());
                ts.resolve_with_opts(self, &opts).await?;
                Ok(ts)
            })
            .await
    }

    fn has_tool_alias(&self, short: &str) -> bool {
        self.all_aliases
            .get(short)
            .is_some_and(|alias| alias.backend.is_some())
            || self.repo_urls.contains_key(short)
    }

    pub(crate) async fn get_tool_opts_with_overrides(
        self: &Arc<Self>,
        backend_arg: &Arc<BackendArg>,
    ) -> Result<ToolOptions> {
        Ok(self
            .resolve_tool_opts_with_overrides(backend_arg)
            .await?
            .into_effective())
    }

    pub(crate) async fn resolve_tool_opts_with_overrides(
        self: &Arc<Self>,
        backend_arg: &Arc<BackendArg>,
    ) -> Result<ResolvedToolOptions> {
        let trs = self.get_tool_request_set().await?;
        let short_match = trs.iter().find(|tr| tr.0.short == backend_arg.short);
        let tool_request = short_match.or_else(|| {
            if !self.has_tool_alias(&backend_arg.short) {
                return None;
            }

            let resolved_ba = BackendArg::new(backend_arg.full(), None);
            trs.iter().find(|tr| tr.0.short == resolved_ba.short)
        });
        let config_opts = tool_request.and_then(|tr| tr.1.first().map(|req| req.options()));
        let alias_opts = self.get_backend_alias_opts(backend_arg);
        Ok(backend_arg.resolve_opts_with_layers(alias_opts, config_opts, None))
    }

    fn get_backend_alias_opts(&self, backend_arg: &BackendArg) -> Option<ToolVersionOptions> {
        if backend_arg.has_env_backend_override() {
            return None;
        }
        let short = backend::unalias_backend(&backend_arg.short);
        let short = short.as_ref();
        self.all_aliases
            .get(short)
            .and_then(|alias| alias.backend.as_deref())
            .and_then(|backend| split_bracketed_opts(backend).map(|(_, opts)| opts))
            .map(crate::toolset::parse_tool_options)
    }

    /// The source a `[plugins]` entry names for `plugin_name`, if any.
    ///
    /// Unlike [`Self::get_repo_url`], this ignores registry shorthands, so it
    /// only returns a source the user configured explicitly.
    pub fn configured_plugin_url(&self, plugin_name: &str) -> Option<String> {
        let url = self.repo_urls.get(plugin_name).or_else(|| {
            self.repo_urls
                .iter()
                .find(|(key, _)| {
                    key.split_once(':')
                        .is_some_and(|(_, name)| name == plugin_name)
                })
                .map(|(_, url)| url)
        })?;
        Some(
            if Path::new(url).is_absolute() || url.starts_with("file://") {
                url.clone()
            } else {
                plugin_entry_to_url(url)
            },
        )
    }

    pub fn get_repo_url(&self, plugin_name: &str) -> Option<String> {
        if let Some(url) = self.repo_urls.get(plugin_name)
            && (Path::new(url).is_absolute() || url.starts_with("file://"))
        {
            return Some(url.clone());
        }

        let plugin_name = self
            .all_aliases
            .get(plugin_name)
            .and_then(|a| a.backend.clone())
            .or_else(|| self.repo_urls.get(plugin_name).cloned())
            .unwrap_or(plugin_name.to_string());
        let plugin_name = plugin_name.strip_prefix("asdf:").unwrap_or(&plugin_name);

        if let Some(url) = self
            .repo_urls
            .keys()
            .find(|k| k.ends_with(&format!(":{plugin_name}")))
            .and_then(|k| self.repo_urls.get(k))
        {
            return Some(
                if Path::new(url).is_absolute() || url.starts_with("file://") {
                    url.clone()
                } else {
                    plugin_entry_to_url(url)
                },
            );
        }

        self.shorthands
            .get(plugin_name)
            .map(|full| registry::full_to_url(&full[0]))
            .or_else(|| {
                if registry::url_like(plugin_name) || plugin_name.split('/').count() == 2 {
                    Some(plugin_entry_to_url(plugin_name))
                } else {
                    None
                }
            })
    }

    pub(crate) fn is_monorepo(&self) -> bool {
        find_monorepo_root(&self.config_files).is_some()
    }

    pub fn monorepo_root(&self) -> Option<PathBuf> {
        find_monorepo_root(&self.config_files)
    }

    pub(crate) fn monorepo_lockfile_discovery_key(
        &self,
    ) -> Option<(PathBuf, Option<bool>, Vec<String>)> {
        let config = find_monorepo_config(&self.config_files)?;
        Some((
            config.monorepo_source.get_path().to_path_buf(),
            config.monorepo.lockfile,
            config.monorepo.config_roots.unwrap_or_default(),
        ))
    }

    /// Returns the root lockfile directory when unified monorepo lockfiles are active.
    ///
    /// Lockfile discovery is intentionally lenient: it only requires
    /// `[monorepo].config_roots` to match directories, because legacy lockfiles
    /// can exist in roots whose live config is idiomatic-only or was removed.
    pub fn monorepo_lockfile_root(&self) -> Option<PathBuf> {
        monorepo_lockfile_root_for(&self.config_files)
    }

    /// Returns true when lockfile creation is enabled by a TOML settings file.
    ///
    /// `PAKRO_LOCKFILE=1` predates automatic creation and continues to mean
    /// "read and maintain existing lockfiles" for backwards compatibility.
    pub fn lockfile_creation_enabled(&self) -> bool {
        Settings::get().lockfile_creation_enabled()
            && self.config_files.values().any(|cf| {
                cf.settings()
                    .is_some_and(|settings| settings.lockfile == Some(true))
            })
    }

    pub(crate) fn monorepo_config_root_dirs_for_lockfiles(&self) -> Result<Vec<PathBuf>> {
        self.monorepo_config_root_dirs(None)
    }

    /// Resolve `[monorepo].config_roots`.
    ///
    /// `None` matches any existing directory and is used for lockfile migration
    /// and routing. `Some(filenames)` requires a recognized config or idiomatic
    /// version file for lockfile discovery.
    pub(crate) fn monorepo_config_root_dirs(
        &self,
        filenames: Option<&[String]>,
    ) -> Result<Vec<PathBuf>> {
        monorepo_config_root_dirs_for(&self.config_files, filenames)
    }

    pub async fn resolve_alias(&self, backend: &ABackend, v: &str) -> Result<String> {
        if let Some(plugin_aliases) = self.all_aliases.get(&backend.ba().short)
            && let Some(alias) = plugin_aliases.versions.get(v)
        {
            return Ok(alias.clone());
        }
        if let Some(alias) = backend.get_aliases()?.get(v) {
            return Ok(alias.clone());
        }
        Ok(v.to_string())
    }

    fn load_all_aliases(&self) -> AliasMap {
        let mut aliases: AliasMap = self.aliases.clone();
        let plugin_aliases: Vec<_> = backend::alias_backends()
            .into_iter()
            .map(|backend| {
                let aliases = backend.get_aliases().unwrap_or_else(|err| {
                    warn!("get_aliases: {err}");
                    BTreeMap::new()
                });
                (backend.ba().clone(), aliases)
            })
            .collect();
        for (ba, plugin_aliases) in plugin_aliases {
            for (from, to) in plugin_aliases {
                aliases
                    .entry(ba.short.to_string())
                    .or_default()
                    .versions
                    .insert(from, to);
            }
        }

        for (short, plugin_aliases) in &self.aliases {
            let alias = aliases.entry(short.clone()).or_default();
            if let Some(full) = &plugin_aliases.backend {
                alias.backend = Some(full.clone());
            }
            for (from, to) in &plugin_aliases.versions {
                alias.versions.insert(from.clone(), to.clone());
            }
        }

        aliases
    }

    pub async fn get_tracked_config_files(&self) -> Result<ConfigMap> {
        config_file::with_global_ignored_config_paths(self.load_tracked_config_files()).await
    }

    async fn load_tracked_config_files(&self) -> Result<ConfigMap> {
        let mut config_files: ConfigMap = ConfigMap::default();
        let mut idiomatic_settings_by_root =
            BTreeMap::<PathBuf, settings::IdiomaticVersionFileSettings>::new();
        let require_trust_before_detection = Settings::get().paranoid && !Settings::safe_mode();
        for path in Tracker::list_all()?.into_iter() {
            if config_path_is_ignored(&path, false) {
                debug!("skipping ignored tracked config: {}", display_path(&path));
                continue;
            }
            let trust_root = config_file::config_trust_root(&path);
            // Paranoid mode must not inspect an untrusted owner's settings before
            // classification. Safe mode intentionally loads configs inertly without trust.
            if require_trust_before_detection
                && !is_global_config(&path)
                && !config_file::is_trusted(&trust_root)
            {
                debug!("skipping untrusted tracked config: {}", display_path(&path));
                continue;
            }
            let detection = match config_file::detect_config_file_type_by_filename(&path) {
                Some(config_type) => config_file::ConfigFileDetection::Recognized(config_type),
                None => {
                    let config_root = config_file::config_root::config_root(&path);
                    let idiomatic_settings = match idiomatic_settings_by_root.get(&config_root) {
                        Some(settings) => settings.clone(),
                        None => {
                            let settings = settings::IdiomaticVersionFileSettings::resolve_from(
                                &config_root,
                                settings::SettingsLoadPolicy::TRUSTED_HIERARCHY,
                            )?;
                            idiomatic_settings_by_root.insert(config_root, settings.clone());
                            settings
                        }
                    };
                    config_file::detect_config_file_with_settings(&path, &idiomatic_settings).await
                }
            };
            // Pre-check trust for config files that require it so tracked
            // config loading (e.g., during `pakro upgrade`) never prompts.
            // Plain .tool-versions and idiomatic version files are safe to
            // parse without trust and must still protect their tool versions.
            if config_file::detection_requires_trust(&path, &detection)
                && !is_global_config(&path)
                && !config_file::is_trusted(&trust_root)
            {
                debug!("skipping untrusted tracked config: {}", display_path(&path));
                continue;
            }
            if matches!(
                &detection,
                config_file::ConfigFileDetection::DisabledIdiomatic
            ) {
                debug!(
                    "skipping disabled idiomatic tracked config: {}",
                    display_path(&path)
                );
                continue;
            }
            match config_file::parse_detected(&path, detection).await {
                Ok(cf) => {
                    config_files.insert(path, cf);
                }
                Err(err) => {
                    warn!(
                        "error loading tracked config file {}: {err:#}",
                        display_path(&path)
                    );
                }
            }
        }
        Ok(config_files)
    }

    pub fn global_config(&self) -> Result<PakroToml> {
        let settings_path = global_config_path();
        match settings_path.exists() {
            false => {
                trace!("settings does not exist {:?}", settings_path);
                Ok(PakroToml::init(&settings_path))
            }
            true => PakroToml::from_file(&settings_path)
                .wrap_err_with(|| eyre!("Error parsing {}", display_path(&settings_path))),
        }
    }

    fn validate(&self) -> eyre::Result<()> {
        self.validate_versions()?;
        Ok(())
    }

    fn validate_versions(&self) -> eyre::Result<()> {
        for cf in self.config_files.values() {
            if let Some(spec) = cf.min_version() {
                Self::enforce_min_version_spec(spec)?;
            }
        }
        Ok(())
    }

    pub(crate) fn enforce_min_version_spec(spec: &MinVersionSpec) -> eyre::Result<()> {
        let cur = &*version::V;
        if let Some(required) = spec.hard_violation(cur) {
            let min = style::eyellow(required);
            let cur = style::eyellow(cur);
            let msg = format!("pakro version {min} is required, but you are using {cur}");
            bail!(crate::upgrade_hint::append_self_update_instructions(msg));
        } else if let Some(recommended) = spec.soft_violation(cur) {
            let min = style::eyellow(recommended);
            let cur = style::eyellow(cur);
            let msg = format!("pakro version {min} is recommended, but you are using {cur}");
            warn!(
                "{}",
                crate::upgrade_hint::append_self_update_instructions(msg)
            );
        }
        Ok(())
    }

    /// Resolve the tools-independent config env.
    ///
    /// `use_cache` is false for a caller that needs the env as it is *now*
    /// rather than as it was when something else first asked — see
    /// [`Self::env_uncached`]. It suppresses both the process-wide memo (the
    /// caller reaches this directly, not through the `OnceCell`) and the
    /// on-disk `CachedNonToolEnv`, in either direction.
    async fn load_env(self: &Arc<Self>, use_cache: bool) -> Result<EnvResults> {
        if Settings::no_env() || Settings::get().no_env.unwrap_or(false) {
            return Ok(EnvResults::default());
        }
        time!("load_env start");
        // `{{ vars.X }}` renders vars into the env, so a var that is decrypted or redacted
        // (see `EnvResults::has_uncacheable`) taints every env value that may use it. Vars
        // are loaded with the config, so a missing result is not expected; if it is, assume
        // the worst.
        let vars_uncacheable = self
            .vars_results_cached()
            .is_none_or(|vars| vars.has_uncacheable);
        let cache_enabled = use_cache && CachedNonToolEnv::is_enabled() && !vars_uncacheable;
        let cache_key = if cache_enabled {
            let config_files: Vec<(PathBuf, u64)> = self
                .config_files
                .keys()
                .map(|p| (p.clone(), get_file_mtime(p).unwrap_or(0)))
                .collect();
            let settings_hash = compute_settings_hash();
            let base_path = join_paths(env::PATH.iter())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(CachedNonToolEnv::compute_cache_key(
                &config_files,
                &settings_hash,
                &base_path,
            ))
        } else {
            None
        };
        if let Some(cache_key) = cache_key.as_ref()
            && let Some(cached) = CachedNonToolEnv::load(cache_key)?
        {
            let mut env_results = EnvResults {
                env: cached.env.clone(),
                vars: Default::default(),
                env_remove: cached.env_remove.clone(),
                env_files: cached.env_files.clone(),
                env_paths: cached.env_paths.clone(),
                env_scripts: cached.env_scripts.clone(),
                redactions: cached.redactions.clone(),
                redaction_exclusions: cached.redaction_exclusions.clone(),
                caller_env_keys: cached.caller_env_keys.clone(),
                tool_add_paths: Vec::new(),
                watch_files: cached.watch_files.clone(),
                has_uncacheable: false,
            };
            // Keep only explicit wrappers in the non-tool environment. Tool-derived
            // wrappers are selected later, after sourced env and tool templates resolve.
            env_results
                .env_paths
                .retain(|path| path != &*dirs::COMMAND_WRAPPERS);
            if !load_command_wrappers(&self.config_files, std::iter::empty())?.is_empty() {
                env_results
                    .env_paths
                    .insert(0, dirs::COMMAND_WRAPPERS.clone());
            }
            let redact_keys = self
                .redaction_keys()
                .into_iter()
                .chain(env_results.redactions.clone())
                .collect_vec();
            self.add_redactions_excluding(
                redact_keys,
                &env_results.redactable_env(&env::PRISTINE_ENV),
                &env_results.redaction_exclusions,
            );
            if log::log_enabled!(log::Level::Trace) {
                trace!("{env_results:#?}");
            } else if !env_results.is_empty() {
                debug!("{env_results:?}");
            }
            trace!("env_cache: using cached non-tool env results");
            return Ok(env_results);
        }
        let entries: Vec<_> = self
            .config_files
            .iter()
            .rev()
            .map(|(source, cf)| {
                cf.env_entries()
                    .map(|ee| ee.into_iter().map(|e| (e, source.clone())))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        // trace!("load_env: entries: {:#?}", entries);
        let mut env_results = EnvResults::resolve(
            self,
            self.tera_ctx.clone(),
            &env::PRISTINE_ENV,
            entries,
            EnvResolveOptions {
                vars: false,
                tools: ToolsFilter::NonToolsOnly,
                warn_on_missing_required: *env::WARN_ON_MISSING_REQUIRED_ENV,
            },
        )
        .await?;
        if !load_command_wrappers(&self.config_files, std::iter::empty())?.is_empty() {
            env_results
                .env_paths
                .insert(0, dirs::COMMAND_WRAPPERS.clone());
        }
        for env_file in Settings::get().env_files() {
            if env_results.env_files.contains(&env_file) {
                continue;
            }
            debug!("env_file: {}", display_path(&env_file));
            match std::fs::read(&env_file) {
                Ok(bytes) => {
                    env_results.env_files.push(env_file.clone());
                    match file::decode_text(&bytes) {
                        Ok(content) => {
                            // Keep the assignments that precede a syntax error, like a
                            // line-by-line loader would, and name the file cut short.
                            let (items, err) = pakro_dotenv::parse_partial(
                                &content,
                                true,
                                crate::env::vars_safe(),
                            );
                            for (k, v) in items {
                                env_results.env.insert(k, (v, env_file.clone()));
                            }
                            if let Some(err) = err {
                                warn!(
                                    "env_file: stopped reading {}: {err}",
                                    display_path(&env_file)
                                );
                            }
                        }
                        Err(err) => {
                            warn!("env_file: ignoring {}: {err:#}", display_path(&env_file))
                        }
                    }
                }
                Err(err) => trace!("env_file: {err}"),
            }
        }
        let redact_keys = self
            .redaction_keys()
            .into_iter()
            .chain(env_results.redactions.clone())
            .collect_vec();
        self.add_redactions_excluding(
            redact_keys,
            &env_results.redactable_env(&env::PRISTINE_ENV),
            &env_results.redaction_exclusions,
        );
        env_results.has_uncacheable |= vars_uncacheable;
        if cache_enabled
            && !env_results.has_uncacheable
            && let Some(cache_key) = cache_key
        {
            let mut watch_files = env_results.watch_files.clone();
            watch_files.extend(env_results.env_files.clone());
            watch_files.extend(env_results.env_scripts.clone());
            let watch_file_mtimes: Vec<u64> = watch_files
                .iter()
                .map(|p| get_file_mtime(p).unwrap_or(0))
                .collect();
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let cached = CachedNonToolEnv {
                env: env_results.env.clone(),
                env_remove: env_results.env_remove.clone(),
                env_files: env_results.env_files.clone(),
                env_paths: env_results.env_paths.clone(),
                env_scripts: env_results.env_scripts.clone(),
                redactions: env_results.redactions.clone(),
                redaction_exclusions: env_results.redaction_exclusions.clone(),
                caller_env_keys: env_results.caller_env_keys.clone(),
                watch_files,
                watch_file_mtimes,
                created_at: now,
                pakro_version: env!("CARGO_PKG_VERSION").to_string(),
                cache_key_debug: cache_key.clone(),
            };
            if let Err(e) = cached.save(&cache_key) {
                debug!("env_cache: failed to save non-tool env cache: {}", e);
            }
        }
        if log::log_enabled!(log::Level::Trace) {
            trace!("{env_results:#?}");
        } else if !env_results.is_empty() {
            debug!("{env_results:?}");
        }
        Ok(env_results)
    }

    pub(crate) async fn hooks(&self) -> Result<&Vec<(PathBuf, Option<PathBuf>, Hook)>> {
        self.hooks
            .get_or_try_init(|| async {
                self.config_files
                    .values()
                    .map(|cf| {
                        let project_root = cf.project_root();
                        let is_global = project_root.is_none();
                        let config_root = cf.config_root();
                        let mut hooks = cf.hooks()?;
                        if is_global {
                            for h in &mut hooks {
                                h.global = true;
                            }
                        }
                        Ok((config_root, project_root, hooks))
                    })
                    .map_ok(|(config_root, project_root, hooks)| {
                        hooks
                            .into_iter()
                            .map(|h| (config_root.clone(), project_root.clone(), h))
                            .collect::<Vec<_>>()
                    })
                    .flatten_ok()
                    .collect()
            })
            .await
    }

    pub async fn watch_files(self: &Arc<Self>) -> Result<BTreeSet<WatchFilePattern>> {
        let env_results = self.env_results().await?;
        Ok(self
            .config_files
            .iter()
            .map(|(p, _cf)| {
                let mut watch_files: Vec<WatchFilePattern> = vec![p.as_path().into()];
                if let Some(parent) = p.parent() {
                    let lockfile = parent.join("pakro.lock");

                    // Only watch lockfiles that currently exist to prevent missing optional
                    // pakro.lock files from keeping hook-env from stabilizing. If one is created
                    // later, should_exit_early_fast() will notice the parent directory mtime
                    // change, force a slow-path run, and this watch set will then include the new
                    // lockfile on that recomputation.
                    if lockfile.exists() {
                        watch_files.push(lockfile.into());
                    }
                }
                Ok(watch_files)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .chain(env_results.env_files.iter().map(|p| p.as_path().into()))
            .chain(env_results.env_scripts.iter().map(|p| p.as_path().into()))
            .chain(env_results.watch_files.iter().map(|p| p.as_path().into()))
            .chain(
                Settings::get()
                    .env_files()
                    .iter()
                    .map(|p| p.as_path().into()),
            )
            .chain(self.tera_files.iter().map(|p| p.as_path().into()))
            .collect())
    }

    pub fn redaction_keys(&self) -> Vec<String> {
        self.config_files
            .values()
            .flat_map(|cf| cf.redactions().0.iter())
            .cloned()
            .collect()
    }
    /// Register values that must never reach a log line.
    ///
    /// Every log record is redacted through this (see `src/logger.rs`), so a
    /// secret registered here is scrubbed from the terminal, `PAKRO_LOG_FILE`,
    /// and the command lines `CmdLineRunner` debug-logs, without each call site
    /// having to remember.
    ///
    /// Append-only for the same reason as `add_redactions_excluding`: removing a
    /// value could expose a different key that happens to share it.
    pub(crate) fn add_redactions(&self, values: impl IntoIterator<Item = String>) {
        let mut r = _REDACTOR.lock().unwrap();
        *r = r.with_additional(values);
    }

    pub(crate) fn add_redactions_excluding(
        &self,
        redactions: impl IntoIterator<Item = String>,
        env: &EnvMap,
        exclusions: &BTreeSet<String>,
    ) {
        let mut r = _REDACTOR.lock().unwrap();
        // Redactions are intentionally append-only. An exclusion prevents this key from
        // contributing its value, but removing an already registered value could expose a
        // different secret key (or concurrent task) that uses the same value.
        let new_redactions = redactions.into_iter().flat_map(|pattern| {
            let matcher = Wildcard::new(vec![pattern]);
            env.iter()
                .filter(|(k, _)| !exclusions.contains(*k) && matcher.match_any(k))
                .map(|(_, v)| v.clone())
                .collect::<Vec<_>>()
        });
        *r = r.with_additional(new_redactions);
    }

    /// Get the current redaction patterns.
    pub(crate) fn redactions(&self) -> Arc<IndexSet<String>> {
        _REDACTOR.lock().unwrap().patterns_arc()
    }

    /// Redact sensitive values from a string using Aho-Corasick for efficiency.
    pub fn redact(&self, input: &str) -> String {
        _REDACTOR.lock().unwrap().redact(input)
    }
}

fn configs_at_root<'a>(dir: &Path, config_files: &'a ConfigMap) -> Vec<&'a Arc<dyn ConfigFile>> {
    // Highest precedence config files are returned first.
    let mut configs: Vec<&'a Arc<dyn ConfigFile>> = DEFAULT_CONFIG_FILENAMES
        .iter()
        .rev()
        .flat_map(|f| {
            if is_glob_pattern(f) {
                // Handle glob patterns by matching against actual config file paths
                load_config_glob(dir, f)
                    .into_iter()
                    .rev()
                    .filter_map(|path| config_files.get(&path))
                    .collect::<Vec<_>>()
            } else {
                // Handle regular filenames
                config_files
                    .get(&dir.join(f))
                    .into_iter()
                    .collect::<Vec<_>>()
            }
        })
        .collect();
    // Remove duplicates while preserving order
    let mut seen = std::collections::HashSet::new();
    configs.retain(|cf| seen.insert(cf.get_path().to_path_buf()));
    configs
}

fn get_project_root(config_files: &ConfigMap) -> Option<PathBuf> {
    let project_root = config_files
        .values()
        .find_map(|cf| cf.project_root())
        .map(|pr| pr.to_path_buf());
    trace!("project_root: {project_root:?}");
    project_root
}

fn find_monorepo_root(config_files: &ConfigMap) -> Option<PathBuf> {
    find_monorepo_config(config_files).map(|config| config.root)
}

#[derive(Clone)]
struct ResolvedMonorepoConfig {
    root: PathBuf,
    root_config: Arc<dyn ConfigFile>,
    monorepo_source: Arc<dyn ConfigFile>,
    monorepo: MonorepoConfig,
}

/// Find the nearest directory whose effective layered config has `monorepo_root = true`.
///
/// `ConfigMap` is ordered nearest-first — [`load_config_paths`] builds it from
/// [`file::all_dirs`], which walks from the cwd upward. Within one directory,
/// `configs_at_root` returns config layers from highest to lowest precedence, so an
/// environment overlay can override `monorepo_root` without becoming a separate root.
/// This preserves nearest-root behavior for nested monorepos while resolving sibling
/// overlays as one logical root configuration.
fn monorepo_lockfile_root_for(config_files: &ConfigMap) -> Option<PathBuf> {
    let config = find_monorepo_config(config_files)?;
    let setting = config.monorepo.lockfile;
    if !monorepo_lockfile_enabled_for_version(&version::V, setting) {
        return None;
    }
    let monorepo_root = config.root;

    // An explicit opt-in always routes descendant configs to the root
    // lockfile, even when config_roots cannot be resolved. Avoid expanding
    // config_roots here because this method is called for every tool during
    // lockfile resolution, including on every shim invocation. Commands
    // that migrate legacy lockfiles validate config_roots separately.
    if setting == Some(true) {
        return Some(monorepo_root);
    }

    match monorepo_config_root_dirs_for(config_files, None) {
        Ok(config_roots) if !config_roots.is_empty() => Some(monorepo_root),
        Ok(_) | Err(_) => None,
    }
}

fn monorepo_config_root_dirs_for(
    config_files: &ConfigMap,
    filenames: Option<&[String]>,
) -> Result<Vec<PathBuf>> {
    let monorepo_config = find_monorepo_config(config_files)
        .ok_or_else(|| eyre!("no config file in scope sets monorepo_root = true"))?;
    let monorepo_root = monorepo_config.root;
    let patterns = monorepo_config
        .monorepo
        .config_roots
        .ok_or_else(|| eyre!("[monorepo].config_roots is required for monorepo operations"))?;
    if patterns.is_empty() {
        bail!("[monorepo].config_roots is required for monorepo operations");
    }
    let roots = match filenames {
        Some(filenames) => {
            expand_config_roots_with_filenames(&monorepo_root, &patterns, filenames)?
        }
        None => expand_config_root_dirs(&monorepo_root, &patterns)?,
    };
    if roots.is_empty() {
        bail!("[monorepo].config_roots did not match any config roots");
    }
    Ok(roots)
}

static MONOREPO_LOCKFILE_ROOT_FROM_DIR: Lazy<Mutex<HashMap<PathBuf, Option<PathBuf>>>> =
    Lazy::new(Default::default);

/// The monorepo lockfile root that applies to `dir`, from the project configs
/// in it and its ancestors rather than from the current directory's config.
/// A tool stub uses this so its lockfile does not depend on where it is run.
pub(crate) fn monorepo_lockfile_root_from_dir(dir: &Path) -> Option<PathBuf> {
    if let Some(root) = MONOREPO_LOCKFILE_ROOT_FROM_DIR.lock().unwrap().get(dir) {
        return root.clone();
    }
    let paranoid = Settings::try_get().is_ok_and(|settings| settings.paranoid);
    let mut config_files = ConfigMap::new();
    for ancestor in all_dirs_from(dir).unwrap_or_default() {
        for path in config_paths_in_dir(&ancestor) {
            // The same committed layers the stub's lockfile is chosen from.
            if path.extension().is_none_or(|ext| ext != "toml")
                || lockfile::is_local_config(&path)
                || lockfile::extract_env_from_config_path(&path).is_some()
                || is_global_config(&path)
                || (paranoid && !config_file::is_path_trusted(&path))
            {
                continue;
            }
            // Only the static monorepo declarations are read, so the config
            // is decoded without a trust check: nothing in it is evaluated,
            // and a stub run must not prompt for or record trust. An
            // unreadable config just cannot declare a root.
            let Ok(body) = file::read_to_string(&path) else {
                continue;
            };
            if let Ok(cf) = PakroToml::for_monorepo_inspection(&body, &path) {
                config_files.insert(path, Arc::new(cf));
            }
        }
    }
    let root = monorepo_lockfile_root_for(&config_files);
    MONOREPO_LOCKFILE_ROOT_FROM_DIR
        .lock()
        .unwrap()
        .insert(dir.to_path_buf(), root.clone());
    root
}

fn find_monorepo_config(config_files: &ConfigMap) -> Option<ResolvedMonorepoConfig> {
    config_files
        .values()
        .filter_map(|cf| cf.project_root())
        .unique()
        .find_map(|root| resolve_monorepo_config_at_root(&root, config_files))
}

fn resolve_monorepo_config_at_root(
    root: &Path,
    config_files: &ConfigMap,
) -> Option<ResolvedMonorepoConfig> {
    let configs = configs_at_root(root, config_files);
    let (root_config, enabled) = configs
        .iter()
        .find_map(|cf| cf.monorepo_root().map(|enabled| ((*cf).clone(), enabled)))?;
    if !enabled {
        return None;
    }

    let mut monorepo = MonorepoConfig::default();
    let mut monorepo_source = root_config.clone();
    for cf in configs.into_iter().rev() {
        let Some(layer) = cf.monorepo() else {
            continue;
        };
        monorepo_source = cf.clone();
        if layer.config_roots.is_some() {
            monorepo.config_roots.clone_from(&layer.config_roots);
        }
        monorepo.path_aliases.extend(layer.path_aliases.clone());
        if layer.lockfile.is_some() {
            monorepo.lockfile = layer.lockfile;
        }
    }

    Some(ResolvedMonorepoConfig {
        root: root.to_path_buf(),
        root_config,
        monorepo_source,
        monorepo,
    })
}

async fn load_idiomatic_filenames() -> BTreeMap<String, Vec<String>> {
    let settings = Settings::get();
    let enable_tools = settings.idiomatic_version_file_enable_tools.clone();
    let disable_files = settings.idiomatic_version_file_disable_files.clone();
    if !settings.idiomatic_version_file_disable_tools.is_empty() {
        deprecated!(
            "idiomatic_version_file_disable_tools",
            "is deprecated, use idiomatic_version_file_enable_tools instead"
        );
    }
    load_idiomatic_filenames_for_tools(&enable_tools, &disable_files).await
}

/// Same as [`load_idiomatic_filenames`] but for explicit `enable_tools`/`disable_files` rather
/// than the process-wide `Settings::get()` snapshot. Used by monorepo union resolution, where a
/// config root's own `idiomatic_version_file_enable_tools` can differ from the settings
/// resolved from the invocation directory (see [`idiomatic_filenames_for_root`]).
async fn load_idiomatic_filenames_for_tools(
    enable_tools: &BTreeSet<String>,
    disable_files: &BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    if enable_tools.is_empty() {
        return BTreeMap::new();
    }
    let mut jset = JoinSet::new();
    for tool in backend::list() {
        let enable_tools = enable_tools.clone();
        let disable_files = disable_files.clone();
        jset.spawn(async move {
            if !enable_tools.contains(tool.id()) {
                return vec![];
            }
            match tool.idiomatic_filenames().await {
                Ok(filenames) => filenames
                    .iter()
                    .filter(|filename| {
                        !idiomatic_version_file_disabled(&disable_files, tool.id(), filename)
                    })
                    .map(|f| (f.to_string(), tool.id().to_string()))
                    .collect::<Vec<_>>(),
                Err(err) => {
                    eprintln!("Error: {err}");
                    vec![]
                }
            }
        });
    }
    let idiomatic = jset
        .join_all()
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    let mut idiomatic_filenames = BTreeMap::new();
    for (filename, plugin) in idiomatic {
        idiomatic_filenames
            .entry(filename)
            .or_insert_with(Vec::new)
            .push(plugin);
    }
    idiomatic_filenames
}

fn idiomatic_version_file_disabled(
    disable_files: &BTreeSet<String>,
    tool: &str,
    filename: &str,
) -> bool {
    disable_files.iter().any(|entry| {
        entry
            .rsplit_once(':')
            .is_some_and(|(disabled_tool, disabled_filename)| {
                disabled_tool == tool && disabled_filename == filename
            })
    })
}

/// Resolves idiomatic version filenames for a single monorepo config root through the
/// canonical settings loader.
///
/// `Settings::get()` is a process-wide snapshot rooted at the invocation directory. Resolving
/// from the monorepo config root keeps normal environment, hierarchy, global/system, safe-mode,
/// and `--no-config` behavior without maintaining a second precedence implementation.
async fn idiomatic_filenames_for_root(
    root: &Path,
    default_idiomatic_filenames: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, Vec<String>>> {
    let rooted = settings::IdiomaticVersionFileSettings::resolve_from(
        root,
        settings::SettingsLoadPolicy::HIERARCHY,
    )?;
    if rooted == settings::IdiomaticVersionFileSettings::current() {
        return Ok(default_idiomatic_filenames.clone());
    }
    Ok(load_idiomatic_filenames_for_tools(&rooted.enable_tools, &rooted.disable_files).await)
}

fn local_config_patterns<'a>(
    tool_versions_filename: &'a str,
    config_filename: &'a str,
    tool_versions_overrides: Option<&[&'a str]>,
    config_overrides: &[&'a str],
) -> IndexSet<&'a str> {
    let mut paths: IndexSet<&str> = IndexSet::new();
    if let Some(o) = tool_versions_overrides {
        paths.extend(o.iter().copied());
    } else {
        paths.extend([".tool-versions", tool_versions_filename]);
    }
    if !config_overrides.is_empty() {
        paths.extend(config_overrides.iter().copied())
    } else {
        paths.extend([
            ".config/pakro/conf.d/*.toml",
            ".config/pakro/conf.d/*/pakro.toml",
            ".config/pakro/conf.d/*/pakro.local.toml",
            ".config/pakro/config.toml",
            ".config/pakro/pakro.toml",
            ".config/pakro.toml",
            ".pakro/conf.d/*.toml",
            ".pakro/conf.d/*/pakro.toml",
            ".pakro/conf.d/*/pakro.local.toml",
            ".pakro/config.toml",
            "pakro/conf.d/*.toml",
            "pakro/conf.d/*/pakro.toml",
            "pakro/conf.d/*/pakro.local.toml",
            "pakro/config.toml",
            "pakro.toml",
            config_filename,
            ".pakro.toml",
            ".config/pakro/config.local.toml",
            ".config/pakro/pakro.local.toml",
            ".config/pakro.local.toml",
            ".pakro/config.local.toml",
            "pakro/config.local.toml",
            "pakro.local.toml",
            ".pakro.local.toml",
        ]);
    }

    paths
}

static LOCAL_CONFIG_FILENAMES: Lazy<IndexSet<&'static str>> = Lazy::new(|| {
    let tool_overrides = env::PAKRO_OVERRIDE_TOOL_VERSIONS_FILENAMES
        .as_ref()
        .map(|values| values.iter().map(String::as_str).collect::<Vec<_>>());
    let config_overrides = env::PAKRO_OVERRIDE_CONFIG_FILENAMES
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    local_config_patterns(
        &env::PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME,
        &env::PAKRO_DEFAULT_CONFIG_FILENAME,
        tool_overrides.as_deref(),
        &config_overrides,
    )
});
/// Config filename patterns for a single PAKRO_ENV environment, in precedence order
/// (later wins, matching LOCAL_CONFIG_FILENAMES ordering)
fn env_config_patterns(env: &str) -> Vec<String> {
    env_config_patterns_with_conf_d(env, env::env_conf_d())
}

/// `env_config_patterns` with the `env_conf_d` decision injected, so tests can
/// cover both sides of the migration without touching global state.
fn env_config_patterns_with_conf_d(env: &str, env_conf_d: bool) -> Vec<String> {
    let env = glob::Pattern::escape(env);
    let mut patterns = vec![];
    if env_conf_d {
        patterns.push(format!(".config/pakro/conf.d/*.{env}.toml"));
    }
    patterns.push(format!(".config/pakro/conf.d/*/pakro.{env}.toml"));
    patterns.extend([
        format!(".config/pakro/config.{env}.toml"),
        format!(".config/pakro.{env}.toml"),
    ]);
    if env_conf_d {
        patterns.push(format!("pakro/conf.d/*.{env}.toml"));
    }
    patterns.push(format!("pakro/conf.d/*/pakro.{env}.toml"));
    patterns.extend([
        format!("pakro/config.{env}.toml"),
        format!("pakro.{env}.toml"),
    ]);
    if env_conf_d {
        patterns.push(format!(".pakro/conf.d/*.{env}.toml"));
    }
    patterns.push(format!(".pakro/conf.d/*/pakro.{env}.toml"));
    patterns.extend([
        format!(".pakro/config.{env}.toml"),
        format!(".pakro.{env}.toml"),
    ]);
    if env_conf_d {
        patterns.push(format!(".config/pakro/conf.d/*.{env}.local.toml"));
    }
    patterns.push(format!(".config/pakro/conf.d/*/pakro.{env}.local.toml"));
    patterns.extend([
        format!(".config/pakro/config.{env}.local.toml"),
        format!(".config/pakro.{env}.local.toml"),
    ]);
    if env_conf_d {
        patterns.push(format!("pakro/conf.d/*.{env}.local.toml"));
    }
    patterns.push(format!("pakro/conf.d/*/pakro.{env}.local.toml"));
    patterns.extend([
        format!("pakro/config.{env}.local.toml"),
        format!("pakro.{env}.local.toml"),
    ]);
    if env_conf_d {
        patterns.push(format!(".pakro/conf.d/*.{env}.local.toml"));
    }
    patterns.push(format!(".pakro/conf.d/*/pakro.{env}.local.toml"));
    patterns.extend([
        format!(".pakro/config.{env}.local.toml"),
        format!(".pakro.{env}.local.toml"),
    ]);
    patterns
}

pub static DEFAULT_CONFIG_FILENAMES: Lazy<Vec<String>> = Lazy::new(|| {
    let mut filenames = LOCAL_CONFIG_FILENAMES
        .iter()
        .map(|f| f.to_string())
        .collect_vec();
    for env in &*env::PAKRO_ENV_WITH_AUTO {
        filenames.extend(env_config_patterns(env));
    }
    filenames
});
static TOML_CONFIG_FILENAMES: Lazy<Vec<String>> = Lazy::new(|| {
    DEFAULT_CONFIG_FILENAMES
        .iter()
        .filter(|s| s.ends_with(".toml"))
        .map(|s| s.to_string())
        .collect()
});
static TOML_CONFIG_MATCHERS: Lazy<Vec<globset::GlobMatcher>> = Lazy::new(|| {
    TOML_CONFIG_FILENAMES
        .iter()
        .filter_map(|pattern| {
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(|e| warn!("failed to compile config glob pattern {pattern}: {e}"))
                .ok()
                .map(|glob| glob.compile_matcher())
        })
        .collect()
});
pub static ALL_CONFIG_FILES: Lazy<IndexSet<PathBuf>> = Lazy::new(|| {
    load_config_paths(&DEFAULT_CONFIG_FILENAMES, false)
        .into_iter()
        .collect()
});
pub static IGNORED_CONFIG_FILES: Lazy<IndexSet<PathBuf>> = Lazy::new(|| {
    load_config_paths(&DEFAULT_CONFIG_FILENAMES, true)
        .into_iter()
        .filter(|p| {
            let ctr = config_trust_root(p);
            // The `ignored_config_paths` setting is a hard block; the persisted
            // ignore list is overridden by `trusted_config_paths`, matching
            // is_trusted so a settings-trusted config is not reported as ignored.
            if config_file::is_ignored_via_setting(&ctr) || config_file::is_ignored_via_setting(p) {
                return true;
            }
            (config_file::is_persisted_ignored(&ctr) || config_file::is_persisted_ignored(p))
                && !config_file::is_trusted_via_config_paths(p)
        })
        .collect()
});
// pub static LOCAL_CONFIG_FILES: Lazy<Vec<PathBuf>> = Lazy::new(|| {
//     ALL_CONFIG_FILES
//         .iter()
//         .filter(|cf| !is_global_config(cf))
//         .cloned()
//         .collect()
// });

type GlobResults = HashMap<(PathBuf, String), Vec<PathBuf>>;
static GLOB_RESULTS: Lazy<Mutex<GlobResults>> = Lazy::new(Default::default);

pub(crate) fn glob(dir: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let mut results = GLOB_RESULTS.lock().unwrap();
    let key = (dir.to_path_buf(), pattern.to_string());
    if let Some(glob) = results.get(&key) {
        return Ok(glob.clone());
    }
    let full_pattern = dir.join(pattern);
    let full_pattern = full_pattern.to_string_lossy();
    let paths = if glob_parent_exists(dir, pattern) {
        glob::glob(full_pattern.as_ref())?
            .filter_map(|p| p.ok())
            .collect_vec()
    } else {
        // Still reject a malformed pattern, as `glob::glob` would, so callers
        // can report it.
        glob::Pattern::new(full_pattern.as_ref())?;
        vec![]
    };
    results.insert(key, paths.clone());
    Ok(paths)
}

/// Whether the directory spelled out by `pattern` before its first wildcard
/// exists under `dir`. Nothing can match below a missing directory, and config
/// discovery globs about a dozen `conf.d` patterns in every ancestor directory,
/// where those directories almost never exist: one lookup here replaces
/// compiling and walking each pattern. A component with any glob metacharacter,
/// escaped ones included, ends the literal part, so this never skips a pattern
/// that could match.
fn glob_parent_exists(dir: &Path, pattern: &str) -> bool {
    let components = Path::new(pattern).components().collect_vec();
    let literal = components
        .iter()
        .take(components.len().saturating_sub(1))
        .take_while(|c| {
            !c.as_os_str()
                .to_string_lossy()
                .contains(['*', '?', '[', ']'])
        })
        .collect::<PathBuf>();
    literal.as_os_str().is_empty() || dir.join(literal).is_dir()
}

fn config_glob(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    // Match global/system conf.d discovery: editor backups and other hidden
    // files are not configuration fragments.
    glob(dir, pattern)
        .unwrap_or_default()
        .into_iter()
        .filter(|path| config_glob_entry_is_visible(path, &[&dirs::CONFIG, &dirs::SYSTEM_CONFIG]))
        .collect()
}

fn config_glob_entry_is_visible(path: &Path, global_roots: &[&Path]) -> bool {
    if path.parent().is_some_and(is_conf_d_file) {
        return is_conf_d_folder_file_with_config_dirs(path, global_roots);
    }
    !is_conf_d_file(path)
        || !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

fn is_unconditional_conf_d_pattern(pattern: &str) -> bool {
    pattern.ends_with("conf.d/*.toml")
}

/// Whether a conf.d fragment reserves a filename component for an environment.
///
/// `name.toml` and `name.local.toml` are unconditional. `name.dev.toml` and
/// `name.dev.local.toml` are environment-specific. Environment names themselves
/// may contain dots; active files are selected by exact glob patterns.
fn conf_d_file_environment(path: &Path) -> Option<(&str, bool)> {
    if !is_conf_d_file(path) {
        return None;
    }
    let filename = path.file_name()?.to_str()?;
    let stem = filename.strip_suffix(".toml")?;
    let (stem, local) = stem
        .strip_suffix(".local")
        .map_or((stem, false), |stem| (stem, true));
    let (name, environment) = stem.split_once('.')?;
    (!name.is_empty() && !environment.is_empty()).then_some((environment, local))
}

fn is_environment_conf_d_file(path: &Path) -> bool {
    conf_d_file_environment(path).is_some()
}

/// Warn about a dotted `conf.d` fragment whose name will become an environment
/// selector once `env_conf_d` defaults to on. Only fires inside the migration
/// window: the setting is unset (so the meaning is still going to change) and
/// the version default is still legacy (so it hasn't changed yet).
fn warn_on_dotted_conf_d_file(path: &Path) {
    if settings::is_loaded()
        && env::env_conf_d_setting().is_none()
        && !env::env_conf_d()
        && is_environment_conf_d_file(path)
    {
        deprecated_at!(
            "2026.8.10",
            "2027.8.10",
            "dotted_conf_d_filename",
            "dots in unconditional conf.d filenames are deprecated because the suffix will become an environment selector; rename fragments such as `node.tools.toml` to `node-tools.toml`, or set `env_conf_d = true` in a pakrorc.toml file to opt in now"
        );
    }
}

fn load_config_glob(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let paths = config_glob(dir, pattern);
    if !env::env_conf_d() {
        for path in &paths {
            warn_on_dotted_conf_d_file(path);
        }
        return paths;
    }
    paths
        .into_iter()
        .filter(|path| conf_d_path_is_active(path, pattern, true, &env::PAKRO_ENV_WITH_AUTO))
        .collect()
}

fn conf_d_path_is_active(
    path: &Path,
    pattern: &str,
    env_conf_d: bool,
    environments: &[String],
) -> bool {
    if !env_conf_d {
        return true;
    }
    if is_unconditional_conf_d_pattern(pattern) {
        !is_environment_conf_d_file(path)
    } else if let Some((environment, _)) = conf_d_file_environment(path) {
        environments.iter().any(|active| active == environment)
    } else {
        true
    }
}

pub fn config_files_in_dir(dir: &Path) -> IndexSet<PathBuf> {
    DEFAULT_CONFIG_FILENAMES
        .iter()
        .flat_map(|f| config_glob(dir, f))
        .collect()
}

pub fn config_paths_in_dir(dir: &Path) -> Vec<PathBuf> {
    config_paths_in_dir_with_filenames(dir, &DEFAULT_CONFIG_FILENAMES)
}

/// Return the active configuration environment encoded in a loaded config
/// filename. A vector matches the ordered PAKRO_ENV model and leaves room for
/// future config forms that represent more than one environment.
pub fn environments_for_config_path(path: &Path) -> Vec<String> {
    let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
        return vec![];
    };
    env::PAKRO_ENV_WITH_AUTO
        .iter()
        .filter(|environment| {
            ["config", "pakro", ".pakro"].into_iter().any(|prefix| {
                filename == format!("{prefix}.{environment}.toml")
                    || filename == format!("{prefix}.{environment}.local.toml")
            }) || (env::env_conf_d()
                && conf_d_file_environment(path)
                    .is_some_and(|(file_environment, _)| file_environment == environment.as_str()))
        })
        .cloned()
        .collect()
}

fn config_paths_in_dir_with_filenames(dir: &Path, filenames: &[String]) -> Vec<PathBuf> {
    let config_paths: Vec<PathBuf> = filenames
        .iter()
        .rev()
        .flat_map(|f| {
            if is_glob_pattern(f) {
                load_config_glob(dir, f).into_iter().rev().collect()
            } else {
                let path = dir.join(f);
                if path.exists() { vec![path] } else { vec![] }
            }
        })
        .collect();

    let mut seen = std::collections::HashSet::new();
    config_paths
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

fn all_dirs() -> Result<Vec<PathBuf>> {
    file::all_dirs(env::current_dir()?, &env::PAKRO_CEILING_PATHS)
}

/// Get all directories in the hierarchy from a starting directory up to ceiling paths
fn all_dirs_from(start_dir: &Path) -> Result<Vec<PathBuf>> {
    file::all_dirs(start_dir, &env::PAKRO_CEILING_PATHS)
}

/// Returns true if a path is a .tool-versions file (lower priority for writes)
pub(crate) fn is_tool_versions_file(p: &Path) -> bool {
    p.file_name()
        .is_some_and(|f| f.to_string_lossy().ends_with(".tool-versions"))
}

/// Get the first (lowest precedence) config file, but skip .tool-versions unless
/// it's the only option. This ensures commands like `pakro use` write to pakro.toml
/// instead of pakro.local.toml or .tool-versions when multiple configs exist.
/// See: https://github.com/jdx/mise/discussions/6475
///
/// The two exclusions are not the same kind. `.tool-versions` is a *preference*: it is passed
/// over while choosing and comes back once nothing else qualifies. A `conf.d` drop-in must not
/// be written to at all, so it is excluded from the fallback as well, and this returns `None`
/// instead — every caller turns that into a config of its own, which is the wanted outcome.
///
/// Carrying the drop-in exclusion in a filtered view rather than repeating it in each arm is
/// what keeps the two apart: the preference needs a fallback, the exclusion must not have one,
/// and a chain of `or_else` arms only holds that line for as long as every arm remembers to.
/// See: <https://github.com/jdx/mise/discussions/5842>
fn first_config_file(files: &IndexSet<PathBuf>) -> Option<&PathBuf> {
    // conf.d fragments, including folder fragments, are never written to
    let writable = || {
        files
            .iter()
            .filter(|p| !is_conf_d_file(p) && !is_conf_d_folder_file(p))
    };
    writable()
        .find(|p| !is_tool_versions_file(p))
        .or_else(|| writable().next())
}

fn is_conf_d_file(p: &Path) -> bool {
    p.parent()
        .is_some_and(|d| d.file_name().is_some_and(|n| n == "conf.d"))
}

/// A config file inside a `conf.d` folder fragment, such as `conf.d/git/pakro.toml`
/// or `conf.d/git/pakro.linux.toml`. The folder is the file's config root, so
/// fragments keep their own sources and helpers next to their config.
///
/// Only `conf.d` directories pakro reads count: a project that merely lives at
/// `nginx/conf.d/site/pakro.toml` is an ordinary project.
pub(crate) fn is_conf_d_folder_file(p: &Path) -> bool {
    is_conf_d_folder_file_with_config_dirs(p, &[&dirs::CONFIG, &dirs::SYSTEM_CONFIG])
}

pub(crate) fn is_conf_d_folder_file_with_config_dirs(p: &Path, global_roots: &[&Path]) -> bool {
    let is_pakro_filename = p
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("pakro.") && name.ends_with(".toml"));
    let Some(folder) = p.parent() else {
        return false;
    };
    if !is_pakro_filename
        || !is_conf_d_file(folder)
        || folder
            .file_name()
            .is_none_or(|name| name.to_string_lossy().starts_with('.'))
    {
        return false;
    }
    folder.parent().and_then(Path::parent).is_some_and(|owner| {
        owner
            .file_name()
            .is_some_and(|name| name == "pakro" || name == ".pakro")
            || global_roots.contains(&owner)
    })
}

/// The config files in `dir` that config loading would actually read, with the same
/// exclusions `load_config_paths` applies.
///
/// [`config_files_in_dir`] deliberately does **not** filter — `pakro trust` has to see a
/// file in order to un-ignore it, and `pakro fmt` and task discovery want everything on
/// disk. Choosing a file to *write* to is the one case that must not.
fn loadable_config_files_in_dir(dir: &Path, filenames: &[String]) -> IndexSet<PathBuf> {
    if config_dir_is_ignored(dir, false) {
        return IndexSet::new();
    }
    filenames
        .iter()
        .flat_map(|f| load_config_glob(dir, f))
        .unique_by(|p| file::desymlink_path(p))
        .filter(|p| !config_path_is_ignored(p, false))
        .collect()
}

/// The config file to write to **inside `dir` itself**, or where one should be created.
///
/// `--path`/`--file` name a specific directory, so the answer has to be inside it: "if a directory
/// is specified, it will look for a config file in that directory" (`pakro use --help`). This
/// deliberately does not walk up. [`config_file_from_dir`] is the walking one, and it is only ever
/// asked about the cwd.
///
/// Candidates that config loading skips are skipped here too: writing into one produces a
/// file pakro will never read back. When nothing in `dir` is loadable the default name is
/// still returned — the directory was named explicitly, so pakro creates what was asked for
/// — but the caller is warned that it will not be read.
pub fn config_file_in_dir(dir: &Path) -> PathBuf {
    let files = loadable_config_files_in_dir(dir, &DEFAULT_CONFIG_FILENAMES);
    if let Some(cf) = first_config_file(&files)
        && !is_global_config(cf)
    {
        return cf.clone();
    }
    let fallback = match Settings::get().asdf_compat {
        true => dir.join(&*PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME),
        false => dir.join(&*PAKRO_DEFAULT_CONFIG_FILENAME),
    };
    if config_dir_is_ignored(dir, false) || config_path_is_ignored(&fallback, false) {
        warn_once!(
            "{p} is excluded from config loading, so pakro will not read back what it writes there",
            p = display_path(&fallback)
        );
    }
    fallback
}

/// The nearest local config file walking up from `start`: the highest-precedence directory
/// that has one, then the lowest-precedence file inside it.
///
/// Both write-target resolvers share this. They were separate implementations of the same
/// rule, and only one of them was kept current — the ignore filter added for
/// <https://github.com/jdx/mise/discussions/7015> reached `local_toml_config_path_from_dir`
/// and not `config_file_from_dir`, so `pakro use` picked a file that config loading
/// deliberately skips and wrote a tool into it that pakro would never read back.
fn nearest_local_config_file(start: &Path, filenames: &[String]) -> Option<PathBuf> {
    if Settings::no_config() {
        return None;
    }
    for dir in all_dirs_from(start).unwrap_or_default() {
        if config_dir_is_ignored(&dir, false) {
            continue;
        }
        let files: IndexSet<PathBuf> = filenames
            .iter()
            .flat_map(|f| glob(&dir, f).unwrap_or_default())
            .unique_by(|p| file::desymlink_path(p))
            .filter(|p| !config_path_is_ignored(p, false))
            .collect();
        if let Some(cf) = first_config_file(&files)
            && !is_global_config(cf)
        {
            return Some(cf.clone());
        }
    }
    None
}

pub(crate) fn config_file_from_dir(p: &Path) -> PathBuf {
    if !p.is_dir() {
        return p.to_path_buf();
    }
    // Walk from the cwd rather than from `p`, which is what this has always done.
    if let Some(cf) = env::current_dir()
        .ok()
        .and_then(|cwd| nearest_local_config_file(&cwd, &DEFAULT_CONFIG_FILENAMES))
    {
        return cf;
    }
    match Settings::get().asdf_compat {
        true => p.join(&*PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME),
        false => p.join(&*PAKRO_DEFAULT_CONFIG_FILENAME),
    }
}

fn load_config_paths_from_dirs(
    dirs: Vec<PathBuf>,
    config_filenames: &[String],
    include_ignored: bool,
) -> Vec<PathBuf> {
    let mut config_files = dirs
        .iter()
        .flat_map(|dir| {
            if config_dir_is_ignored(dir, include_ignored) {
                vec![]
            } else {
                config_paths_in_dir_with_filenames(dir, config_filenames)
            }
        })
        .collect::<Vec<_>>();

    // rev: these groups are lowest-first, this list is highest-first
    config_files.extend(global_config_files().into_iter().rev());
    config_files.extend(system_config_files().into_iter().rev());

    config_files
        .into_iter()
        .unique_by(|p| file::desymlink_path(p))
        .filter(|p| !config_path_is_ignored(p, include_ignored))
        .collect()
}

pub(crate) fn load_config_paths_from(
    start_dir: &Path,
    config_filenames: &[String],
    include_ignored: bool,
) -> Vec<PathBuf> {
    if Settings::no_config() {
        return vec![];
    }
    load_config_paths_from_dirs(
        all_dirs_from(start_dir).unwrap_or_default(),
        config_filenames,
        include_ignored,
    )
}

pub fn load_config_paths(config_filenames: &[String], include_ignored: bool) -> Vec<PathBuf> {
    if Settings::no_config() {
        return vec![];
    }
    load_config_paths_from_dirs(
        all_dirs().unwrap_or_default(),
        config_filenames,
        include_ignored,
    )
}

/// Whether to emit the phase-2 auto_env rollout warning. Pure for unit testing.
/// Warns only when the user hasn't decided (setting unset), auto envs aren't
/// already active, and the pakro version is in the warning window that precedes
/// the 2027.6.0 default flip.
fn should_warn_auto_env(
    version: &versions::Versioning,
    setting: Option<bool>,
    auto_envs_active: bool,
) -> bool {
    setting.is_none()
        && !auto_envs_active
        && *version >= versions::Versioning::new("2026.12.0").unwrap()
        && !env::auto_env_default_for_version(version)
}

/// Default for monorepo lockfile routing when `[monorepo].lockfile` is unset.
/// Keep legacy colocated lockfiles until the scheduled default flip.
fn monorepo_lockfile_default_for_version(version: &versions::Versioning) -> bool {
    *version >= versions::Versioning::new(MONOREPO_LOCKFILE_DEFAULT_AT).unwrap()
}

fn monorepo_lockfile_enabled_for_version(
    version: &versions::Versioning,
    setting: Option<bool>,
) -> bool {
    setting.unwrap_or_else(|| monorepo_lockfile_default_for_version(version))
}

/// Whether to emit the phase-2 monorepo lockfile rollout warning. Pure for unit testing.
/// Warns only when the user has not explicitly chosen `lockfile = true` or `false`
/// and the pakro version is in the warning window before the default flip.
fn should_warn_monorepo_lockfile_default(
    version: &versions::Versioning,
    setting: Option<bool>,
    lockfile_enabled: bool,
    monorepo_lockfiles_exist: bool,
) -> bool {
    setting.is_none()
        && lockfile_enabled
        && monorepo_lockfiles_exist
        && *version >= versions::Versioning::new(MONOREPO_LOCKFILE_WARN_AT).unwrap()
        && !monorepo_lockfile_default_for_version(version)
}

fn warn_if_monorepo_lockfile_default_changes(config: &Config) {
    // Dead code once the default flips on: unset configs use the new behavior,
    // so this warning path should be removed when the rollout completes.
    debug_assert!(
        !monorepo_lockfile_default_for_version(&version::V),
        "monorepo lockfiles are now default-on; remove warn_if_monorepo_lockfile_default_changes() and should_warn_monorepo_lockfile_default()"
    );
    let Some(monorepo_config) = find_monorepo_config(&config.config_files) else {
        return;
    };
    let setting = monorepo_config.monorepo.lockfile;
    if !should_warn_monorepo_lockfile_default(
        &version::V,
        setting,
        Settings::get().lockfile_enabled(),
        monorepo_lockfiles_exist(config, &monorepo_config),
    ) {
        return;
    }

    warn_once!(
        "Monorepo lockfiles will default to a single root lockfile starting in pakro {MONOREPO_LOCKFILE_DEFAULT_AT}. \
        Set `[monorepo] lockfile = true` in {} to opt in now, or `lockfile = false` to keep per-subproject lockfiles and silence this warning.",
        display_path(monorepo_config.monorepo_source.get_path())
    );
}

fn monorepo_lockfiles_exist(config: &Config, monorepo_config: &ResolvedMonorepoConfig) -> bool {
    let monorepo_root = &monorepo_config.root;
    let mut lockfile_paths = IndexSet::new();

    for (config_path, cf) in &config.config_files {
        if !config_path.starts_with(monorepo_root) || !cf.source().is_pakro_toml() {
            continue;
        }
        lockfile_paths.insert(lockfile::lockfile_path_for_config(config_path, None).0);
        lockfile_paths.insert(
            lockfile::lockfile_path_for_config(config_path, Some(monorepo_root.as_path())).0,
        );
    }

    if let Some(patterns) = &monorepo_config.monorepo.config_roots
        && let Ok(config_roots) = expand_config_root_dirs(monorepo_root, patterns)
    {
        for config_root in config_roots {
            for lockfile_path in lockfile::lockfile_variant_paths_in_dir(&config_root) {
                lockfile_paths.insert(lockfile_path);
            }
            for config_path in config_paths_in_dir(&config_root) {
                lockfile_paths.insert(lockfile::lockfile_path_for_config(&config_path, None).0);
                lockfile_paths.insert(
                    lockfile::lockfile_path_for_config(&config_path, Some(monorepo_root.as_path()))
                        .0,
                );
            }
        }
    }

    lockfile_paths.iter().any(|path| path.exists())
}

/// Phase-2 rollout warning for auto_env: starting with 2026.12.0, tell users about
/// platform-specific config files (e.g. pakro.windows.toml) that pakro will begin
/// loading automatically when auto_env defaults to true in 2027.6.0.
fn warn_if_auto_env_files_exist() {
    // dead code once the default flips on: the files load, so there is nothing to warn about
    debug_assert!(
        !env::auto_env_default_for_version(&version::V),
        "auto_env is now default-on; remove warn_if_auto_env_files_exist() and should_warn_auto_env()"
    );
    if !should_warn_auto_env(
        &version::V,
        env::auto_env_setting(),
        !env::AUTO_ENV_NAMES.is_empty(),
    ) || *env::IS_RUNNING_AS_SHIM
    {
        return;
    }
    // Skip hook-env to keep the every-prompt path free of extra filesystem checks.
    // Match the subcommand anywhere before `--` rather than guessing its positional
    // slot (flags with separate values like `--cd /path` would shift it); the warning
    // is advisory, so erring toward skipping it is the safe direction.
    if env::ARGS
        .read()
        .unwrap()
        .iter()
        .skip(1)
        .take_while(|a| *a != "--")
        .any(|a| a == "hook-env")
    {
        return;
    }
    let found = detect_auto_env_candidate_files();
    if !found.is_empty() {
        warn_once!(
            "Found platform-specific config file(s) that pakro will load automatically starting in 2027.6.0: {}. \
            Set PAKRO_AUTO_ENV=true (or `auto_env = true` in .pakrorc.toml) to enable this now, \
            or `auto_env = false` to keep the current behavior and silence this warning. \
            See https://mise.jdx.dev/configuration/environments.html#platform-environments",
            found.iter().map(display_path).join(", ")
        );
    }
}

/// Config files that exist on disk and would be loaded if auto_env were enabled,
/// but are not loaded today because their platform env is not in PAKRO_ENV.
fn detect_auto_env_candidate_files() -> Vec<PathBuf> {
    let candidate_envs = env::platform_env_names()
        .into_iter()
        .filter(|name| !env::PAKRO_ENV.contains(name))
        .collect_vec();
    // IndexSet: the all_dirs() walk and the global/system dir checks can find the
    // same file (e.g. ~/.config/pakro/config.{env}.toml when cwd is under $HOME)
    let mut found = IndexSet::new();
    for dir in all_dirs().unwrap_or_default() {
        if config_file::is_ignored_via_setting(&dir) {
            continue;
        }
        for env_name in &candidate_envs {
            for pattern in env_config_patterns(env_name) {
                found.extend(
                    // config_glob applies loading's exclusions, such as hidden
                    // conf.d fragments and folders
                    config_glob(&dir, &pattern).into_iter().filter(|path| {
                        !is_conf_d_file(path)
                            || conf_d_file_environment(path)
                                .is_some_and(|(environment, _)| environment == env_name)
                    }),
                );
            }
        }
    }
    for dir in [*dirs::CONFIG, *dirs::SYSTEM_CONFIG] {
        for env_name in &candidate_envs {
            if env::env_conf_d() {
                found.extend(conf_d_environment_files(dir, env_name, false));
                found.extend(conf_d_environment_files(dir, env_name, true));
            }
            for local in ["", ".local"] {
                found.extend(
                    glob(
                        dir,
                        &format!(
                            "conf.d/*/pakro.{}{local}.toml",
                            glob::Pattern::escape(env_name)
                        ),
                    )
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|p| is_conf_d_folder_file(p)),
                );
            }
            for filename in [
                format!("config.{env_name}.toml"),
                format!("pakro.{env_name}.toml"),
                format!("config.{env_name}.local.toml"),
                format!("pakro.{env_name}.local.toml"),
            ] {
                let p = dir.join(filename);
                if p.is_file() {
                    found.insert(p);
                }
            }
        }
    }
    // apply the same filters as load_config_paths so we don't warn about files
    // that wouldn't be loaded even with auto_env enabled
    found
        .into_iter()
        .filter(|p| !config_path_is_ignored(p, false))
        .collect()
}

/// Load config hierarchy from a specific directory.
/// This loads all config files from start_dir up through parent directories,
/// including PAKRO_ENV-specific configs and idiomatic version files.
/// Returns (paths, idiomatic_filenames) so callers can pass the map to
/// load_config_files_from_paths without a redundant second computation.
///
/// `start_dir` is typically a monorepo config root (or a directory under one)
/// that differs from the process's invocation directory. Idiomatic version
/// file discovery is therefore resolved from `start_dir`'s own config
/// hierarchy via `idiomatic_filenames_for_root`, the same helper
pub async fn load_config_hierarchy_from_dir(
    start_dir: &Path,
) -> Result<(Vec<PathBuf>, BTreeMap<String, Vec<String>>)> {
    if Settings::no_config() {
        return Ok((vec![], BTreeMap::new()));
    }

    let default_idiomatic_files = load_idiomatic_filenames().await;
    let idiomatic_files = idiomatic_filenames_for_root(start_dir, &default_idiomatic_files).await?;
    let config_filenames: Vec<String> = idiomatic_files
        .keys()
        .cloned()
        .chain(DEFAULT_CONFIG_FILENAMES.iter().cloned())
        .collect();

    // Get all directories from start_dir up to root/ceiling
    let dirs = all_dirs_from(start_dir)?;

    let mut config_files = dirs
        .iter()
        .flat_map(|dir| {
            if config_dir_is_ignored(dir, false) {
                vec![]
            } else {
                config_filenames
                    .iter()
                    .rev()
                    .flat_map(|f| load_config_glob(dir, f).into_iter().rev())
                    .collect()
            }
        })
        .collect::<Vec<_>>();

    // rev: these groups are lowest-first, this list is highest-first
    config_files.extend(global_config_files().into_iter().rev());
    config_files.extend(system_config_files().into_iter().rev());

    let paths = config_files
        .into_iter()
        .unique_by(|p| file::desymlink_path(p))
        .filter(|p| !config_path_is_ignored(p, false))
        .collect();

    Ok((paths, idiomatic_files))
}

pub fn is_global_config(path: &Path) -> bool {
    config_set_contains(&global_config_files(), path) || is_system_config(path)
}

pub fn is_system_config(path: &Path) -> bool {
    config_set_contains(&system_config_files(), path)
}

/// Membership test that tolerates symlinked path prefixes (e.g. Fedora Atomic's
/// `/home` -> `/var/home`, where the shell PWD and `$HOME` disagree on the
/// prefix). The fast path is raw equality (no filesystem hit); only on a miss do
/// we canonicalize via [`file::desymlink_path`], matching how `load_config_paths`
/// already dedupes config paths. Without this, a global config discovered via the
/// PWD prefix is not byte-equal to its `$HOME`-derived entry and is wrongly
/// treated as a local config (stripping global-only settings).
fn config_set_contains(set: &IndexSet<PathBuf>, path: &Path) -> bool {
    if set.contains(path) {
        return true;
    }
    // This runs for every config file each time a tool resolves, so resolving
    // the same paths again would dominate short commands.
    let target = file::desymlink_path_cached(path);
    set.iter().any(|p| file::desymlink_path_cached(p) == target)
}

/// Returns true if the path should be filtered out due to PAKRO_CONFIG_DIR override.
/// When PAKRO_CONFIG_DIR is set to a non-default location, this filters out configs
/// found under the default location (~/.config/pakro) during traversal.
/// See: https://github.com/jdx/mise/discussions/7015
fn is_default_config_dir_override_filtered(path: &Path) -> bool {
    *env::PAKRO_CONFIG_DIR_OVERRIDDEN
        && !config_set_contains(&global_config_files(), path)
        && path.starts_with(&*env::PAKRO_DEFAULT_CONFIG_DIR)
}

pub(crate) fn config_dir_is_ignored(dir: &Path, include_ignored: bool) -> bool {
    !include_ignored && config_file::is_ignored_via_setting(dir)
}

pub(crate) fn config_path_is_ignored(path: &Path, include_ignored: bool) -> bool {
    if is_default_config_dir_override_filtered(path) {
        return true;
    }
    if include_ignored {
        return false;
    }
    let ctr = config_trust_root(path);
    // The `ignored_config_paths` setting is a hard filter.
    if config_file::is_ignored_via_setting(&ctr) || config_file::is_ignored_via_setting(path) {
        return true;
    }
    // The persisted ignore list (dismissed prompt / `pakro trust --ignore`) is
    // overridden by `trusted_config_paths`, matching is_trusted's precedence so
    // a settings-trusted config is still discovered and loaded.
    if config_file::is_persisted_ignored(&ctr) || config_file::is_persisted_ignored(path) {
        return !config_file::is_trusted_via_config_paths(path);
    }
    false
}

static GLOBAL_CONFIG_FILES: Lazy<Mutex<Option<IndexSet<PathBuf>>>> = Lazy::new(Default::default);
static SYSTEM_CONFIG_FILES: Lazy<Mutex<Option<IndexSet<PathBuf>>>> = Lazy::new(Default::default);

pub(crate) fn global_config_files() -> IndexSet<PathBuf> {
    let mut g = GLOBAL_CONFIG_FILES.lock().unwrap();
    if let Some(g) = &*g {
        return g.clone();
    }
    if let Some(global_config_file) = &*env::PAKRO_GLOBAL_CONFIG_FILE {
        return vec![global_config_file.clone()].into_iter().collect();
    }
    let mut config_files: IndexSet<PathBuf> = config_files_from_dir(&dirs::CONFIG);
    if !*env::PAKRO_USE_TOML {
        // only add ~/.tool-versions if PAKRO_CONFIG_FILE is not set
        // because that's how the user overrides the default
        config_files.insert(dirs::HOME.join(env::PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME.as_str()));
    };
    *g = Some(config_files.clone());
    config_files
}

pub(crate) fn system_config_files() -> IndexSet<PathBuf> {
    let mut s = SYSTEM_CONFIG_FILES.lock().unwrap();
    if let Some(s) = &*s {
        return s.clone();
    }
    if let Some(p) = &*env::PAKRO_SYSTEM_CONFIG_FILE {
        return vec![p.clone()].into_iter().collect();
    }
    let config_files = config_files_from_dir(&dirs::SYSTEM_CONFIG);
    *s = Some(config_files.clone());
    config_files
}

/// Config files in a global/system config dir, lowest precedence first: `conf.d`,
/// then `config.toml`/`pakro.toml`, then the `.local` variants.
fn config_files_from_dir(dir: &Path) -> IndexSet<PathBuf> {
    config_files_with_incoming(dir, &Default::default())
}

/// Discover the configuration that would exist after an incoming batch,
/// using the same precedence and environment selection as live discovery.
pub(crate) fn config_files_with_incoming(
    dir: &Path,
    incoming: &std::collections::BTreeSet<PathBuf>,
) -> IndexSet<PathBuf> {
    let mut files = IndexSet::new();
    let conf_dir = dir.join("conf.d");
    let conf_files: std::collections::BTreeSet<PathBuf> = file::ls(&conf_dir)
        .unwrap_or_default()
        .into_iter()
        .chain(
            incoming
                .iter()
                .filter(|p| p.parent() == Some(&conf_dir))
                .cloned(),
        )
        .collect();
    // Folder fragments (`conf.d/<name>/pakro.toml`) are found by their folder, so an
    // incoming batch can introduce one through any of its files.
    let conf_folders: std::collections::BTreeSet<PathBuf> = conf_files
        .iter()
        .filter(|p| p.is_dir())
        .cloned()
        .chain(
            incoming
                .iter()
                .filter_map(|p| p.parent())
                .filter(|folder| folder.parent() == Some(&conf_dir))
                .map(Path::to_path_buf),
        )
        .filter(|folder| {
            folder
                .file_name()
                .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
        })
        .collect();
    let conf_folder_files = |name: &str| {
        conf_folders
            .iter()
            .map(|folder| folder.join(name))
            .collect::<Vec<_>>()
    };
    for p in &conf_files {
        if let Some(file_name) = p.file_name().map(|f| f.to_string_lossy().to_string())
            && !file_name.starts_with(".")
            && file_name.ends_with(".toml")
            && (!env::env_conf_d() || !is_environment_conf_d_file(p))
        {
            warn_on_dotted_conf_d_file(p);
            files.insert(p.clone());
        }
    }
    files.extend(conf_folder_files("pakro.toml"));
    files.extend(conf_folder_files("pakro.local.toml"));
    files.extend([dir.join("config.toml"), dir.join("pakro.toml")]);
    for environment in &*env::PAKRO_ENV_WITH_AUTO {
        if env::env_conf_d() {
            files.extend(conf_d_environment_candidates(
                &conf_files,
                environment,
                false,
            ));
        }
        files.extend(conf_folder_files(&format!("pakro.{environment}.toml")));
        files.extend([
            dir.join(format!("config.{environment}.toml")),
            dir.join(format!("pakro.{environment}.toml")),
        ]);
    }
    files.extend([dir.join("config.local.toml"), dir.join("pakro.local.toml")]);
    for environment in &*env::PAKRO_ENV_WITH_AUTO {
        if env::env_conf_d() {
            files.extend(conf_d_environment_candidates(
                &conf_files,
                environment,
                true,
            ));
        }
        files.extend(conf_folder_files(&format!(
            "pakro.{environment}.local.toml"
        )));
        files.extend([
            dir.join(format!("config.{environment}.local.toml")),
            dir.join(format!("pakro.{environment}.local.toml")),
        ]);
    }
    files
        .into_iter()
        .filter(|p| p.is_file() || incoming.contains(p))
        .collect()
}

fn conf_d_environment_files(dir: &Path, environment: &str, local: bool) -> Vec<PathBuf> {
    conf_d_environment_candidates(
        &file::ls(&dir.join("conf.d"))
            .unwrap_or_default()
            .into_iter()
            .collect(),
        environment,
        local,
    )
}

fn conf_d_environment_candidates(
    candidates: &std::collections::BTreeSet<PathBuf>,
    environment: &str,
    local: bool,
) -> Vec<PathBuf> {
    candidates
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.starts_with('.'))
                && conf_d_file_environment(path) == Some((environment, local))
        })
        .cloned()
        .collect()
}

/// the preferred global config file to write to, or the path where it should be created.
/// Uses first_config_file() to pick the lowest-precedence non-local TOML (i.e., config.toml
/// rather than config.local.toml) so that `pakro use -g` writes to config.toml.
/// See: https://github.com/jdx/mise/discussions/8236
pub fn global_config_path() -> PathBuf {
    let files = global_config_files();
    first_config_file(&files)
        .cloned()
        .or_else(|| env::PAKRO_GLOBAL_CONFIG_FILE.clone())
        .unwrap_or_else(|| dirs::CONFIG.join("config.toml"))
}

/// The shared global config file: where declarations meant for every
/// machine go. `PAKRO_GLOBAL_CONFIG_FILE`, else the global TOML file that
/// already exists (`pakro.toml` when that is what the user keeps), else
/// `config.toml`. An explicitly selected global file is honored even when
/// it is `.local.toml`; implicit selection prefers a shared file.
pub fn global_shared_config_path() -> PathBuf {
    let local = |path: &PathBuf| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".local.toml"))
    };
    // an explicit global file is the one pakro loads: declarations go there
    // (writing TOML into a file that is not TOML fails, rather than landing
    // in a file that is never read)
    if let Some(path) = env::PAKRO_GLOBAL_CONFIG_FILE.clone() {
        return path;
    }
    let files = global_config_files();
    first_config_file(&files)
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && !local(path))
        .cloned()
        .unwrap_or_else(|| dirs::CONFIG.join("config.toml"))
}

/// The preferred system config file to write to, or the path where it should be created.
pub fn system_config_path() -> PathBuf {
    let files = system_config_files();
    first_config_file(&files)
        .cloned()
        .or_else(|| env::PAKRO_SYSTEM_CONFIG_FILE.clone())
        .unwrap_or_else(|| dirs::SYSTEM_CONFIG.join("config.toml"))
}

/// the top-most pakro.toml (local or global)
pub fn top_toml_config() -> Option<PathBuf> {
    load_config_paths(&TOML_CONFIG_FILENAMES, false)
        .iter()
        .find(|p| p.to_string_lossy().ends_with(".toml"))
        .map(|p| p.to_path_buf())
}

pub static ALL_TOML_CONFIG_FILES: Lazy<IndexSet<PathBuf>> = Lazy::new(|| {
    load_config_paths(&TOML_CONFIG_FILENAMES, false)
        .into_iter()
        .collect()
});

/// The lowest-precedence TOML config in the nearest local config directory, or
/// the path where it should be written.
pub fn local_toml_config_path() -> PathBuf {
    static CWD: Lazy<PathBuf> = Lazy::new(|| PathBuf::from("."));
    local_toml_config_path_from_dir(dirs::CWD.as_ref().unwrap_or(&CWD))
}

/// The lowest-precedence TOML config in the nearest directory that has one.
///
/// This matches the write-target rule from the docs: choose the highest-precedence
/// directory first, then the lowest-precedence file inside that directory.
pub(crate) fn local_toml_config_path_from_dir(cwd: &Path) -> PathBuf {
    nearest_local_config_file(cwd, &TOML_CONFIG_FILENAMES)
        .unwrap_or_else(|| cwd.join(&*env::PAKRO_DEFAULT_CONFIG_FILENAME))
}

/// Options for resolving target config file path
#[derive(Debug, Default)]
pub struct ConfigPathOptions {
    pub global: bool,
    pub path: Option<PathBuf>,
    pub env: Option<String>,
    pub cwd: Option<PathBuf>,
    pub prefer_toml: bool,
    pub prevent_home_local: bool,
}

/// Unified config file path resolution for both `pakro use` and `pakro set`
///
/// This function centralizes the logic for determining which config file to target
/// based on various options, ensuring consistent behavior between commands.
pub fn resolve_target_config_path(opts: ConfigPathOptions) -> Result<PathBuf> {
    let cwd = match opts.cwd {
        Some(ref path) => path.clone(),
        None => env::current_dir()?,
    };

    // If path is provided, handle it (file or directory) - explicit paths take precedence.
    // Absolutized once here so every arm returns an absolute path: a relative one would still
    // resolve correctly against the cwd for reads, but it silently disables the monorepo lockfile
    // redirection in `lockfile::lockfile_path_for_config`, which compares against an absolute root.
    if let Some(ref path) = opts.path {
        let path = path.absolutize()?.to_path_buf();
        if path.is_file() {
            return Ok(path);
        } else if path.is_dir() {
            let resolved = config_file_in_dir(&path);
            if opts.prefer_toml && !resolved.to_string_lossy().ends_with(".toml") {
                // For TOML-only commands, ensure we get a TOML file in the specified directory
                return Ok(path.join(&*env::PAKRO_DEFAULT_CONFIG_FILENAME));
            }
            return Ok(resolved);
        } else {
            // Path doesn't exist yet, return it as-is
            return Ok(path);
        }
    }

    // If global flag is set and no explicit path provided, use global config
    if opts.global {
        return Ok(global_config_path());
    }

    // If env-specific config is requested
    if let Some(ref env_name) = opts.env {
        let dotfile_path = cwd.join(format!(".pakro.{}.toml", env_name));
        if dotfile_path.exists() {
            return Ok(dotfile_path);
        } else {
            return Ok(cwd.join(format!("pakro.{}.toml", env_name)));
        }
    }

    // If we're in HOME directory and prevent_home_local is true, use global config
    if opts.prevent_home_local && env::in_home_dir() {
        return Ok(global_config_path());
    }

    // Default: determine based on current directory
    if opts.prefer_toml {
        // For TOML-only commands, choose the nearest config directory and write
        // to its lowest-precedence TOML file.
        Ok(local_toml_config_path_from_dir(&cwd))
    } else {
        // For pakro use, use existing config_file_from_dir logic which respects ASDF compat
        Ok(config_file_from_dir(&cwd))
    }
}

/// Whether `err` is the untrusted-config error for a config the user has just
/// declined to trust, so the declined config is now ignored.
fn declined_trust_prompt(path: &Path, err: &eyre::Report) -> bool {
    matches!(
        err.downcast_ref::<crate::errors::Error>(),
        Some(crate::errors::Error::UntrustedConfig(_))
    ) && (config_file::is_ignored(&config_trust_root(path)) || config_file::is_ignored(path))
}

static SKIP_UNTRUSTED_CONFIGS: AtomicBool = AtomicBool::new(false);
static SKIPPED_UNTRUSTED_CONFIGS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Makes config loading skip untrusted config files instead of failing, so the
/// remaining (e.g. global) configs still apply. Used by `hook-env`, where entering
/// a directory is not an explicit request to run its config.
pub fn skip_untrusted_configs() {
    SKIP_UNTRUSTED_CONFIGS.store(true, Ordering::Relaxed);
}

/// Untrusted config files skipped so far when [`skip_untrusted_configs`] is active.
pub fn skipped_untrusted_configs() -> Vec<PathBuf> {
    SKIPPED_UNTRUSTED_CONFIGS.lock().unwrap().clone()
}

async fn load_all_config_files(
    config_filenames: &[PathBuf],
    idiomatic_filenames: &BTreeMap<String, Vec<String>>,
) -> Result<ConfigMap> {
    backend::load_tools().await?;
    let mut groups = config_filenames
        .iter()
        .unique()
        .filter(|path| !path.is_dir())
        .fold(
            IndexMap::<PathBuf, Vec<&PathBuf>>::new(),
            |mut groups, path| {
                groups
                    .entry(config_trust_root(path))
                    .or_default()
                    .push(path);
                groups
            },
        )
        .into_iter()
        .collect_vec();
    // Resolve trusted ancestor roots before parsing descendants so a stale
    // monorepo marker cannot authorize a child after an overlay disables it.
    groups.sort_by_key(|(root, _)| root.components().count());

    let mut parsed = HashMap::new();
    for (_, paths) in groups {
        let mut root_configs = ConfigMap::new();
        for f in paths {
            let cf = match parse_config_file(f, idiomatic_filenames).await {
                Ok(cfg) => cfg,
                // Declining the trust prompt records an ignore marker, which
                // makes the config skipped on every later load. Skip it for
                // the run that was asked too, rather than failing it.
                Err(err) if declined_trust_prompt(f, &err) => {
                    debug!(
                        "skipping config file ignored at the trust prompt: {}",
                        display_path(f)
                    );
                    continue;
                }
                Err(err)
                    if SKIP_UNTRUSTED_CONFIGS.load(Ordering::Relaxed)
                        && matches!(
                            err.downcast_ref::<crate::errors::Error>(),
                            Some(crate::errors::Error::UntrustedConfig(_))
                        ) =>
                {
                    debug!("skipping untrusted config file: {}", display_path(f));
                    let mut skipped = SKIPPED_UNTRUSTED_CONFIGS.lock().unwrap();
                    if !skipped.contains(f) {
                        skipped.push(f.clone());
                    }
                    continue;
                }
                Err(err) => {
                    return Err(err.wrap_err(format!(
                        "error parsing config file: {}",
                        style::ebold(display_path(f))
                    )));
                }
            };
            if !readonly_invocation()
                && let Err(err) = Tracker::track(f)
            {
                warn!("tracking config: {err:#}");
            }
            parsed.insert(f.clone(), cf.clone());
            root_configs.insert(f.clone(), cf);
        }

        if !readonly_invocation()
            && let Some(project_root) = root_configs.values().find_map(|cf| cf.project_root())
        {
            let enabled = resolve_monorepo_config_at_root(&project_root, &root_configs).is_some();
            if let Some(source) = root_configs.values().next()
                && let Err(err) = config_file::set_monorepo_root_marker(source.get_path(), enabled)
            {
                warn!("failed to synchronize monorepo root marker: {err:#}");
            }
        }
    }

    Ok(config_filenames
        .iter()
        .filter_map(|path| parsed.remove(path).map(|cf| (path.clone(), cf)))
        .collect())
}

/// Load config files from an explicit list of paths.
/// Accepts a pre-computed idiomatic filenames map to avoid redundant computation
/// when called after load_config_hierarchy_from_dir.
pub async fn load_config_files_from_paths(
    config_paths: &[PathBuf],
    idiomatic_filenames: &BTreeMap<String, Vec<String>>,
) -> Result<ConfigMap> {
    backend::load_tools().await?;
    let mut config_map = ConfigMap::default();

    for f in config_paths.iter().unique() {
        if f.is_dir() {
            continue;
        }
        let cf = match parse_config_file(f, idiomatic_filenames).await {
            Ok(cfg) => cfg,
            Err(err) => {
                return Err(err.wrap_err(format!(
                    "error parsing config file: {}",
                    style::ebold(display_path(f))
                )));
            }
        };

        config_map.insert(f.clone(), cf);
    }
    Ok(config_map)
}

async fn parse_config_file(
    f: &PathBuf,
    idiomatic_filenames: &BTreeMap<String, Vec<String>>,
) -> Result<Arc<dyn ConfigFile>> {
    warn_on_dotted_conf_d_file(f);
    let plugins = matching_idiomatic_tools(f, idiomatic_filenames);
    if plugins.is_empty() {
        config_file::parse(f).await
    } else {
        trace!("idiomatic version file: {}", display_path(f));
        let tools = backend::list()
            .into_iter()
            .filter(|backend| plugins.contains(&backend.to_string()))
            .collect::<Vec<_>>();
        IdiomaticVersionFile::parse(f.into(), tools)
            .await
            .map(|f| Arc::new(f) as Arc<dyn ConfigFile>)
    }
}

fn matching_idiomatic_tools(
    path: &Path,
    idiomatic_filenames: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    config_file::matching_idiomatic_filenames(path, idiomatic_filenames.keys().map(String::as_str))
        .into_iter()
        .flat_map(|filename| idiomatic_filenames.get(filename).into_iter().flatten())
        .unique()
        .cloned()
        .collect()
}

fn load_aliases(config_files: &ConfigMap) -> Result<AliasMap> {
    let mut aliases: AliasMap = AliasMap::new();

    for config_file in config_files.values() {
        for (plugin, plugin_aliases) in config_file.aliases()? {
            let alias = aliases.entry(plugin).or_default();
            if let Some(full) = plugin_aliases.backend {
                alias.backend = Some(full);
            }
            for (from, to) in plugin_aliases.versions {
                alias.versions.insert(from, to);
            }
        }
    }
    trace!("load_aliases: {}", aliases.len());

    Ok(aliases)
}

fn load_shell_aliases(config_files: &ConfigMap) -> Result<EnvWithSources> {
    let mut shell_aliases: EnvWithSources = EnvWithSources::new();

    // In safe mode, shell aliases from project (non-global) config are ignored.
    // They are emitted through `hook-env` into the interactive shell, so an
    // untrusted config could otherwise redefine commands (e.g. `ls`, `git`) —
    // a code-execution vector now that safe mode loads untrusted config without
    // a trust prompt. Global/system config is operator-owned and still applies.
    let safe_mode = Settings::safe_mode();

    // Iterate in reverse order (global -> local) so child directories override parent configs
    for config_file in config_files.values().rev() {
        let path = config_file.get_path().to_path_buf();
        if safe_mode && !is_global_config(&path) {
            continue;
        }
        for (name, cmd) in config_file.shell_aliases()? {
            shell_aliases.insert(name, (cmd, path.clone()));
        }
    }
    trace!("load_shell_aliases: {}", shell_aliases.len());

    Ok(shell_aliases)
}

/// Load command wrappers from global through project scope.
pub fn load_command_wrappers<'a>(
    config_files: &ConfigMap,
    tools: impl IntoIterator<Item = &'a crate::toolset::ToolRequest>,
) -> Result<IndexMap<String, CommandWrapper>> {
    let mut wrappers = IndexMap::new();
    let safe_mode = Settings::safe_mode();
    for config_file in config_files.values().rev() {
        if safe_mode && !is_global_config(config_file.get_path()) {
            continue;
        }
        for (name, wrapper) in config_file.command_wrappers()? {
            if wrapper.command().trim().is_empty() {
                bail!("command wrapper for {name:?} must have a non-blank command");
            }
            wrappers.insert(name, wrapper);
        }
    }
    command_wrapper::add_rust_wrapper(&mut wrappers, tools)?;
    Ok(wrappers)
}

/// Expands a `[plugins]` value to a URL, keeping any `#ref` outside the
/// expansion so `owner/repo#v1` doesn't become `…/repo#v1.git`.
fn plugin_entry_to_url(entry: &str) -> String {
    match entry.split_once('#') {
        Some((source, git_ref)) => format!("{}#{git_ref}", registry::full_to_url(source)),
        None => registry::full_to_url(entry),
    }
}

fn load_plugins(config_files: &ConfigMap) -> Result<HashMap<String, String>> {
    let mut plugins = HashMap::new();
    for config_file in config_files.values() {
        for (plugin, url) in config_file.plugins()? {
            PluginType::from_plugin_config(&plugin)?;
            if url.starts_with("packslip:") {
                bail!("Packslip plugin archives have been removed: {plugin}");
            }
            plugins.insert(plugin.clone(), url.clone());
        }
    }
    trace!("load_plugins: {}", plugins.len());
    Ok(plugins)
}

pub(crate) async fn resolve_vars_from_config_files(
    config: &Arc<Config>,
    config_files: &ConfigMap,
) -> Result<EnvResults> {
    let entries = config_files
        .iter()
        .rev()
        .map(|(source, cf)| {
            cf.vars_entries()
                .map(|ee| ee.into_iter().map(|e| (e, source.clone())))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();

    EnvResults::resolve(
        config,
        config.tera_ctx.clone(),
        &env::PRISTINE_ENV,
        entries,
        EnvResolveOptions {
            vars: true,
            tools: ToolsFilter::NonToolsOnly,
            warn_on_missing_required: false,
        },
    )
    .await
}

async fn load_vars(config: &Arc<Config>) -> Result<EnvResults> {
    time!("load_vars start");
    let vars_results = resolve_vars_from_config_files(config, &config.config_files).await?;
    time!("load_vars done");
    if log::log_enabled!(log::Level::Trace) {
        trace!("{vars_results:#?}");
    } else if !vars_results.is_empty() {
        debug!("{vars_results:?}");
    }
    Ok(vars_results)
}

impl Debug for Config {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let config_files = self
            .config_files
            .iter()
            .map(|(p, _)| display_path(p))
            .collect::<Vec<_>>();
        let mut s = f.debug_struct("Config");
        s.field("Config Files", &config_files);
        if let Some(env) = self.env_maybe()
            && !env.is_empty()
        {
            s.field("Env", &env);
            // s.field("Env Sources", &self.env_sources);
        }
        if let Some(env_results) = self.env.get() {
            if !env_results.env_files.is_empty() {
                s.field("Path Dirs", &env_results.env_paths);
            }
            if !env_results.env_scripts.is_empty() {
                s.field("Scripts", &env_results.env_scripts);
            }
            if !env_results.env_files.is_empty() {
                s.field("Files", &env_results.env_files);
            }
        }
        if !self.aliases.is_empty() {
            s.field("Aliases", &self.aliases);
        }
        s.finish()
    }
}
pub async fn rebuild_shims_and_runtime_symlinks(
    config: &Arc<Config>,
    ts: &Toolset,
    new_versions: &[ToolVersion],
    lockfile_update_mode: lockfile::LockfileUpdateMode,
) -> Result<()> {
    let changed_install_paths = new_versions
        .iter()
        .map(ToolVersion::install_path)
        .collect::<Vec<_>>();
    rebuild_shims_and_runtime_symlinks_for_changes(
        config,
        ts,
        new_versions,
        &changed_install_paths,
        lockfile_update_mode,
    )
    .await?;
    Ok(())
}

/// Reconcile runtime links and shim farms after versions have been removed.
/// Removed versions determine which physical farm changed, but are not passed
/// to lockfile update logic as newly installed versions.
pub async fn rebuild_shims_and_runtime_symlinks_after_removal(
    config: &Arc<Config>,
    ts: &Toolset,
    removed_install_paths: &[PathBuf],
) -> Result<()> {
    rebuild_shims_and_runtime_symlinks_for_changes(
        config,
        ts,
        &[],
        removed_install_paths,
        lockfile::LockfileUpdateMode::Normal,
    )
    .await
}

/// Reconcile a shim farm after a configuration-only change, where there is no
/// installed or removed tool path from which to infer ownership.
pub async fn rebuild_shims_and_runtime_symlinks_for_scope(
    config: &Arc<Config>,
    ts: &Toolset,
    scope: shims::ShimScope,
) -> Result<()> {
    let system_installs = Settings::get().system_installs_dir().to_path_buf();
    let changed_install_paths = match scope {
        shims::ShimScope::User => vec![dirs::INSTALLS.to_path_buf()],
        shims::ShimScope::System => vec![system_installs],
        shims::ShimScope::Both => vec![dirs::INSTALLS.to_path_buf(), system_installs],
    };
    rebuild_shims_and_runtime_symlinks_for_changes(
        config,
        ts,
        &[],
        &changed_install_paths,
        lockfile::LockfileUpdateMode::Normal,
    )
    .await
}

async fn rebuild_shims_and_runtime_symlinks_for_changes(
    config: &Arc<Config>,
    ts: &Toolset,
    new_versions: &[ToolVersion],
    changed_install_paths: &[PathBuf],
    lockfile_update_mode: lockfile::LockfileUpdateMode,
) -> Result<()> {
    measure!("rebuilding runtime symlinks", {
        runtime_symlinks::rebuild_for_toolset(config, ts)
            .await
            .wrap_err("failed to rebuild runtime symlinks")?;
    });
    measure!("rebuilding shims", {
        let system_installs = Settings::get().system_installs_dir().to_path_buf();
        let installs_collocated = file::storage_paths_eq(&system_installs, &dirs::INSTALLS);
        let system_changed = installs_collocated
            || changed_install_paths
                .iter()
                .any(|path| path.starts_with(&system_installs));
        let user_changed = installs_collocated
            || changed_install_paths.is_empty()
            || changed_install_paths
                .iter()
                .any(|path| !path.starts_with(&system_installs));
        if user_changed {
            shims::reshim_for(config, ts, false, shims::ShimScope::User)
                .await
                .wrap_err("failed to rebuild user shims")?;
        }
        if system_changed {
            shims::reshim_for(config, ts, false, shims::ShimScope::System)
                .await
                .wrap_err("failed to rebuild system shims")?;
        }
    });
    if Settings::get().generate_lockfiles() {
        return Ok(());
    }
    lockfile::migrate_monorepo_lockfiles(config, false)?;
    // Snapshot the lockfiles' platform keys BEFORE update_lockfiles writes
    // current-platform entries — auto-lock uses this to tell a curated lockfile
    // (existing entries are authoritative) from a fresh one (expand to common).
    let pre_install_platforms = if new_versions.is_empty() {
        Default::default()
    } else {
        lockfile::snapshot_pre_install_platforms(config, ts, new_versions)
    };
    let has_deferred_provenance = measure!("updating lockfiles", {
        lockfile::update_lockfiles(config, ts, new_versions, lockfile_update_mode)
            .wrap_err("failed to update lockfiles")?
    });
    if !new_versions.is_empty() || has_deferred_provenance {
        measure!("auto-locking platforms", {
            lockfile::auto_lock_new_versions(
                config,
                ts,
                new_versions,
                &pre_install_platforms,
                lockfile_update_mode,
            )
            .await
            .wrap_err("failed to auto-lock platforms")?;
        });
    }

    Ok(())
}

fn expand_config_root_dirs(root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    expand_config_roots_inner(root, patterns, None)
}

fn expand_config_roots_with_filenames(
    root: &Path,
    patterns: &[String],
    filenames: &[String],
) -> Result<Vec<PathBuf>> {
    expand_config_roots_inner(root, patterns, Some(filenames))
}

fn expand_config_roots_inner(
    root: &Path,
    patterns: &[String],
    filenames: Option<&[String]>,
) -> Result<Vec<PathBuf>> {
    expand_config_roots_inner_for("monorepo", root, patterns, filenames, false)
}

/// Expands safe, single-level config-root patterns for a named config section.
fn expand_config_roots_inner_for(
    section: &str,
    root: &Path,
    patterns: &[String],
    filenames: Option<&[String]>,
    canonicalize_results: bool,
) -> Result<Vec<PathBuf>> {
    let mut subdirs = Vec::new();
    let canonical_root = match root.canonicalize() {
        Ok(root) => root,
        Err(err) => {
            warn!(
                "[{section}].config_roots: failed to resolve config root {}: {err}",
                root.display()
            );
            return Ok(subdirs);
        }
    };

    for pattern in patterns {
        // Reject absolute paths and parent directory escapes.
        if Path::new(pattern).components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            warn!(
                "[{section}].config_roots: '{}' must be a relative path within the config root",
                pattern
            );
            continue;
        }

        // Reject recursive glob patterns (**)
        if pattern.contains("**") {
            warn!(
                "[{section}].config_roots: recursive glob '**' not supported in '{}', use single-level '*' instead",
                pattern
            );
            continue;
        }

        if pattern.contains('*') {
            // Single-level glob expansion
            let full_pattern = root.join(pattern);
            match glob::glob(&full_pattern.to_string_lossy()) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(path) => {
                                let canonical = match path.canonicalize() {
                                    Ok(path) => path,
                                    Err(err) => {
                                        warn!(
                                            "[{section}].config_roots: failed to resolve glob match {}: {err}",
                                            path.display()
                                        );
                                        continue;
                                    }
                                };
                                if !canonical.starts_with(&canonical_root) {
                                    warn!(
                                        "[{section}].config_roots: glob matched path outside config root: {}",
                                        path.display()
                                    );
                                    continue;
                                }
                                if canonical.is_dir()
                                    && filenames.is_none_or(|filenames| {
                                        has_pakro_config_with_filenames(&canonical, filenames)
                                    })
                                {
                                    subdirs.push(if canonicalize_results {
                                        canonical
                                    } else {
                                        path
                                    });
                                }
                            }
                            Err(e) => {
                                warn!("[{section}].config_roots glob error: {e}");
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("[{section}].config_roots invalid glob pattern '{pattern}': {e}");
                }
            }
        } else {
            // Explicit path
            let path = root.join(pattern);
            let canonical = match path.canonicalize() {
                Ok(path) => path,
                Err(_) => {
                    warn!("[{section}].config_roots: '{}' does not exist", pattern);
                    continue;
                }
            };
            if !canonical.starts_with(&canonical_root) {
                warn!(
                    "[{section}].config_roots: '{}' resolves outside config root",
                    pattern
                );
                continue;
            }
            if canonical.is_dir() {
                if filenames
                    .is_none_or(|filenames| has_pakro_config_with_filenames(&canonical, filenames))
                {
                    subdirs.push(if canonicalize_results {
                        canonical
                    } else {
                        path
                    });
                } else {
                    warn!(
                        "[{section}].config_roots: '{}' has no pakro config file",
                        pattern
                    );
                }
            } else {
                warn!("[{section}].config_roots: '{}' is not a directory", pattern);
            }
        }
    }

    Ok(subdirs)
}

fn has_pakro_config_with_filenames(dir: &Path, filenames: &[String]) -> bool {
    has_config_file_with_filenames(dir, filenames)
}

fn has_config_file_with_filenames(dir: &Path, filenames: &[String]) -> bool {
    filenames.iter().any(|f| {
        if is_glob_pattern(f) {
            !load_config_glob(dir, f).is_empty()
        } else {
            dir.join(f).exists()
        }
    })
}
pub fn is_glob_pattern(pattern: &str) -> bool {
    // Check for unescaped glob metacharacters: *, ?, [, ], {, }
    // Note: This is a simple check that may have false positives with escaped chars,
    // but glob() will handle those correctly
    pattern.contains('*')
        || pattern.contains('?')
        || pattern.contains('[')
        || pattern.contains(']')
        || pattern.contains('{')
        || pattern.contains('}')
}
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use insta::assert_debug_snapshot;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs::{self, File};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_plugin_entry_to_url_keeps_ref_outside_shorthand_expansion() {
        assert_eq!(
            plugin_entry_to_url("owner/repo#v1.2.0"),
            "https://github.com/owner/repo.git#v1.2.0"
        );
        assert_eq!(
            plugin_entry_to_url("owner/repo"),
            "https://github.com/owner/repo.git"
        );
        assert_eq!(
            plugin_entry_to_url("https://example.com/repo.git#main"),
            "https://example.com/repo.git#main"
        );
    }

    #[tokio::test]
    async fn test_load() {
        let config = Config::reset().await.unwrap();
        assert_debug_snapshot!(config);
    }

    #[tokio::test]
    async fn test_reset_reloads_settings() {
        let _settings = crate::test::SettingsGuard::lock();
        Settings::reset(None);
        let before = Settings::get();

        Config::reset().await.unwrap();
        let after = Settings::get();

        assert!(!Arc::ptr_eq(&before, &after));
    }

    #[test]
    fn test_config_set_contains_matches_symlinked_prefix() {
        // Regression for https://github.com/jdx/mise/discussions/10483:
        // a global config discovered via a symlinked path prefix (e.g. Fedora
        // Atomic's `/home` -> `/var/home`) must still be recognized as global.
        let tmp = TempDir::new().unwrap();
        let real_dir = tmp.path().join("real");
        fs::create_dir_all(&real_dir).unwrap();
        let real_file = real_dir.join("config.toml");
        fs::write(&real_file, "").unwrap();
        // Symlinked alias of the dir, mimicking `/home` -> `/var/home`.
        let link_dir = tmp.path().join("link");
        std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();
        let aliased_file = link_dir.join("config.toml");

        let mut set = IndexSet::new();
        set.insert(real_file.clone());

        // exact match (fast path)
        assert!(config_set_contains(&set, &real_file));
        // same file reached via a symlinked prefix — false before the fix
        assert!(config_set_contains(&set, &aliased_file));
        // an unrelated path is not a member
        assert!(!config_set_contains(&set, &real_dir.join("other.toml")));
    }

    #[test]
    fn test_has_pakro_config_with_glob_filenames() -> Result<()> {
        let tmp = TempDir::new()?;
        for pattern in [
            ".config/pakro/conf.d/*.toml",
            ".pakro/conf.d/*.toml",
            "pakro/conf.d/*.toml",
        ] {
            let confd = tmp.path().join(pattern.trim_end_matches("/*.toml"));
            fs::create_dir_all(&confd)?;
            fs::write(confd.join("tools.toml"), "[tools]\n")?;

            assert!(has_pakro_config_with_filenames(
                tmp.path(),
                &[pattern.to_string()]
            ));
        }

        Ok(())
    }

    #[test]
    fn test_project_visible_pakro_conf_d_precedence() -> Result<()> {
        let tmp = TempDir::new()?;
        let confd = tmp.path().join("pakro/conf.d");
        fs::create_dir_all(&confd)?;
        fs::write(confd.join("01-base.toml"), "[env]\nORDER = 'base'\n")?;
        fs::write(
            confd.join("02-override.toml"),
            "[env]\nORDER = 'override'\n",
        )?;
        fs::write(confd.join(".hidden.toml"), "[env]\nORDER = 'hidden'\n")?;
        fs::write(
            tmp.path().join("pakro/config.toml"),
            "[env]\nORDER = 'config'\n",
        )?;

        let filenames = vec![
            "pakro/conf.d/*.toml".to_string(),
            "pakro/config.toml".to_string(),
        ];
        let paths = config_paths_in_dir_with_filenames(tmp.path(), &filenames);
        let relative = paths
            .iter()
            .map(|path| path.strip_prefix(tmp.path()).unwrap())
            .collect_vec();
        assert_eq!(
            relative,
            vec![
                Path::new("pakro/config.toml"),
                Path::new("pakro/conf.d/02-override.toml"),
                Path::new("pakro/conf.d/01-base.toml"),
            ]
        );

        Ok(())
    }

    #[test]
    fn test_project_conf_d_dotted_fragments_remain_unconditional() -> Result<()> {
        let tmp = TempDir::new()?;
        let confd = tmp.path().join(".pakro/conf.d");
        fs::create_dir_all(&confd)?;
        for filename in [
            "01-base.toml",
            "02-local.local.toml",
            "03-dev.dev.toml",
            "04-dev-local.dev.local.toml",
            "05-ci.ci.toml",
        ] {
            fs::write(confd.join(filename), "[env]\n")?;
        }

        let filenames = vec![".pakro/conf.d/*.toml".to_string()];
        let paths = config_paths_in_dir_with_filenames(tmp.path(), &filenames);
        let relative = paths
            .iter()
            .map(|path| path.strip_prefix(tmp.path()).unwrap())
            .collect_vec();
        assert_eq!(
            relative,
            vec![
                Path::new(".pakro/conf.d/05-ci.ci.toml"),
                Path::new(".pakro/conf.d/04-dev-local.dev.local.toml"),
                Path::new(".pakro/conf.d/03-dev.dev.toml"),
                Path::new(".pakro/conf.d/02-local.local.toml"),
                Path::new(".pakro/conf.d/01-base.toml"),
            ]
        );

        assert!(is_environment_conf_d_file(&confd.join("03-dev.dev.toml")));
        assert_eq!(
            conf_d_file_environment(&confd.join("03-dev.dev.toml")),
            Some(("dev", false))
        );
        assert!(is_environment_conf_d_file(
            &confd.join("04-dev-local.dev.local.toml")
        ));
        assert_eq!(
            conf_d_file_environment(&confd.join("module.qa.prod.local.toml")),
            Some(("qa.prod", true))
        );
        assert!(!is_environment_conf_d_file(
            &confd.join("02-local.local.toml")
        ));

        Ok(())
    }

    #[test]
    fn test_env_config_patterns() {
        assert_eq!(
            env_config_patterns_with_conf_d("linux", true),
            vec![
                ".config/pakro/conf.d/*.linux.toml",
                ".config/pakro/conf.d/*/pakro.linux.toml",
                ".config/pakro/config.linux.toml",
                ".config/pakro.linux.toml",
                "pakro/conf.d/*.linux.toml",
                "pakro/conf.d/*/pakro.linux.toml",
                "pakro/config.linux.toml",
                "pakro.linux.toml",
                ".pakro/conf.d/*.linux.toml",
                ".pakro/conf.d/*/pakro.linux.toml",
                ".pakro/config.linux.toml",
                ".pakro.linux.toml",
                ".config/pakro/conf.d/*.linux.local.toml",
                ".config/pakro/conf.d/*/pakro.linux.local.toml",
                ".config/pakro/config.linux.local.toml",
                ".config/pakro.linux.local.toml",
                "pakro/conf.d/*.linux.local.toml",
                "pakro/conf.d/*/pakro.linux.local.toml",
                "pakro/config.linux.local.toml",
                "pakro.linux.local.toml",
                ".pakro/conf.d/*.linux.local.toml",
                ".pakro/conf.d/*/pakro.linux.local.toml",
                ".pakro/config.linux.local.toml",
                ".pakro.linux.local.toml",
            ]
        );
        // Folder fragments are new, so their environment files are never
        // subject to the env_conf_d migration.
        assert!(
            env_config_patterns_with_conf_d("linux", false)
                .iter()
                .filter(|pattern| pattern.contains("conf.d"))
                .all(|pattern| pattern.contains("conf.d/*/pakro."))
        );
    }

    #[test]
    fn test_glob_skips_patterns_under_missing_directories() -> Result<()> {
        let missing = TempDir::new()?;
        assert!(glob(missing.path(), ".config/pakro/conf.d/*.toml")?.is_empty());
        assert!(glob(missing.path(), ".pakro/conf.d/*/pakro.toml")?.is_empty());

        // glob caches by directory and pattern, so the matching case needs a
        // directory of its own.
        let tmp = TempDir::new()?;
        let confd = tmp.path().join(".pakro/conf.d");
        fs::create_dir_all(confd.join("node"))?;
        fs::write(confd.join("tools.toml"), "")?;
        fs::write(confd.join("node/pakro.toml"), "")?;
        assert_eq!(
            glob(tmp.path(), ".pakro/conf.d/*.toml")?,
            vec![confd.join("tools.toml")]
        );
        assert_eq!(
            glob(tmp.path(), ".pakro/conf.d/*/pakro.toml")?,
            vec![confd.join("node/pakro.toml")]
        );
        // A malformed pattern is an error even when its directory is missing.
        assert!(glob(missing.path(), "missing/[unclosed/*.toml").is_err());
        // A pattern with no wildcard names a file, not a directory to check.
        fs::write(tmp.path().join("pakro.toml"), "")?;
        assert_eq!(
            glob(tmp.path(), "pakro.toml")?,
            vec![tmp.path().join("pakro.toml")]
        );
        Ok(())
    }

    #[test]
    fn test_env_config_patterns_escape_glob_characters() -> Result<()> {
        let tmp = TempDir::new()?;
        let confd = tmp.path().join(".pakro/conf.d");
        fs::create_dir_all(&confd)?;
        fs::write(confd.join("tools.qa*.toml"), "[env]\n")?;
        fs::write(confd.join("tools.qa1.toml"), "[env]\n")?;

        let pattern = env_config_patterns_with_conf_d("qa*", true)
            .into_iter()
            .find(|pattern| pattern.starts_with(".pakro/conf.d/") && !pattern.contains(".local."))
            .unwrap();
        let matches = glob(tmp.path(), &pattern)?;
        assert_eq!(matches, vec![confd.join("tools.qa*.toml")]);

        Ok(())
    }

    #[test]
    fn test_env_config_patterns_with_non_star_glob_characters() -> Result<()> {
        for env_name in ["qa?", "qa[1]", "qa]"] {
            let tmp = TempDir::new()?;
            let path = tmp.path().join(format!("pakro.{env_name}.toml"));
            fs::write(&path, "[env]\n")?;

            let patterns = env_config_patterns(env_name);
            assert!(config_paths_in_dir_with_filenames(tmp.path(), &patterns).contains(&path));
            assert!(has_config_file_with_filenames(tmp.path(), &patterns));
        }

        Ok(())
    }

    #[test]
    fn test_should_warn_auto_env() {
        let v = |s: &str| versions::Versioning::new(s).unwrap();
        // before the warning window: never warn
        assert!(!should_warn_auto_env(&v("2026.6.2"), None, false));
        // inside the warning window: warn only when the user hasn't decided
        assert!(should_warn_auto_env(&v("2026.12.0"), None, false));
        assert!(should_warn_auto_env(&v("2027.5.9"), None, false));
        assert!(!should_warn_auto_env(&v("2026.12.0"), Some(false), false));
        assert!(!should_warn_auto_env(&v("2026.12.0"), Some(true), true));
        // auto envs already active (e.g. opted in): nothing to warn about
        assert!(!should_warn_auto_env(&v("2026.12.0"), None, true));
        // default flipped on: warning is obsolete
        assert!(!should_warn_auto_env(&v("2027.6.0"), None, true));
        assert!(!should_warn_auto_env(&v("2027.6.0"), Some(false), false));
    }

    #[test]
    fn test_monorepo_lockfile_rollout() {
        let v = |s: &str| versions::Versioning::new(s).unwrap();

        assert!(!monorepo_lockfile_enabled_for_version(
            &v("2026.6.15"),
            None
        ));
        assert!(monorepo_lockfile_enabled_for_version(
            &v("2026.6.15"),
            Some(true)
        ));
        assert!(!monorepo_lockfile_enabled_for_version(
            &v("2027.6.0"),
            Some(false)
        ));
        assert!(monorepo_lockfile_enabled_for_version(&v("2027.6.0"), None));

        assert!(!should_warn_monorepo_lockfile_default(
            &v("2026.11.9"),
            None,
            true,
            true
        ));
        assert!(should_warn_monorepo_lockfile_default(
            &v("2026.12.0"),
            None,
            true,
            true
        ));
        assert!(should_warn_monorepo_lockfile_default(
            &v("2027.5.9"),
            None,
            true,
            true
        ));
        assert!(!should_warn_monorepo_lockfile_default(
            &v("2026.12.0"),
            None,
            false,
            true
        ));
        assert!(!should_warn_monorepo_lockfile_default(
            &v("2026.12.0"),
            None,
            true,
            false
        ));
        assert!(!should_warn_monorepo_lockfile_default(
            &v("2026.12.0"),
            Some(true),
            true,
            true
        ));
        assert!(!should_warn_monorepo_lockfile_default(
            &v("2026.12.0"),
            Some(false),
            true,
            true
        ));
        assert!(!should_warn_monorepo_lockfile_default(
            &v("2027.6.0"),
            None,
            true,
            true
        ));
    }

    #[tokio::test]
    async fn test_get_tool_opts_with_overrides_keeps_inline_opts_with_config_entry() -> Result<()> {
        crate::toolset::install_state::init().await?;

        let source = crate::toolset::ToolSource::PakroToml(PathBuf::from("pakro.toml"));
        let resolved_ba = Arc::new(BackendArg::from("github:jdx/mise-test-fixtures"));
        let config_opts =
            crate::toolset::parse_tool_options("api_url=https://config.example/api/v3,foo=config");
        let mut trs = ToolRequestSet::new();
        trs.add_version(
            crate::toolset::ToolRequest::new_with_options(
                resolved_ba,
                "1.0.0",
                config_opts,
                source.clone(),
            )?,
            &source,
        );

        let mut repo_urls = HashMap::new();
        repo_urls.insert(
            "tiny".to_string(),
            "github:jdx/mise-test-fixtures".to_string(),
        );
        let config = Config {
            tera_ctx: BASE_CONTEXT.clone(),
            config_files: Default::default(),

            env: OnceCell::new(),
            env_with_sources: OnceCell::new(),
            shorthands: get_shorthands(&Settings::get()),
            hooks: OnceCell::new(),

            tool_request_set: OnceCell::new(),
            toolset: OnceCell::new(),
            all_aliases: Default::default(),
            aliases: Default::default(),
            project_root: Default::default(),
            repo_urls,
            shell_aliases: Default::default(),
            tera_files: Default::default(),
            vars: Default::default(),
            vars_results: OnceCell::new(),
            lockfile_discovery: Default::default(),
            readonly_receipt: None,
        };
        config.tool_request_set.set(trs).ok();
        let config = Arc::new(config);
        let ba = Arc::new(BackendArg::new_raw(
            "tiny".to_string(),
            Some("github:jdx/mise-test-fixtures".to_string()),
            "jdx/mise-test-fixtures".to_string(),
            Some(crate::toolset::parse_tool_options(
                "api_url=https://inline.example/api/v3",
            )),
            crate::args::BackendResolution::new(true),
        ));

        let opts = config.get_tool_opts_with_overrides(&ba).await?;

        assert_eq!(opts.get("api_url"), Some("https://inline.example/api/v3"));
        assert_eq!(opts.get("foo"), Some("config"));
        Ok(())
    }

    #[tokio::test]
    async fn test_get_tool_opts_with_overrides_keeps_inline_opts_without_config_entry() -> Result<()>
    {
        let config = Config::reset().await?;
        let ba = Arc::new(BackendArg::from(
            "tiny[api_url=https://inline.example/api/v3]",
        ));

        let opts = config.get_tool_opts_with_overrides(&ba).await?;

        assert_eq!(opts.get("api_url"), Some("https://inline.example/api/v3"));
        Ok(())
    }

    #[tokio::test]
    async fn backend_aliases_keep_their_tool_spelling() -> Result<()> {
        crate::backend::load_tools().await?;
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("pakro.toml");
        fs::write(
            &path,
            r#"
[alias."cargo:ripgrep"]
versions = { stable = "14.1.1" }
[alias.formatter]
backend = "cargo:ripgrep"
"#,
        )?;
        let mut files: ConfigMap = Default::default();
        files.insert(
            path.clone(),
            Arc::new(config_file::pakro_toml::PakroToml::from_file(&path)?),
        );
        let all_aliases = load_aliases(&files)?;
        assert_eq!(
            all_aliases["formatter"].backend.as_deref(),
            Some("cargo:ripgrep")
        );
        let config = Config {
            tera_ctx: BASE_CONTEXT.clone(),
            config_files: Default::default(),

            env: OnceCell::new(),
            env_with_sources: OnceCell::new(),
            shorthands: get_shorthands(&Settings::get()),
            hooks: OnceCell::new(),

            tool_request_set: OnceCell::new(),
            toolset: OnceCell::new(),
            all_aliases,
            aliases: Default::default(),
            project_root: Default::default(),
            repo_urls: Default::default(),
            shell_aliases: Default::default(),
            tera_files: Default::default(),
            vars: Default::default(),
            vars_results: OnceCell::new(),
            lockfile_discovery: Default::default(),
            readonly_receipt: None,
        };
        let legacy = crate::backend::get(&BackendArg::from("cargo:ripgrep")).unwrap();
        let preferred =
            crate::backend::get(&BackendArg::from("github:BurntSushi/ripgrep")).unwrap();
        assert_eq!(config.resolve_alias(&legacy, "stable").await?, "14.1.1");
        assert_eq!(config.resolve_alias(&preferred, "stable").await?, "stable");
        Ok(())
    }

    #[tokio::test]
    async fn test_resolve_tool_opts_tracks_alias_config_and_inline_sources() -> Result<()> {
        crate::toolset::install_state::init().await?;

        let source = crate::toolset::ToolSource::PakroToml(PathBuf::from("pakro.toml"));
        let config_ba = Arc::new(BackendArg::from("tiny"));
        let config_opts =
            crate::toolset::parse_tool_options("asset_pattern=config-pattern,bar=config");
        let mut trs = ToolRequestSet::new();
        trs.add_version(
            crate::toolset::ToolRequest::new_with_options(
                config_ba,
                "1.0.0",
                config_opts,
                source.clone(),
            )?,
            &source,
        );

        let mut all_aliases = AliasMap::default();
        all_aliases.insert(
            "tiny".to_string(),
            Alias {
                backend: Some(
                    "github:jdx/mise-test-fixtures[api_url=https://alias.example/api/v3,asset_pattern=alias-pattern,foo=alias]"
                        .to_string(),
                ),
                versions: Default::default(),
            },
        );
        let config = Config {
            tera_ctx: BASE_CONTEXT.clone(),
            config_files: Default::default(),

            env: OnceCell::new(),
            env_with_sources: OnceCell::new(),
            shorthands: get_shorthands(&Settings::get()),
            hooks: OnceCell::new(),

            tool_request_set: OnceCell::new(),
            toolset: OnceCell::new(),
            all_aliases,
            aliases: Default::default(),
            project_root: Default::default(),
            repo_urls: Default::default(),
            shell_aliases: Default::default(),
            tera_files: Default::default(),
            vars: Default::default(),
            vars_results: OnceCell::new(),
            lockfile_discovery: Default::default(),
            readonly_receipt: None,
        };
        config.tool_request_set.set(trs).ok();
        let config = Arc::new(config);
        let ba = Arc::new(BackendArg::from(
            "tiny[api_url=https://inline.example/api/v3]",
        ));

        let resolved = config.resolve_tool_opts_with_overrides(&ba).await?;
        let opts = resolved.effective();

        assert_eq!(opts.get("api_url"), Some("https://inline.example/api/v3"));
        assert_eq!(opts.get("asset_pattern"), Some("config-pattern"));
        assert_eq!(opts.get("foo"), Some("alias"));
        assert_eq!(opts.get("bar"), Some("config"));
        assert_eq!(
            resolved.source_for_key("api_url"),
            Some(crate::toolset::ToolOptionSource::InlineBackendArg)
        );
        assert_eq!(
            resolved.source_for_key("asset_pattern"),
            Some(crate::toolset::ToolOptionSource::Config)
        );
        assert_eq!(
            resolved.source_for_key("foo"),
            Some(crate::toolset::ToolOptionSource::BackendAlias)
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_resolve_tool_opts_prefers_config_over_install_manifest_opts() -> Result<()> {
        crate::toolset::install_state::init().await?;

        let source = crate::toolset::ToolSource::PakroToml(PathBuf::from("pakro.toml"));
        let config_ba = Arc::new(BackendArg::from("http:manifest-opts"));
        let config_opts =
            crate::toolset::parse_tool_options("version_json_path=.current,config_only=true");
        let mut trs = ToolRequestSet::new();
        trs.add_version(
            crate::toolset::ToolRequest::new_with_options(
                config_ba,
                "1.0.0",
                config_opts,
                source.clone(),
            )?,
            &source,
        );

        let mut manifest_opts = BTreeMap::new();
        manifest_opts.insert(
            "version_json_path".to_string(),
            toml::Value::String(".manifest".to_string()),
        );
        manifest_opts.insert(
            "manifest_only".to_string(),
            toml::Value::String("true".to_string()),
        );
        let ba = Arc::new(BackendArg::from(
            crate::toolset::install_state::InstallStateTool {
                short: "http:manifest-opts".to_string(),
                full: Some("http:manifest-opts".to_string()),
                versions: vec!["1.0.0".to_string()],
                explicit_backend: true,
                opts: manifest_opts,
                installs_path: None,
            },
        ));

        let config = Config {
            tera_ctx: BASE_CONTEXT.clone(),
            config_files: Default::default(),

            env: OnceCell::new(),
            env_with_sources: OnceCell::new(),
            shorthands: get_shorthands(&Settings::get()),
            hooks: OnceCell::new(),

            tool_request_set: OnceCell::new(),
            toolset: OnceCell::new(),
            all_aliases: Default::default(),
            aliases: Default::default(),
            project_root: Default::default(),
            repo_urls: Default::default(),
            shell_aliases: Default::default(),
            tera_files: Default::default(),
            vars: Default::default(),
            vars_results: OnceCell::new(),
            lockfile_discovery: Default::default(),
            readonly_receipt: None,
        };
        config.tool_request_set.set(trs).ok();
        let config = Arc::new(config);

        let resolved = config.resolve_tool_opts_with_overrides(&ba).await?;
        let opts = resolved.effective();

        assert_eq!(opts.get("version_json_path"), Some(".current"));
        assert_eq!(
            resolved.source_for_key("version_json_path"),
            Some(crate::toolset::ToolOptionSource::Config)
        );
        assert_eq!(opts.get("manifest_only"), Some("true"));
        assert_eq!(
            resolved.source_for_key("manifest_only"),
            Some(crate::toolset::ToolOptionSource::InstallManifest)
        );
        assert_eq!(opts.get("config_only"), Some("true"));
        assert_eq!(
            resolved.source_for_key("config_only"),
            Some(crate::toolset::ToolOptionSource::Config)
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_resolve_tool_opts_prefers_s3_listing_config_over_install_manifest_opts()
    -> Result<()> {
        crate::toolset::install_state::init().await?;

        let source = crate::toolset::ToolSource::PakroToml(PathBuf::from("pakro.toml"));
        let config_ba = Arc::new(BackendArg::from("s3:manifest-opts"));
        let config_opts = crate::toolset::parse_tool_options(
            "version_prefix=current/,version_regex=current-(.*)",
        );
        let mut trs = ToolRequestSet::new();
        trs.add_version(
            crate::toolset::ToolRequest::new_with_options(
                config_ba,
                "1.0.0",
                config_opts,
                source.clone(),
            )?,
            &source,
        );

        let mut manifest_opts = BTreeMap::new();
        manifest_opts.insert(
            "version_prefix".to_string(),
            toml::Value::String("manifest/".to_string()),
        );
        manifest_opts.insert(
            "version_regex".to_string(),
            toml::Value::String("manifest-(.*)".to_string()),
        );
        manifest_opts.insert(
            "endpoint".to_string(),
            toml::Value::String("https://manifest.example".to_string()),
        );
        let ba = Arc::new(BackendArg::from(
            crate::toolset::install_state::InstallStateTool {
                short: "s3:manifest-opts".to_string(),
                full: Some("s3:manifest-opts".to_string()),
                versions: vec!["1.0.0".to_string()],
                explicit_backend: true,
                opts: manifest_opts,
                installs_path: None,
            },
        ));

        let config = Config {
            tera_ctx: BASE_CONTEXT.clone(),
            config_files: Default::default(),

            env: OnceCell::new(),
            env_with_sources: OnceCell::new(),
            shorthands: get_shorthands(&Settings::get()),
            hooks: OnceCell::new(),

            tool_request_set: OnceCell::new(),
            toolset: OnceCell::new(),
            all_aliases: Default::default(),
            aliases: Default::default(),
            project_root: Default::default(),
            repo_urls: Default::default(),
            shell_aliases: Default::default(),
            tera_files: Default::default(),
            vars: Default::default(),
            vars_results: OnceCell::new(),
            lockfile_discovery: Default::default(),
            readonly_receipt: None,
        };
        config.tool_request_set.set(trs).ok();
        let config = Arc::new(config);

        let resolved = config.resolve_tool_opts_with_overrides(&ba).await?;
        let opts = resolved.effective();

        assert_eq!(opts.get("version_prefix"), Some("current/"));
        assert_eq!(
            resolved.source_for_key("version_prefix"),
            Some(crate::toolset::ToolOptionSource::Config)
        );
        assert_eq!(opts.get("version_regex"), Some("current-(.*)"));
        assert_eq!(
            resolved.source_for_key("version_regex"),
            Some(crate::toolset::ToolOptionSource::Config)
        );
        assert_eq!(opts.get("endpoint"), Some("https://manifest.example"));
        assert_eq!(
            resolved.source_for_key("endpoint"),
            Some(crate::toolset::ToolOptionSource::InstallManifest)
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_resolve_tool_opts_prefers_env_backend_override_over_alias_opts() -> Result<()> {
        unsafe {
            std::env::set_var("PAKRO_BACKENDS_ENV_OPTS_TEST", "github:env/repo[foo=env]");
        }

        let result = async {
            let mut all_aliases = AliasMap::default();
            all_aliases.insert(
                "env-opts-test".to_string(),
                Alias {
                    backend: Some("github:alias/repo[foo=alias,bar=alias]".to_string()),
                    versions: Default::default(),
                },
            );
            let config = Config {
                tera_ctx: BASE_CONTEXT.clone(),
                config_files: Default::default(),

                env: OnceCell::new(),
                env_with_sources: OnceCell::new(),
                shorthands: get_shorthands(&Settings::get()),
                hooks: OnceCell::new(),

                tool_request_set: OnceCell::new(),
                toolset: OnceCell::new(),
                all_aliases,
                aliases: Default::default(),
                project_root: Default::default(),
                repo_urls: Default::default(),
                shell_aliases: Default::default(),
                tera_files: Default::default(),
                vars: Default::default(),
                vars_results: OnceCell::new(),
                lockfile_discovery: Default::default(),
                readonly_receipt: None,
            };
            config.tool_request_set.set(ToolRequestSet::new()).ok();
            let config = Arc::new(config);
            let ba = Arc::new(BackendArg::from("env-opts-test"));

            let resolved = config.resolve_tool_opts_with_overrides(&ba).await?;
            let opts = resolved.effective();

            assert_eq!(ba.full(), "github:env/repo[foo=env]");
            assert_eq!(opts.get("foo"), Some("env"));
            assert_eq!(opts.get("bar"), None);
            assert_eq!(
                resolved.source_for_key("foo"),
                Some(crate::toolset::ToolOptionSource::BackendAlias)
            );
            Ok(())
        }
        .await;

        unsafe {
            std::env::remove_var("PAKRO_BACKENDS_ENV_OPTS_TEST");
        }

        result
    }

    #[tokio::test]
    async fn test_load_all_config_files_skips_directories() -> Result<()> {
        let _config = Config::get().await?;
        let temp_dir = TempDir::new()?;
        let temp_path = temp_dir.path();

        let sub_dir = temp_path.join("subdir");
        fs::create_dir(&sub_dir)?;

        let file1_path = temp_path.join("config1.toml");
        let file2_path = temp_path.join("config2.toml");
        File::create(&file1_path)?;
        File::create(&file2_path)?;

        fs::write(&file1_path, "key1 = 'value1'")?;
        fs::write(&file2_path, "key2 = 'value2'")?;

        let config_filenames = vec![file1_path.clone(), file2_path.clone(), sub_dir.clone()];
        let idiomatic_filenames = BTreeMap::new();

        let result = load_all_config_files(&config_filenames, &idiomatic_filenames).await?;

        // the result should have only two entries for the files, the directory should not be present
        assert_eq!(result.len(), 2);

        // Check that the directory is not in the result
        assert!(result.contains_key(&file1_path));
        assert!(result.contains_key(&file2_path));
        assert!(!result.contains_key(&sub_dir));

        Ok(())
    }

    #[test]
    fn test_matching_idiomatic_tools_uses_relative_paths() {
        let idiomatic_filenames = BTreeMap::from([
            (
                "subdir/tool.json".to_string(),
                vec!["nested-tool".to_string()],
            ),
            ("tool.json".to_string(), vec!["root-tool".to_string()]),
        ]);

        assert_eq!(
            matching_idiomatic_tools(
                Path::new("/tmp/project/subdir/tool.json"),
                &idiomatic_filenames
            ),
            vec!["nested-tool"]
        );
    }

    #[test]
    fn test_idiomatic_version_file_disabled_is_tool_specific() {
        let disabled = BTreeSet::from([
            "aqua:owner/tool:tool.json".to_string(),
            "node:package.json".to_string(),
            "python:.python-version".to_string(),
        ]);

        assert!(idiomatic_version_file_disabled(
            &disabled,
            "aqua:owner/tool",
            "tool.json"
        ));
        assert!(idiomatic_version_file_disabled(
            &disabled,
            "node",
            "package.json"
        ));
        assert!(!idiomatic_version_file_disabled(
            &disabled,
            "pnpm",
            "package.json"
        ));
        assert!(!idiomatic_version_file_disabled(
            &disabled, "node", ".nvmrc"
        ));
    }

    #[tokio::test]
    async fn test_get_repo_url_ssh() -> Result<()> {
        let config = Config::reset().await?;
        let urls = [
            "ssh://git@gitlab.dev/mobile/asdf-gitique.git",
            "git@github.com:user/repo.git",
            "git://example.com/repo.git",
            "http://example.com/repo.git",
            "https://example.com/repo.git",
        ];

        for url in urls {
            assert!(
                config.get_repo_url(url).is_some(),
                "URL should be considered valid: {url}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_get_repo_url_preserves_explicit_local_paths() {
        let temp = tempfile::tempdir().unwrap();
        let local_asdf = temp
            .path()
            .join("plugins/local-asdf")
            .to_string_lossy()
            .into_owned();
        let repo_urls = HashMap::from([
            ("local-asdf".to_string(), local_asdf.clone()),
            ("remote-asdf".to_string(), "owner/asdf-plugin".to_string()),
            (
                "asdf:explicit-asdf".to_string(),
                "owner/asdf-plugin".to_string(),
            ),
        ]);
        let config = Config {
            tera_ctx: BASE_CONTEXT.clone(),
            config_files: Default::default(),

            env: OnceCell::new(),
            env_with_sources: OnceCell::new(),
            shorthands: get_shorthands(&Settings::get()),
            hooks: OnceCell::new(),

            tool_request_set: OnceCell::new(),
            toolset: OnceCell::new(),
            all_aliases: Default::default(),
            aliases: Default::default(),
            project_root: Default::default(),
            repo_urls,
            shell_aliases: Default::default(),
            tera_files: Default::default(),
            vars: Default::default(),
            vars_results: OnceCell::new(),
            lockfile_discovery: Default::default(),
            readonly_receipt: None,
        };

        assert_eq!(
            config.get_repo_url("local-asdf").as_deref(),
            Some(local_asdf.as_str())
        );
        assert_eq!(
            config.get_repo_url("remote-asdf").as_deref(),
            Some("https://github.com/owner/asdf-plugin.git")
        );

        for key in ["vfox:retired", "vfox-backend:retired", "package:retired"] {
            let path = temp.path().join("removed.pakro.toml");
            let body = format!("[plugins]\n\"{key}\" = \"owner/retired\"\n");
            let cf = PakroToml::from_str(&body, &path).unwrap();
            let files = ConfigMap::from([(path, Arc::new(cf) as Arc<dyn ConfigFile>)]);
            assert!(
                config.with_config_files(files).is_err(),
                "removed plugin {key} must not fall back to the previous plugin map"
            );
        }
        for body in ["[plugins]\nretired = \"packslip:owner/plugin\"\n"] {
            let path = temp.path().join("removed.pakro.toml");
            let cf: PakroToml = toml::from_str(body).unwrap();
            let files = ConfigMap::from([(path, Arc::new(cf) as Arc<dyn ConfigFile>)]);
            assert!(
                config.with_config_files(files).is_err(),
                "removed Lua plugin declaration must fail: {body}"
            );
        }
    }
}

/// Deliberately not inside the `#[cfg(unix)]` module above: `first_config_file` only inspects
/// path components, so it behaves the same everywhere and is worth compiling on Windows too.
#[cfg(test)]
mod write_target_tests {
    use super::*;

    fn set(paths: &[&str]) -> IndexSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_drop_in_never_wins_over_tool_versions() {
        // `global_config_files` lists conf.d entries first and appends `~/.tool-versions`
        // after them, so a fallback that does not skip drop-ins hands one back — and
        // `global_config_path`, refusing it, creates a `config.toml` rather than writing to
        // the `.tool-versions` sitting right there. That ordering is what makes this
        // reachable, and nothing else in the file says so.
        let files = set(&[
            "/home/u/.config/pakro/conf.d/10-drop-in.toml",
            "/home/u/.tool-versions",
        ]);
        assert_eq!(
            first_config_file(&files),
            Some(&PathBuf::from("/home/u/.tool-versions"))
        );
    }

    #[test]
    fn nothing_but_drop_ins_means_no_write_target() {
        // Every caller turns `None` into a config of its own, which is the wanted outcome:
        // creating `config.toml` beats editing somebody's drop-in.
        let files = set(&["/home/u/.config/pakro/conf.d/10-drop-in.toml"]);
        assert_eq!(first_config_file(&files), None);
    }

    #[test]
    fn a_real_config_still_wins() {
        let files = set(&[
            "/home/u/.config/pakro/conf.d/10-drop-in.toml",
            "/home/u/.config/pakro/config.toml",
            "/home/u/.tool-versions",
        ]);
        assert_eq!(
            first_config_file(&files),
            Some(&PathBuf::from("/home/u/.config/pakro/config.toml"))
        );
    }

    #[test]
    fn tool_versions_stays_deprioritised() {
        // Control for the change above: `.tool-versions` is skipped *while choosing* and only
        // returned when nothing else qualifies. Excluding drop-ins outright must not turn that
        // preference into an exclusion as well.
        let files = set(&["/proj/.tool-versions", "/proj/pakro.toml"]);
        assert_eq!(
            first_config_file(&files),
            Some(&PathBuf::from("/proj/pakro.toml"))
        );

        let only = set(&["/proj/.tool-versions"]);
        assert_eq!(
            first_config_file(&only),
            Some(&PathBuf::from("/proj/.tool-versions"))
        );
    }
}
