//! A single read of project inputs. No registration, recovery, trust prompt,
//! backend loading or remote fetch is reachable through this boundary.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::config::config_file::{
    ConfigDocumentFormat, StaticConfigDeclarations, StaticConfigError, parse_static_config_as,
    validate_static_include,
};
use crate::config::{
    ConfigDiscoveryError, ConfigDiscoveryLimits, DiscoveredConfigInputs, FrozenConfigDiscovery,
    discover_project_configs,
};
use crate::lockfile::transaction::{
    ReadOnlyFileError, ReadOnlyFileLimits, ReadOnlyFileObservation, ReadOnlyPathObservation,
};
use crate::packages::binding::{OwnedPackageRequest, merge_owned_requests};
use crate::packages::python_project::{
    BoundPythonProjectFile, PythonProjectFileError, PythonProjectFileLimits,
};
use crate::toolset::ToolSource;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ProjectObservationError {
    #[error("project input path must be an absolute path without parent components")]
    InvalidPath,
    #[error("project input is unavailable or has an unsupported type")]
    Unavailable,
    #[error("project inputs changed; observe the project again")]
    StaleInput,
    #[error("project input is invalid")]
    InvalidDocument,
    #[error("project input needs an explicit static include binding")]
    MissingStaticInclude,
    #[error("project input requires static configuration data")]
    MissingStaticInput,
    #[error("project input budget exceeded")]
    ResourceLimit,
    #[error("project observation deadline expired")]
    DeadlineExceeded,
    #[error("project observation was cancelled")]
    Cancelled,
    #[error(transparent)]
    Configuration(#[from] StaticConfigError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadScope {
    Project,
    LocalOnly,
    GlobalOnly,
}

#[derive(Clone)]
pub(crate) struct StaticIncludeBinding {
    pub including: PathBuf,
    pub reference: String,
    pub path: PathBuf,
}

/// Inputs supplied once by the application. None are reread from process globals.
#[derive(Clone)]
pub(crate) struct ReadPolicy {
    pub discovery: FrozenConfigDiscovery,
    pub scope: ReadScope,
    pub deadline: Instant,
    pub cancellation: CancellationToken,
    pub file_limits: ReadOnlyFileLimits,
    pub discovery_limits: ConfigDiscoveryLimits,
    pub max_files: usize,
    pub max_total_bytes: usize,
    pub cli_arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub static_includes: Vec<StaticIncludeBinding>,
    /// Explicitly resolved by the application; absence keeps colocated locks.
    pub monorepo_lock_root: Option<PathBuf>,
}

impl ReadPolicy {
    pub(crate) fn new(discovery: FrozenConfigDiscovery, deadline: Instant) -> Self {
        Self {
            discovery,
            scope: ReadScope::Project,
            deadline,
            cancellation: CancellationToken::new(),
            file_limits: ReadOnlyFileLimits::default(),
            discovery_limits: ConfigDiscoveryLimits::default(),
            max_files: 1024,
            max_total_bytes: 16 * 1024 * 1024,
            cli_arguments: Vec::new(),
            environment: BTreeMap::new(),
            static_includes: Vec::new(),
            monorepo_lock_root: None,
        }
    }
}

pub(crate) struct ObservedProjectInputs {
    root: PathBuf,
    policy: ReadPolicy,
    discovery: DiscoveredConfigInputs,
    files: Vec<ObservedDocument>,
    python_project: Option<(PathBuf, BoundPythonProjectFile)>,
    missing_auxiliary: Vec<PathBuf>,
    root_owner: Option<ReadOnlyPathObservation>,
    namespace_inputs: Vec<ReadOnlyPathObservation>,
}

struct ObservedDocument {
    path: PathBuf,
    owner: ReadOnlyFileObservation,
    declarations: Option<StaticConfigDeclarations>,
    semantic_path: PathBuf,
    format: Option<ConfigDocumentFormat>,
    editable: bool,
}

impl fmt::Debug for ObservedProjectInputs {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("ObservedProjectInputs")
            .finish_non_exhaustive()
    }
}

pub(crate) fn observe_project(
    root: &Path,
    mut policy: ReadPolicy,
) -> Result<ObservedProjectInputs, ProjectObservationError> {
    validate_path(root)?;
    checkpoint(&policy)?;
    // A global query has no implicit local project, lock, or pyproject input.
    if policy.scope == ReadScope::GlobalOnly {
        policy.discovery.local_patterns.clear();
        policy.discovery.ceiling_paths = vec![root.to_path_buf()];
    }
    if policy.scope == ReadScope::LocalOnly {
        // Global files can also match an ancestor's local patterns. Keep their
        // frozen classification as exclusions (including aliases and globs),
        // rather than merely removing the final global discovery pass.
        policy.discovery.ignored_paths.extend(
            policy
                .discovery
                .global_candidates
                .iter()
                .chain(&policy.discovery.system_candidates)
                .map(|candidate| candidate.directory.join(&candidate.pattern)),
        );
        policy.discovery.global_candidates.clear();
        policy.discovery.system_candidates.clear();
    }
    let input_bytes = policy
        .cli_arguments
        .iter()
        .map(String::len)
        .chain(
            policy
                .environment
                .iter()
                .flat_map(|(key, value)| [key.len(), value.len()]),
        )
        .chain(
            policy
                .static_includes
                .iter()
                .map(|binding| binding.reference.len()),
        )
        .try_fold(0usize, |total, size| total.checked_add(size))
        .ok_or(ProjectObservationError::ResourceLimit)?;
    if policy.max_files == 0 || input_bytes > policy.max_total_bytes {
        return Err(ProjectObservationError::ResourceLimit);
    }
    let discovery = discover_project_configs(
        root,
        &policy.discovery,
        policy.discovery_limits,
        policy.deadline,
    )
    .map_err(discovery_error)?;
    let root_owner = if policy.scope != ReadScope::GlobalOnly {
        let owner = ReadOnlyPathObservation::open(root, policy.file_limits, policy.deadline)
            .map_err(file_error)?;
        if !owner.exists() {
            return Err(ProjectObservationError::Unavailable);
        }
        Some(owner)
    } else {
        None
    };
    let mut observed = ObservedProjectInputs {
        root: root.to_path_buf(),
        policy,
        discovery,
        files: Vec::new(),
        python_project: None,
        missing_auxiliary: Vec::new(),
        root_owner,
        namespace_inputs: Vec::new(),
    };
    let mut remaining = observed.policy.max_total_bytes - input_bytes;
    for path in observed.discovery.files.clone() {
        let owner = observed.open_file(&path, &mut remaining)?;
        let format = observed
            .policy
            .discovery
            .format_for(&path)
            .map_err(discovery_error)?;
        let declarations = parse_static_config_as(owner.bytes(), &path, format)?;
        let includes = declarations.includes.clone();
        observed.files.push(ObservedDocument {
            path: path.clone(),
            semantic_path: path.clone(),
            format: Some(format),
            owner,
            declarations: Some(declarations),
            editable: true,
        });
        // Existing include semantics: the including document wins, then the
        // last include wins over earlier includes. Each source keeps its owner.
        for reference in includes.iter().rev() {
            let mut bindings = observed
                .policy
                .static_includes
                .iter()
                .filter(|binding| binding.including == path && binding.reference == *reference);
            let binding = bindings
                .next()
                .ok_or(ProjectObservationError::MissingStaticInclude)?;
            if bindings.next().is_some() {
                return Err(ProjectObservationError::MissingStaticInclude);
            }
            let include_path = binding.path.clone();
            let owner = observed.open_file(&include_path, &mut remaining)?;
            validate_static_include(owner.bytes())?;
            let declarations = parse_static_config_as(
                owner.bytes(),
                &include_path,
                ConfigDocumentFormat::PakroToml,
            )?;
            observed.files.push(ObservedDocument {
                path: include_path,
                semantic_path: path.clone(),
                format: Some(ConfigDocumentFormat::PakroToml),
                owner,
                declarations: Some(declarations),
                editable: false,
            });
        }
    }
    if !observed.policy.discovery.no_config {
        let mut auxiliary = if observed.policy.scope == ReadScope::GlobalOnly {
            Vec::new()
        } else {
            vec![root.join("pyproject.toml"), root.join("pakro.lock")]
        };
        let global_dirs: Vec<_> = observed
            .policy
            .discovery
            .global_candidates
            .iter()
            .chain(&observed.policy.discovery.system_candidates)
            .map(|candidate| candidate.directory.as_path())
            .collect();
        if let Some(root) = &observed.policy.monorepo_lock_root {
            validate_path(root)?;
        }
        for config in &observed.discovery.files {
            let folder =
                crate::config::is_conf_d_folder_file_with_config_dirs(config, &global_dirs);
            let path = crate::lockfile::lockfile_path_for_config_with_folder_fragment(
                config,
                observed.policy.monorepo_lock_root.as_deref(),
                folder,
            )
            .0;
            if !auxiliary.contains(&path) {
                auxiliary.push(path);
            }
        }
        for path in auxiliary {
            if !entry_exists(&path)? {
                observed.missing_auxiliary.push(path);
            } else if path == root.join("pyproject.toml") {
                observed.check_file_budget()?;
                let mut limits = PythonProjectFileLimits::default();
                limits.document.max_document_bytes = observed.allowed_file_bytes(remaining)?;
                limits.max_path_bytes = observed.policy.file_limits.max_path_bytes;
                limits.max_components = observed.policy.file_limits.max_components;
                limits.max_symlink_hops = observed.policy.file_limits.max_symlink_hops;
                let owner = BoundPythonProjectFile::open(&path, limits, observed.policy.deadline)
                    .map_err(|error| match error {
                    PythonProjectFileError::Observation(error) => file_error(error),
                    _ => ProjectObservationError::InvalidDocument,
                })?;
                remaining -= owner.bytes().len();
                observed.python_project = Some((path, owner));
            } else {
                let owner = observed.open_file(&path, &mut remaining)?;
                observed.files.push(ObservedDocument {
                    semantic_path: path.clone(),
                    path,
                    owner,
                    declarations: None,
                    format: None,
                    editable: false,
                });
            }
        }
    }
    // Missing entries have namespace owners too: re-discovery alone would miss
    // replacing an alias with a different symlink to the same empty directory.
    let mut namespace_paths: std::collections::BTreeSet<_> = observed
        .discovery
        .missing_candidates
        .iter()
        .chain(&observed.missing_auxiliary)
        .cloned()
        .collect();
    for scan in &observed.discovery.glob_scans {
        let prefix: PathBuf = Path::new(&scan.pattern)
            .components()
            .take_while(|part| !crate::config::is_glob_pattern(&part.as_os_str().to_string_lossy()))
            .collect();
        namespace_paths.insert(scan.directory.join(prefix));
    }
    if namespace_paths.len() > observed.policy.discovery_limits.max_candidates {
        return Err(ProjectObservationError::ResourceLimit);
    }
    for path in namespace_paths {
        checkpoint(&observed.policy)?;
        observed.namespace_inputs.push(
            ReadOnlyPathObservation::open(
                &path,
                observed.policy.file_limits,
                observed.policy.deadline,
            )
            .map_err(file_error)?,
        );
    }
    observed.recheck()?;
    Ok(observed)
}

impl ObservedProjectInputs {
    pub(crate) fn recheck(&self) -> Result<(), ProjectObservationError> {
        checkpoint(&self.policy)?;
        if let Some(owner) = &self.root_owner {
            owner.recheck().map_err(file_error)?;
        }
        for owner in &self.namespace_inputs {
            checkpoint(&self.policy)?;
            owner.recheck().map_err(file_error)?;
        }
        let current = discover_project_configs(
            &self.root,
            &self.policy.discovery,
            self.policy.discovery_limits,
            self.policy.deadline,
        )
        .map_err(discovery_error)?;
        if current != self.discovery {
            return Err(ProjectObservationError::StaleInput);
        }
        for document in &self.files {
            checkpoint(&self.policy)?;
            document.owner.recheck().map_err(file_error)?;
        }
        if let Some((_, owner)) = &self.python_project {
            owner.recheck().map_err(|error| match error {
                PythonProjectFileError::Observation(error) => file_error(error),
                _ => ProjectObservationError::StaleInput,
            })?;
        }
        for path in &self.missing_auxiliary {
            if entry_exists(path)? {
                return Err(ProjectObservationError::StaleInput);
            }
        }
        checkpoint(&self.policy)
    }
    pub(crate) fn document(&self, path: &Path) -> Option<&[u8]> {
        self.files
            .iter()
            .find(|document| document.path == path)
            .map(|document| document.owner.bytes())
            .or_else(|| {
                self.python_project
                    .as_ref()
                    .filter(|(candidate, _)| candidate == path)
                    .map(|(_, owner)| owner.bytes())
            })
    }
    pub(crate) fn contains_edit_target(&self, path: &Path) -> bool {
        self.files
            .iter()
            .any(|document| document.path == path && document.editable)
            || self
                .discovery
                .missing_candidates
                .iter()
                .any(|candidate| candidate == path)
            || self
                .python_project
                .as_ref()
                .is_some_and(|(candidate, _)| candidate == path)
    }
    pub(crate) fn document_byte_limit(&self) -> usize {
        self.policy.file_limits.max_bytes
    }
    pub(crate) fn document_format(&self, path: &Path) -> Option<ConfigDocumentFormat> {
        if let Some(document) = self.files.iter().find(|document| document.path == path) {
            return document.format;
        }
        self.discovery
            .missing_candidates
            .contains(&path.to_path_buf())
            .then(|| self.policy.discovery.format_for(path).ok())
            .flatten()
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn paths(&self) -> impl Iterator<Item = &Path> {
        self.files
            .iter()
            .map(|document| document.path.as_path())
            .chain(self.python_project.iter().map(|(path, _)| path.as_path()))
    }
    pub(crate) fn cli_arguments(&self) -> &[String] {
        &self.policy.cli_arguments
    }
    pub(crate) fn environment(&self) -> &BTreeMap<String, String> {
        &self.policy.environment
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.policy.deadline
    }
    pub(crate) fn scope(&self) -> ReadScope {
        self.policy.scope
    }
    pub(crate) fn cancellation(&self) -> &CancellationToken {
        &self.policy.cancellation
    }
    pub(crate) fn declarations(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&Path, &StaticConfigDeclarations)> {
        self.files.iter().filter_map(|document| {
            document
                .declarations
                .as_ref()
                .map(|declarations| (document.semantic_path.as_path(), declarations))
        })
    }
    pub(crate) fn source_documents(
        &self,
    ) -> impl Iterator<Item = (&Path, &Path, &StaticConfigDeclarations)> {
        self.files.iter().filter_map(|document| {
            document.declarations.as_ref().map(|declarations| {
                (
                    document.path.as_path(),
                    document.semantic_path.as_path(),
                    declarations,
                )
            })
        })
    }
    pub(crate) fn effective_packages(
        &self,
    ) -> Result<Vec<OwnedPackageRequest>, ProjectObservationError> {
        self.recheck()?;
        Ok(merge_owned_requests(
            self.declarations().rev().map(|(path, declarations)| {
                (
                    ToolSource::PakroToml(path.to_path_buf()),
                    declarations.packages.clone(),
                )
            }),
            &[],
            &[],
        ))
    }
    pub(crate) fn python_project(&self) -> Option<&BoundPythonProjectFile> {
        self.python_project.as_ref().map(|(_, owner)| owner)
    }
    fn check_file_budget(&self) -> Result<(), ProjectObservationError> {
        checkpoint(&self.policy)?;
        if self.files.len() + usize::from(self.python_project.is_some()) >= self.policy.max_files {
            return Err(ProjectObservationError::ResourceLimit);
        }
        Ok(())
    }
    fn allowed_file_bytes(&self, remaining: usize) -> Result<usize, ProjectObservationError> {
        let allowed = self.policy.file_limits.max_bytes.min(remaining);
        if allowed == 0 {
            return Err(ProjectObservationError::ResourceLimit);
        }
        Ok(allowed)
    }
    fn open_file(
        &self,
        path: &Path,
        remaining: &mut usize,
    ) -> Result<ReadOnlyFileObservation, ProjectObservationError> {
        validate_path(path)?;
        self.check_file_budget()?;
        let limits = ReadOnlyFileLimits {
            max_bytes: self.allowed_file_bytes(*remaining)?,
            ..self.policy.file_limits
        };
        let owner = ReadOnlyFileObservation::open(path, limits, self.policy.deadline)
            .map_err(file_error)?;
        *remaining -= owner.bytes().len();
        Ok(owner)
    }
}

fn checkpoint(policy: &ReadPolicy) -> Result<(), ProjectObservationError> {
    if policy.cancellation.is_cancelled() {
        return Err(ProjectObservationError::Cancelled);
    }
    if Instant::now() >= policy.deadline {
        return Err(ProjectObservationError::DeadlineExceeded);
    }
    Ok(())
}

fn validate_path(path: &Path) -> Result<(), ProjectObservationError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return Err(ProjectObservationError::InvalidPath);
    }
    Ok(())
}

fn entry_exists(path: &Path) -> Result<bool, ProjectObservationError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(ProjectObservationError::Unavailable),
    }
}

fn file_error(error: ReadOnlyFileError) -> ProjectObservationError {
    match error {
        ReadOnlyFileError::Changed => ProjectObservationError::StaleInput,
        ReadOnlyFileError::Expired => ProjectObservationError::DeadlineExceeded,
        ReadOnlyFileError::Limit(_) | ReadOnlyFileError::InvalidLimits => {
            ProjectObservationError::ResourceLimit
        }
        _ => ProjectObservationError::Unavailable,
    }
}

fn discovery_error(error: ConfigDiscoveryError) -> ProjectObservationError {
    match error {
        ConfigDiscoveryError::Budget => ProjectObservationError::ResourceLimit,
        ConfigDiscoveryError::Expired => ProjectObservationError::DeadlineExceeded,
        ConfigDiscoveryError::RelativePath => ProjectObservationError::InvalidPath,
        ConfigDiscoveryError::InvalidPattern => ProjectObservationError::InvalidDocument,
        ConfigDiscoveryError::Unavailable => ProjectObservationError::Unavailable,
    }
}

#[cfg(all(test, unix))]
mod tests;
