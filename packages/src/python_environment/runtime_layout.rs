//! Fixed selected-runtime layout observations; values are not source/prefix authority.
//!
//! Decoding validates finite selected-process facts. Source capture separately
//! authenticates the input bytes and repeats the selected runtime probe.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eyre::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::python_runtime::{PythonImplementation, PythonRuntimeDescriptor, PythonVersion};

#[derive(Debug, Clone, Copy)]
pub(crate) struct RuntimeLayoutLimits {
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_paths: usize,
    pub max_path_bytes: usize,
    pub max_schemes: usize,
    pub max_config_vars: usize,
    pub max_input_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_loader_images: usize,
}

impl Default for RuntimeLayoutLimits {
    fn default() -> Self {
        Self {
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
            max_paths: 4096,
            max_path_bytes: 16_384,
            max_schemes: 64,
            max_config_vars: 64,
            max_input_files: 4096,
            max_file_bytes: 8 * 1024 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024 * 1024,
            max_loader_images: 512,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeLayoutIdentity {
    pub implementation: PythonImplementation,
    pub language_version: PythonVersion,
    pub implementation_version: PythonVersion,
    pub soabi: Option<String>,
    pub ext_suffix: Option<String>,
    pub abiflags: Option<String>,
    pub gil_disabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeLayoutPathApi {
    Sysconfig,
    DistutilsSysconfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RuntimeLayoutPathStatus {
    Missing,
    Error {
        kind: String,
    },
    Present {
        canonical: PathBuf,
        regular_file: bool,
        size: u64,
        mode: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeLayoutPathFact {
    pub advertised: PathBuf,
    pub status: RuntimeLayoutPathStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RuntimeLoaderObservation {
    Unknown { reason: String },
    Observed { images: Vec<PathBuf> },
}

/// Publicly deserializable value DTO. Every consumer must call the validator;
/// direct deserialization/mutation is not an authenticated runtime binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeLayoutObservation {
    pub schema: u32,
    pub observation_mode: String,
    pub executable: PathBuf,
    pub identity: RuntimeLayoutIdentity,
    pub prefix: PathBuf,
    pub exec_prefix: PathBuf,
    pub base_prefix: Option<PathBuf>,
    pub base_exec_prefix: Option<PathBuf>,
    pub sys_path: Vec<PathBuf>,
    pub sysconfig_origin: Option<PathBuf>,
    pub path_api: RuntimeLayoutPathApi,
    pub scheme_names: Vec<String>,
    pub selected_scheme: Option<String>,
    pub base_paths: BTreeMap<String, PathBuf>,
    pub target_prefix: PathBuf,
    pub target_paths: BTreeMap<String, PathBuf>,
    pub config_vars: BTreeMap<String, serde_json::Value>,
    pub files: BTreeMap<String, RuntimeLayoutPathFact>,
    pub loader: RuntimeLoaderObservation,
}

pub(crate) fn decode_runtime_layout(
    bytes: &[u8],
    selected: &PythonRuntimeDescriptor,
    final_prefix: &Path,
    limits: RuntimeLayoutLimits,
) -> Result<RuntimeLayoutObservation> {
    validate_limits(limits)?;
    ensure!(
        bytes.len() <= limits.max_stdout_bytes,
        "layout limit: output bytes exceeded"
    );
    let observed: RuntimeLayoutObservation =
        serde_json::from_slice(bytes).context("layout protocol JSON is invalid")?;
    ensure!(
        observed.schema == 1 && observed.observation_mode == "base_layout_site_disabled",
        "layout protocol schema or mode disagrees"
    );
    selected
        .validate_compatibility()
        .context("runtime identity is inconsistent")?;
    ensure!(
        observed.identity
            == RuntimeLayoutIdentity {
                implementation: selected.implementation,
                language_version: selected.language_version.clone(),
                implementation_version: selected.implementation_version.clone(),
                soabi: selected.soabi.clone(),
                ext_suffix: selected.ext_suffix.clone(),
                abiflags: selected.abiflags.clone(),
                gil_disabled: selected.gil_disabled,
            },
        "runtime identity differs from selected descriptor"
    );
    ensure!(
        observed.executable == selected.executable,
        "executable binding differs from selected runtime"
    );
    ensure!(
        observed.target_prefix == final_prefix,
        "final prefix differs from selected allocation"
    );
    validate_path(final_prefix, limits)?;
    ensure!(
        final_prefix.as_os_str().as_encoded_bytes().is_ascii(),
        "final prefix must be ASCII"
    );
    ensure!(
        observed.scheme_names.len() <= limits.max_schemes,
        "layout limit: too many schemes"
    );
    ensure!(
        observed.sys_path.len() <= limits.max_paths,
        "layout limit: too many search paths"
    );
    ensure!(
        observed.base_paths.len() <= 8
            && observed.files.len() <= 8
            && observed.config_vars.len() <= limits.max_config_vars,
        "layout limit: too many finite observations"
    );
    for name in &observed.scheme_names {
        ensure!(
            !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "scheme observation has invalid name"
        );
    }
    let unique: std::collections::BTreeSet<_> = observed.scheme_names.iter().collect();
    ensure!(
        unique.len() == observed.scheme_names.len(),
        "scheme observation contains duplicates"
    );
    match observed.path_api {
        RuntimeLayoutPathApi::Sysconfig => ensure!(
            observed
                .selected_scheme
                .as_ref()
                .is_some_and(|name| unique.contains(name)),
            "scheme observation selected an unreported scheme"
        ),
        RuntimeLayoutPathApi::DistutilsSysconfig => ensure!(
            observed.selected_scheme.is_none() && observed.scheme_names.is_empty(),
            "scheme observation invented a modern legacy scheme"
        ),
    }
    for path in [
        &observed.executable,
        &observed.prefix,
        &observed.exec_prefix,
    ]
    .into_iter()
    .chain(observed.base_prefix.iter())
    .chain(observed.base_exec_prefix.iter())
    .chain(observed.sysconfig_origin.iter())
    .chain(observed.sys_path.iter())
    {
        validate_path(path, limits)?;
    }
    let base_keys = [
        "stdlib",
        "platstdlib",
        "purelib",
        "platlib",
        "include",
        "platinclude",
        "scripts",
        "data",
    ];
    ensure!(
        observed
            .base_paths
            .keys()
            .all(|key| base_keys.contains(&key.as_str())),
        "layout protocol has unknown base path key"
    );
    for path in observed.base_paths.values() {
        validate_path(path, limits)?;
    }
    let target_keys = ["purelib", "platlib", "headers", "scripts", "data"];
    ensure!(
        observed.target_paths.len() == target_keys.len()
            && target_keys
                .iter()
                .all(|key| observed.target_paths.contains_key(*key)),
        "target scheme has missing or unknown keys"
    );
    for path in observed.target_paths.values() {
        validate_path(path, limits).context("target scheme path is invalid")?;
        ensure!(
            path.starts_with(final_prefix),
            "target scheme escapes final prefix"
        );
    }
    const CONFIG: &[&str] = &[
        "BINDIR",
        "LIBDIR",
        "LIBPL",
        "LDLIBRARY",
        "LIBRARY",
        "INSTSONAME",
        "INCLUDEPY",
        "CONFINCLUDEPY",
        "SOABI",
        "EXT_SUFFIX",
        "ABIFLAGS",
        "VERSION",
        "LDVERSION",
        "platlibdir",
        "Py_ENABLE_SHARED",
        "Py_GIL_DISABLED",
        "PYTHONFRAMEWORK",
        "PYTHONFRAMEWORKPREFIX",
        "PYTHONFRAMEWORKDIR",
        "PYTHONFRAMEWORKINSTALLDIR",
        "LIBDEST",
        "BINLIBDEST",
        "MULTIARCH",
        "prefix",
        "exec_prefix",
        "base",
        "platbase",
    ];
    for (key, value) in &observed.config_vars {
        ensure!(
            CONFIG.contains(&key.as_str())
                && (value.is_null()
                    || value.is_boolean()
                    || value.as_i64().is_some()
                    || value.as_u64().is_some()
                    || value.is_string()),
            "config observation has unknown key or non-scalar value"
        );
        if let Some(value) = value.as_str() {
            ensure!(
                value.len() <= limits.max_path_bytes && !value.contains('\0'),
                "layout limit: config string exceeds bounds"
            );
        }
    }
    for (key, fact) in &observed.files {
        let expected = match key.as_str() {
            "python_h" => observed
                .base_paths
                .get("include")
                .map(|path| path.join("Python.h")),
            "plat_python_h" => observed
                .base_paths
                .get("platinclude")
                .map(|path| path.join("Python.h")),
            _ => key.split_once('_').and_then(|(directory, file)| {
                if !["libdir", "libpl"].contains(&directory)
                    || !["ldlibrary", "library", "instsoname"].contains(&file)
                {
                    return None;
                }
                let directory = observed
                    .config_vars
                    .get(&directory.to_ascii_uppercase())?
                    .as_str()?;
                let file = observed
                    .config_vars
                    .get(&file.to_ascii_uppercase())?
                    .as_str()?;
                Some(Path::new(directory).join(file))
            }),
        };
        ensure!(
            expected.as_ref() == Some(&fact.advertised),
            "file fact binding differs from declared runtime input"
        );
        validate_path(&fact.advertised, limits)?;
        match &fact.status {
            RuntimeLayoutPathStatus::Missing => {}
            RuntimeLayoutPathStatus::Error { kind } => ensure!(
                !kind.is_empty() && kind.len() <= 128 && !kind.chars().any(char::is_control),
                "layout protocol path error kind is invalid"
            ),
            RuntimeLayoutPathStatus::Present {
                canonical, mode, ..
            } => {
                validate_path(canonical, limits)?;
                ensure!(*mode <= 0o777, "layout protocol path mode is invalid");
            }
        }
    }
    match &observed.loader {
        RuntimeLoaderObservation::Unknown { reason } => ensure!(
            !reason.is_empty() && reason.len() <= 128 && !reason.chars().any(char::is_control),
            "layout protocol loader unknown reason is invalid"
        ),
        RuntimeLoaderObservation::Observed { images } => {
            ensure!(
                images.len() <= limits.max_loader_images,
                "layout limit: too many loader images"
            );
            for path in images {
                validate_path(path, limits)?;
            }
        }
    }
    Ok(observed)
}

pub(super) fn validate_limits(limits: RuntimeLayoutLimits) -> Result<()> {
    ensure!(
        limits.max_stdout_bytes != 0
            && limits.max_stderr_bytes != 0
            && limits.max_paths != 0
            && limits.max_path_bytes != 0
            && limits.max_schemes != 0
            && limits.max_config_vars != 0
            && limits.max_input_files != 0
            && limits.max_file_bytes != 0
            && limits.max_total_bytes != 0
            && limits.max_loader_images != 0,
        "layout limit values must be nonzero"
    );
    Ok(())
}

fn validate_path(path: &Path, limits: RuntimeLayoutLimits) -> Result<()> {
    let bytes = path.as_os_str().as_encoded_bytes();
    ensure!(
        bytes.len() <= limits.max_path_bytes,
        "layout limit: path exceeds byte budget"
    );
    ensure!(
        path.is_absolute()
            && !bytes.contains(&0)
            && !path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
        "layout path must be absolute and contained"
    );
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) mod test_fixtures;
