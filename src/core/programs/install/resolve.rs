//! Exact roots use the existing registry solver and metadata closure engine.
//! The closed owner retains verified wheel sources through shared publication.

use std::collections::BTreeMap;

use eyre::{Result, ensure};
use sha2::{Digest as _, Sha256};

use crate::libraries::language::lease::LanguageLease;
use crate::packages::python_environment::model::{
    PackageGraphEdge, PackageGraphNode, PackageGraphRecord, PackageSourcePin,
};
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use crate::packages::python_resolution::source::registry_depot::closed::{
    ClosedRegistryGraph, RetainedGraphEvidence, close_registry_plan,
};
use crate::packages::python_resolution::source::registry_depot::{
    RegistryDepotContext, RegistryPreparation, RegistryRootScope, ResolveRegistryDepot,
};
use crate::packages::python_wheel_source::InspectedWheelArchive;
use crate::programs::OperationContext;
use crate::programs::shared::python_roots::{self, PythonLockIntent, PythonRoot};

pub(crate) struct ClosedGraph<'root> {
    roots: Vec<PackageRoot>,
    graph: ClosedRegistryGraph<'root>,
    builds: Vec<(
        crate::packages::python_resolution::lock_model::PinId,
        crate::packages::python_environment::model::SourceBuildRecord,
    )>,
    build_locators: BTreeMap<String, String>,
}
pub(crate) struct PackageRoot {
    name: String,
    version: String,
}
impl PackageRoot {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn version(&self) -> &str {
        &self.version
    }
}
impl<'root> ClosedGraph<'root> {
    pub(crate) fn builds(
        &self,
    ) -> &[(
        crate::packages::python_resolution::lock_model::PinId,
        crate::packages::python_environment::model::SourceBuildRecord,
    )] {
        &self.builds
    }
    /// Gather only actual held source observations and prior locked facts.
    pub(crate) fn locators_for_lock(
        &self,
        root: &BoundPythonLock,
        model: &crate::packages::python_resolution::lock_model::PythonLockModel,
    ) -> Result<BTreeMap<String, String>> {
        root.recheck()?;
        self.graph.recheck_for(root)?;
        let mut urls = self.build_locators.clone();
        for node in &self.graph.plan().selected {
            let owner = self.graph.owner(node.key).ok_or_else(|| eyre::eyre!("closed source owner missing"))?;
            let id = hex::encode(owner.pin().id().as_bytes());
            if let Some(url) = self.graph.registry_url(node.key) {
                if let Some(previous) = urls.get(&id) {
                    ensure!(previous == url, "selected source has conflicting registry URLs");
                } else {
                    urls.insert(id, url.to_owned());
                }
            } else if !urls.contains_key(&id) {
                return Err(eyre::eyre!("selected registry source has no retained portable URL"));
            }
        }
        for pin in model.pins() {
            let id = hex::encode(pin.id().as_bytes());
            if !urls.contains_key(&id) {
                let url = root.model().and_then(|old| old.locator(pin.id()))
                    .ok_or_else(|| eyre::eyre!("retained lock source has no portable URL"))?;
                urls.insert(id, url.to_owned());
            }
        }
        let used = model.pins().iter().map(|pin| hex::encode(pin.id().as_bytes()))
            .collect::<std::collections::BTreeSet<_>>();
        urls.retain(|id, _| used.contains(id));
        self.graph.recheck_for(root)?;
        Ok(urls)
    }
    pub(crate) fn roots(&self) -> &[PackageRoot] {
        &self.roots
    }
    pub(crate) fn graph(&self) -> &ClosedRegistryGraph<'root> {
        &self.graph
    }
    /// Projection order is the native plan's order. Consumed archives must be
    /// matched by package name because source owners have their own key order.
    /// Instance identities come from the shared publisher's checked identities.
    pub(crate) fn projection(&self, instances: &[[u8; 32]]) -> Result<PackageGraphRecord> {
        let selected = &self.graph.plan().selected;
        ensure!(
            instances.len() == selected.len(),
            "package graph instance count differs from its closure"
        );
        let indices: BTreeMap<_, _> = selected
            .iter()
            .enumerate()
            .map(|(i, n)| (n.key, i))
            .collect();
        let mut nodes = Vec::with_capacity(selected.len());
        for (node, instance) in selected.iter().zip(instances) {
            let owner = self
                .graph
                .owner(node.key)
                .ok_or_else(|| eyre::eyre!("closed wheel owner missing"))?;
            let pin = owner.pin();
            nodes.push(PackageGraphNode {
                name: node.name.clone(),
                version: node.version.clone(),
                instance: *instance,
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
                    build: owner.build_record().cloned(),
                    conda: None,
                },
            });
        }
        let roots = self
            .graph
            .checked()
            .roots
            .iter()
            .map(|root| indices[&root.to])
            .collect();
        let mut edges = Vec::new();
        for edge in &self.graph.checked().edges {
            let owner = self
                .graph
                .owner(edge.from)
                .ok_or_else(|| eyre::eyre!("dependency owner missing"))?;
            let declaration = owner
                .facts()
                .requirements()
                .get(edge.declaration)
                .ok_or_else(|| eyre::eyre!("dependency declaration missing"))?;
            edges.push(PackageGraphEdge {
                from: indices[&edge.from],
                to: indices[&edge.to],
                declaration: declaration.raw_for_semantic_resolution().to_owned(),
            });
        }
        Ok(PackageGraphRecord {
            roots,
            nodes,
            edges,
        })
    }
    pub(crate) fn fingerprint(&self) -> Result<[u8; 32]> {
        let zero = vec![[0; 32]; self.graph.plan().selected.len()];
        self.projection(&zero)?.fingerprint()
    }
    /// A node's instance depends only on its own reachable dependency facts.
    /// Unrelated roots cannot force a second copy of a common package tree.
    pub(crate) fn package_facts(
        &self,
        language: &LanguageLease,
    ) -> Result<Vec<crate::libraries::package::manifest::PackageFacts>> {
        use crate::libraries::language::identity::IdentityLimits;
        use crate::libraries::package::identity::{
            PackageIdentityInput, PackageInstanceId, PackageRole, SourceIdentity,
        };
        use crate::libraries::package::manifest::PackageFacts;
        language.check_binding()?;
        let target = language.shared_target_identity()?;
        let selected = &self.graph.plan().selected;
        let raw = self.projection(&vec![[0; 32]; selected.len()])?;
        let mut identities = Vec::with_capacity(selected.len());
        for (index, node) in selected.iter().enumerate() {
            let mut reached = std::collections::BTreeSet::new();
            let mut pending = vec![index];
            while let Some(next) = pending.pop() {
                if reached.insert(next) {
                    pending.extend(
                        raw.edges
                            .iter()
                            .filter(|edge| edge.from == next)
                            .map(|edge| edge.to),
                    );
                }
            }
            let mut facts = BTreeMap::new();
            for i in &reached {
                let mut edges: Vec<_> = raw
                    .edges
                    .iter()
                    .filter(|edge| edge.from == *i)
                    .map(|edge| (raw.nodes[edge.to].name.as_str(), edge.declaration.as_str()))
                    .collect();
                edges.sort_unstable();
                facts.insert(
                    raw.nodes[*i].name.as_str(),
                    (&raw.nodes[*i].version, &raw.nodes[*i].source_pin, edges),
                );
            }
            let mut hash = Sha256::new();
            hash.update(b"pakro.package-dependency-context/v1\0");
            // Installed entry points bind this actual interpreter path.
            hash.update(language.key().fingerprint());
            hash.update(serde_json::to_vec(&facts)?);
            let mut built_dependencies = BTreeMap::new();
            for reached in &reached {
                if let Some(build) = &raw.nodes[*reached].pin.build {
                    built_dependencies
                        .insert(raw.nodes[*reached].name.as_str(), build.fingerprint()?);
                }
            }
            if !built_dependencies.is_empty() {
                hash.update(b"pakro.built-dependency-outputs/v1\0");
                hash.update(serde_json::to_vec(&built_dependencies)?);
            }
            let owner = self
                .graph
                .owner(node.key)
                .ok_or_else(|| eyre::eyre!("closed package source missing"))?;
            let source = SourceIdentity::new(
                owner.pin().file().registry_id(),
                *owner.pin().id().as_bytes(),
                IdentityLimits::default(),
            )?;
            identities.push(
                PackageInstanceId::python(
                    PackageIdentityInput {
                        name: &node.name,
                        exact_version: &node.version,
                        source: &source,
                        role: PackageRole::Runtime,
                        target: &target,
                        dependency_context: hash.finalize().into(),
                    },
                    IdentityLimits::default(),
                )?
                .fingerprint(),
            );
        }
        let facts = raw
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let mut dependencies: Vec<_> = raw
                    .edges
                    .iter()
                    .filter(|edge| edge.from == index)
                    .map(|edge| identities[edge.to])
                    .collect();
                dependencies.sort_unstable();
                dependencies.dedup();
                PackageFacts {
                    key: identities[index],
                    name: node.name.clone(),
                    version: node.version.clone(),
                    artifact: node.artifact.clone(),
                    source: node.source_pin.clone(),
                    dependencies,
                }
            })
            .collect();
        language.check_binding()?;
        Ok(facts)
    }
    pub(crate) fn into_package_sources(self) -> Result<(
        Vec<crate::packages::python_resolution::source::registry_depot::closed::SharedRegistryPackageSource>,
        RetainedGraphEvidence<'root>,
    )>{
        Ok(self.graph.into_shared_package_sources()?)
    }
    #[cfg(test)]
    pub(crate) fn into_sources(
        self,
    ) -> Result<(Vec<InspectedWheelArchive>, RetainedGraphEvidence<'root>)> {
        Ok(self.graph.into_shared_sources()?)
    }
}

#[cfg(test)]
pub(crate) async fn resolve_exact_roots<'root, 'config>(
    context: &OperationContext,
    language: &'config LanguageLease,
    root: &'root BoundPythonLock,
    roots: &[(&str, &str)],
    registry: RegistryDepotContext<'config>,
) -> Result<ClosedGraph<'root>> {
    let roots = roots
        .iter()
        .map(|(name, version)| PythonRoot::plain(name, version))
        .collect::<Result<Vec<_>>>()?;
    resolve_project_roots(
        context,
        language,
        root,
        &roots,
        PythonLockIntent::Preserve,
        registry,
    )
    .await
}

pub(crate) async fn resolve_project_roots<'root, 'config>(
    context: &OperationContext,
    language: &'config LanguageLease,
    root: &'root BoundPythonLock,
    roots: &[PythonRoot],
    intent: PythonLockIntent,
    mut registry: RegistryDepotContext<'config>,
) -> Result<ClosedGraph<'root>> {
    context.checkpoint()?;
    language.check_binding()?;
    registry.environment = language.target().environment();
    registry.supported = language.target().supported();
    registry.semantics = language.target().semantics();
    registry.root_scope = RegistryRootScope::Environment;
    let result =
        resolve_registry_declarations(context, root, roots, intent, registry, Some(language))
            .await?;
    language.check_binding()?;
    Ok(result)
}

#[cfg(test)]
async fn resolve_registry_roots<'root>(
    context: &OperationContext,
    root: &'root BoundPythonLock,
    roots: &[(&str, &str)],
    registry: RegistryDepotContext<'_>,
    language: Option<&LanguageLease>,
) -> Result<ClosedGraph<'root>> {
    let roots = roots
        .iter()
        .map(|(name, version)| PythonRoot::plain(name, version))
        .collect::<Result<Vec<_>>>()?;
    resolve_registry_declarations(
        context,
        root,
        &roots,
        PythonLockIntent::Preserve,
        registry,
        language,
    )
    .await
}

async fn resolve_registry_declarations<'root>(
    context: &OperationContext,
    root: &'root BoundPythonLock,
    roots: &[PythonRoot],
    intent: PythonLockIntent,
    mut registry: RegistryDepotContext<'_>,
    language: Option<&LanguageLease>,
) -> Result<ClosedGraph<'root>> {
    context.checkpoint()?;
    ensure!(
        context.policy().writes.libraries,
        "operation policy forbids package acquisition"
    );
    ensure!(
        roots.len() <= registry.limits.max_roots,
        "package root limit exceeded"
    );
    let exact = roots
        .iter()
        .map(|root| PackageRoot {
            name: root.name.clone(),
            version: root.version.clone(),
        })
        .collect();
    let raw = python_roots::declarations(roots);
    registry.deadline = registry.deadline.min(context.policy().deadline);
    root.recheck()?;
    let declarations: Vec<_> = raw.iter().map(String::as_str).collect();
    let mut budget = super::build::BuildBudget::new();
    let prepared = if let Some(language) = language {
        if intent == PythonLockIntent::Preserve
            && (context.policy().locked
                || root.model().is_some_and(|model| !model.graphs().is_empty()))
        {
            super::lock::validate_project_lock_roots(root, language, roots)?;
            super::build::replay_registry(context, language, root, raw, registry, &mut budget, 0)
                .await?
        } else {
            super::build::prepare_registry(context, language, root, raw, registry, &mut budget, 0)
                .await?
        }
    } else {
        let depot = ResolveRegistryDepot::new(root, &declarations, registry)?;
        let RegistryPreparation::Ready(prepared) = depot.prepare().await? else {
            eyre::bail!("source builds require an actual shared Python language lease")
        };
        prepared
    };
    let graph = close_registry_plan(prepared, Default::default())?;
    context.checkpoint()?;
    graph.recheck_for(root)?;
    Ok(ClosedGraph {
        roots: exact,
        graph,
        builds: budget.records(),
        build_locators: budget.locators(),
    })
}

#[cfg(test)]
mod tests;
