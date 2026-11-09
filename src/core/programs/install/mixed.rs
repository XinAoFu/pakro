//! PyPI wheels resolved against actual, held Conda-provided distributions.
use std::collections::{BTreeMap, BTreeSet};

use eyre::{ContextCompat, Result, ensure};
use pakro_resolver::python_native::CandidateKey;
use sha2::{Digest as _, Sha256};

use crate::libraries::language::identity::IdentityLimits;
use crate::libraries::language::lease::LanguageLease;
use crate::libraries::package::identity::{
    PackageIdentityInput, PackageInstanceId, PackageRole, SourceIdentity,
};
use crate::libraries::package::lease::PackageLease;
use crate::libraries::package::manifest::PackageFacts;
use crate::packages::python_environment::model::{
    PackageGraphEdge, PackageGraphNode, PackageGraphRecord, PackageSourcePin,
};
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use crate::packages::python_resolution::source::registry_depot::closed::{
    RetainedMixedGraphEvidence, SharedRegistryPackageSource, close_mixed_registry_plan,
};
use crate::packages::python_resolution::source::registry_depot::conda_metadata::CondaDistribution;
use crate::packages::python_resolution::source::registry_depot::{
    RegistryDepotContext, RegistryPreparation, RegistryRootScope, ResolveRegistryDepot,
};
use crate::programs::OperationContext;

pub(crate) struct MixedWheels<'root> {
    pub graph: PackageGraphRecord,
    pub wheels: Vec<PackageLease>,
    pub evidence: RetainedMixedGraphEvidence<'root>,
    lock_edges: Vec<crate::packages::python_resolution::lock_model::MixedLockEdge>,
}

impl MixedWheels<'_> {
    pub(crate) fn lock_graph(
        &self,
        language: &LanguageLease,
        declarations: Vec<String>,
    ) -> Result<(
        crate::packages::python_resolution::lock_model::MixedLockGraph,
        Vec<crate::packages::python_resolution::lock_model::ContentPin>,
    )> {
        use crate::packages::python_resolution::lock_model::{
            MixedLockGraph, MixedLockNode, MixedLockRoot, MixedLockSource,
        };
        self.evidence.recheck()?;
        let selected = &self.evidence.plan().selected;
        let names: BTreeMap<_, _> = selected
            .iter()
            .map(|node| (node.key, node.name.as_str()))
            .collect();
        let mut nodes = Vec::new();
        let mut pins = Vec::new();
        for node in selected {
            let source = if let Some(owner) = self.evidence.conda_owner(node.key) {
                MixedLockSource::Conda {
                    source_pin: crate::libraries::package::conda::record_source(
                        owner.tree().record(),
                    )?,
                    metadata_path: owner
                        .path()
                        .to_str()
                        .context("Conda metadata path is not UTF-8")?
                        .to_owned(),
                    metadata_sha256: Sha256::digest(owner.metadata().raw()).into(),
                }
            } else {
                let pin = self
                    .evidence
                    .registry_pin(node.key)
                    .context("mixed registry pin is missing")?;
                pins.push(pin.clone());
                MixedLockSource::Registry {
                    pin: hex::encode(pin.id().as_bytes()),
                    url: Some(
                        self.evidence
                            .registry_url(node.key)
                            .context("mixed registry file has no portable locked URL")?
                            .to_owned(),
                    ),
                }
            };
            nodes.push(MixedLockNode {
                name: node.name.clone(),
                version: node.version.clone(),
                source,
            });
        }
        let roots = self
            .evidence
            .checked()
            .roots
            .iter()
            .map(|root| MixedLockRoot {
                declaration_index: root.declaration,
                node: names[&root.to].to_owned(),
            })
            .collect();
        let edges = self.lock_edges.clone();
        self.evidence.recheck()?;
        Ok((
            MixedLockGraph {
                config_scope: String::new(),
                group: crate::programs::shared::commit::DEFAULT_GROUP.into(),
                target_fingerprint: language.key().target(),
                language: language.key().fingerprint(),
                marker_semantics: "current2026".into(),
                root_declarations: declarations,
                roots,
                nodes,
                edges,
            },
            pins,
        ))
    }
}

pub(crate) async fn resolve_project_wheels<'root>(
    context: &OperationContext,
    language: &LanguageLease,
    root: &'root BoundPythonLock,
    requests: &[crate::packages::PackageInstallRequest],
    fixed: Vec<CondaDistribution<'root>>,
    warm_packages: &[PackageLease],
    intent: crate::programs::shared::python_roots::PythonLockIntent,
    preferred_pins: Vec<crate::packages::python_resolution::lock_model::ContentPin>,
) -> Result<MixedWheels<'root>> {
    use crate::packages::python_resolution::candidate_policy::{
        CandidatePolicy, Prereleases, Sdists, Yanked,
    };
    use crate::packages::python_resolution::source::registry_depot::RegistryEndpoint;
    use pakro_index::artifact::NetworkPolicy;
    use pakro_index::python_source::AuthScopeKey;
    use pakro_index::simple::{
        AuthOriginGrants, PythonSourceSession, SimpleFetchPolicy, SimpleIndexClient,
        SourceFileSession,
    };

    let strict = intent == crate::programs::shared::python_roots::PythonLockIntent::Preserve
        && (context.policy().locked || root.model().is_some_and(|model| !model.mixed().is_empty()));
    let declarations = requests
        .iter()
        .map(|request| request.requirement().map_err(|error| eyre::eyre!(error)))
        .collect::<Result<Vec<_>>>()?;
    let cache = context.requested_cache_root().join("shared-python");
    let scratch = cache.join("scratch");
    let client = SimpleIndexClient::new(
        cache.join("simple"),
        crate::programs::python::shared_registry_http_config(context.settings()),
    )?;
    let sources = context.python_sources()?;
    ensure!(
        sources.registries.len() == 1,
        "mixed PyPI resolution requires one public registry"
    );
    let registry = &sources.registries[0];
    let scope = AuthScopeKey::from_local_revision([0; 32]);
    let credentials = crate::programs::python::PublicCredentials;
    let grants = AuthOriginGrants::default();
    let route = crate::programs::python::FrozenRouter {
        settings: context.settings(),
    };
    let endpoints = [RegistryEndpoint {
        project: PythonSourceSession {
            registry,
            auth_scope: &scope,
            credentials: &credentials,
            auth_origins: &grants,
            transport: &route,
        },
        file: SourceFileSession {
            credential_ref: None,
            auth_scope: &scope,
            credentials: &credentials,
            auth_origins: &grants,
            transport: &route,
        },
    }];
    let registry_context = RegistryDepotContext {
        preferred_pins,
        client: &client,
        endpoints: &endpoints,
        source_policy: &sources.policy,
        candidate_policy: CandidatePolicy {
            prereleases: Prereleases::IfNoMatchingFinal,
            yanked: Yanked::Disallow,
            sdists: Sdists::Include,
            installed_version: None,
        },
        supported: language.target().supported(),
        environment: language.target().environment(),
        root_scope: RegistryRootScope::Environment,
        semantics: language.target().semantics(),
        fetch: SimpleFetchPolicy {
            network: if context.policy().offline {
                NetworkPolicy::Offline
            } else {
                NetworkPolicy::Allowed
            },
            refresh: intent == crate::programs::shared::python_roots::PythonLockIntent::Update,
            fresh_for: std::time::Duration::from_secs(3600),
            now: std::time::SystemTime::now(),
        },
        limits: Default::default(),
        cache_root: &cache,
        scratch_root: &scratch,
        deadline: context.policy().deadline,
    };
    if strict {
        let closed =
            crate::packages::python_resolution::source::registry_depot::mixed_replay::replay(
                root,
                &declarations,
                registry_context,
                fixed,
                warm_packages,
                language.key().fingerprint(),
                language.key().target(),
            )
            .await?;
        publish_wheels(context, language, closed)
    } else {
        resolve_wheels(
            context,
            language,
            root,
            &declarations,
            registry_context,
            fixed,
        )
        .await
    }
}

/// The caller holds each extracted tree and its published Conda package. A
/// registry wheel is never substituted for a Conda-provided distribution.
pub(crate) async fn resolve_wheels<'root, 'config>(
    context: &OperationContext,
    language: &'config LanguageLease,
    root: &'root BoundPythonLock,
    declarations: &[String],
    mut registry: RegistryDepotContext<'config>,
    fixed: Vec<CondaDistribution<'root>>,
) -> Result<MixedWheels<'root>> {
    context.checkpoint()?;
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids package acquisition"
    );
    language.check_binding()?;
    registry.environment = language.target().environment();
    registry.supported = language.target().supported();
    registry.semantics = language.target().semantics();
    registry.root_scope = RegistryRootScope::Environment;
    registry.deadline = registry.deadline.min(context.policy().deadline);
    let raw: Vec<_> = declarations.iter().map(String::as_str).collect();
    let depot = ResolveRegistryDepot::new(root, &raw, registry)?.with_conda(fixed)?;
    let prepared = match depot.prepare().await? {
        RegistryPreparation::Mixed(value) => value,
        RegistryPreparation::Ready(value) => value.into_mixed(),
        RegistryPreparation::NeedsBuild(_) => {
            eyre::bail!("mixed source builds are not supported yet")
        }
        RegistryPreparation::NeedsSourceAdapter(_) => {
            eyre::bail!("mixed direct-source dependencies are not supported")
        }
        RegistryPreparation::NeedsConstraintPolicy(_) => {
            eyre::bail!("mixed constraint-policy continuation is not supported yet")
        }
    };
    let closed = close_mixed_registry_plan(prepared, Default::default())?;
    publish_wheels(context, language, closed)
}

fn publish_wheels<'root>(
    context: &OperationContext,
    language: &LanguageLease,
    closed: crate::packages::python_resolution::source::registry_depot::closed::ClosedMixedRegistryGraph<'root>,
) -> Result<MixedWheels<'root>> {
    closed.recheck()?;
    let selected = &closed.plan().selected;
    let indices: BTreeMap<_, _> = selected
        .iter()
        .enumerate()
        .map(|(i, node)| (node.key, i))
        .collect();
    let mut nodes = Vec::with_capacity(selected.len());
    let mut conda_instances = BTreeMap::new();
    let mut mapping = Vec::with_capacity(selected.len());
    for node in selected {
        if let Some(owner) = closed.conda_owner(node.key) {
            let key = owner.package().manifest().key();
            let index = if let Some(&index) = conda_instances.get(&key) {
                index
            } else {
                let index = nodes.len();
                nodes.push(PackageGraphNode::from_conda_record(
                    tree_record(owner),
                    key,
                )?);
                conda_instances.insert(key, index);
                index
            };
            mapping.push(index);
        } else {
            mapping.push(nodes.len());
            let owner = closed
                .registry_owner(node.key)
                .context("mixed registry owner is missing")?;
            ensure!(
                owner.build_record().is_none(),
                "mixed source builds are not supported yet"
            );
            let pin = owner.pin();
            nodes.push(PackageGraphNode {
                name: node.name.clone(),
                version: node.version.clone(),
                instance: [0; 32],
                source_pin: hex::encode(pin.id().as_bytes()),
                artifact: owner.output_digest().to_string(),
                pin: PackageSourcePin {
                    registry: pin.file().registry_id().into(),
                    base: *pin.file().canonical_base_fingerprint(),
                    filename: pin.file().filename().into(),
                    size: pin.size(),
                    advertised: matches!(
                        pin.evidence(),
                        crate::packages::python_resolution::lock_model::PinEvidence::Advertised
                    ),
                    build: None,
                    conda: None,
                },
            });
        }
    }
    let mut distribution_edges = Vec::new();
    for edge in &closed.checked().edges {
        let facts = if let Some(owner) = closed.conda_owner(edge.from) {
            owner.facts()
        } else {
            closed
                .registry_owner(edge.from)
                .context("mixed dependency owner is missing")?
                .facts()
        };
        let declaration = facts
            .requirements()
            .get(edge.declaration)
            .context("mixed dependency declaration is missing")?
            .raw_for_semantic_resolution()
            .to_owned();
        distribution_edges.push(PackageGraphEdge {
            from: indices[&edge.from],
            to: indices[&edge.to],
            declaration,
        });
    }
    let lock_edges = distribution_lock_edges(selected, &distribution_edges);
    let distribution_roots = closed
        .checked()
        .roots
        .iter()
        .map(|root| indices[&root.to])
        .collect::<Vec<_>>();
    let (roots, edges) =
        project_distribution_graph(&mapping, &distribution_roots, &distribution_edges);
    let target = language.shared_target_identity()?;
    for (selected_index, selected) in selected.iter().enumerate() {
        let index = mapping[selected_index];
        if closed.conda_owner(selected.key).is_some() {
            continue;
        }
        let mut reached = BTreeSet::new();
        let mut pending = vec![index];
        while let Some(next) = pending.pop() {
            if reached.insert(next) {
                pending.extend(
                    edges
                        .iter()
                        .filter(|edge| edge.from == next)
                        .map(|edge| edge.to),
                );
            }
        }
        let mut facts = BTreeMap::new();
        for i in reached {
            let mut dependencies: Vec<_> = edges
                .iter()
                .filter(|edge| edge.from == i)
                .map(|edge| (nodes[edge.to].name.as_str(), edge.declaration.as_str()))
                .collect();
            dependencies.sort_unstable();
            facts.insert(
                nodes[i].name.as_str(),
                (&nodes[i].version, &nodes[i].source_pin, dependencies),
            );
        }
        let mut hash = Sha256::new();
        hash.update(b"pakro.package-dependency-context/v1\0");
        hash.update(language.key().fingerprint());
        hash.update(serde_json::to_vec(&facts)?);
        let owner = closed
            .registry_owner(selected.key)
            .context("mixed registry owner is missing")?;
        let source = SourceIdentity::new(
            owner.pin().file().registry_id(),
            *owner.pin().id().as_bytes(),
            IdentityLimits::default(),
        )?;
        nodes[index].instance = PackageInstanceId::python(
            PackageIdentityInput {
                name: &selected.name,
                exact_version: &selected.version,
                source: &source,
                role: PackageRole::Runtime,
                target: &target,
                dependency_context: hash.finalize().into(),
            },
            IdentityLimits::default(),
        )?
        .fingerprint();
    }
    let graph = PackageGraphRecord {
        roots,
        nodes,
        edges,
    };
    let facts: BTreeMap<_, _> = selected
        .iter()
        .enumerate()
        .filter_map(|(selected_index, node)| {
            let index = mapping[selected_index];
            closed.registry_owner(node.key).map(|_| {
                let mut dependencies: Vec<_> = graph
                    .edges
                    .iter()
                    .filter(|edge| edge.from == index)
                    .map(|edge| graph.nodes[edge.to].instance)
                    .collect();
                dependencies.sort_unstable();
                dependencies.dedup();
                (
                    node.key,
                    PackageFacts {
                        key: graph.nodes[index].instance,
                        name: node.name.clone(),
                        version: node.version.clone(),
                        artifact: graph.nodes[index].artifact.clone(),
                        source: graph.nodes[index].source_pin.clone(),
                        dependencies,
                    },
                )
            })
        })
        .collect();
    let (sources, evidence) = closed.into_shared_package_sources()?;
    let mut sources: BTreeMap<CandidateKey, _> = sources.into_iter().collect();
    let mut pending = facts;
    let mut wheels = Vec::new();
    while !pending.is_empty() {
        let ready: Vec<_> = pending
            .iter()
            .filter_map(|(key, facts)| {
                facts
                    .dependencies
                    .iter()
                    .all(|dependency| {
                        !pending
                            .values()
                            .any(|candidate| candidate.key == *dependency)
                    })
                    .then_some(*key)
            })
            .collect();
        ensure!(
            !ready.is_empty(),
            "mixed wheel dependency cycles are not supported yet; pending packages blocked by the cycle: [{}]",
            pending.values().map(|facts| facts.name.as_str()).collect::<Vec<_>>().join(", ")
        );
        for key in ready {
            evidence.recheck()?;
            let fact = pending.remove(&key).expect("selected pending wheel");
            let source = sources
                .remove(&key)
                .context("mixed wheel source is missing")?;
            let lease = match source {
                SharedRegistryPackageSource::Archive(archive) => {
                    crate::libraries::package::tree::prepare_package_tree(
                        context,
                        language,
                        fact.clone(),
                        archive,
                    )?
                }
                SharedRegistryPackageSource::Existing(lease) => {
                    ensure!(
                        lease.manifest().facts == fact,
                        "existing mixed wheel facts differ"
                    );
                    lease
                }
            };
            wheels.push(lease);
        }
    }
    ensure!(sources.is_empty(), "unexpected mixed registry source");
    evidence.recheck()?;
    language.check_binding()?;
    Ok(MixedWheels {
        graph,
        wheels,
        evidence,
        lock_edges,
    })
}

/// Project distribution-level reachability onto actual held package instances.
fn project_distribution_graph(
    mapping: &[usize],
    roots: &[usize],
    edges: &[PackageGraphEdge],
) -> (Vec<usize>, Vec<PackageGraphEdge>) {
    let mut seen_roots = BTreeSet::new();
    let roots = roots
        .iter()
        .map(|index| mapping[*index])
        .filter(|index| seen_roots.insert(*index))
        .collect();
    let mut seen_edges = BTreeSet::new();
    let edges = edges
        .iter()
        .filter_map(|edge| {
            let from = mapping[edge.from];
            let to = mapping[edge.to];
            (from != to && seen_edges.insert((from, to, edge.declaration.clone()))).then(|| {
                PackageGraphEdge {
                    from,
                    to,
                    declaration: edge.declaration.clone(),
                }
            })
        })
        .collect();
    (roots, edges)
}

fn distribution_lock_edges(
    selected: &[pakro_resolver::python_native::SelectedCandidate],
    edges: &[PackageGraphEdge],
) -> Vec<crate::packages::python_resolution::lock_model::MixedLockEdge> {
    edges
        .iter()
        .map(
            |edge| crate::packages::python_resolution::lock_model::MixedLockEdge {
                from: selected[edge.from].name.clone(),
                to: selected[edge.to].name.clone(),
                declaration: edge.declaration.clone(),
            },
        )
        .collect()
}

fn tree_record<'a>(owner: &'a CondaDistribution<'_>) -> &'a rattler_conda_types::RepoDataRecord {
    owner.tree().record()
}

/// Validate an unpublished mixed lock against the layout's retained artifacts.
/// This only reads held packages and does not solve or contact a registry.
pub(crate) fn validate_held_metadata(
    context: &OperationContext,
    model: &crate::packages::python_resolution::lock_model::PythonLockModel,
    language: &LanguageLease,
    layout: &crate::libraries::package::lease::LayoutLease,
) -> Result<()> {
    use crate::packages::python_resolution::graph_check::GraphCandidateObservation;
    use crate::packages::python_resolution::lock_model::{MixedLockSource, PinId};
    use crate::packages::python_resolution::source::registry_depot::{
        conda_metadata::inspect_conda_distributions,
        mixed_replay::{candidate_key, validate_graph},
    };
    use crate::packages::python_wheel::{WheelLimits, parse_installed_core_metadata};
    use pakro_index::python_metadata::PythonMetadataLimits;
    use pakro_index::python_requirements::parse_python_requirement;

    context.checkpoint()?;
    layout.check_binding()?;
    language.check_binding()?;
    ensure!(
        layout.language() == language.key().fingerprint(),
        "mixed layout language differs"
    );
    for graph in model.mixed().iter().filter(|graph| {
        graph.language == language.key().fingerprint()
            && graph.target_fingerprint == language.key().target()
    }) {
        let mut trees = Vec::new();
        for package in layout.packages() {
            let Some(record) = package.manifest().conda.as_ref() else {
                continue;
            };
            let source = crate::libraries::package::conda::record_source(record)?;
            if !graph.nodes.iter().any(|node| {
                matches!(
                    &node.source, MixedLockSource::Conda { source_pin, .. } if *source_pin == source
                )
            }) {
                continue;
            }
            let mut artifact =
                crate::libraries::package::conda::held_conda_artifact(context, package)?;
            let tree = crate::packages::conda_artifact::extract(context, &mut artifact, record)?;
            trees.push((package, tree));
        }
        let mut fixed = BTreeMap::new();
        for (package, tree) in &trees {
            for distribution in inspect_conda_distributions(context, package, tree)? {
                ensure!(
                    fixed
                        .insert(
                            distribution.facts().name().as_str().to_owned(),
                            distribution
                        )
                        .is_none(),
                    "multiple held Conda distributions have the same name"
                );
            }
        }
        let mut registry_facts = BTreeMap::new();
        for node in &graph.nodes {
            match &node.source {
                MixedLockSource::Conda {
                    source_pin,
                    metadata_path,
                    metadata_sha256,
                } => {
                    let owner = fixed
                        .get(&node.name)
                        .context("locked Conda distribution is missing from layout")?;
                    owner.recheck()?;
                    ensure!(
                        owner.facts().version() == node.version
                            && owner.path().to_str() == Some(metadata_path.as_str())
                            && <[u8; 32]>::from(Sha256::digest(owner.metadata().raw()))
                                == *metadata_sha256
                            && crate::libraries::package::conda::record_source(
                                owner.tree().record()
                            )? == *source_pin,
                        "locked Conda distribution metadata changed"
                    );
                }
                MixedLockSource::Registry { pin, .. } => {
                    let id = PinId::parse_hex(pin)?;
                    let content = model
                        .pins()
                        .iter()
                        .find(|content| content.id() == id)
                        .context("locked PyPI artifact pin is missing")?;
                    let wheel = pakro_index::parse_wheel_filename(content.file().filename())
                        .context("mixed locked source builds are not supported yet")?;
                    ensure!(
                        language
                            .target()
                            .supported()
                            .best_rank(&wheel.tags)?
                            .is_some(),
                        "held wheel is incompatible with the locked target"
                    );
                    let mut matching = layout.packages().iter().filter(|package| {
                        let manifest = package.manifest();
                        manifest.conda.is_none()
                            && manifest.language == Some(graph.language)
                            && manifest.facts.source == *pin
                    });
                    let package = matching
                        .next()
                        .context("locked wheel is missing from layout")?;
                    ensure!(
                        matching.next().is_none(),
                        "multiple held wheels match the locked pin"
                    );
                    let manifest = package.manifest();
                    ensure!(
                        manifest.facts.artifact == content.digest().to_string()
                            && manifest.filename == content.file().filename()
                            && manifest.name() == content.file().project(),
                        "held wheel source differs from lock"
                    );
                    let bytes =
                        package.read_wheel_metadata(WheelLimits::default().max_metadata_bytes)?;
                    let metadata = parse_installed_core_metadata(
                        bytes,
                        content.file().project(),
                        content.file().version(),
                        WheelLimits::default(),
                    )?;
                    let facts = crate::packages::python_resolution::source::wheel_metadata::project_core_metadata(
                        &metadata, content.file().project(), content.file().version(), PythonMetadataLimits::default())?;
                    ensure!(
                        facts.name().as_str() == node.name && facts.version() == node.version,
                        "held wheel metadata differs from locked distribution"
                    );
                    registry_facts.insert(node.name.as_str(), facts);
                }
            }
        }
        let roots = graph
            .root_declarations
            .iter()
            .map(|raw| parse_python_requirement(raw, Default::default()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let observations = graph
            .nodes
            .iter()
            .map(|node| {
                let facts = match &node.source {
                    MixedLockSource::Conda { .. } => {
                        fixed.get(&node.name).map(|owner| owner.facts())
                    }
                    MixedLockSource::Registry { .. } => registry_facts.get(node.name.as_str()),
                }
                .expect("locked metadata was checked above");
                GraphCandidateObservation {
                    key: candidate_key(&node.name, &node.version),
                    name: &node.name,
                    version: &node.version,
                    facts,
                }
            })
            .collect::<Vec<_>>();
        validate_graph(
            graph,
            &roots,
            &observations,
            language.target().environment(),
            language.target().semantics(),
            Default::default(),
        )?;
    }
    layout.check_binding()?;
    context.checkpoint()?;
    Ok(())
}

#[cfg(test)]
mod distribution_projection_tests {
    use super::*;
    use pakro_resolver::python_native::SelectedCandidate;

    fn edge(from: usize, to: usize, declaration: &str) -> PackageGraphEdge {
        PackageGraphEdge {
            from,
            to,
            declaration: declaration.into(),
        }
    }

    #[test]
    fn shared_conda_owner_keeps_distribution_edges_but_one_package_instance() {
        let selected = ["alpha", "beta", "consumer"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| SelectedCandidate {
                name: name.into(),
                key: CandidateKey([i as u8; 32]),
                version: "1.0".into(),
                requested_extras: Vec::new(),
            })
            .collect::<Vec<_>>();
        // Alpha and beta are supplied by the same Conda artifact. The wheel
        // depends on both; alpha's dependency on beta is internal to that artifact.
        let distribution_edges = vec![
            edge(0, 1, "beta>=1"),
            edge(2, 0, "alpha>=1"),
            edge(2, 1, "beta>=1"),
        ];
        let locked = distribution_lock_edges(&selected, &distribution_edges);
        assert_eq!(
            locked
                .iter()
                .map(|edge| (
                    edge.from.as_str(),
                    edge.to.as_str(),
                    edge.declaration.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("alpha", "beta", "beta>=1"),
                ("consumer", "alpha", "alpha>=1"),
                ("consumer", "beta", "beta>=1")
            ]
        );
        let (roots, edges) =
            project_distribution_graph(&[0, 0, 1], &[0, 1, 2], &distribution_edges);
        assert_eq!(roots, vec![0, 1]);
        assert_eq!(edges, vec![edge(1, 0, "alpha>=1"), edge(1, 0, "beta>=1")]);
    }

    #[test]
    fn distinct_owners_keep_original_graph_order() {
        let input = vec![edge(2, 0, "a"), edge(1, 0, "b")];
        let (roots, edges) = project_distribution_graph(&[0, 1, 2], &[2, 0], &input);
        assert_eq!(roots, vec![2, 0]);
        assert_eq!(edges, input);
    }
}
