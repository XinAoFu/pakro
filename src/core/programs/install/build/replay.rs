//! Strict original-pin source replay. This module never invokes a solver.
use super::*;
use crate::artifact_snapshot::VerifiedArtifact;
use crate::packages::python_resolution::source::locked_graph::{
    LockedBuildInputEvidence, LockedBuildInputPreparation, LockedGraphContext,
    LockedGraphPreparation, LockedRegistryEndpoint, LockedRegistryReplay, acquire_build_input,
};

// Like fresh preparation, replay owns a heap frame at its reentrant boundary;
// its largest continuation must not multiply through outer install futures.
pub(crate) fn replay_registry<'root, 'config, 'call>(
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
        let endpoints = registry
            .endpoints
            .iter()
            .map(|endpoint| LockedRegistryEndpoint {
                project: pakro_index::simple::PythonSourceSession {
                    registry: endpoint.project.registry,
                    auth_scope: endpoint.project.auth_scope,
                    credentials: endpoint.project.credentials,
                    auth_origins: endpoint.project.auth_origins,
                    transport: endpoint.project.transport,
                },
                file: pakro_index::simple::SourceFileSession {
                    credential_ref: endpoint.file.credential_ref,
                    auth_scope: endpoint.file.auth_scope,
                    credentials: endpoint.file.credentials,
                    auth_origins: endpoint.file.auth_origins,
                    transport: endpoint.file.transport,
                },
            })
            .collect::<Vec<_>>();
        let key = super::super::lock::group_key(language)?;
        let graph = root
            .model()
            .and_then(|model| model.graph(&key))
            .context("strict replay is missing its complete graph")?;
        for node in graph.nodes() {
            let pin = root
                .existing_pin(node.source_pin())?
                .context("strict replay is missing its original pin")?;
            let output = root
                .source_build(node.source_pin())
                .map_or(0, |record| record.output_size);
            charge_bytes(
                budget,
                pin.pin()
                    .size()
                    .checked_add(output)
                    .context("strict graph byte size overflow")?,
            )?;
        }
        let raw = declarations.iter().map(String::as_str).collect::<Vec<_>>();
        let mut state = LockedRegistryReplay::new(
            root,
            &key,
            &raw,
            language.target(),
            LockedGraphContext {
                client: registry.client,
                endpoints: &endpoints,
                source_policy: registry.source_policy,
                fetch: registry.fetch,
                limits: Default::default(),
                cache_root: registry.cache_root,
                scratch_root: registry.scratch_root,
                deadline: context.policy().deadline.min(registry.deadline),
            },
        )?
        .prepare()
        .boxed()
        .await?;
        loop {
            context.checkpoint()?;
            match state {
                LockedGraphPreparation::Ready(prepared) => {
                    let prepared = prepared.into_registry_plan()?;
                    charge_text(budget, prepared.usage().retained_utf8)?;
                    budget.metadata = budget
                        .metadata
                        .checked_add(prepared.usage().metadata)
                        .filter(|n| *n <= 8192)
                        .context("cumulative strict metadata count exceeded")?;
                    return Ok(prepared);
                }
                LockedGraphPreparation::NeedsBuild(mut continuation) => {
                    let id = continuation.pin().id();
                    let expected = continuation.expected_record()?.clone();
                    if let Some(package) =
                        super::reusable_published_package(context, root, language, id, &expected)?
                    {
                        budget.records.insert(id, expected);
                        state = continuation
                            .resume_with_published(package, language.clone())
                            .boxed()
                            .await?;
                        continue;
                    }
                    let source = continuation.source_verified_copy(registry.scratch_root)?;
                    let built = rebuild(
                        context,
                        language,
                        root,
                        id,
                        source,
                        expected,
                        registry.clone(),
                        budget,
                        depth,
                    )
                    .await?;
                    state = continuation.resume_with_built(built).boxed().await?;
                }
                LockedGraphPreparation::NeedsSourceAdapter(_) => {
                    eyre::bail!("strict source replay requires an unsupported source adapter")
                }
            }
        }
    })
}

pub(super) fn rebuild<'root, 'config, 'call>(
    context: &'call OperationContext,
    language: &'call LanguageLease,
    root: &'root BoundPythonLock,
    source_id: PinId,
    source: VerifiedArtifact,
    expected: SourceBuildRecord,
    registry: RegistryDepotContext<'config>,
    budget: &'call mut BuildBudget,
    depth: usize,
) -> BoxFuture<'call, Result<CompletedSourceBuild>>
where
    'root: 'call,
    'config: 'call,
{
    Box::pin(async move {
        context.checkpoint()?;
        language.check_held_binding()?;
        expected.validate()?;
        let pin = root
            .existing_pin(source_id)?
            .context("strict source pin is missing")?;
        let source_bytes = source.size();
        ensure!(
            source.filename() == pin.pin().file().filename()
                && source.digest() == pin.pin().digest()
                && source_bytes == pin.pin().size(),
            "strict source copy differs from its actual original lock pin"
        );
        let deadline = context.policy().deadline.min(registry.deadline);
        let cache_key = (
            *source_id.as_bytes(),
            language.key().fingerprint(),
            *language.target().fingerprint(),
        );
        budget.charge_copy(source_bytes)?;
        if let Some((completed, cached_source_bytes)) = budget.completed.remove(&cache_key) {
            ensure!(
                cached_source_bytes == source_bytes
                    && completed.language_key() == language.key().fingerprint()
                    && completed.target_fingerprint() == *language.target().fingerprint()
                    && completed.record().source_digest == source.digest().to_string()
                    && completed.record().fingerprint()? == expected.fingerprint()?,
                "cached strict source build differs from its locked inputs or output"
            );
            let output_bytes = expected.output_size;
            budget.charge_copy(
                source_bytes
                    .checked_add(output_bytes)
                    .context("strict cached build copy size overflow")?,
            )?;
            let (retained, replay) = completed.duplicate_for_attempt(
                registry.scratch_root,
                deadline,
                source_bytes,
                output_bytes,
            )?;
            root.recheck()?;
            language.check_binding()?;
            context.checkpoint()?;
            budget.records.insert(source_id, replay.record().clone());
            budget.completed.insert(cache_key, (retained, source_bytes));
            return Ok(replay);
        }
        let output_maximum = registry
            .limits
            .wheel
            .max_archive_bytes
            .min(SourceBuildLimits::default().max_generated_wheel_bytes)
            .min(
                (4_u64 * 1024 * 1024 * 1024)
                    .checked_sub(budget.copied_bytes)
                    .context("cached source output copy budget exceeded")?,
            );
        ensure!(
            expected.output_size <= output_maximum,
            "cached source output exceeds the remaining byte budget"
        );
        let cached = crate::artifact_snapshot::cached_verified_artifact(
            &expected.output_filename,
            expected.output_digest.parse()?,
            registry.cache_root,
            output_maximum,
        )?;
        if let Some(output) = cached {
            ensure!(
                output.size() == expected.output_size,
                "cached source output size differs from its original receipt"
            );
            budget.charge_copy(expected.output_size)?;
            let output = crate::packages::python_wheel_source::inspect_owned_wheel(
                output,
                expected.output_digest.parse()?,
                &crate::packages::python_wheel::WheelExpectation {
                    filename: &expected.output_filename,
                    name: pin.pin().file().project(),
                    version: pin.pin().file().version(),
                    supported: language.target().supported(),
                },
                registry.limits.wheel,
            )?;
            let workspace_limit = SourceBuildLimits::default().max_workspace_bytes.min(
                (4_u64 * 1024 * 1024 * 1024)
                    .checked_sub(budget.workspace_reserved)
                    .context("cached source verification workspace budget exceeded")?,
            );
            ensure!(
                workspace_limit > 0,
                "cached source verification workspace budget exceeded"
            );
            budget.workspace_reserved += workspace_limit;
            let reused = async {
                let mut session = SourceBuildSession::prepare(
                    source,
                    pin.pin().file().project(),
                    pin.pin().file().version(),
                    language.clone(),
                    registry.scratch_root,
                    deadline,
                    SourceBuildLimits {
                        max_workspace_bytes: workspace_limit,
                        max_decoded_bytes: (2_u64 * 1024 * 1024 * 1024)
                            .checked_sub(budget.decoded)
                            .context("cached source expansion budget exceeded")?
                            .min(SourceBuildLimits::default().max_decoded_bytes),
                        max_entries: 131_072usize
                            .checked_sub(budget.extracted_entries)
                            .context("cached source entry budget exceeded")?,
                        max_total_path_bytes: (64usize * 1024 * 1024)
                            .checked_sub(budget.extracted_path_bytes)
                            .context("cached source path budget exceeded")?,
                        ..Default::default()
                    },
                )?;
                session.configure_native(context, Some(&expected)).boxed().await?;
                budget.decoded = budget
                    .decoded
                    .checked_add(session.decoded_bytes())
                    .context("cached source expansion overflow")?;
                budget.extracted_entries = budget
                    .extracted_entries
                    .checked_add(session.extracted_entries())
                    .context("cached source entry overflow")?;
                budget.extracted_path_bytes = budget
                    .extracted_path_bytes
                    .checked_add(session.extracted_path_bytes())
                    .context("cached source path overflow")?;
                let reused = session.reuse_output(&pin, output)?;
                ensure!(
                    reused.record() == &expected,
                    "cached source output differs from the selected original build record"
                );
                root.recheck()?;
                context.checkpoint()?;
                Ok::<_, eyre::Report>(reused)
            }.await;
            budget.workspace_reserved -= workspace_limit;
            let reused = reused?;
            budget.records.insert(source_id, reused.record().clone());
            return Ok(reused);
        }
        let workspace_limit = SourceBuildLimits::default().max_workspace_bytes.min(
            (4_u64 * 1024 * 1024 * 1024)
                .checked_sub(budget.workspace_reserved)
                .context("active strict source workspace budget exceeded")?,
        );
        ensure!(
            workspace_limit > 0,
            "active strict source workspace budget exceeded"
        );
        budget.enter(*source_id.as_bytes(), depth)?;
        budget.workspace_reserved += workspace_limit;
        let outcome = async {
            charge_text(budget, serde_json::to_vec(&expected)?.len())?;
            let observation = expected
                .observation
                .as_ref()
                .context("strict source replay is missing its initial build environment")?;
            let pin = root
                .existing_pin(source_id)?
                .context("strict source pin is missing")?;
            let scratch = registry.scratch_root;
            let mut session = SourceBuildSession::prepare(
                source,
                pin.pin().file().project(),
                pin.pin().file().version(),
                language.clone(),
                scratch,
                deadline,
                SourceBuildLimits {
                    max_workspace_bytes: workspace_limit,
                    max_decoded_bytes: (2_u64 * 1024 * 1024 * 1024)
                        .checked_sub(budget.decoded)
                        .context("cumulative strict source expansion budget exceeded")?
                        .min(SourceBuildLimits::default().max_decoded_bytes),
                    max_entries: 131_072usize
                        .checked_sub(budget.extracted_entries)
                        .context("cumulative strict source entries exceeded")?,
                    max_total_path_bytes: (64usize * 1024 * 1024)
                        .checked_sub(budget.extracted_path_bytes)
                        .context("cumulative strict source paths exceeded")?,
                    ..Default::default()
                },
            )?;
            session.configure_native(context, Some(&expected)).boxed().await?;
            budget.decoded = budget
                .decoded
                .checked_add(session.decoded_bytes())
                .context("strict source expansion overflow")?;
            budget.extracted_entries = budget
                .extracted_entries
                .checked_add(session.extracted_entries())
                .context("strict source entry overflow")?;
            budget.extracted_path_bytes = budget
                .extracted_path_bytes
                .checked_add(session.extracted_path_bytes())
                .context("strict source path overflow")?;
            session.check_locked_record(&pin)?;
            ensure!(
                session.initial_requirements() == observation.initial_requirements,
                "strict source initial requirements changed"
            );
            let (initial, initial_evidence) = inputs(
                context,
                language,
                root,
                source_id,
                &observation.initial_inputs,
                registry.clone(),
                budget,
                depth + 1,
            )
            .await?;
            check_environment(language, &observation.initial_requirements, &initial)?;
            let dynamic = session.get_requires(initial).boxed().await?;
            recheck_inputs(root, &initial_evidence)?;
            ensure!(
                dynamic == observation.dynamic_requirements,
                "strict source dynamic requirements changed"
            );
            let built = if dynamic.is_empty() {
                session.build_with_initial_environment().boxed().await?
            } else {
                let mut requirements = observation.initial_requirements.clone();
                requirements.extend(dynamic);
                requirements.extend(
                    observation
                        .initial_inputs
                        .iter()
                        .map(|input| format!("{}=={}", input.name, input.version)),
                );
                requirements.sort();
                requirements.dedup();
                let (final_inputs, final_evidence) = inputs(
                    context,
                    language,
                    root,
                    source_id,
                    &expected.inputs,
                    registry.clone(),
                    budget,
                    depth + 1,
                )
                .await?;
                check_environment(language, &requirements, &final_inputs)?;
                let built = session.build(final_inputs).boxed().await?;
                recheck_inputs(root, &final_evidence)?;
                built
            };
            ensure!(
                built.record().fingerprint()? == expected.fingerprint()?,
                "strict source rebuilt output differs from the locked artifact"
            );
            recheck_inputs(root, &initial_evidence)?;
            root.recheck()?;
            context.checkpoint()?;
            budget.records.insert(source_id, built.record().clone());
            Ok(built)
        }
        .await;
        budget.active.remove(source_id.as_bytes());
        budget.workspace_reserved -= workspace_limit;
        let built = outcome?;
        let output_bytes = built.record().output_size;
        budget.charge_copy(output_bytes)?;
        let built =
            built.publish_output(registry.cache_root, registry.limits.wheel.max_archive_bytes)?;
        let retained_bytes = source_bytes
            .checked_add(output_bytes)
            .context("strict retained source build size overflow")?;
        budget.cached_bytes = budget
            .cached_bytes
            .checked_add(retained_bytes)
            .filter(|n| *n <= 2_u64 * 1024 * 1024 * 1024)
            .context("retained strict source build cache budget exceeded")?;
        budget.charge_copy(retained_bytes)?;
        let (retained, replay) = built.duplicate_for_attempt(
            registry.scratch_root,
            deadline,
            source_bytes,
            output_bytes,
        )?;
        budget.completed.insert(cache_key, (retained, source_bytes));
        Ok(replay)
    })
}

fn inputs<'root, 'config, 'call>(
    context: &'call OperationContext,
    language: &'call LanguageLease,
    root: &'root BoundPythonLock,
    parent: PinId,
    expected: &'call [SourceBuildInput],
    registry: RegistryDepotContext<'config>,
    budget: &'call mut BuildBudget,
    depth: usize,
) -> BoxFuture<
    'call,
    Result<(
        Vec<(SourceBuildInput, InspectedWheelArchive)>,
        Vec<LockedBuildInputEvidence<'root>>,
    )>,
>
where
    'root: 'call,
    'config: 'call,
{
    Box::pin(async move {
        let mut archives = Vec::new();
        let mut evidence = Vec::new();
        for input in expected {
            context.checkpoint()?;
            charge_bytes(
                budget,
                input
                    .source_size
                    .checked_add(input.output_size)
                    .context("strict build input byte size overflow")?,
            )?;
            budget.charge_copy(input.source_size)?;
            let ready = match acquire_build_input(root, parent, &input.name, registry.clone())
                .boxed()
                .await?
            {
                LockedBuildInputPreparation::Ready(ready) => ready,
                LockedBuildInputPreparation::NeedsBuild(mut continuation) => {
                    let id = continuation.pin().id();
                    let record = continuation.expected_record()?.clone();
                    let source = continuation.source_verified_copy(registry.scratch_root)?;
                    let built = rebuild(
                        context,
                        language,
                        root,
                        id,
                        source,
                        record,
                        registry.clone(),
                        budget,
                        depth,
                    )
                    .await?;
                    continuation.resume_with_built(built)?
                }
            };
            let (actual, archive, proof) = ready.into_parts();
            ensure!(
                actual == *input,
                "strict initial build input differs from the final locked input"
            );
            proof.recheck(root)?;
            budget.metadata = budget
                .metadata
                .checked_add(1)
                .filter(|n| *n <= 8192)
                .context("cumulative strict build metadata count exceeded")?;
            let metadata = archive.manifest().core_metadata();
            charge_text(
                budget,
                metadata
                    .raw()
                    .len()
                    .checked_mul(3)
                    .and_then(|n| n.checked_add(metadata.description().len()))
                    .context("strict build metadata text overflow")?,
            )?;
            for member in archive.manifest().members() {
                charge_text(
                    budget,
                    member
                        .archive_path()
                        .len()
                        .checked_add(member.relative_path().len())
                        .context("strict build archive path text overflow")?,
                )?;
            }
            archives.push((actual, archive));
            evidence.push(proof);
        }
        Ok((archives, evidence))
    })
}

fn recheck_inputs(root: &BoundPythonLock, evidence: &[LockedBuildInputEvidence<'_>]) -> Result<()> {
    for proof in evidence {
        proof.recheck(root)?;
    }
    Ok(())
}

fn charge_bytes(budget: &mut BuildBudget, bytes: u64) -> Result<()> {
    budget.bytes = budget
        .bytes
        .checked_add(bytes)
        .filter(|n| *n <= 4_u64 * 1024 * 1024 * 1024)
        .context("cumulative strict artifact byte budget exceeded")?;
    Ok(())
}
fn charge_text(budget: &mut BuildBudget, bytes: usize) -> Result<()> {
    budget.utf8 = budget
        .utf8
        .checked_add(bytes)
        .filter(|n| *n <= 64 * 1024 * 1024)
        .context("cumulative strict retained text budget exceeded")?;
    Ok(())
}

fn check_environment(
    language: &LanguageLease,
    requirements: &[String],
    archives: &[(SourceBuildInput, InspectedWheelArchive)],
) -> Result<()> {
    use crate::packages::python_resolution::graph_check::{
        GraphCandidateObservation, derive_graph,
    };
    use pakro_resolver::python_native::CandidateKey;
    let roots = requirements
        .iter()
        .map(|raw| {
            pakro_index::python_requirements::parse_python_requirement(raw, Default::default())
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let facts = archives
        .iter()
        .map(|(input, archive)| {
            crate::packages::python_resolution::source::wheel_metadata::project_manifest_metadata(
                archive.manifest(),
                &input.name,
                &input.version,
                Default::default(),
            )
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let observations = archives
        .iter()
        .zip(&facts)
        .map(|((input, _), facts)| {
            Ok(GraphCandidateObservation {
                key: CandidateKey(*input.content_pin()?.id().as_bytes()),
                name: &input.name,
                version: &input.version,
                facts,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    derive_graph(
        &roots,
        &observations,
        language.target().environment(),
        pakro_index::python_markers::PythonMarkerScope::Environment,
        language.target().semantics(),
        Default::default(),
        Default::default(),
    )?;
    Ok(())
}
