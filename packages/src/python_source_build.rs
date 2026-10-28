//! Owned source-distribution preparation and isolated PEP 517 execution.
//!
//! A verified source, shared language lease and private workspace stay alive
//! until real backend output is checked. Descriptive records grant no writes.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use eyre::{Context, ContextCompat, Result, ensure};
use sha2::{Digest as _, Sha256};

use crate::artifact_snapshot::{VerifiedArtifact, spool_observed_reader, spool_verified_reader};
use crate::libraries::language::lease::LanguageLease;
use crate::packages::python_environment::model::{
    SourceBuildInput, SourceBuildObservation, SourceBuildRecord,
};
use crate::packages::python_environment::prefix::verify::{
    PrefixInventoryLimits, TreeInventory, observe_tree_inventory,
};
use crate::packages::python_environment::prefix::{
    EnvironmentStore, PrefixLimits, UnpublishedPrefix,
};
use crate::packages::python_wheel::{WheelExpectation, WheelLimits, WheelScheme};
use crate::packages::python_wheel_source::InspectedWheelArchive;
use crate::packages::python_wheel_source::inspect_owned_wheel;

mod archive;
mod cache;
pub(crate) mod cargo;
pub(crate) mod toolchain;
pub(crate) mod native;
mod worker;

#[cfg(test)]
static OBSERVED_HOOK_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(crate) fn observed_hook_calls() -> usize {
    OBSERVED_HOOK_CALLS.load(std::sync::atomic::Ordering::SeqCst)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceBuildLimits {
    pub max_archive_bytes: u64,
    pub max_decoded_bytes: u64,
    pub max_entries: usize,
    pub max_path_bytes: usize,
    pub max_total_path_bytes: usize,
    pub max_file_bytes: u64,
    pub max_hook_output_bytes: usize,
    pub max_metadata_bytes: u64,
    pub max_generated_wheel_bytes: u64,
    pub max_requirements: usize,
    pub max_hook_calls: usize,
    pub max_workspace_bytes: u64,
    pub max_workspace_entries: usize,
    pub max_decoder_memory_bytes: u64,
}

impl Default for SourceBuildLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 256 * 1024 * 1024,
            max_decoded_bytes: 512 * 1024 * 1024,
            max_entries: 131_072,
            max_path_bytes: 16_384,
            max_total_path_bytes: 64 * 1024 * 1024,
            max_file_bytes: 128 * 1024 * 1024,
            max_hook_output_bytes: 4 * 1024 * 1024,
            max_metadata_bytes: 16 * 1024 * 1024,
            max_generated_wheel_bytes: 512 * 1024 * 1024,
            max_requirements: 4096,
            max_hook_calls: 8,
            max_workspace_bytes: 1024 * 1024 * 1024,
            max_workspace_entries: 262_144,
            max_decoder_memory_bytes: 64 * 1024 * 1024,
        }
    }
}

impl SourceBuildLimits {
    fn validate(self) -> Result<()> {
        ensure!(
            [
                self.max_archive_bytes,
                self.max_decoded_bytes,
                self.max_file_bytes,
                self.max_metadata_bytes,
                self.max_generated_wheel_bytes,
                self.max_workspace_bytes
            ]
            .into_iter()
            .all(|value| value > 0)
                && [
                    self.max_entries,
                    self.max_path_bytes,
                    self.max_total_path_bytes,
                    self.max_hook_output_bytes,
                    self.max_requirements,
                    self.max_hook_calls,
                    self.max_workspace_entries
                ]
                .into_iter()
                .all(|value| value > 0),
            "source build limits must be positive"
        );
        ensure!(
            self.max_decoder_memory_bytes > 0 && self.max_decoder_memory_bytes <= 64 * 1024 * 1024,
            "source decoder memory must be positive and at most 64 MiB"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceArchiveFormat {
    TarGz,
    Zip,
    TarXz,
}

/// An owned private input tree, not a language build/publication receipt.
pub(crate) struct RawSourceTree {
    workspace: tempfile::TempDir,
    native_cleanup: pakro_util::native_build::NativeWorkspaceOwnership,
    reader: archive::WorkspaceReader,
    root: PathBuf,
    format: SourceArchiveFormat,
    deadline: Instant,
    max_file_bytes: u64,
    decoded_bytes: u64,
    extracted_entries: usize,
    extracted_path_bytes: usize,
}
impl RawSourceTree {
    pub(crate) fn workspace_path(&self) -> &Path { self.workspace.path() }
    pub(crate) fn native_cleanup_owner(&self) -> pakro_util::native_build::NativeWorkspaceOwnership { self.native_cleanup.clone() }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn format(&self) -> SourceArchiveFormat {
        self.format
    }
    pub(crate) fn decoded_bytes(&self) -> u64 {
        self.decoded_bytes
    }
    pub(crate) fn extracted_entries(&self) -> usize {
        self.extracted_entries
    }
    pub(crate) fn extracted_path_bytes(&self) -> usize {
        self.extracted_path_bytes
    }
    pub(crate) fn read_relative(&self, relative: &Path, maximum: u64) -> Result<Vec<u8>> {
        ensure!(
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "raw source member path must be a contained relative path"
        );
        ensure!(
            maximum > 0 && maximum <= self.max_file_bytes,
            "raw source read exceeds its original file budget"
        );
        ensure!(
            Instant::now() < self.deadline,
            "source extraction deadline expired"
        );
        let bytes = archive::read_bounded(&self.reader, &self.root.join(relative), maximum)?;
        ensure!(
            Instant::now() < self.deadline,
            "source extraction deadline expired"
        );
        Ok(bytes)
    }
}

struct ExtractedSource {
    raw: RawSourceTree,
    initial_requirements: Vec<String>,
    backend: String,
    backend_paths: Vec<PathBuf>,
}
impl std::ops::Deref for ExtractedSource {
    type Target = RawSourceTree;
    fn deref(&self) -> &RawSourceTree {
        &self.raw
    }
}
impl Drop for RawSourceTree {
    fn drop(&mut self) {
        if self.native_cleanup.is_delegated() {
            // The watcher alone may chmod/remove the tree until it confirms
            // every owned process stopped. A dropped future cannot await it.
            self.workspace.disable_cleanup(true);
            return;
        }
        #[cfg(unix)]
        {
            let _ = self.reader.restore_directory_permissions(1_048_576, |_| {});
        }
    }
}

pub(crate) fn extract_raw_source(
    source: &mut VerifiedArtifact,
    scratch: &Path,
    deadline: Instant,
    limits: SourceBuildLimits,
) -> Result<RawSourceTree> {
    archive::extract_raw(source, scratch, deadline, limits)
}

fn extract_source(
    source: &mut VerifiedArtifact,
    scratch: &Path,
    deadline: Instant,
    limits: SourceBuildLimits,
) -> Result<ExtractedSource> {
    archive::extract(source, scratch, deadline, limits)
}

pub(crate) struct SourceBuildSession {
    native: Option<native::NativeSourceContext>,
    source: VerifiedArtifact,
    name: String,
    version: String,
    language: LanguageLease,
    // Drop this controlled prefix before the enclosing private workspace.
    initial_environment: Option<BuildEnvironment>,
    extracted: ExtractedSource,
    scratch: PathBuf,
    deadline: Instant,
    limits: SourceBuildLimits,
    hook_calls: usize,
    previous_inputs: Vec<SourceBuildInput>,
    last_requirements: Option<Vec<String>>,
}

struct BuildEnvironment {
    prefix: UnpublishedPrefix,
    inventory: TreeInventory,
    site: PathBuf,
    inputs: Vec<SourceBuildInput>,
}

impl BuildEnvironment {
    fn recheck(&self) -> Result<()> {
        ensure!(
            observe_tree_inventory(&self.prefix, PrefixInventoryLimits::default())?
                == self.inventory,
            "fixed source build dependency content changed"
        );
        Ok(())
    }
}

impl SourceBuildSession {
    pub(crate) fn prepare(
        mut source: VerifiedArtifact,
        name: &str,
        version: &str,
        language: LanguageLease,
        scratch: &Path,
        deadline: Instant,
        limits: SourceBuildLimits,
    ) -> Result<Self> {
        use pakro_resolver::solve::{Pep440, VersionAdapter};
        limits.validate()?;
        ensure!(Instant::now() < deadline, "source build deadline expired");
        let name = pakro_index::python_source::NormalizedPythonName::new(name)?
            .as_str()
            .to_owned();
        ensure!(
            version.trim() == version && Pep440::parse_version(version).is_some(),
            "source build needs an exact upstream version"
        );
        language.check_binding()?;
        let extracted = extract_source(&mut source, scratch, deadline, limits)?;
        validate_requirements(&extracted.initial_requirements, limits)?;
        Ok(Self {
            native: None,
            source,
            name,
            version: version.into(),
            language,
            initial_environment: None,
            extracted,
            scratch: scratch.into(),
            deadline,
            limits,
            hook_calls: 0,
            previous_inputs: Vec::new(),
            last_requirements: None,
        })
    }

    pub(crate) fn initial_requirements(&self) -> &[String] {
        &self.extracted.initial_requirements
    }

    pub(crate) async fn configure_native(&mut self, context: &crate::programs::OperationContext,
        expected: Option<&SourceBuildRecord>) -> Result<()> {
        if expected.is_some_and(|record| record.recipe == 1) { return Ok(()); }
        let backend = self.extracted.backend.split(':').next().unwrap_or_default();
        let supported = matches!(backend, "setuptools.build_meta" | "scikit_build_core.build" | "mesonpy" | "maturin");
        if !supported { ensure!(expected.is_none_or(|record| record.recipe == 1), "native source recipe has an unsupported backend"); return Ok(()); }
        let source_inputs = native::inspect_source(&self.extracted, context, self.limits)?;
        if backend == "setuptools.build_meta" && expected.is_none() && !source_inputs.native { return Ok(()); }
        self.native = Some(native::NativeSourceContext::prepare(context, &self.language, &self.extracted,
            self.limits, expected, source_inputs).await?);
        Ok(())
    }

    pub(crate) fn decoded_bytes(&self) -> u64 {
        self.extracted.decoded_bytes
    }

    pub(crate) fn extracted_entries(&self) -> usize {
        self.extracted.extracted_entries
    }

    pub(crate) fn extracted_path_bytes(&self) -> usize {
        self.extracted.extracted_path_bytes
    }

    pub(crate) async fn build_with_initial_environment(mut self) -> Result<CompletedSourceBuild> {
        ensure!(
            self.last_requirements.as_ref().is_some_and(Vec::is_empty),
            "source build still has unresolved dynamic requirements"
        );
        let environment = self
            .initial_environment
            .take()
            .context("source build has no original requirements environment")?;
        self.build_in_environment(environment).await
    }

    pub(crate) fn workspace_path(&self) -> &Path {
        self.extracted.workspace.path()
    }

    pub(crate) async fn get_requires(
        &mut self,
        dependencies: Vec<(SourceBuildInput, InspectedWheelArchive)>,
    ) -> Result<Vec<String>> {
        let environment = self.environment(dependencies)?;
        let response = self
            .hook(&environment, "get_requires_for_build_wheel", None)
            .await?;
        let requirements = if response.missing {
            Vec::new()
        } else {
            serde_json::from_value(response.value)
                .context("source build requirements hook must return a list of strings")?
        };
        validate_requirements(&requirements, self.limits)?;
        self.previous_inputs = environment.inputs.clone();
        self.last_requirements = Some(requirements.clone());
        self.initial_environment = Some(environment);
        Ok(requirements)
    }

    pub(crate) async fn build(
        self,
        dependencies: Vec<(SourceBuildInput, InspectedWheelArchive)>,
    ) -> Result<CompletedSourceBuild> {
        let environment = self.environment(dependencies)?;
        self.build_in_environment(environment).await
    }

    async fn build_in_environment(
        mut self,
        environment: BuildEnvironment,
    ) -> Result<CompletedSourceBuild> {
        ensure!(
            self.last_requirements.is_some(),
            "source build requirements were not observed"
        );
        let response = self
            .hook(&environment, "prepare_metadata_for_build_wheel", None)
            .await?;
        let prepared_metadata = if response.missing {
            None
        } else {
            let name = single_component(&response.value, ".dist-info", self.limits.max_path_bytes)?;
            let path = response.directory.join(name);
            Some(capture_metadata(
                &self.extracted.reader,
                &path,
                self.deadline,
                self.limits,
            )?)
        };
        let response = self
            .hook(
                &environment,
                "build_wheel",
                prepared_metadata.as_ref().map(|value| value.root.as_path()),
            )
            .await?;
        ensure!(!response.missing, "source backend has no build_wheel hook");
        if let Some(native) = &mut self.native { native.close().await?; }
        let filename =
            single_component(&response.value, ".whl", self.limits.max_path_bytes)?.to_owned();
        let mut wheels = Vec::new();
        let directory = self.extracted.reader.directory(&response.directory)?;
        #[cfg(unix)]
        {
            use nix::fcntl::OFlag;
            use nix::sys::stat::Mode;
            use std::os::unix::ffi::OsStrExt;
            let mut listing = nix::dir::Dir::openat(
                &directory,
                ".",
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?;
            let mut entries = 0usize;
            for entry in listing.iter() {
                self.checkpoint()?;
                let entry = entry?;
                let name = std::ffi::OsStr::from_bytes(entry.file_name().to_bytes());
                if matches!(entry.file_name().to_bytes(), b"." | b"..") {
                    continue;
                }
                entries = entries
                    .checked_add(1)
                    .context("source output count overflow")?;
                ensure!(
                    entries <= self.limits.max_workspace_entries,
                    "source output entry budget exceeded"
                );
                if Path::new(name)
                    .extension()
                    .is_some_and(|value| value == "whl")
                {
                    wheels.push(name.to_owned());
                }
                ensure!(
                    wheels.len() <= 1,
                    "source backend produced ambiguous wheel outputs"
                );
            }
        }
        ensure!(
            wheels == [std::ffi::OsString::from(&filename)],
            "source backend did not return its sole wheel output"
        );
        let path = response.directory.join(&filename);
        let file = open_output(
            &self.extracted.reader,
            &path,
            self.limits.max_generated_wheel_bytes,
        )?;
        let observed = spool_observed_reader(
            DeadlineRead {
                input: file,
                deadline: self.deadline,
            },
            &filename,
            self.limits.max_generated_wheel_bytes,
            &self.scratch,
        )?;
        let digest = pakro_store::Digest::Sha256(observed.observed_sha256());
        let verified = spool_verified_reader(
            DeadlineRead {
                input: observed,
                deadline: self.deadline,
            },
            &filename,
            digest,
            self.limits.max_generated_wheel_bytes,
            &self.scratch,
        )?;
        let archive = inspect_owned_wheel(
            verified,
            digest,
            &WheelExpectation {
                filename: &filename,
                name: &self.name,
                version: &self.version,
                supported: self.language.target().supported(),
            },
            WheelLimits::default(),
        )
        .context("source backend output wheel validation failed")?;
        if let Some(metadata) = prepared_metadata {
            ensure!(
                capture_metadata(
                    &self.extracted.reader,
                    &metadata.root,
                    self.deadline,
                    self.limits,
                )?
                .files
                    == metadata.files,
                "prepared source metadata changed during wheel build"
            );
            ensure!(
                metadata.root.file_name().and_then(|value| value.to_str())
                    == Some(archive.manifest().dist_info()),
                "source wheel metadata directory differs from prepared metadata"
            );
            for (relative, (digest, size)) in metadata.files {
                // PEP 517 permits backend-private decision files here. They
                // remain available and unchanged for build_wheel, but need
                // not be members of the resulting wheel.
                if !recognized_wheel_metadata(&relative) {
                    continue;
                }
                let expected_path = format!(
                    "{}/{}",
                    archive.manifest().dist_info(),
                    relative.to_string_lossy().replace('\\', "/")
                );
                let member = archive
                    .manifest()
                    .members()
                    .iter()
                    .find(|member| member.archive_path() == expected_path)
                    .context("source wheel omitted prepared metadata")?;
                ensure!(
                    member.size() == size && member.observed_sha256() == &digest,
                    "source wheel metadata differs from prepared metadata"
                );
            }
        }
        environment.recheck()?;
        self.checkpoint()?;
        let request = self.request_id(&environment.inputs)?;
        let target_fingerprint = observed_target_fingerprint(&self.language)?;
        let language_key = self.language.key().fingerprint();
        let record = SourceBuildRecord {
            source_digest: self.source.digest().to_string(),
            request_id: request.fingerprint(),
            recipe: if self.native.is_some() { 2 } else { 1 },
            backend: self.extracted.backend.clone(),
            inputs: environment.inputs.clone(),
            output_filename: filename,
            output_digest: archive.digest().to_string(),
            output_size: archive.size(),
            metadata_sha256: Sha256::digest(archive.manifest().core_metadata().raw()).into(),
            observation: Some(SourceBuildObservation {
                initial_requirements: self.extracted.initial_requirements.clone(),
                initial_inputs: self.previous_inputs.clone(),
                dynamic_requirements: self
                    .last_requirements
                    .clone()
                    .context("source build requirements were not observed")?,
            }),
            native: self.native.as_ref().map(|native| native.record().clone()),
        };
        record.fingerprint()?;
        drop(environment);
        drop(self.initial_environment.take());
        Ok(CompletedSourceBuild {
            archive,
            record,
            source: self.source,
            language: self.language,
            request,
            deadline: self.deadline,
            target_fingerprint,
            language_key,
        })
    }

    fn checkpoint(&self) -> Result<()> {
        ensure!(
            Instant::now() < self.deadline,
            "source build deadline expired"
        );
        self.language.check_held_binding()
    }

    fn environment(
        &self,
        mut dependencies: Vec<(SourceBuildInput, InspectedWheelArchive)>,
    ) -> Result<BuildEnvironment> {
        use crate::packages::python_wheel_install::{
            WheelInstallLimits, WheelInstallTarget, materialize_wheel_graph, prepare_wheel_graph,
        };
        use crate::packages::python_wheel_plan::{WheelInstallScheme, WheelPlanLimits};
        use crate::packages::python_wheel_plan::{WheelReservedDestination, WheelReservedKind};
        self.checkpoint()?;
        let used = worker::workspace_usage(&self.extracted, self.limits, self.deadline)?;
        // New prefix control files and the wrapper need a small fixed reserve.
        // Preparation may also hold anonymous transformed/generated spools,
        // so never assign all remaining bytes to destination payload alone.
        let available = self
            .limits
            .max_workspace_bytes
            .checked_sub(used.bytes)
            .and_then(|remaining| remaining.checked_sub(64 * 1024))
            .context("source build has no remaining workspace byte budget")?;
        let payload_budget = available / 2;
        ensure!(
            payload_budget > 0,
            "source build has no remaining workspace payload budget"
        );
        let entry_budget = self
            .limits
            .max_workspace_entries
            .checked_sub(used.entries)
            .and_then(|remaining| remaining.checked_sub(64))
            .filter(|remaining| *remaining > 0)
            .context("source build has no remaining workspace entry budget")?;
        let path_budget = self
            .limits
            .max_total_path_bytes
            .checked_sub(used.path_bytes)
            .and_then(|remaining| remaining.checked_sub(64 * 1024))
            .filter(|remaining| *remaining > 0)
            .context("source build has no remaining workspace path budget")?;
        ensure!(
            dependencies.len() <= self.limits.max_requirements,
            "source build dependency budget exceeded"
        );
        dependencies.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        let mut inputs = Vec::new();
        let mut archives = Vec::new();
        let mut total_bytes = 0u64;
        let mut previous_name: Option<String> = None;
        for (input, archive) in dependencies {
            input.content_pin()?;
            ensure!(
                Instant::now() < self.deadline,
                "source build dependency preparation deadline expired"
            );
            ensure!(
                previous_name.as_deref() != Some(input.name.as_str()),
                "duplicate source build dependency name"
            );
            previous_name = Some(input.name.clone());
            ensure!(
                input.name == archive.manifest().name()
                    && input.version == archive.manifest().version()
                    && input.output_filename == archive.filename()
                    && input.output_digest == archive.digest().to_string()
                    && input.output_size == archive.size(),
                "source build input description differs from its owned wheel"
            );
            total_bytes = total_bytes
                .checked_add(archive.actual_uncompressed_bytes())
                .filter(|value| *value <= payload_budget)
                .context("source build dependency expanded byte budget exceeded")?;
            inputs.push(input);
            archives.push(archive);
        }
        // Reject invalid fixed provenance before any backend code is run.
        self.request_id(&inputs)?;
        for previous in &self.previous_inputs {
            ensure!(
                inputs
                    .iter()
                    .any(|input| canonical_input(input) == canonical_input(previous)),
                "source build initial dependency identity changed"
            );
        }
        let store_root = self
            .extracted
            .workspace
            .path()
            .join(format!("deps-{}", uuid::Uuid::now_v7().simple()));
        let store = EnvironmentStore::open_shared_writer(
            &store_root,
            current_uid(),
            PrefixLimits {
                max_total_bytes: payload_budget,
                max_entries: entry_budget,
                ..Default::default()
            },
            self.deadline,
        )?;
        let mut prefix = store.allocate()?;
        let site = prefix.final_path().join("site-packages");
        let roots = BTreeMap::from([
            (WheelScheme::Purelib, site.clone()),
            (WheelScheme::Platlib, site.clone()),
            (WheelScheme::Headers, prefix.final_path().join("include")),
            (WheelScheme::Scripts, prefix.final_path().join("bin")),
            (WheelScheme::Data, prefix.final_path().join("data")),
        ]);
        let scheme = WheelInstallScheme::new(prefix.final_path().to_owned(), roots)?;
        // Use the same lightweight reserved adapter as shared package trees.
        // The real hook process still executes the held shared interpreter.
        let interpreter = prefix.final_path().join("bin/.pakro python");
        let executable = self
            .language
            .entrypoint()
            .to_str()
            .context("source build interpreter is not UTF-8")?;
        ensure!(
            !executable.contains(['\n', '\r', '\0']),
            "source build interpreter path is unsafe"
        );
        let wrapper = format!(
            "#!/bin/sh\nexport PYTHONDONTWRITEBYTECODE=1\nexport PYTHONNOUSERSITE=1\nexec '{}' \"$@\"\n",
            executable.replace('\'', "'\\''")
        );
        prefix.create_dir_all(Path::new("bin"))?;
        {
            let mut file = prefix.create_file_exclusive(Path::new("bin/.pakro python"), 0o755)?;
            file.write_all(wrapper.as_bytes())?;
            file.finish()?;
        }
        let reserved = [WheelReservedDestination {
            path: interpreter.clone(),
            kind: WheelReservedKind::File,
        }];
        let prepared = prepare_wheel_graph(
            archives,
            &WheelInstallTarget {
                scheme: &scheme,
                interpreter: &interpreter,
                reserved: &reserved,
            },
            WheelLimits {
                max_total_uncompressed: payload_budget,
                max_member_bytes: payload_budget,
                ..Default::default()
            },
            WheelPlanLimits {
                max_wheels: self.limits.max_requirements,
                max_sources: entry_budget,
                max_files: entry_budget,
                max_directories: entry_budget,
                max_total_bytes: payload_budget,
                max_path_bytes: self.limits.max_path_bytes,
            },
            WheelInstallLimits {
                max_generated_bytes: payload_budget,
                max_record_bytes: usize::try_from(payload_budget.min(64 * 1024 * 1024))?,
                max_record_rows: entry_budget,
                ..Default::default()
            },
            self.extracted.workspace.path(),
        )?;
        let planned_path_bytes = prepared
            .plan()
            .files()
            .iter()
            .map(|entry| entry.destination())
            .chain(
                prepared
                    .plan()
                    .directories()
                    .iter()
                    .map(|entry| entry.destination()),
            )
            .try_fold(0usize, |total, path| {
                total
                    .checked_add(self.extracted.reader.relative_bytes(path)?)
                    .filter(|total| *total <= path_budget)
                    .context("source build dependency workspace path budget exceeded")
            })?;
        ensure!(
            planned_path_bytes <= path_budget,
            "source build dependency workspace path budget exceeded"
        );
        materialize_wheel_graph(prepared, &mut prefix)?;
        // Empty dependency sets still expose one explicit, empty search root.
        if !site.exists() {
            prefix.create_dir_all(Path::new("site-packages"))?;
        }
        let inventory = observe_tree_inventory(&prefix, PrefixInventoryLimits::default())?;
        worker::workspace_usage(&self.extracted, self.limits, self.deadline)?;
        if self.native.is_some() { prefix.delegate_cleanup_to_workspace(self.workspace_path())?; }
        Ok(BuildEnvironment {
            prefix,
            inventory,
            site,
            inputs,
        })
    }

    async fn hook(
        &mut self,
        environment: &BuildEnvironment,
        hook: &str,
        metadata: Option<&Path>,
    ) -> Result<worker::HookReply> {
        self.checkpoint()?;
        environment.recheck()?;
        self.hook_calls = self
            .hook_calls
            .checked_add(1)
            .filter(|value| *value <= self.limits.max_hook_calls)
            .context("source build hook call budget exceeded")?;
        #[cfg(test)]
        OBSERVED_HOOK_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let result = if let Some(native) = &mut self.native {
            native.run(&self.language, &self.extracted, environment, hook, metadata, self.deadline, self.limits).await
        } else { worker::run(
            &self.language,
            &self.extracted,
            environment,
            hook,
            metadata,
            self.deadline,
            self.limits,
        )
        .await };
        self.checkpoint()?;
        environment.recheck()?;
        result
    }

    fn request_id(
        &self,
        inputs: &[SourceBuildInput],
    ) -> Result<crate::libraries::package::identity::BuildRequestId> {
        use crate::libraries::package::identity::{
            ArtifactId, BuildRequestId, BuildRequestInput, PackageIdentityInput, PackageInstanceId,
            PackageRole, SourceIdentity,
        };
        let target = self.language.shared_target_identity()?;
        let mut keys = Vec::new();
        for input in inputs {
            let pin =
                hex::decode(&input.source_pin).context("source build dependency pin is invalid")?;
            let pin: [u8; 32] = pin
                .try_into()
                .map_err(|_| eyre::eyre!("source build dependency pin length is invalid"))?;
            let source = SourceIdentity::new(&input.registry, pin, Default::default())?;
            keys.push(PackageInstanceId::python(
                PackageIdentityInput {
                    name: &input.name,
                    exact_version: &input.version,
                    source: &source,
                    role: PackageRole::Build,
                    target: &target,
                    dependency_context: Sha256::digest(serde_json::to_vec(&canonical_input(
                        input,
                    ))?)
                    .into(),
                },
                Default::default(),
            )?);
        }
        let driver_hash: [u8; 32] = if self.native.is_some() {
            Sha256::digest(include_bytes!("python_source_build/hook_native.py")).into()
        } else { Sha256::digest(include_bytes!("python_source_build/hook.py")).into() };
        let mut toolchain = Sha256::new();
        toolchain.update(driver_hash);
        toolchain.update(self.language.key().as_ref());
        if let Some(native) = &self.native { toolchain.update(native.record().fingerprint()?); }
        let relative_backend_paths: Vec<_> = self
            .extracted
            .backend_paths
            .iter()
            .map(|path| {
                path.strip_prefix(&self.extracted.root)
                    .map(|path| path.to_string_lossy().into_owned())
            })
            .collect::<std::result::Result<_, _>>()?;
        let mut configuration = BTreeMap::from([
            ("backend".into(), self.extracted.backend.clone()),
            (
                "backend-paths".into(),
                hex::encode(Sha256::digest(serde_json::to_vec(&relative_backend_paths)?)),
            ),
            ("language".into(), hex::encode(self.language.key().as_ref())),
            ("worker".into(), "isolated-no-fork-v1".into()),
            ("source-date-epoch".into(), "315532800".into()),
        ]);
        if let Some(native) = &self.native {
            configuration.insert("worker".into(), "supervised-native-v2".into());
            configuration.insert("native-toolchain".into(), hex::encode(native.record().fingerprint()?));
        }
        Ok(BuildRequestId::new(
            BuildRequestInput {
                source: ArtifactId::new(self.source.digest()),
                host: &target,
                target: &target,
                build_dependencies: &keys,
                toolchain: ArtifactId::new(pakro_store::Digest::Sha256(
                    toolchain.finalize().into(),
                )),
                configuration: &configuration,
                recipe: if self.native.is_some() { 2 } else { 1 },
            },
            Default::default(),
        )?)
    }
}

pub(crate) struct CompletedSourceBuild {
    archive: InspectedWheelArchive,
    record: SourceBuildRecord,
    source: VerifiedArtifact,
    language: LanguageLease,
    request: crate::libraries::package::identity::BuildRequestId,
    deadline: Instant,
    target_fingerprint: [u8; 32],
    language_key: [u8; 32],
}

impl CompletedSourceBuild {
    pub(crate) fn target_fingerprint(&self) -> [u8; 32] {
        self.target_fingerprint
    }

    pub(crate) fn language_key(&self) -> [u8; 32] {
        self.language_key
    }

    pub(crate) fn record(&self) -> &SourceBuildRecord {
        &self.record
    }

    pub(crate) fn recheck(&self) -> Result<()> {
        ensure!(
            Instant::now() < self.deadline,
            "source build completion deadline expired"
        );
        self.language.check_binding()?;
        self.recheck_held_output()
    }

    fn recheck_held_output(&self) -> Result<()> {
        ensure!(
            Instant::now() < self.deadline,
            "source build completion deadline expired"
        );
        self.language.check_held_binding()?;
        ensure!(
            self.language_key == self.language.key().fingerprint()
                && self.target_fingerprint == observed_target_fingerprint(&self.language)?,
            "source build completion target changed"
        );
        self.archive.check_limits(WheelLimits::default())?;
        self.record.fingerprint()?;
        ensure!(
            self.record.request_id == self.request.fingerprint()
                && self.record.source_digest == self.source.digest().to_string()
                && self.record.output_filename == self.archive.filename()
                && self.record.output_digest == self.archive.digest().to_string()
                && self.record.output_size == self.archive.size()
                && self.record.metadata_sha256
                    == <[u8; 32]>::from(Sha256::digest(
                        self.archive.manifest().core_metadata().raw()
                    )),
            "source build completion differs from actual owned output"
        );
        Ok(())
    }

    pub(crate) fn into_parts(self) -> Result<(InspectedWheelArchive, SourceBuildRecord)> {
        self.recheck()?;
        Ok((self.archive, self.record))
    }
}

fn observed_target_fingerprint(language: &LanguageLease) -> Result<[u8; 32]> {
    use crate::packages::python_resolution::source::locked_graph::FrozenRegistryTarget;
    Ok(*FrozenRegistryTarget::new(
        language.target().environment(),
        language.target().supported(),
        pakro_index::python_markers::PythonMarkerScope::Environment,
        language.target().semantics(),
        Default::default(),
    )?
    .fingerprint())
}

fn canonical_input(input: &SourceBuildInput) -> SourceBuildInput {
    let mut input = input.clone();
    input.advertised = false;
    input
}

fn validate_requirements(requirements: &[String], limits: SourceBuildLimits) -> Result<()> {
    ensure!(
        requirements.len() <= limits.max_requirements,
        "source build requirement budget exceeded"
    );
    let mut bytes = 0u64;
    for raw in requirements {
        bytes = bytes
            .checked_add(raw.len() as u64)
            .filter(|value| *value <= limits.max_metadata_bytes)
            .context("source build requirement byte budget exceeded")?;
        let parsed =
            pakro_index::python_requirements::parse_python_requirement(raw, Default::default())?;
        ensure!(
            parsed.unresolved_reference().is_none(),
            "source build requirements currently require registry references"
        );
    }
    Ok(())
}

fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        nix::unistd::Uid::current().as_raw()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn single_component<'a>(
    value: &'a serde_json::Value,
    suffix: &str,
    maximum: usize,
) -> Result<&'a str> {
    let name = value
        .as_str()
        .context("source hook must return an output basename")?;
    ensure!(
        !name.is_empty()
            && name.len() <= maximum
            && !matches!(name, "." | "..")
            && !name.contains(['/', '\\'])
            && !name.chars().any(char::is_control)
            && name.ends_with(suffix),
        "source hook returned an unsafe output basename"
    );
    Ok(name)
}

fn open_output(
    workspace: &archive::WorkspaceReader,
    path: &Path,
    maximum: u64,
) -> Result<fs::File> {
    workspace.regular(path, maximum)
}

struct DeadlineRead<R> {
    input: R,
    deadline: Instant,
}
impl<R: Read> Read for DeadlineRead<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "source output verification deadline expired",
            ));
        }
        self.input.read(bytes)
    }
}

struct PreparedMetadata {
    root: PathBuf,
    files: BTreeMap<PathBuf, ([u8; 32], u64)>,
}

fn recognized_wheel_metadata(path: &Path) -> bool {
    matches!(
        path.to_str(),
        Some(
            "METADATA" | "WHEEL" | "entry_points.txt" | "top_level.txt" | "namespace_packages.txt"
        )
    ) || path
        .strip_prefix("licenses")
        .is_ok_and(|relative| !relative.as_os_str().is_empty())
}

fn capture_metadata(
    workspace: &archive::WorkspaceReader,
    root: &Path,
    deadline: Instant,
    limits: SourceBuildLimits,
) -> Result<PreparedMetadata> {
    #[cfg(not(unix))]
    eyre::bail!("source metadata verification requires a supported Unix platform");
    #[cfg(unix)]
    {
        use nix::fcntl::{AtFlags, OFlag};
        use nix::sys::stat::{Mode, fstatat};
        use std::os::unix::ffi::OsStrExt;
        let root_directory = workspace.directory(root)?;
        let mut files = BTreeMap::new();
        let mut pending = vec![(root_directory, root.to_owned())];
        let mut bytes = 0u64;
        let mut count = 0usize;
        while let Some((directory, directory_path)) = pending.pop() {
            let mut listing = nix::dir::Dir::openat(
                &directory,
                ".",
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?;
            for entry in listing.iter() {
                ensure!(
                    Instant::now() < deadline,
                    "source metadata verification deadline expired"
                );
                let entry = entry?;
                let raw = entry.file_name().to_bytes();
                if matches!(raw, b"." | b"..") {
                    continue;
                }
                let name = std::ffi::OsStr::from_bytes(raw);
                count += 1;
                ensure!(
                    count <= limits.max_entries,
                    "source metadata entry budget exceeded"
                );
                let path = directory_path.join(name);
                let metadata = fstatat(&directory, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
                let kind = metadata.st_mode & nix::libc::S_IFMT;
                if kind == nix::libc::S_IFDIR {
                    pending.push((workspace.directory(&path)?, path));
                    continue;
                }
                ensure!(
                    kind == nix::libc::S_IFREG,
                    "source metadata links and special files are forbidden"
                );
                bytes = bytes
                    .checked_add(
                        u64::try_from(metadata.st_size).context("source metadata size invalid")?,
                    )
                    .filter(|value| *value <= limits.max_metadata_bytes)
                    .context("source metadata byte budget exceeded")?;
                let raw = archive::read_bounded(workspace, &path, limits.max_metadata_bytes)?;
                files.insert(
                    path.strip_prefix(root)?.to_owned(),
                    (Sha256::digest(&raw).into(), raw.len() as u64),
                );
            }
        }
        ensure!(
            files.contains_key(Path::new("METADATA")),
            "source metadata hook omitted METADATA"
        );
        Ok(PreparedMetadata {
            root: root.to_owned(),
            files,
        })
    }
}

#[cfg(test)]
mod tests;
