//! Read-only projections of committed project registrations and selections.

use std::path::PathBuf;

use super::package_graph::CommittedGraph;
use crate::packages::python_environment::model::InputKind;
use eyre::{Result, ensure};

use pakro_store::Digest;
use uuid::Uuid;

use super::identity::ProjectIdentity;
use super::repository::ProjectRepository;

pub(crate) struct RegisteredProject {
    identity: ProjectIdentity,
    root: PathBuf,
    revision: u64,
}

impl RegisteredProject {
    pub(crate) fn identity(&self) -> &ProjectIdentity {
        &self.identity
    }
    pub(crate) fn root(&self) -> &PathBuf {
        &self.root
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}

/// The committed language-only selection of one project. `generation_allocation`
/// stays `None` in F1: a selection without a real package layout must never
/// carry a fabricated `g-*` allocation or receipt.
pub(crate) struct LanguageSelectionView {
    group: String,
    language: [u8; 32],
    packages: Vec<[u8; 32]>,
    generation_allocation: Option<String>,
    revision: u64,
}

impl LanguageSelectionView {
    pub(crate) fn group(&self) -> &str {
        &self.group
    }
    pub(crate) fn language(&self) -> &[u8; 32] {
        &self.language
    }
    pub(crate) fn package_references(&self) -> &[[u8; 32]] {
        &self.packages
    }
    pub(crate) fn generation_allocation(&self) -> Option<&str> {
        self.generation_allocation.as_deref()
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}

pub(crate) struct ProjectReadViews<'a> {
    repository: &'a ProjectRepository,
}

/// All committed active slots, rather than just the directory's default.
/// This is a projection of the catalog, never an independent root registry.
pub(crate) struct SelectionProtection {
    pub project: Uuid,
    pub group: String,
    pub target: [u8; 32],
    pub revision: u64,
    pub root: PathBuf,
    pub language: [u8; 32],
    pub layout: Option<[u8; 32]>,
    pub packages: Vec<[u8; 32]>,
}

impl<'a> ProjectReadViews<'a> {
    pub(super) fn new(repository: &'a ProjectRepository) -> Self {
        Self { repository }
    }

    pub(crate) fn reclamation_roots(&self) -> Result<Vec<SelectionProtection>> {
        let mut roots = Vec::new();
        for (key, stamp) in self.repository.model().selection_slots() {
            if stamp.language().is_none() && stamp.generation().is_none() {
                continue;
            }
            let project = self
                .repository
                .model()
                .project_registration(key.project())
                .ok_or_else(|| eyre::eyre!("active selection has no registered project"))?;
            let mut languages = std::collections::BTreeSet::new();
            if let Some(language) = stamp.language() {
                languages.insert(sha256_key(language)?);
            }
            let mut packages = Vec::new();
            let layout = if let Some(allocation) = stamp.generation() {
                let generation =
                    self.repository
                        .model()
                        .generation(allocation)
                        .ok_or_else(|| {
                            eyre::eyre!(
                                "selected generation is missing; reclamation roots are unknown"
                            )
                        })?;
                ensure!(
                    generation.target() == key.target(),
                    "selected generation target differs"
                );
                for input in generation.inputs() {
                    match input.kind() {
                        InputKind::Language => {
                            languages.insert(sha256_key(input.digest())?);
                        }
                        InputKind::Package => packages.push(sha256_key(input.digest())?),
                        InputKind::Runtime | InputKind::BuildInput => {
                            eyre::bail!(
                                "active legacy generation has no typed reclamation identities"
                            );
                        }
                        _ => {} // Download/build caches have their own lifecycle.
                    }
                }
                Some(*generation.shared_layout().ok_or_else(|| {
                    eyre::eyre!("active generation has no actual shared layout identity")
                })?)
            } else {
                None
            };
            ensure!(
                languages.len() == 1,
                "active selection must protect one exact language"
            );
            packages.sort_unstable();
            packages.dedup();
            roots.push(SelectionProtection {
                project: key.project(),
                group: key.group().to_owned(),
                target: *key.target(),
                revision: stamp.revision(),
                root: PathBuf::from(project.root()),
                language: *languages.first().expect("checked one language"),
                layout,
                packages,
            });
        }
        Ok(roots)
    }

    /// Committed registrations only. Damage in the catalog surfaces at the
    /// repository level; these views never reconstruct or repair anything.
    pub(crate) fn registered_projects(&self) -> Vec<RegisteredProject> {
        self.repository
            .model()
            .registered_projects()
            .map(|(project, fact)| RegisteredProject {
                identity: ProjectIdentity::from_uuid(project),
                root: PathBuf::from(fact.root()),
                revision: self.latest_revision(project),
            })
            .collect()
    }

    /// The one committed directory default selection. Repository validation
    /// rejects several active target slots instead of comparing their revisions.
    pub(crate) fn language_selection(
        &self,
        identity: &ProjectIdentity,
    ) -> Option<LanguageSelectionView> {
        let (key, stamp) = self
            .repository
            .model()
            .selection_slots()
            .filter(|(key, stamp)| {
                key.project() == identity.as_uuid()
                    && key.group() == crate::programs::shared::commit::DEFAULT_GROUP
                    && (stamp.language().is_some() || stamp.generation().is_some())
            })
            .next()?;
        let record = stamp
            .generation()
            .and_then(|allocation| self.repository.model().generation(allocation));
        let language = stamp.language().or_else(|| {
            record?
                .inputs()
                .iter()
                .find(|input| input.kind() == InputKind::Language)
                .map(|input| input.digest())
        })?;
        let Digest::Sha256(fingerprint) = language.parse::<Digest>().ok()? else {
            return None;
        };
        Some(LanguageSelectionView {
            group: key.group().to_string(),
            language: fingerprint,
            packages: record
                .map(|record| {
                    record
                        .inputs()
                        .iter()
                        .filter(|input| input.kind() == InputKind::Package)
                        .filter_map(|input| match input.digest().parse::<Digest>().ok()? {
                            Digest::Sha256(value) => Some(value),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            generation_allocation: stamp.generation().map(str::to_string),
            revision: stamp.revision(),
        })
    }

    pub(crate) fn package_graph(
        &self,
        identity: &ProjectIdentity,
    ) -> Result<Option<CommittedGraph>> {
        let selection = self
            .repository
            .model()
            .selection_slots()
            .filter(|(key, stamp)| {
                key.project() == identity.as_uuid()
                    && key.group() == crate::programs::shared::commit::DEFAULT_GROUP
                    && stamp.generation().is_some()
            })
            .next();
        let Some((_, stamp)) = selection else {
            return Ok(None);
        };
        let allocation = stamp.generation().expect("filtered generation");
        let record = self
            .repository
            .model()
            .generation(allocation)
            .ok_or_else(|| eyre::eyre!("selected graph allocation is missing"))?;
        CommittedGraph::from_record(record)
    }

    /// Existing content can serve a newly discovered project. Identity is
    /// language + exact direct roots; several different closed results are
    /// returned separately so the caller must report ambiguity.
    pub(crate) fn matching_package_graphs(
        &self,
        language: [u8; 32],
        roots: &[(&str, &str)],
    ) -> Result<Vec<CommittedGraph>> {
        use pakro_resolver::solve::{Pep440, VersionAdapter};
        let mut expected = std::collections::BTreeMap::new();
        for (name, version) in roots {
            let name = pakro_index::python_source::NormalizedPythonName::new(name)?;
            let version = Pep440::parse_version(version)
                .ok_or_else(|| eyre::eyre!("root version is not exact"))?;
            ensure!(
                expected.insert(name.as_str().to_owned(), version).is_none(),
                "duplicate direct package root"
            );
        }
        let mut results = std::collections::BTreeMap::new();
        for record in self.repository.model().generations() {
            let Some(graph) = CommittedGraph::from_record(record)? else {
                continue;
            };
            if *graph.language() != language {
                continue;
            }
            let mut actual = std::collections::BTreeMap::new();
            for root in graph.roots() {
                let node = graph
                    .nodes()
                    .get(*root)
                    .ok_or_else(|| eyre::eyre!("committed graph root is missing"))?;
                actual.insert(
                    node.name().to_owned(),
                    Pep440::parse_version(node.version())
                        .ok_or_else(|| eyre::eyre!("committed graph version is invalid"))?,
                );
            }
            if actual == expected {
                results.insert(*graph.layout(), graph);
            }
        }
        Ok(results.into_values().collect())
    }

    pub(crate) fn conda_package_graphs(&self, language: [u8; 32]) -> Result<Vec<CommittedGraph>> {
        let mut results = std::collections::BTreeMap::new();
        for record in self.repository.model().generations() {
            let Some(graph) = CommittedGraph::from_record(record)? else { continue };
            if *graph.language() == language
                && graph.nodes().iter().any(|node| node.pin.conda.is_some())
            {
                results.insert(*graph.layout(), graph);
            }
        }
        Ok(results.into_values().collect())
    }

    /// Project roots and real dependency declarations explain each protection.
    /// Missing project registration or an incomplete generation is an error.
    pub(crate) fn reference_chains(&self, fingerprint: [u8; 32]) -> Result<Vec<String>> {
        let digest = Digest::Sha256(fingerprint).to_string();
        let mut result = std::collections::BTreeSet::new();
        for (key, stamp) in self.repository.model().selection_slots() {
            if stamp.language().is_none() && stamp.generation().is_none() {
                continue;
            }
            let project = self
                .repository
                .model()
                .project_registration(key.project())
                .ok_or_else(|| eyre::eyre!("selection has no registered project location"))?;
            if stamp.language() == Some(digest.as_str()) {
                result.insert(format!("{} -> Python {}", project.root(), digest));
            }
            let Some(allocation) = stamp.generation() else {
                continue;
            };
            let record = self
                .repository
                .model()
                .generation(allocation)
                .ok_or_else(|| eyre::eyre!("selected generation is missing"))?;
            if record
                .inputs()
                .iter()
                .any(|input| input.kind() == InputKind::Language && input.digest() == digest)
            {
                result.insert(format!(
                    "{} -> layout {} -> Python {}",
                    project.root(),
                    allocation,
                    digest
                ));
            }
            let Some(graph) = record.package_graph() else {
                ensure!(!record.inputs().iter().any(|input|input.kind()==InputKind::Package && input.digest()==digest),
                    "package references exist without dependency evidence");
                continue;
            };
            // Bound traversal by nodes + edges, retain one real root chain per
            // target rather than enumerating exponentially many cyclic paths.
            let mut adjacent = vec![Vec::new(); graph.nodes.len()];
            for edge in &graph.edges {
                let list = adjacent
                    .get_mut(edge.from)
                    .ok_or_else(|| eyre::eyre!("dependency source is missing"))?;
                ensure!(edge.to < graph.nodes.len(), "dependency target is missing");
                list.push(edge.to);
            }
            let mut parents = vec![None; graph.nodes.len()];
            let mut reached = vec![false; graph.nodes.len()];
            let mut pending = std::collections::VecDeque::new();
            for index in &graph.roots {
                let visited = reached
                    .get_mut(*index)
                    .ok_or_else(|| eyre::eyre!("dependency root is missing"))?;
                if !*visited {
                    *visited = true;
                    pending.push_back(*index);
                }
            }
            while let Some(index) = pending.pop_front() {
                if graph.nodes[index].instance == fingerprint {
                    let mut path = vec![index];
                    let mut current = index;
                    while let Some(parent) = parents[current] {
                        path.push(parent);
                        current = parent;
                    }
                    let mut chain = project.root().to_owned();
                    for node in path.into_iter().rev().map(|index| &graph.nodes[index]) {
                        ensure!(
                            chain
                                .len()
                                .saturating_add(node.name.len())
                                .saturating_add(node.version.len())
                                <= 16 * 1024 * 1024,
                            "reference chain byte limit exceeded"
                        );
                        chain.push_str(&format!(" -> {}=={}", node.name, node.version));
                    }
                    result.insert(chain);
                    break;
                }
                for next in &adjacent[index] {
                    if !reached[*next] {
                        reached[*next] = true;
                        parents[*next] = Some(index);
                        pending.push_back(*next);
                    }
                }
            }
        }
        Ok(result.into_iter().collect())
    }

    fn latest_revision(&self, project: Uuid) -> u64 {
        self.repository
            .model()
            .selection_slots()
            .filter(|(key, stamp)| {
                key.project() == project
                    && key.group() == crate::programs::shared::commit::DEFAULT_GROUP
                    && (stamp.language().is_some() || stamp.generation().is_some())
            })
            .map(|(_, stamp)| stamp.revision())
            .max()
            .unwrap_or(0)
    }
}

fn sha256_key(value: &str) -> Result<[u8; 32]> {
    let digest: Digest = value.parse()?;
    ensure!(
        digest.to_string() == value,
        "reclamation key is not canonical"
    );
    let Digest::Sha256(key) = digest else {
        eyre::bail!("reclamation object identity is not SHA256");
    };
    Ok(key)
}
