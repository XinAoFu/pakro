//! Shared content descriptions and independent project choices over one catalog.
//! This pure facade grants no publication, lease, registration or removal permit.

use eyre::{Result, ensure};
use std::collections::BTreeSet;

use crate::libraries::language::identity::{IdentityLimits, LanguageKey, TargetIdentity};
use crate::libraries::package::identity::{
    ArtifactId, BuildRequestId, OutputDigest, PackageInstanceId, SharedLayoutInput, SharedLayoutKey,
};
use crate::packages::python_environment::model::{
    CatalogCodecLimits, CatalogModel, GenerationRecord, InputKind, InputReference, ModelLimits,
};
pub(crate) use crate::packages::python_environment::model::{SelectionKey, SelectionStamp};
use pakro_store::Digest;
use uuid::Uuid;

pub(crate) type TypedReferenceSet = BTreeSet<InputReference>;

pub(crate) struct SharedContentInput<'a> {
    pub allocation: &'a str,
    pub language: LanguageKey,
    pub target: &'a TargetIdentity,
    pub packages: &'a [PackageInstanceId],
    pub graph: [u8; 32],
    pub build_requests: &'a [BuildRequestId],
    pub artifacts: &'a [ArtifactId],
    pub build_outputs: &'a [OutputDigest],
    pub layout_recipe: u32,
    /// Digest of the layout verification receipt, not the build request key.
    pub receipt: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedContentReference {
    record: GenerationRecord,
}
impl SharedContentReference {
    pub(crate) fn allocation(&self) -> &str {
        self.record.allocation()
    }
    pub(crate) fn layout(&self) -> Option<&[u8; 32]> {
        self.record.shared_layout()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectSelection {
    key: SelectionKey,
    stamp: SelectionStamp,
    language: Option<InputReference>,
    packages: Vec<InputReference>,
    layout: Option<[u8; 32]>,
}
impl ProjectSelection {
    pub(crate) fn key(&self) -> &SelectionKey {
        &self.key
    }
    pub(crate) fn stamp(&self) -> &SelectionStamp {
        &self.stamp
    }
    pub(crate) fn language(&self) -> Option<&InputReference> {
        self.language.as_ref()
    }
    pub(crate) fn packages(&self) -> &[InputReference] {
        &self.packages
    }
    pub(crate) fn layout(&self) -> Option<&[u8; 32]> {
        self.layout.as_ref()
    }
}

/// The model is the sole mutable authority. Views above are copied descriptions.
pub(crate) struct ProjectSelections {
    model: CatalogModel,
    limits: ModelLimits,
}
impl ProjectSelections {
    pub(crate) fn new(limits: ModelLimits) -> Result<Self> {
        Ok(Self {
            model: CatalogModel::new(limits)?,
            limits,
        })
    }
    pub(crate) fn register_content(
        &mut self,
        input: SharedContentInput<'_>,
    ) -> Result<SharedContentReference> {
        let count = [
            input.packages.len(),
            input.build_requests.len(),
            input.artifacts.len(),
            input.build_outputs.len(),
        ]
        .into_iter()
        .try_fold(1usize, usize::checked_add)
        .filter(|count| *count <= self.limits.max_refs_per_generation)
        .ok_or_else(|| eyre::eyre!("shared content input reference limit exceeded"))?;
        let target = input.target.fingerprint();
        ensure!(
            input.language.target() == target
                && input
                    .packages
                    .iter()
                    .all(|package| package.target() == target),
            "shared content language or package target differs from the layout"
        );
        let layout = SharedLayoutKey::new(
            SharedLayoutInput {
                language: input.language,
                graph: input.graph,
                build_requests: input.build_requests,
                target: input.target,
                recipe: input.layout_recipe,
            },
            IdentityLimits {
                max_collection_items: self.limits.max_refs_per_generation,
                ..Default::default()
            },
        )?;
        let mut inputs = Vec::new();
        inputs
            .try_reserve_exact(count)
            .map_err(|_| eyre::eyre!("shared content reference allocation failed"))?;
        inputs.push(InputReference::new(
            InputKind::Language,
            Digest::Sha256(input.language.fingerprint()),
        ));
        inputs.extend(input.packages.iter().map(|package| {
            InputReference::new(InputKind::Package, Digest::Sha256(package.fingerprint()))
        }));
        inputs.extend(input.build_requests.iter().map(|build| {
            InputReference::new(InputKind::BuildRequest, Digest::Sha256(build.fingerprint()))
        }));
        inputs.extend(
            input
                .artifacts
                .iter()
                .map(|artifact| InputReference::new(InputKind::Artifact, artifact.digest())),
        );
        inputs.extend(
            input
                .build_outputs
                .iter()
                .map(|output| InputReference::new(InputKind::BuildOutput, output.digest())),
        );
        let record = GenerationRecord::new_shared(
            input.allocation,
            target,
            input.graph,
            input.receipt,
            layout.fingerprint(),
            inputs,
            self.limits,
        )?;
        let allocation = self.model.register_shared_generation(record)?;
        let record = self
            .model
            .generation(&allocation)
            .ok_or_else(|| eyre::eyre!("registered shared content record is missing"))?
            .clone();
        Ok(SharedContentReference { record })
    }
    /// Adds a real graph to shared content before any selection publishes it.
    pub(crate) fn register_content_graph(
        &mut self,
        input: SharedContentInput<'_>,
        graph: crate::packages::python_environment::model::PackageGraphRecord,
    ) -> Result<SharedContentReference> {
        // Build the pure record in an isolated candidate model, then validate
        // complete graph facts before mutating the authoritative catalog.
        let mut candidate = Self::new(self.limits)?;
        let content = candidate.register_content(input)?;
        let record = content.record.with_package_graph(graph, self.limits)?;
        let allocation = self.model.register_shared_generation(record)?;
        Ok(SharedContentReference {
            record: self
                .model
                .generation(&allocation)
                .expect("registered content")
                .clone(),
        })
    }
    pub(crate) fn apply_selection(
        &mut self,
        key: &SelectionKey,
        expected: &SelectionStamp,
        next: Option<&SharedContentReference>,
        transaction: Uuid,
    ) -> Result<SelectionStamp> {
        // An exact nonce replay is historical fact even after safe cleanup has
        // forgotten its retired content. It must never rewind the current slot.
        if self.model.applied_transaction(transaction).is_none()
            && let Some(next) = next
        {
            ensure!(
                next.record.shared_layout().is_some()
                    && self.model.generation(next.allocation()) == Some(&next.record),
                "shared content reference does not match the current catalog facts"
            );
        }
        self.model.apply_selection(
            key,
            expected,
            next.map(SharedContentReference::allocation),
            transaction,
        )?;
        self.model
            .applied_stamp(transaction)
            .ok_or_else(|| eyre::eyre!("applied shared selection transaction is missing"))
    }
    pub(crate) fn snapshot(&self, key: &SelectionKey) -> Result<ProjectSelection> {
        SelectionKey::new(key.project(), key.group(), *key.target(), self.limits)?;
        let stamp = self.model.snapshot(key);
        let (language, packages, layout) = if let Some(allocation) = stamp.generation() {
            let record = self
                .model
                .generation(allocation)
                .ok_or_else(|| eyre::eyre!("selected shared content is missing"))?;
            let layout = *record.shared_layout().ok_or_else(|| {
                eyre::eyre!("selected generic record has no shared content identity")
            })?;
            let mut languages = record
                .inputs()
                .iter()
                .filter(|input| input.kind() == InputKind::Language);
            let language = languages
                .next()
                .ok_or_else(|| eyre::eyre!("shared language reference is missing"))?
                .clone();
            ensure!(
                languages.next().is_none(),
                "shared content has multiple language references"
            );
            let packages = record
                .inputs()
                .iter()
                .filter(|input| input.kind() == InputKind::Package)
                .cloned()
                .collect();
            (Some(language), packages, Some(layout))
        } else {
            (None, Vec::new(), None)
        };
        Ok(ProjectSelection {
            key: key.clone(),
            stamp,
            language,
            packages,
            layout,
        })
    }
    pub(crate) fn protecting_references(&self) -> TypedReferenceSet {
        self.model.protecting_references()
    }
    pub(crate) fn forget_retired(&mut self, allocation: &str) -> Result<()> {
        self.model.forget_retired(allocation)
    }
    pub(crate) fn encode(&self, limits: CatalogCodecLimits) -> Result<Vec<u8>> {
        self.model.encode_catalog(limits)
    }
    pub(crate) fn decode(bytes: &[u8], limits: CatalogCodecLimits) -> Result<Self> {
        Ok(Self {
            model: CatalogModel::decode_catalog(bytes, limits)?,
            limits: limits.model,
        })
    }
}

#[cfg(test)]
mod tests;
