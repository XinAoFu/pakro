//! Portable mixed-distribution facts; no source or filesystem authority.
use super::*;
use crate::packages::python_environment::model::SourceBuildRecord;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MixedLockSource {
    Registry {
        pin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Conda {
        source_pin: String,
        metadata_path: String,
        metadata_sha256: [u8; 32],
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MixedLockNode {
    pub name: String,
    pub version: String,
    pub source: MixedLockSource,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MixedLockRoot {
    pub declaration_index: usize,
    pub node: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MixedLockEdge {
    pub from: String,
    pub to: String,
    pub declaration: String,
}

/// One distribution graph can map several distributions to a single Conda
/// archive. Its edges describe Python requirements, not additional objects.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MixedLockGraph {
    pub config_scope: String,
    pub group: String,
    pub target_fingerprint: [u8; 32],
    pub language: [u8; 32],
    pub marker_semantics: String,
    pub root_declarations: Vec<String>,
    pub roots: Vec<MixedLockRoot>,
    pub nodes: Vec<MixedLockNode>,
    pub edges: Vec<MixedLockEdge>,
}

impl MixedLockGraph {
    pub(super) fn validate(
        &self,
        model: &PythonLockModel,
        limits: LockModelLimits,
    ) -> LockModelResult<()> {
        valid_scope(&self.config_scope)?;
        if self.group.is_empty() || self.marker_semantics != "current2026" {
            return Err(LockModelError::InvalidField(LockField::Group));
        }
        group_shape(
            self.root_declarations.len(),
            self.roots.len(),
            self.nodes.len(),
            self.edges.len(),
            limits,
        )?;
        let conda = model
            .conda
            .iter()
            .find(|graph| {
                graph.config_scope == self.config_scope
                    && graph.group == self.group
                    && graph.target_fingerprint == self.target_fingerprint
            })
            .ok_or(LockModelError::Reference(LockReference::NodePin))?;
        let python = conda
            .records
            .iter()
            .find(|record| record.package_record.name.as_normalized() == "python")
            .ok_or(LockModelError::Reference(LockReference::NodePin))?;
        let plan =
            crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::from_conda_record(
                python,
            )
            .map_err(|_| LockModelError::IdentityMismatch)?;
        let language = plan
            .language_key(false)
            .map_err(|_| LockModelError::IdentityMismatch)?;
        if language.fingerprint() != self.language || language.target() != self.target_fingerprint {
            return Err(LockModelError::IdentityMismatch);
        }
        let sources = conda
            .records
            .iter()
            .filter(|record| record.package_record.name.as_normalized() != "python")
            .map(|record| {
                crate::libraries::package::conda::record_source(record)
                    .map_err(|_| LockModelError::IdentityMismatch)
            })
            .collect::<LockModelResult<BTreeSet<_>>>()?;
        let mut usage = Usage::default();
        text(
            &self.config_scope,
            LockField::Scope,
            false,
            &mut usage,
            limits,
        )?;
        text(&self.group, LockField::Group, false, &mut usage, limits)?;
        let mut names = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for node in &self.nodes {
            text(&node.name, LockField::Project, false, &mut usage, limits)?;
            text(&node.version, LockField::Version, false, &mut usage, limits)?;
            if normalized(&node.name)? != node.name
                || exact_version(&node.version)? != node.version
                || !names.insert(node.name.as_str())
            {
                return Err(LockModelError::IdentityMismatch);
            }
            match &node.source {
                MixedLockSource::Registry { pin, url } => {
                    let id = PinId::parse_hex(pin)?;
                    let actual = model
                        .pins
                        .iter()
                        .find(|candidate| candidate.id() == id)
                        .ok_or(LockModelError::Reference(LockReference::NodePin))?;
                    if let Some(url) = url {
                        text(url, LockField::Filename, false, &mut usage, limits)?;
                        if !valid_registry_url(url, actual.file().filename()) {
                            return Err(LockModelError::IdentityMismatch);
                        }
                    }
                    if actual.file().project() != node.name
                        || !same_version(actual.file().version(), &node.version)
                    {
                        return Err(LockModelError::IdentityMismatch);
                    }
                }
                MixedLockSource::Conda {
                    source_pin,
                    metadata_path,
                    ..
                } => {
                    text(source_pin, LockField::PinId, false, &mut usage, limits)?;
                    text(
                        metadata_path,
                        LockField::Filename,
                        false,
                        &mut usage,
                        limits,
                    )?;
                    let path = std::path::Path::new(metadata_path);
                    let parent = path
                        .parent()
                        .and_then(|parent| parent.file_name())
                        .and_then(|part| part.to_str());
                    let stem = match path.file_name().and_then(|name| name.to_str()) {
                        Some("METADATA") => parent.and_then(|part| part.strip_suffix(".dist-info")),
                        Some("PKG-INFO") => parent.and_then(|part| part.strip_suffix(".egg-info")),
                        _ => None,
                    };
                    let identity = stem.and_then(|stem| stem.rsplit_once('-'));
                    if !sources.contains(source_pin)
                        || metadata_path.contains('\\')
                        || !path
                            .components()
                            .all(|part| matches!(part, std::path::Component::Normal(_)))
                        || !identity.is_some_and(|(name, version)| {
                            normalized(name).is_ok_and(|name| name == node.name)
                                && same_version(version, &node.version)
                        })
                        || !paths.insert((source_pin, metadata_path))
                    {
                        return Err(LockModelError::IdentityMismatch);
                    }
                }
            }
        }
        for declaration in &self.root_declarations {
            text(
                declaration,
                LockField::Declaration,
                true,
                &mut usage,
                limits,
            )?;
        }
        let mut roots = BTreeSet::new();
        let mut pending = VecDeque::new();
        for root in &self.roots {
            if root.declaration_index >= self.root_declarations.len()
                || !roots.insert(root.declaration_index)
                || !names.contains(root.node.as_str())
            {
                return Err(LockModelError::Reference(LockReference::Root));
            }
            pending.push_back(root.node.as_str());
        }
        let mut adjacency = BTreeMap::<&str, Vec<&str>>::new();
        for edge in &self.edges {
            text(
                &edge.declaration,
                LockField::Declaration,
                true,
                &mut usage,
                limits,
            )?;
            if !names.contains(edge.from.as_str()) || !names.contains(edge.to.as_str()) {
                return Err(LockModelError::Reference(LockReference::Edge));
            }
            adjacency.entry(&edge.from).or_default().push(&edge.to);
        }
        let mut reached = BTreeSet::new();
        while let Some(name) = pending.pop_front() {
            if reached.insert(name) {
                if let Some(next) = adjacency.get(name) {
                    pending.extend(next.iter().copied());
                }
            }
        }
        if reached.len() != self.nodes.len() {
            return Err(LockModelError::Reference(LockReference::UnreachableNode));
        }
        Ok(())
    }
}

impl PythonLockModel {
    pub(crate) fn mixed(&self) -> &[MixedLockGraph] {
        &self.mixed
    }

    /// Add the already checked mixed graph with its real registry pins. The
    /// caller must retain actual Conda and registry owners until commit.
    pub(crate) fn with_mixed(
        mut self,
        graph: MixedLockGraph,
        pins: &[ContentPin],
        builds: &[(PinId, SourceBuildRecord)],
        limits: LockModelLimits,
    ) -> LockModelResult<Self> {
        for pin in pins {
            if let Some(previous) = self.pins.iter().find(|old| old.id() == pin.id()) {
                if previous != pin {
                    return Err(LockModelError::IdentityMismatch);
                }
            } else {
                self.pins.push(pin.clone());
            }
        }
        for (id, build) in builds {
            if let Some((_, previous)) = self.builds.iter().find(|(old, _)| old == id) {
                if previous != build {
                    return Err(LockModelError::IdentityMismatch);
                }
            } else {
                self.builds.push((*id, build.clone()));
            }
        }
        self.mixed.push(graph);
        validate_owned(&self, limits)?;
        self.mixed
            .last()
            .expect("just inserted")
            .validate(&self, limits)?;
        close_owned(self, limits)
    }
}

/// Portable locator, without credentials or ephemeral request parameters.
pub(crate) fn valid_registry_url(raw: &str, filename: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.as_str() == raw
        && url.path_segments().and_then(|mut p| p.next_back()) == Some(filename)
}

#[cfg(test)]
mod locator_tests {
    use super::valid_registry_url;

    #[test]
    fn portable_locator_rejects_credentials_queries_and_wrong_filename() {
        let filename = "demo_pkg-1.0-py3-none-any.whl";
        assert!(valid_registry_url(
            "https://files.example/files/demo_pkg-1.0-py3-none-any.whl",
            filename
        ));
        for url in [
            "https://user:secret@files.example/files/demo_pkg-1.0-py3-none-any.whl",
            "https://files.example/files/demo_pkg-1.0-py3-none-any.whl?token=secret",
            "https://files.example/files/demo_pkg-1.0-py3-none-any.whl#sha256=123",
            "https://files.example/files/other.whl",
        ] {
            assert!(!valid_registry_url(url, filename));
        }
    }
}
