//! Deterministic old-pin acquisitions and original-AST closure; no solving.
use super::super::replay::metadata::{LockedMetadataState, route_locked_metadata};
use super::super::replay::{LockedReplayContext, acquire_locked_registry_file};
use super::*;
use crate::packages::python_resolution::graph_check::{
    GraphCandidateObservation, derive_graph_scoped,
};
use crate::packages::python_resolution::lock_session::ExistingFileSlot;

fn clock(deadline: Instant) -> Result<(), LockedGraphError> {
    if Instant::now() >= deadline {
        Err(LockedGraphError::Deadline)
    } else {
        Ok(())
    }
}
fn actual_slot(
    root: &BoundPythonLock,
    id: PinId,
) -> Result<ExistingFileSlot<'_>, LockedGraphError> {
    match root.existing_pin(id) {
        Ok(Some(slot)) => Ok(slot),
        Ok(None) => Err(LockedGraphError::GraphMismatch),
        Err(_) => {
            root.recheck().map_err(|_| LockedGraphError::Revoked)?;
            Err(LockedGraphError::GraphMismatch)
        }
    }
}
fn charge(used: &mut usize, bytes: usize, maximum: usize) -> Result<(), LockedGraphError> {
    *used = used
        .checked_add(bytes)
        .filter(|total| *total <= maximum)
        .ok_or(LockedGraphError::Limit)?;
    Ok(())
}
fn retain_metadata(
    used: &mut usize,
    owner: &LockedWheelCandidate<'_>,
    maximum: usize,
) -> Result<(), LockedGraphError> {
    // Bound the directly retained archive metadata and projected text before
    // retaining this owner in the whole-attempt map. Parser-internal/ZIP memory
    // has its own explicit limits; this is not an exact heap/RSS assertion.
    charge(used, owner.manifest().core_metadata().raw().len(), maximum)?;
    let facts = owner.facts();
    for value in [
        facts.name().as_str(),
        facts.version(),
        facts.requires_python().unwrap_or(""),
    ] {
        charge(used, value.len(), maximum)?;
    }
    for value in facts.provided_extras().iter().chain(facts.dynamic_fields()) {
        charge(used, value.len(), maximum)?;
    }
    for requirement in facts.requirements() {
        for value in [
            requirement.name(),
            requirement.raw_for_semantic_resolution(),
        ] {
            charge(used, value.len(), maximum)?;
        }
        for extra in requirement.requested_extras() {
            charge(used, extra.len(), maximum)?;
        }
        if let Some(reference) = requirement.unresolved_reference() {
            charge(used, reference.raw_for_source_binding().len(), maximum)?;
        }
        if let Some(marker) = requirement.exact_marker() {
            charge(used, marker.raw_for_semantic_resolution().len(), maximum)?;
        }
    }
    Ok(())
}
fn current(replay: &LockedRegistryReplay<'_, '_>) -> Result<(), LockedGraphError> {
    let deadline = replay.context.deadline;
    clock(deadline)?;
    replay
        .root
        .recheck()
        .map_err(|_| LockedGraphError::Revoked)?;
    for owner in replay.owners.values() {
        clock(deadline)?;
        owner
            .recheck_for(replay.root)
            .map_err(|_| LockedGraphError::Revoked)?;
    }
    for source in replay.builds.values() {
        clock(deadline)?;
        source
            .recheck_for(replay.root)
            .map_err(|_| LockedGraphError::Revoked)?;
    }
    clock(deadline)
}
pub(super) async fn run<'lock, 'config>(
    replay: LockedRegistryReplay<'lock, 'config>,
    finish: impl FnOnce(),
) -> Result<LockedGraphPreparation<'lock, 'config>, LockedGraphError> {
    let deadline = replay.context.deadline;
    clock(deadline)?;
    let result = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        work(replay, finish),
    )
    .await
    .map_err(|_| LockedGraphError::Deadline)?;
    // Tokio polls synchronous work before its timeout. A real owner is never
    // returned merely because an expired inner future became Ready first.
    clock(deadline)?;
    result
}
async fn work<'lock, 'config>(
    mut replay: LockedRegistryReplay<'lock, 'config>,
    finish: impl FnOnce(),
) -> Result<LockedGraphPreparation<'lock, 'config>, LockedGraphError> {
    current(&replay)?;
    let limits = replay.context.limits;
    let mut missing = Vec::new();
    for index in 0..replay.nodes.len() {
        clock(replay.context.deadline)?;
        let binding = &replay.nodes[index];
        let key = binding.key;
        let root = replay.root;
        let pin = binding.pin;
        let endpoint = &replay.context.endpoints[binding.endpoint];
        let slot = actual_slot(root, pin.id())?;
        let artifact = acquire_locked_registry_file(
            slot,
            LockedReplayContext {
                client: replay.context.client,
                project_session: PythonSourceSession {
                    registry: endpoint.project.registry,
                    auth_scope: endpoint.project.auth_scope,
                    credentials: endpoint.project.credentials,
                    auth_origins: endpoint.project.auth_origins,
                    transport: endpoint.project.transport,
                },
                file_session: SourceFileSession {
                    credential_ref: endpoint.file.credential_ref,
                    auth_scope: endpoint.file.auth_scope,
                    credentials: endpoint.file.credentials,
                    auth_origins: endpoint.file.auth_origins,
                    transport: endpoint.file.transport,
                },
                policy: replay.context.fetch,
                project_limits: limits.project,
                source_limits: limits.source,
                lock_limits: limits.lock,
                cache_root: replay.context.cache_root,
                scratch_root: replay.context.scratch_root,
                deadline: replay.context.deadline,
            },
        )
        .await;
        let artifact = match artifact {
            Ok(value) => value,
            Err(LockedReplayError::MissingInputs)
                if replay.context.fetch.network == NetworkPolicy::Offline =>
            {
                missing
                    .try_reserve(1)
                    .map_err(|_| LockedGraphError::Limit)?;
                missing.push(pin.id());
                continue;
            }
            Err(error) => return Err(LockedGraphError::Replay(error)),
        };
        clock(replay.context.deadline)?;
        let metadata = route_locked_metadata(
            artifact,
            replay.target.supported(),
            limits.wheel,
            limits.metadata,
        )
        .map_err(LockedGraphError::Metadata)?;
        match metadata {
            LockedMetadataState::Wheel(owner) => {
                if owner.pin().id() != pin.id() {
                    return Err(LockedGraphError::GraphMismatch);
                }
                retain_metadata(
                    &mut replay.retained_utf8,
                    &owner,
                    limits.max_retained_utf8_bytes,
                )?;
                if replay.owners.insert(key, owner).is_some() {
                    return Err(LockedGraphError::GraphMismatch);
                }
            }
            LockedMetadataState::NeedsBuild(source) => {
                if source.pin().id() != pin.id() || replay.builds.insert(key, source).is_some() {
                    return Err(LockedGraphError::GraphMismatch);
                }
            }
        }
    }
    current(&replay)?;
    if !missing.is_empty() {
        return Err(LockedGraphError::MissingInputs(missing));
    }
    if let Some(key) = replay
        .nodes
        .iter()
        .map(|node| node.key)
        .find(|key| replay.builds.contains_key(key))
    {
        let source = replay
            .builds
            .remove(&key)
            .ok_or(LockedGraphError::GraphMismatch)?;
        source
            .recheck_for(replay.root)
            .map_err(|_| LockedGraphError::Revoked)?;
        clock(replay.context.deadline)?;
        return Ok(LockedGraphPreparation::NeedsBuild(
            LockedGraphBuildContinuation { replay, source },
        ));
    }
    if replay.owners.len() != replay.nodes.len() {
        return Err(LockedGraphError::GraphMismatch);
    }
    let mut observations = Vec::new();
    observations
        .try_reserve_exact(replay.nodes.len())
        .map_err(|_| LockedGraphError::Limit)?;
    for binding in &replay.nodes {
        let owner = replay
            .owners
            .get(&binding.key)
            .ok_or(LockedGraphError::GraphMismatch)?;
        observations.push(GraphCandidateObservation {
            key: binding.key,
            name: binding.node.name(),
            version: binding.node.version(),
            facts: owner.facts(),
        });
    }
    let mut graph_limits = limits.graph;
    graph_limits.max_evaluation_nodes = graph_limits
        .max_evaluation_nodes
        .checked_sub(replay.root_work)
        .filter(|remaining| *remaining > 0)
        .ok_or(LockedGraphError::Limit)?;
    let derived = with_root_scopes(replay.root_scopes.as_deref(), &replay.target, |scopes| {
        derive_graph_scoped(
            &replay.roots,
            &observations,
            replay.target.environment(),
            scopes,
            replay.target.semantics(),
            limits.target.markers,
            graph_limits,
        )
    });
    drop(observations);
    let derived = match derived {
        Ok(value) => value,
        Err(GraphCheckError::SourceReference(location)) => {
            current(&replay)?;
            return Ok(LockedGraphPreparation::NeedsSourceAdapter(
                LockedGraphSourceContinuation { replay, location },
            ));
        }
        Err(error) => return Err(LockedGraphError::Graph(error)),
    };
    compare(&replay, &derived)?;
    finish();
    current(&replay)?;
    let LockedRegistryReplay {
        root,
        graph,
        roots,
        root_scopes: replay_root_scopes,
        target,
        context,
        owners,
        bindings,
        ..
    } = replay;
    Ok(LockedGraphPreparation::Ready(LockedPreparedPlan {
        root,
        graph,
        roots,
        root_scopes: replay_root_scopes,
        target,
        owners,
        bindings,
        derived,
        deadline: context.deadline,
    }))
}
fn compare(
    replay: &LockedRegistryReplay<'_, '_>,
    derived: &DerivedGraphData,
) -> Result<(), LockedGraphError> {
    let name = |key: &CandidateKey| {
        replay
            .owners
            .get(key)
            .map(|owner| owner.facts().name().as_str())
            .ok_or(LockedGraphError::GraphMismatch)
    };
    if derived.graph.roots.len() != replay.graph.roots().len() {
        return Err(LockedGraphError::GraphMismatch);
    }
    for (actual, old) in derived.graph.roots.iter().zip(replay.graph.roots()) {
        if actual.declaration != old.declaration_index() || name(&actual.to)? != old.node() {
            return Err(LockedGraphError::GraphMismatch);
        }
    }
    let mut actual_edges = BTreeMap::new();
    for edge in &derived.graph.edges {
        let declaration = replay
            .owners
            .get(&edge.from)
            .and_then(|owner| owner.facts().requirements().get(edge.declaration))
            .ok_or(LockedGraphError::GraphMismatch)?
            .raw_for_semantic_resolution();
        let count = actual_edges
            .entry((name(&edge.from)?, name(&edge.to)?, declaration))
            .or_insert(0usize);
        *count = count.checked_add(1).ok_or(LockedGraphError::Limit)?;
    }
    let mut old_edges = BTreeMap::new();
    for edge in replay.graph.edges() {
        let count = old_edges
            .entry((edge.from(), edge.to(), edge.declaration()))
            .or_insert(0usize);
        *count = count.checked_add(1).ok_or(LockedGraphError::Limit)?;
    }
    if actual_edges != old_edges {
        return Err(LockedGraphError::GraphMismatch);
    }
    Ok(())
}
