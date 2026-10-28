//! Official CPython source inputs and portable build results. These records
//! describe a build; only a live language owner can publish its actual output.
use super::identity::{IdentityLimits, LanguageIdentityInput, LanguageInput, LanguageKey, TargetIdentity, TargetIdentityInput};
use crate::libraries::package::identity::{ArtifactId, BuildRequestId, BuildRequestInput};
use crate::packages::python_environment::model::NativeToolchainRecord;
use crate::plugins::core::python::native::{RuntimeArchiveTarget, RuntimePlatform};
use eyre::{ContextCompat, Result, ensure};
use pakro_index::artifact::SourceArtifact;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod output;
pub(super) use output::{configuration_file, portable_output_digest};

#[cfg(all(test, unix))]
mod tests;

#[cfg(unix)]
mod build;
#[cfg(unix)]
pub(crate) use build::install_source;

pub(crate) const LOGICAL_PREFIX: &str = "/__pakro_cpython_v1__";
pub(crate) const SOURCE_RECIPE: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourcePythonLayout {
    pub(crate) schema: u32,
    /// Local layout bindings only. They never enter the portable source pin.
    pub(crate) bindings: BTreeMap<String, PathBuf>,
    /// Observed names for diagnostics, not permission to load an external SDK.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) system_runtime: Option<SourceSystemRuntime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceSystemRuntime {
    pub(crate) policy: u32,
    pub(crate) libraries: std::collections::BTreeSet<String>,
    pub(crate) optional_missing: std::collections::BTreeSet<String>,
}

#[cfg(unix)]
pub(crate) struct SourcePythonBuildReceipt {
    key: LanguageKey,
    inventory: crate::packages::python_environment::prefix::verify::TreeInventory,
    descriptor: crate::python_runtime::PythonRuntimeDescriptor,
    record: SourcePythonBuildRecord,
    layout: SourcePythonLayout,
}

#[cfg(unix)]
impl SourcePythonBuildReceipt {
    pub(super) fn validate_owner(&self, owner: &super::repository::LanguageBuildOwner) -> Result<()> {
        owner.checkpoint()?;
        ensure!(owner.spec().key() == self.key && owner.capture_inventory()? == self.inventory,
            "source CPython receipt differs from its actual installation owner");
        let input = owner.spec().plan().source_build().context("source CPython receipt has a prebuilt owner")?;
        ensure!(input.record(self.record.output_sha256, self.record.runtime_target)? == self.record,
            "source CPython receipt differs from its fixed original build inputs");
        ensure!(self.layout.system_runtime.as_ref().is_some_and(|runtime| runtime.policy == 1
            && runtime.libraries.len() <= 1024), "source CPython has no supported system-runtime observation");
        Ok(())
    }
    pub(super) fn into_parts(self) -> (crate::packages::python_environment::prefix::verify::TreeInventory,
        crate::python_runtime::PythonRuntimeDescriptor, SourcePythonBuildRecord, SourcePythonLayout) {
        (self.inventory, self.descriptor, self.record, self.layout)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PythonRuntimeSource { #[default] Prebuilt, Source }
impl PythonRuntimeSource {
    pub(crate) fn from_options(options: &toml::Table) -> Result<Self> {
        ensure!(options.keys().all(|key| key == "source"), "unsupported Python runtime options; only source is supported");
        match options.get("source") {
            None => Ok(Self::Prebuilt),
            Some(toml::Value::String(value)) if value == "prebuilt" => Ok(Self::Prebuilt),
            Some(toml::Value::String(value)) if value == "source" => Ok(Self::Source),
            _ => eyre::bail!("Python runtime source must be prebuilt or source; Conda runtime installation is not implemented"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PythonRuntimeRequest { pub(crate) version: String, pub(crate) source: PythonRuntimeSource }

#[derive(Debug, Clone)]
pub(crate) struct SourcePythonBuildInput {
    pub(crate) recipe: u32,
    pub(crate) version: String,
    pub(crate) source: SourceArtifact,
    pub(crate) target: RuntimeArchiveTarget,
    pub(crate) toolchain: NativeToolchainRecord,
    pub(crate) request: BuildRequestId,
    pub(crate) target_identity: TargetIdentity,
    pub(crate) expected_output: Option<[u8; 32]>,
    pub(crate) expected_runtime_target: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourcePythonBuildRecord {
    pub(crate) schema: u32,
    pub(crate) version: String,
    pub(crate) url: String,
    pub(crate) filename: String,
    pub(crate) artifact: String,
    pub(crate) arch: String,
    pub(crate) platform: String,
    pub(crate) toolchain: NativeToolchainRecord,
    pub(crate) recipe: u32,
    pub(crate) request_id: [u8; 32],
    pub(crate) output_sha256: [u8; 32],
    pub(crate) runtime_target: [u8; 32],
}

impl SourcePythonBuildInput {
    pub(crate) fn new(version: &str, source: SourceArtifact, target: RuntimeArchiveTarget,
        toolchain: NativeToolchainRecord, expected_output: Option<[u8; 32]>) -> Result<Self> {
        Self::with_recipe(version, source, target, toolchain, expected_output, SOURCE_RECIPE)
    }

    fn with_recipe(version: &str, source: SourceArtifact, target: RuntimeArchiveTarget,
        toolchain: NativeToolchainRecord, expected_output: Option<[u8; 32]>, recipe: u32) -> Result<Self> {
        ensure!(matches!(recipe, 1 | 2), "unsupported source CPython build recipe");
        let parts = version.split('.').map(str::parse::<u32>).collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(parts.len() == 3 && parts[0] == 3 && (11..=15).contains(&parts[1])
            && version == format!("{}.{}.{}", parts[0], parts[1], parts[2]),
            "source CPython requires a complete supported ordinary release version without a PBS build suffix");
        let filename = format!("Python-{version}.tar.xz");
        let url = format!("https://www.python.org/ftp/python/{version}/{filename}");
        ensure!(source.filename() == filename && source.url().as_str() == url,
            "source CPython requires the fixed official python.org tar.xz artifact");
        let (os, libc) = match target.platform() {
            RuntimePlatform::Macos => ("macos", "none"),
            RuntimePlatform::LinuxGnu => ("linux", "gnu"),
            RuntimePlatform::LinuxMusl => ("linux", "musl"),
        };
        ensure!(matches!(target.arch(), "aarch64" | "x86_64"), "source CPython native architecture is unsupported");
        ensure!(toolchain.platform == format!("{os}-{}-{libc}", target.arch()),
            "source CPython does not support cross compilation or a foreign toolchain");
        ensure!(["cc","cxx","ar","linker","make"].iter().all(|role| toolchain.programs.iter().any(|program| program.role == *role)),
            "source CPython build record is missing its required native tools");
        let toolchain_digest = toolchain.fingerprint()?;
        let compatibility = Sha256::digest(format!("cpython-source-release-gil-v1:{version}"));
        let target_identity = TargetIdentity::new(TargetIdentityInput { os, arch: target.arch(),
            libc: if libc == "gnu" { "glibc" } else { libc }, abi: &format!("cpython-{}{}-release-gil", parts[0], parts[1]),
            compatibility: compatibility.into() }, IdentityLimits::default())?;
        let mut configuration = BTreeMap::from([
            ("prefix-layout".into(), "logical-prefix-v1".into()),
            ("worker".into(), "supervised-cpython-v1".into()),
            ("release".into(), "gil-no-pgo-no-ensurepip".into()),
            ("source-date-epoch".into(), "315532800".into()),
        ]);
        if recipe == 2 { configuration.insert("test-modules".into(), "disabled".into()); }
        let request = BuildRequestId::new(BuildRequestInput { source: ArtifactId::new(source.digest()),
            host: &target_identity, target: &target_identity, build_dependencies: &[],
            toolchain: ArtifactId::new(pakro_store::Digest::Sha256(toolchain_digest)),
            configuration: &configuration, recipe }, Default::default())?;
        Ok(Self { recipe, version: version.into(), source, target, toolchain, request, target_identity, expected_output, expected_runtime_target: None })
    }

    pub(crate) fn includes_test_modules(&self) -> bool { self.recipe == 1 }

    #[cfg(test)]
    pub(crate) fn for_test_recipe(&self, recipe: u32) -> Result<Self> {
        let mut input = Self::with_recipe(&self.version, self.source.clone(), self.target.clone(),
            self.toolchain.clone(), self.expected_output, recipe)?;
        input.expected_runtime_target = self.expected_runtime_target;
        Ok(input)
    }

    pub(crate) fn language_key(&self) -> Result<LanguageKey> {
        Ok(LanguageKey::new(LanguageIdentityInput { implementation: "cpython", distribution: "cpython-source",
            exact_version: &self.version, target: &self.target_identity,
            input: LanguageInput::Build(self.request), recipe: 3 }, IdentityLimits::default())?)
    }

    pub(crate) fn record(&self, output_sha256: [u8; 32], runtime_target: [u8; 32]) -> Result<SourcePythonBuildRecord> {
        ensure!(output_sha256 != [0; 32], "source CPython output digest is zero");
        if let Some(expected) = self.expected_output {
            ensure!(expected == output_sha256,
                "rebuilt CPython output differs from its exact original lock result: expected {}, observed {}",
                hex::encode(expected), hex::encode(output_sha256));
        }
        ensure!(runtime_target != [0; 32] && self.expected_runtime_target.is_none_or(|expected| expected == runtime_target),
            "rebuilt CPython target differs from its exact original lock result");
        Ok(SourcePythonBuildRecord { schema: 1, version: self.version.clone(), url: self.source.url().to_string(),
            filename: self.source.filename().into(), artifact: self.source.digest().to_string(), arch: self.target.arch().into(),
            platform: match self.target.platform() { RuntimePlatform::Macos => "macos", RuntimePlatform::LinuxGnu => "linux-gnu", RuntimePlatform::LinuxMusl => "linux-musl" }.into(),
            toolchain: self.toolchain.clone(), recipe: self.recipe, request_id: self.request.fingerprint(), output_sha256, runtime_target })
    }
}

impl SourcePythonBuildRecord {
    /// Entry validation for a persisted source result. Internal callers retain
    /// the returned typed input instead of reinterpreting its source strings.
    pub(crate) fn input(&self) -> Result<SourcePythonBuildInput> {
        ensure!(self.schema == 1 && self.output_sha256 != [0; 32] && self.runtime_target != [0; 32],
            "unsupported or incomplete source CPython build result");
        let platform = match self.platform.as_str() { "macos" => RuntimePlatform::Macos,
            "linux-gnu" => RuntimePlatform::LinuxGnu, "linux-musl" => RuntimePlatform::LinuxMusl,
            _ => eyre::bail!("unsupported source CPython result platform") };
        let source = SourceArtifact::new(&self.filename, &self.url, self.artifact.parse()?)?;
        let mut input = SourcePythonBuildInput::with_recipe(&self.version, source, RuntimeArchiveTarget::new(&self.arch, platform)?,
            self.toolchain.clone(), Some(self.output_sha256), self.recipe)?;
        input.expected_runtime_target = Some(self.runtime_target);
        ensure!(input.request.fingerprint() == self.request_id, "source CPython build request identity differs from its fixed inputs");
        Ok(input)
    }
}

pub(crate) fn input_from_pin(version: &str, pin: &crate::lockfile::PlatformInfo) -> Result<Option<SourcePythonBuildInput>> {
    let Some(record) = &pin.source_python else { return Ok(None); };
    let input = record.input()?;
    ensure!(version == input.version && pin.install.as_deref() == Some("source")
        && pin.url.as_deref() == Some(input.source.url().as_str())
        && pin.checksum.as_deref() == Some(input.source.digest().to_string().replacen('-', ":", 1).as_str())
        && pin.conda_deps.is_none() && pin.additional_artifacts.is_empty(),
        "source CPython runtime pin differs from its fixed original source/build result");
    Ok(Some(input))
}
