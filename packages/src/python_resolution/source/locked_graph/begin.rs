//! Complete bounded preflight over the actual original graph, before sources.
use super::*;
use crate::packages::python_resolution::lock_model::{
    LockMarkerSemantics, RegistryArtifactKind, canonical_registry_base_fingerprint,
};
use pakro_index::python_markers::evaluate_python_marker;
use pakro_index::python_requirements::{PythonRequirementError, parse_python_requirement};
use pakro_index::python_source::ordered_python_source_indices;
use pakro_resolver::pyver::{
    RawVersionConstraint, RawVersionConstraintError, RawVersionConstraintLimits,
};

fn validate(limits: LockedGraphLimits) -> Result<(), LockedGraphError> {
    if [
        limits.max_nodes,
        limits.max_registries,
        limits.max_retained_utf8_bytes,
        limits.requirements.max_input_bytes,
        limits.requirements.max_requested_extras,
        limits.requirements.max_syntax_tokens,
        limits.requirements.max_marker_comparisons,
    ]
    .contains(&0)
        || limits.max_retained_archive_bytes == 0
    {
        return Err(LockedGraphError::InvalidLimits);
    }
    limits.target.validate()?;
    limits
        .requirements
        .marker
        .validate()
        .map_err(|_| LockedGraphError::InvalidLimits)?;
    limits
        .lock
        .validate()
        .map_err(|_| LockedGraphError::InvalidLimits)?;
    limits
        .project
        .validate()
        .map_err(|_| LockedGraphError::InvalidLimits)?;
    limits
        .source
        .validate()
        .map_err(|_| LockedGraphError::InvalidLimits)?;
    limits
        .graph
        .validate()
        .map_err(|_| LockedGraphError::InvalidLimits)?;
    super::super::wheel_metadata::validate_limits(limits.wheel, limits.metadata)
        .map_err(|_| LockedGraphError::InvalidLimits)
}
fn clock(deadline: Instant) -> Result<(), LockedGraphError> {
    if Instant::now() >= deadline {
        Err(LockedGraphError::Deadline)
    } else {
        Ok(())
    }
}
fn charge(used: &mut usize, count: usize, maximum: usize) -> Result<(), LockedGraphError> {
    *used = used
        .checked_add(count)
        .filter(|total| *total <= maximum)
        .ok_or(LockedGraphError::Limit)?;
    Ok(())
}
fn label(index: usize, pin: PinId) -> Result<CandidateKey, LockedGraphError> {
    let index = u64::try_from(index).map_err(|_| LockedGraphError::Limit)?;
    let mut hash = sha2::Sha256::default();
    sha2::Digest::update(&mut hash, b"pakro.python-locked-node/v1\0");
    sha2::Digest::update(&mut hash, index.to_be_bytes());
    sha2::Digest::update(&mut hash, pin.as_bytes());
    Ok(CandidateKey(sha2::Digest::finalize(hash).into()))
}
pub(super) fn new<'lock, 'config>(
    root: &'lock BoundPythonLock,
    key: &GroupKey,
    raw_roots: &[&str],
    target: &FrozenRegistryTarget,
    context: LockedGraphContext<'config>,
) -> Result<LockedRegistryReplay<'lock, 'config>, LockedGraphError> {
    new_inner(root, key, raw_roots, None, target, context)
}
pub(super) fn new_scoped<'lock, 'config>(
    root: &'lock BoundPythonLock,
    key: &GroupKey,
    raw_roots: &[&str],
    root_scopes: &[OwnedRootMarkerScope],
    target: &FrozenRegistryTarget,
    context: LockedGraphContext<'config>,
) -> Result<LockedRegistryReplay<'lock, 'config>, LockedGraphError> {
    new_inner(root, key, raw_roots, Some(root_scopes), target, context)
}
fn new_inner<'lock, 'config>(
    root: &'lock BoundPythonLock,
    key: &GroupKey,
    raw_roots: &[&str],
    root_scopes: Option<&[OwnedRootMarkerScope]>,
    target: &FrozenRegistryTarget,
    context: LockedGraphContext<'config>,
) -> Result<LockedRegistryReplay<'lock, 'config>, LockedGraphError> {
    let limits = context.limits;
    validate(limits)?;
    clock(context.deadline)?;
    if root_scopes.is_some_and(|scopes| scopes.len() != raw_roots.len()) {
        return Err(LockedGraphError::RootMismatch);
    }
    // This validates the existing immutable data and the actual original wire
    // independently; no encoding is used to guess the original byte length.
    if root.check_limits(limits.lock).is_err() {
        root.recheck().map_err(|_| LockedGraphError::Revoked)?;
        return Err(LockedGraphError::Limit);
    }
    target.check_limits(limits.target)?;
    with_root_scopes(root_scopes, target, |scopes| {
        scopes.check_limits(raw_roots.len(), limits.graph)
    })
    .map_err(LockedGraphError::Graph)?;
    if key.target() != target.fingerprint() {
        return Err(LockedGraphError::TargetMismatch);
    }
    let model = root.model().ok_or(LockedGraphError::MissingGroup)?;
    let graph = model.graph(key).ok_or(LockedGraphError::MissingGroup)?;
    if !matches!(
        (graph.semantics(), target.semantics()),
        (
            LockMarkerSemantics::Current2026,
            pakro_index::python_markers::MarkerSemantics::Current2026
        )
    ) || graph.runtime().target() != target.fingerprint()
    {
        return Err(LockedGraphError::TargetMismatch);
    }
    if graph.root_declarations().len() != raw_roots.len()
        || graph
            .root_declarations()
            .iter()
            .zip(raw_roots)
            .any(|(old, current)| old != current)
    {
        return Err(LockedGraphError::RootMismatch);
    }
    if graph.nodes().len() > limits.max_nodes
        || graph.nodes().len() > limits.graph.max_nodes
        || raw_roots.len() > limits.graph.max_roots
        || graph.edges().len() > limits.graph.max_edges
        || context.endpoints.len() > limits.max_registries
    {
        return Err(LockedGraphError::Limit);
    }
    let mut bytes = 0usize;
    for raw in raw_roots {
        charge(&mut bytes, raw.len(), limits.max_retained_utf8_bytes)?;
    }
    for scope in root_scopes.into_iter().flatten() {
        for (index, value) in scope.values().enumerate() {
            if index >= limits.target.markers.max_set_entries
                || value.len() > limits.target.max_field_bytes
                || value.len() > limits.target.markers.max_environment_field_bytes
            {
                return Err(LockedGraphError::Limit);
            }
            charge(&mut bytes, value.len(), limits.max_retained_utf8_bytes)?;
        }
    }
    let environment = target.environment();
    for field in [
        environment.os_name,
        environment.sys_platform,
        environment.platform_release,
        environment.platform_system,
        environment.platform_version,
        environment.platform_machine,
        environment.platform_python_implementation,
        environment.python_version,
        environment.python_full_version,
        environment.implementation_name,
        environment.implementation_version,
    ] {
        charge(&mut bytes, field.len(), limits.max_retained_utf8_bytes)?;
    }
    for tag in target.supported().ordered() {
        for atom in [tag.interpreter(), tag.abi(), tag.platform()] {
            charge(&mut bytes, atom.len(), limits.max_retained_utf8_bytes)?;
        }
    }
    target.with_root_scope(|scope| {
        let (extras, groups): (&[&str], &[&str]) = match scope {
            pakro_index::python_markers::PythonMarkerScope::Environment => (&[], &[]),
            pakro_index::python_markers::PythonMarkerScope::Dependency { requested_extras } => {
                (requested_extras, &[])
            }
            pakro_index::python_markers::PythonMarkerScope::Lock {
                selected_extras,
                dependency_groups,
            } => (selected_extras, dependency_groups),
        };
        for value in extras.iter().chain(groups) {
            charge(&mut bytes, value.len(), limits.max_retained_utf8_bytes)?;
        }
        Ok::<_, LockedGraphError>(())
    })?;
    for endpoint in context.endpoints {
        for field in [
            endpoint.project.registry.id().as_str(),
            endpoint.project.registry.canonical_base().as_url().as_str(),
            endpoint
                .project
                .registry
                .credential_ref()
                .map_or("", |v| v.as_str()),
            endpoint.file.credential_ref.map_or("", |v| v.as_str()),
        ] {
            charge(&mut bytes, field.len(), limits.max_retained_utf8_bytes)?;
        }
    }
    let policy_ids = match context.source_policy {
        PythonSourcePolicy::Explicit(id) => std::slice::from_ref(id),
        PythonSourcePolicy::FirstProject(ids) => ids.as_slice(),
    };
    if policy_ids.len() > limits.max_registries {
        return Err(LockedGraphError::Limit);
    }
    for id in policy_ids {
        charge(
            &mut bytes,
            id.as_str().len(),
            limits.max_retained_utf8_bytes,
        )?;
    }
    let registries = context
        .endpoints
        .iter()
        .map(|e| e.project.registry)
        .collect::<Vec<_>>();
    let order = ordered_python_source_indices(&registries, context.source_policy)
        .map_err(|_| LockedGraphError::InvalidConfiguration)?;
    // Membership only: strict replay never asks which source is available and
    // never changes a pinned registry according to FirstProject order.
    let allowed = order
        .into_iter()
        .map(|index| {
            (
                context.endpoints[index].project.registry.id().as_str(),
                index,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let pins = model
        .pins()
        .iter()
        .map(|pin| (pin.id(), pin))
        .collect::<BTreeMap<_, _>>();
    let mut archive_bytes = 0u64;
    for node in graph.nodes() {
        let pin = pins
            .get(&node.source_pin())
            .ok_or(LockedGraphError::GraphMismatch)?;
        archive_bytes = archive_bytes
            .checked_add(pin.size())
            .filter(|total| *total <= limits.max_retained_archive_bytes)
            .ok_or(LockedGraphError::Limit)?;
        for field in [node.name(), node.version()] {
            charge(&mut bytes, field.len(), limits.max_retained_utf8_bytes)?;
        }
        let endpoint = *allowed
            .get(pin.file().registry_id())
            .ok_or(LockedGraphError::RegistryMismatch)?;
        if pin.file().canonical_base_fingerprint()
            != &canonical_registry_base_fingerprint(
                context.endpoints[endpoint]
                    .project
                    .registry
                    .canonical_base(),
            )
        {
            return Err(LockedGraphError::RegistryMismatch);
        }
        if pin.file().kind() == RegistryArtifactKind::Wheel {
            let file = pakro_index::parse_wheel_filename(pin.file().filename())
                .ok_or(LockedGraphError::GraphMismatch)?;
            if target
                .supported()
                .best_rank(&file.tags)
                .map_err(|_| LockedGraphError::TargetMismatch)?
                .is_none()
            {
                return Err(LockedGraphError::TargetMismatch);
            }
        }
        clock(context.deadline)?;
    }
    let by_name = graph
        .nodes()
        .iter()
        .map(|node| (node.name(), node))
        .collect::<BTreeMap<_, _>>();
    let mut roots = Vec::new();
    roots
        .try_reserve_exact(raw_roots.len())
        .map_err(|_| LockedGraphError::Limit)?;
    let mut active = Vec::new();
    active
        .try_reserve_exact(raw_roots.len())
        .map_err(|_| LockedGraphError::Limit)?;
    let mut root_work = 0usize;
    for (index, raw) in raw_roots.iter().enumerate() {
        clock(context.deadline)?;
        let parsed =
            parse_python_requirement(raw, limits.requirements).map_err(|error| match error {
                PythonRequirementError::InvalidLimits => LockedGraphError::InvalidLimits,
                PythonRequirementError::LimitExceeded(_) => LockedGraphError::Limit,
                _ => LockedGraphError::InvalidRoot,
            })?;
        charge(
            &mut root_work,
            parsed.marker().map_or(1, |m| m.node_count()),
            limits.graph.max_evaluation_nodes,
        )?;
        let enabled = match parsed.marker() {
            None => true,
            Some(expression) => with_root_scopes(root_scopes, target, |scopes| {
                scopes.with_scope(index, |scope| {
                    evaluate_python_marker(
                        expression,
                        environment,
                        scope,
                        target.semantics(),
                        limits.target.markers,
                    )
                })
            })
            .map_err(LockedGraphError::Graph)?
            .map_err(|_| LockedGraphError::Graph(GraphCheckError::Marker))?
            .value(),
        };
        if enabled {
            let node = *by_name
                .get(parsed.name())
                .ok_or(LockedGraphError::Graph(GraphCheckError::Unsatisfied))?;
            if let Some(raw) = parsed.raw_version_specifiers() {
                let constraint = RawVersionConstraint::parse(
                    raw,
                    RawVersionConstraintLimits {
                        max_specifier_bytes: limits.requirements.max_input_bytes,
                        max_clauses: limits.requirements.max_syntax_tokens,
                        max_literal_bytes: limits.requirements.max_input_bytes,
                        max_candidate_bytes: limits.lock.max_field_bytes,
                    },
                )
                .map_err(|error| match error {
                    RawVersionConstraintError::Limit(_) => LockedGraphError::Limit,
                    _ => LockedGraphError::InvalidRoot,
                })?;
                if !constraint
                    .matches(node.version())
                    .map_err(|_| LockedGraphError::Limit)?
                {
                    return Err(LockedGraphError::Graph(GraphCheckError::Unsatisfied));
                }
            }
            active.push((index, node.name()));
        }
        roots.push(parsed);
    }
    if active.len() != graph.roots().len()
        || active
            .iter()
            .zip(graph.roots())
            .any(|((index, name), old)| *index != old.declaration_index() || *name != old.node())
    {
        return Err(LockedGraphError::GraphMismatch);
    }
    let mut bindings = BTreeMap::new();
    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(graph.nodes().len())
        .map_err(|_| LockedGraphError::Limit)?;
    for (index, node) in graph.nodes().iter().enumerate() {
        let pin = *pins
            .get(&node.source_pin())
            .ok_or(LockedGraphError::GraphMismatch)?;
        let key = label(index, pin.id())?;
        if bindings.insert(node.name().to_owned(), key).is_some() {
            return Err(LockedGraphError::GraphMismatch);
        }
        nodes.push(LockedGraphNode {
            key,
            node,
            pin,
            endpoint: *allowed
                .get(pin.file().registry_id())
                .ok_or(LockedGraphError::RegistryMismatch)?,
        });
    }
    clock(context.deadline)?;
    root.recheck().map_err(|_| LockedGraphError::Revoked)?;
    clock(context.deadline)?;
    Ok(LockedRegistryReplay {
        root,
        graph,
        roots,
        root_scopes: root_scopes.map(<[OwnedRootMarkerScope]>::to_vec),
        target: target.clone(),
        context,
        owners: BTreeMap::new(),
        bindings,
        nodes,
        retained_utf8: bytes,
        root_work,
        builds: BTreeMap::new(),
    })
}
