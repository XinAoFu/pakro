//! Owned explicit resolution facts, not proof of a runtime or configuration.

use super::LockedGraphError;
use crate::packages::python_resolution::metadata_input::{
    MetadataInputError, checked_metadata_context,
};
use pakro_index::SupportedWheelTags;
use pakro_index::python_markers::{
    MarkerSemantics, PythonMarkerEnvironment, PythonMarkerLimits, PythonMarkerScope,
};
use pakro_index::python_source::NormalizedPythonName;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy)]
pub(crate) struct FrozenTargetLimits {
    pub max_supported_tags: usize,
    pub max_field_bytes: usize,
    pub max_total_utf8_bytes: usize,
    pub markers: PythonMarkerLimits,
}
impl Default for FrozenTargetLimits {
    fn default() -> Self {
        Self {
            max_supported_tags: 65_536,
            max_field_bytes: 16 * 1024,
            max_total_utf8_bytes: 64 * 1024 * 1024,
            markers: PythonMarkerLimits::default(),
        }
    }
}
impl FrozenTargetLimits {
    pub(crate) fn validate(self) -> Result<(), LockedGraphError> {
        if [
            self.max_supported_tags,
            self.max_field_bytes,
            self.max_total_utf8_bytes,
        ]
        .contains(&0)
            || self.markers.validate().is_err()
        {
            return Err(LockedGraphError::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
enum FrozenScope {
    Environment,
    Dependency(Vec<String>),
    Lock {
        extras: Vec<String>,
        groups: Vec<String>,
    },
}

#[derive(Clone)]
pub(crate) struct FrozenRegistryTarget {
    environment: [String; 11],
    supported: SupportedWheelTags,
    root_scope: FrozenScope,
    semantics: MarkerSemantics,
    fingerprint: [u8; 32],
}
impl FrozenRegistryTarget {
    pub(crate) fn new(
        environment: PythonMarkerEnvironment<'_>,
        supported: &SupportedWheelTags,
        root_scope: PythonMarkerScope<'_>,
        semantics: MarkerSemantics,
        limits: FrozenTargetLimits,
    ) -> Result<Self, LockedGraphError> {
        let fields = [
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
        ];
        let (extras, groups): (&[&str], &[&str]) = match root_scope {
            PythonMarkerScope::Environment => (&[], &[]),
            PythonMarkerScope::Dependency { requested_extras } => (requested_extras, &[]),
            PythonMarkerScope::Lock {
                selected_extras,
                dependency_groups,
            } => (selected_extras, dependency_groups),
        };
        check_input_limits(
            fields,
            supported,
            extras.iter().copied(),
            groups.iter().copied(),
            limits,
        )?;
        // Reuse the full existing consistency checker, with no host defaults
        // or new claim about the provenance of these explicit observations.
        checked_metadata_context(environment, limits.markers).map_err(|error| match error {
            MetadataInputError::InvalidLimits => LockedGraphError::InvalidLimits,
            MetadataInputError::Limit => LockedGraphError::Limit,
            _ => LockedGraphError::InvalidTarget,
        })?;
        let root_scope = match root_scope {
            PythonMarkerScope::Environment => FrozenScope::Environment,
            PythonMarkerScope::Dependency { requested_extras } => {
                FrozenScope::Dependency(normalized_set(requested_extras)?)
            }
            PythonMarkerScope::Lock {
                selected_extras,
                dependency_groups,
            } => FrozenScope::Lock {
                extras: normalized_set(selected_extras)?,
                groups: normalized_set(dependency_groups)?,
            },
        };
        let mut hasher = Sha256::new();
        hasher.update(b"pakro.python-registry-target/v1\0");
        match semantics {
            MarkerSemantics::Current2026 => hash_text(&mut hasher, "current2026")?,
        }
        for field in fields {
            hash_text(&mut hasher, field)?;
        }
        hash_length(&mut hasher, supported.ordered().len())?;
        for tag in supported.ordered() {
            for atom in [tag.interpreter(), tag.abi(), tag.platform()] {
                hash_text(&mut hasher, atom)?;
            }
        }
        match &root_scope {
            FrozenScope::Environment => hasher.update([0]),
            FrozenScope::Dependency(extras) => {
                hasher.update([1]);
                hash_set(&mut hasher, extras)?;
            }
            FrozenScope::Lock { extras, groups } => {
                hasher.update([2]);
                hash_set(&mut hasher, extras)?;
                hash_set(&mut hasher, groups)?;
            }
        }
        Ok(Self {
            environment: fields.map(str::to_owned),
            supported: supported.clone(),
            root_scope,
            semantics,
            fingerprint: hasher.finalize().into(),
        })
    }
    pub(crate) fn fingerprint(&self) -> &[u8; 32] {
        &self.fingerprint
    }
    /// Pure data equality, never proof of an observed runtime or configuration.
    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.fingerprint == other.fingerprint
            && self.environment == other.environment
            && self.supported.ordered() == other.supported.ordered()
            && self.root_scope == other.root_scope
            && self.semantics == other.semantics
    }
    pub(crate) fn check_limits(&self, limits: FrozenTargetLimits) -> Result<(), LockedGraphError> {
        let fields = self.environment.each_ref().map(String::as_str);
        match &self.root_scope {
            FrozenScope::Environment => check_input_limits(
                fields,
                &self.supported,
                std::iter::empty(),
                std::iter::empty(),
                limits,
            ),
            FrozenScope::Dependency(extras) => check_input_limits(
                fields,
                &self.supported,
                extras.iter().map(String::as_str),
                std::iter::empty(),
                limits,
            ),
            FrozenScope::Lock { extras, groups } => check_input_limits(
                fields,
                &self.supported,
                extras.iter().map(String::as_str),
                groups.iter().map(String::as_str),
                limits,
            ),
        }
    }
    pub(crate) fn environment(&self) -> PythonMarkerEnvironment<'_> {
        PythonMarkerEnvironment {
            os_name: &self.environment[0],
            sys_platform: &self.environment[1],
            platform_release: &self.environment[2],
            platform_system: &self.environment[3],
            platform_version: &self.environment[4],
            platform_machine: &self.environment[5],
            platform_python_implementation: &self.environment[6],
            python_version: &self.environment[7],
            python_full_version: &self.environment[8],
            implementation_name: &self.environment[9],
            implementation_version: &self.environment[10],
        }
    }
    pub(crate) fn supported(&self) -> &SupportedWheelTags {
        &self.supported
    }
    pub(crate) fn semantics(&self) -> MarkerSemantics {
        self.semantics
    }
    pub(crate) fn with_root_scope<T>(
        &self,
        use_scope: impl FnOnce(PythonMarkerScope<'_>) -> T,
    ) -> T {
        match &self.root_scope {
            FrozenScope::Environment => use_scope(PythonMarkerScope::Environment),
            FrozenScope::Dependency(extras) => {
                let extras = extras.iter().map(String::as_str).collect::<Vec<_>>();
                use_scope(PythonMarkerScope::Dependency {
                    requested_extras: &extras,
                })
            }
            FrozenScope::Lock { extras, groups } => {
                let extras = extras.iter().map(String::as_str).collect::<Vec<_>>();
                let groups = groups.iter().map(String::as_str).collect::<Vec<_>>();
                use_scope(PythonMarkerScope::Lock {
                    selected_extras: &extras,
                    dependency_groups: &groups,
                })
            }
        }
    }
}
fn check_input_limits<'a>(
    fields: [&'a str; 11],
    supported: &SupportedWheelTags,
    extras: impl ExactSizeIterator<Item = &'a str>,
    groups: impl ExactSizeIterator<Item = &'a str>,
    limits: FrozenTargetLimits,
) -> Result<(), LockedGraphError> {
    limits.validate()?;
    if supported.ordered().len() > limits.max_supported_tags {
        return Err(LockedGraphError::Limit);
    }
    extras
        .len()
        .checked_add(groups.len())
        .filter(|count| *count <= limits.markers.max_set_entries)
        .ok_or(LockedGraphError::Limit)?;
    let mut input_bytes = 0usize;
    let mut marker_bytes = 0usize;
    // Input iterators expose their counts before any collection. The owned
    // recheck walks canonical retained strings; construction walks raw aliases.
    for value in fields.into_iter().chain(extras).chain(groups) {
        charge_input(&mut input_bytes, value, limits)?;
        if value.len() > limits.markers.max_environment_field_bytes {
            return Err(LockedGraphError::Limit);
        }
        charge(
            &mut marker_bytes,
            value.len(),
            limits.markers.max_environment_bytes,
        )?;
    }
    for tag in supported.ordered() {
        for atom in [tag.interpreter(), tag.abi(), tag.platform()] {
            charge_input(&mut input_bytes, atom, limits)?;
        }
    }
    Ok(())
}
fn charge(used: &mut usize, amount: usize, maximum: usize) -> Result<(), LockedGraphError> {
    *used = used
        .checked_add(amount)
        .filter(|sum| *sum <= maximum)
        .ok_or(LockedGraphError::Limit)?;
    Ok(())
}
fn charge_input(
    used: &mut usize,
    value: &str,
    limits: FrozenTargetLimits,
) -> Result<(), LockedGraphError> {
    if value.len() > limits.max_field_bytes {
        return Err(LockedGraphError::Limit);
    }
    charge(used, value.len(), limits.max_total_utf8_bytes)
}
fn normalized_set(values: &[&str]) -> Result<Vec<String>, LockedGraphError> {
    let mut canonical = BTreeSet::new();
    for value in values {
        canonical
            .insert(NormalizedPythonName::new(value).map_err(|_| LockedGraphError::InvalidTarget)?);
    }
    Ok(canonical
        .into_iter()
        .map(|name| name.as_str().to_owned())
        .collect())
}
fn hash_length(hasher: &mut Sha256, length: usize) -> Result<(), LockedGraphError> {
    hasher.update(
        u64::try_from(length)
            .map_err(|_| LockedGraphError::Limit)?
            .to_be_bytes(),
    );
    Ok(())
}
fn hash_text(hasher: &mut Sha256, text: &str) -> Result<(), LockedGraphError> {
    hash_length(hasher, text.len())?;
    hasher.update(text.as_bytes());
    Ok(())
}
fn hash_set(hasher: &mut Sha256, values: &[String]) -> Result<(), LockedGraphError> {
    hash_length(hasher, values.len())?;
    for value in values {
        hash_text(hasher, value)?;
    }
    Ok(())
}
impl std::fmt::Debug for FrozenRegistryTarget {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("FrozenRegistryTarget")
            .field("supported_tags", &self.supported.ordered().len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "target/tests.rs"]
mod tests;
