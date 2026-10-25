//! Native registry source-build coordination over the original solver/owners.
//! Build requirements may contain ranges; user direct pins remain exact.

use std::collections::{BTreeMap, BTreeSet};

use eyre::{Context, ContextCompat, Result, ensure};
use futures_util::future::BoxFuture;

use crate::libraries::language::lease::LanguageLease;
use crate::packages::python_environment::model::{SourceBuildInput, SourceBuildRecord};
use crate::packages::python_resolution::lock_model::{PinEvidence, PinId, RegistryArtifactKind};
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use crate::packages::python_resolution::source::registry_depot::closed::{
    ClosedRegistryGraph, close_registry_plan,
};
use crate::packages::python_resolution::source::registry_depot::{
    RegistryDepotContext, RegistryDepotUsage, RegistryPreparation, RegistryPreparedPlan,
    ResolveRegistryDepot,
};
use crate::packages::python_source_build::{
    CompletedSourceBuild, SourceBuildLimits, SourceBuildSession,
};
use crate::packages::python_wheel_source::InspectedWheelArchive;
use crate::programs::OperationContext;

#[path = "build/replay.rs"]
mod replay;
pub(crate) use replay::replay_registry;

pub(crate) struct BuildBudget {
    active: BTreeSet<[u8; 32]>,
    builds: usize,
    bytes: u64,
    utf8: usize,
    steps: usize,
    projects: usize,
    metadata: usize,
    policy_work: usize,
    policy_utf8: usize,
    decoded: u64,
    extracted_entries: usize,
    extracted_path_bytes: usize,
    workspace_reserved: u64,
    cached_bytes: u64,
    copied_bytes: u64,
    // This budget belongs to one frozen operation/settings/recipe. Each key
    // also binds the original registry ContentPin and complete actual target.
    completed: BTreeMap<([u8; 32], [u8; 32], [u8; 32]), (CompletedSourceBuild, u64)>,
    records: BTreeMap<PinId, SourceBuildRecord>,
}
impl BuildBudget {
    pub(crate) fn new() -> Self {
        Self {
            active: BTreeSet::new(),
            builds: 0,
            bytes: 0,
            utf8: 0,
            steps: 0,
            projects: 0,
            metadata: 0,
            policy_work: 0,
            policy_utf8: 0,
            decoded: 0,
            extracted_entries: 0,
            extracted_path_bytes: 0,
            workspace_reserved: 0,
            cached_bytes: 0,
            copied_bytes: 0,
            completed: BTreeMap::new(),
            records: BTreeMap::new(),
        }
    }
    pub(crate) fn records(&self) -> Vec<(PinId, SourceBuildRecord)> {
        self.records
            .iter()
            .map(|(id, record)| (*id, record.clone()))
            .collect()
    }
    fn charge(&mut self, previous: &mut RegistryDepotUsage, now: RegistryDepotUsage) -> Result<()> {
        let bytes = now
            .retained_bytes
            .checked_sub(previous.retained_bytes)
            .context("registry source budget was reset while resuming")?;
        let utf8 = now
            .retained_utf8
            .checked_sub(previous.retained_utf8)
            .context("registry text budget was reset while resuming")?;
        let steps = now
            .steps
            .checked_sub(previous.steps)
            .context("registry work budget was reset while resuming")?;
        let projects = now
            .projects
            .checked_sub(previous.projects)
            .context("registry project budget was reset while resuming")?;
        let metadata = now
            .metadata
            .checked_sub(previous.metadata)
            .context("registry metadata budget was reset while resuming")?;
        let policy_work = now
            .policy_work
            .checked_sub(previous.policy_work)
            .context("registry candidate work budget was reset while resuming")?;
        let policy_utf8 = now
            .policy_utf8
            .checked_sub(previous.policy_utf8)
            .context("registry candidate text budget was reset while resuming")?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= 4 * 1024 * 1024 * 1024)
            .context("cumulative source-build artifact budget exceeded")?;
        self.utf8 = self
            .utf8
            .checked_add(utf8)
            .filter(|n| *n <= 64 * 1024 * 1024)
            .context("cumulative source-build text budget exceeded")?;
        self.steps = self
            .steps
            .checked_add(steps)
            .filter(|n| *n <= 131_072)
            .context("cumulative source-build work budget exceeded")?;
        self.projects = self
            .projects
            .checked_add(projects)
            .filter(|n| *n <= 4096)
            .context("cumulative source-build project budget exceeded")?;
        self.metadata = self
            .metadata
            .checked_add(metadata)
            .filter(|n| *n <= 8192)
            .context("cumulative source-build metadata budget exceeded")?;
        self.policy_work = self
            .policy_work
            .checked_add(policy_work)
            .filter(|n| *n <= 16_777_216)
            .context("cumulative source-build candidate work budget exceeded")?;
        self.policy_utf8 = self
            .policy_utf8
            .checked_add(policy_utf8)
            .filter(|n| *n <= 64 * 1024 * 1024)
            .context("cumulative source-build candidate text budget exceeded")?;
        *previous = now;
        Ok(())
    }
    fn enter(&mut self, source: [u8; 32], depth: usize) -> Result<()> {
        ensure!(depth < 16, "source-build dependency depth exceeded");
        ensure!(
            !self.active.contains(&source),
            "source-build dependency cycle detected"
        );
        self.builds = self
            .builds
            .checked_add(1)
            .filter(|n| *n <= 128)
            .context("source-build count budget exceeded")?;
        self.active.insert(source);
        Ok(())
    }
    fn charge_copy(&mut self, bytes: u64) -> Result<()> {
        self.copied_bytes = self
            .copied_bytes
            .checked_add(bytes)
            .filter(|n| *n <= 4_u64 * 1024 * 1024 * 1024)
            .context("cumulative sealed source-build copy budget exceeded")?;
        Ok(())
    }
}

pub(crate) fn prepare_registry<'root, 'config, 'call>(
    context: &'call OperationContext,
    language: &'call LanguageLease,
    root: &'root BoundPythonLock,
    declarations: Vec<String>,
    registry: RegistryDepotContext<'config>,
    budget: &'call mut BuildBudget,
    depth: usize,
) -> BoxFuture<'call, Result<RegistryPreparedPlan<'root>>>
where
    'root: 'call,
    'config: 'call,
{
    Box::pin(async move {
        context.checkpoint()?;
        language.check_binding()?;
        let references = declarations.iter().map(String::as_str).collect::<Vec<_>>();
        let depot = ResolveRegistryDepot::new(root, &references, registry.clone())?;
        let mut state = depot.prepare().await?;
        let mut accounted = RegistryDepotUsage {
            retained_bytes: 0,
            retained_utf8: 0,
            steps: 0,
            projects: 0,
            metadata: 0,
            policy_work: 0,
            policy_utf8: 0,
        };
        loop {
            context.checkpoint()?;
            language.check_binding()?;
            match state {
                RegistryPreparation::Ready(prepared) => {
                    budget.charge(&mut accounted, prepared.usage())?;
                    return Ok(prepared);
                }
                RegistryPreparation::NeedsBuild(mut continuation) => {
                    budget.charge(&mut accounted, continuation.usage())?;
                    let source_id = *continuation.pin().id().as_bytes();
                    let source_name = continuation.pin().file().project().to_owned();
                    let source_version = continuation.pin().file().version().to_owned();
                    let source_bytes = continuation.pin().size();
                    let target = crate::packages::python_resolution::source::locked_graph::FrozenRegistryTarget::new(
                        language.target().environment(),
                        language.target().supported(),
                        pakro_index::python_markers::PythonMarkerScope::Environment,
                        language.target().semantics(),
                        Default::default(),
                    )?;
                    let cache_key = (
                        source_id,
                        language.key().fingerprint(),
                        *target.fingerprint(),
                    );
                    let scratch = context.requested_cache_root().join("shared-python/scratch");
                    if let Some((completed, cached_source_bytes)) =
                        budget.completed.remove(&cache_key)
                    {
                        ensure!(
                            source_bytes == cached_source_bytes,
                            "cached source build differs from its original source size"
                        );
                        let output_bytes = completed.record().output_size;
                        budget.charge_copy(
                            source_bytes
                                .checked_add(output_bytes)
                                .context("sealed build copy size overflow")?,
                        )?;
                        let (retained, replay) = completed.duplicate_for_attempt(
                            &scratch,
                            context.policy().deadline,
                            source_bytes,
                            output_bytes,
                        )?;
                        budget.completed.insert(cache_key, (retained, source_bytes));
                        state = continuation.resume_with_built(replay).await?;
                        continue;
                    }
                    budget.enter(source_id, depth)?;
                    let workspace_limit = SourceBuildLimits::default().max_workspace_bytes.min(
                        (4_u64 * 1024 * 1024 * 1024)
                            .checked_sub(budget.workspace_reserved)
                            .context("active source-build workspace budget exceeded")?,
                    );
                    ensure!(
                        workspace_limit > 0,
                        "active source-build workspace budget exceeded"
                    );
                    budget.workspace_reserved += workspace_limit;
                    let progress = crate::ui::multi_progress_report::MultiProgressReport::get()
                        .add_pre_backend(&format!("{source_name}@{source_version}"));
                    progress.set_message("prepare source build".into());
                    let outcome = async {
                        std::fs::create_dir_all(&scratch)?;
                        let workspace = tempfile::Builder::new()
                            .prefix("source-coordinator-")
                            .tempdir_in(&scratch)?;
                        let source = continuation.source_verified_copy(workspace.path())?;
                        budget.charge(&mut accounted, continuation.usage())?;
                        let mut session = SourceBuildSession::prepare(
                            source,
                            &source_name,
                            &source_version,
                            language.clone(),
                            &scratch,
                            context.policy().deadline,
                            SourceBuildLimits {
                                max_decoded_bytes: (2_u64 * 1024 * 1024 * 1024)
                                    .checked_sub(budget.decoded)
                                    .context("cumulative source expansion budget exceeded")?
                                    .min(SourceBuildLimits::default().max_decoded_bytes),
                                max_workspace_bytes: workspace_limit,
                                max_entries: 131_072usize
                                    .checked_sub(budget.extracted_entries)
                                    .context("cumulative extracted entry budget exceeded")?,
                                max_total_path_bytes: (64usize * 1024 * 1024)
                                    .checked_sub(budget.extracted_path_bytes)
                                    .context("cumulative extracted path budget exceeded")?,
                                ..Default::default()
                            },
                        )?;
                        budget.decoded = budget
                            .decoded
                            .checked_add(session.decoded_bytes())
                            .filter(|n| *n <= 2_u64 * 1024 * 1024 * 1024)
                            .context("cumulative source expansion budget exceeded")?;
                        budget.extracted_entries = budget
                            .extracted_entries
                            .checked_add(session.extracted_entries())
                            .filter(|n| *n <= 131_072)
                            .context("cumulative extracted entry budget exceeded")?;
                        budget.extracted_path_bytes = budget
                            .extracted_path_bytes
                            .checked_add(session.extracted_path_bytes())
                            .filter(|n| *n <= 64 * 1024 * 1024)
                            .context("cumulative extracted path budget exceeded")?;
                        let initial = session.initial_requirements().to_vec();
                        progress.set_message("resolve initial build requirements".into());
                        // LockSession serializes one entire parent directory,
                        // regardless of the requested lock filename. These
                        // two live sessions therefore need distinct parents.
                        let initial_directory = workspace.path().join("initial");
                        std::fs::create_dir(&initial_directory)?;
                        let initial_root = BoundPythonLock::acquire(
                            &initial_directory.join("pakro.lock"),
                            Default::default(),
                            context.policy().deadline,
                        )?;
                        let initial_plan = prepare_registry(
                            context,
                            language,
                            &initial_root,
                            initial.clone(),
                            registry.clone(),
                            budget,
                            depth + 1,
                        )
                        .await?;
                        let initial_graph = close_registry_plan(initial_plan, Default::default())?;
                        let initial_inputs = build_inputs(&initial_graph)?;
                        let (initial_archives, initial_evidence) =
                            initial_graph.into_shared_sources()?;
                        initial_evidence
                            .recheck(tokio::time::Instant::from_std(context.policy().deadline))?;
                        progress.set_message("observe source build requirements".into());
                        let dynamic = session
                            .get_requires(pair(initial_inputs.clone(), initial_archives)?)
                            .await?;
                        initial_evidence
                            .recheck(tokio::time::Instant::from_std(context.policy().deadline))?;
                        ensure!(
                            dynamic.len() <= 4096,
                            "dynamic build requirement count exceeded"
                        );
                        if dynamic.is_empty() {
                            progress.set_message("build and verify source wheel".into());
                            let built = session.build_with_initial_environment().await?;
                            initial_evidence.recheck(tokio::time::Instant::from_std(
                                context.policy().deadline,
                            ))?;
                            initial_root.recheck()?;
                            context.checkpoint()?;
                            language.check_binding()?;
                            return Ok(built);
                        }
                        let mut combined = initial;
                        combined.extend(dynamic);
                        // The backend observed its initial exact environment. New
                        // requirements cannot silently replace those input versions.
                        combined.extend(
                            initial_inputs
                                .iter()
                                .map(|input| format!("{}=={}", input.name, input.version)),
                        );
                        combined.sort();
                        combined.dedup();
                        ensure!(
                            combined.len() <= 8192,
                            "combined build requirement count exceeded"
                        );
                        let final_directory = workspace.path().join("final");
                        std::fs::create_dir(&final_directory)?;
                        let final_root = BoundPythonLock::acquire(
                            &final_directory.join("pakro.lock"),
                            Default::default(),
                            context.policy().deadline,
                        )?;
                        progress.set_message("resolve dynamic build requirements".into());
                        let final_plan = prepare_registry(
                            context,
                            language,
                            &final_root,
                            combined,
                            registry.clone(),
                            budget,
                            depth + 1,
                        )
                        .await?;
                        let final_graph = close_registry_plan(final_plan, Default::default())?;
                        let final_inputs = build_inputs(&final_graph)?;
                        for initial in &initial_inputs {
                            let Some(final_input) =
                                final_inputs.iter().find(|input| input.name == initial.name)
                            else {
                                eyre::bail!(
                                    "an initial build input disappeared after dynamic requirements"
                                );
                            };
                            let mut left = initial.clone();
                            let mut right = final_input.clone();
                            left.advertised = false;
                            right.advertised = false;
                            ensure!(
                                left == right,
                                "an initial fixed build input changed after dynamic requirements"
                            );
                        }
                        let (final_archives, final_evidence) = final_graph.into_shared_sources()?;
                        final_evidence
                            .recheck(tokio::time::Instant::from_std(context.policy().deadline))?;
                        progress.set_message("build and verify source wheel".into());
                        let built = session.build(pair(final_inputs, final_archives)?).await?;
                        final_evidence
                            .recheck(tokio::time::Instant::from_std(context.policy().deadline))?;
                        initial_root.recheck()?;
                        final_root.recheck()?;
                        context.checkpoint()?;
                        language.check_binding()?;
                        Ok::<_, eyre::Report>(built)
                    }
                    .await;
                    budget.active.remove(&source_id);
                    budget.workspace_reserved -= workspace_limit;
                    if outcome.is_err() {
                        progress.finish_with_icon(
                            "source build failed".into(),
                            crate::ui::progress_report::ProgressIcon::Error,
                        );
                    }
                    let built = outcome.with_context(|| {
                        format!("building source package {source_name}=={source_version}")
                    })?;
                    budget
                        .records
                        .insert(continuation.pin().id(), built.record().clone());
                    ensure!(
                        built.language_key() == language.key().fingerprint(),
                        "source wheel was built with a different shared Python instance"
                    );
                    let output_bytes = built.record().output_size;
                    let owned_bytes = source_bytes
                        .checked_add(output_bytes)
                        .context("sealed source-build cache size overflow")?;
                    budget.cached_bytes = budget
                        .cached_bytes
                        .checked_add(owned_bytes)
                        .filter(|n| *n <= 2_u64 * 1024 * 1024 * 1024)
                        .context("retained sealed source-build cache budget exceeded")?;
                    budget.charge_copy(owned_bytes)?;
                    let (retained, replay) = built.duplicate_for_attempt(
                        &scratch,
                        context.policy().deadline,
                        source_bytes,
                        output_bytes,
                    )?;
                    budget.completed.insert(cache_key, (retained, source_bytes));
                    progress.set_message("resume dependency resolution".into());
                    let resumed = continuation.resume_with_built(replay).await;
                    if resumed.is_ok() {
                        progress.finish_with_message("source wheel ready".into());
                    } else {
                        progress.finish_with_icon(
                            "source output rejected".into(),
                            crate::ui::progress_report::ProgressIcon::Error,
                        );
                    }
                    state = resumed?;
                }
                RegistryPreparation::NeedsSourceAdapter(_) => {
                    eyre::bail!(
                        "this source-build request requires an unsupported non-registry source adapter"
                    );
                }
                RegistryPreparation::NeedsConstraintPolicy(_) => {
                    eyre::bail!(
                        "this source-build request requires an unsupported constraint policy"
                    );
                }
            }
        }
    })
}

fn build_inputs(graph: &ClosedRegistryGraph<'_>) -> Result<Vec<SourceBuildInput>> {
    graph
        .plan()
        .selected
        .iter()
        .map(|node| {
            let owner = graph
                .owner(node.key)
                .context("closed build dependency owner is missing")?;
            let pin = owner.pin();
            Ok(SourceBuildInput {
                name: node.name.clone(),
                version: node.version.clone(),
                source_pin: hex::encode(pin.id().as_bytes()),
                registry: pin.file().registry_id().into(),
                base: *pin.file().canonical_base_fingerprint(),
                source_filename: pin.file().filename().into(),
                source_digest: pin.digest().to_string(),
                source_size: pin.size(),
                source_is_sdist: pin.file().kind() == RegistryArtifactKind::Sdist,
                advertised: matches!(pin.evidence(), PinEvidence::Advertised),
                output_filename: owner.output_filename().into(),
                output_digest: owner.output_digest().to_string(),
                output_size: owner.output_size(),
            })
        })
        .collect()
}

fn pair(
    inputs: Vec<SourceBuildInput>,
    archives: Vec<InspectedWheelArchive>,
) -> Result<Vec<(SourceBuildInput, InspectedWheelArchive)>> {
    ensure!(
        inputs.len() == archives.len(),
        "build dependency archive count differs from its closed plan"
    );
    let mut remaining = inputs;
    let mut result = Vec::with_capacity(archives.len());
    for archive in archives {
        let position = remaining
            .iter()
            .position(|input| {
                input.name == archive.manifest().name()
                    && input.version == archive.manifest().version()
            })
            .context("build dependency archive has no original exact input")?;
        let input = remaining.swap_remove(position);
        ensure!(
            input.output_filename == archive.filename()
                && input.output_digest == archive.digest().to_string()
                && input.output_size == archive.size(),
            "build dependency archive differs from its actual output facts"
        );
        result.push((input, archive));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
