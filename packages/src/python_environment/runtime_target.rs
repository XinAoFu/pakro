//! Pure projection of observed runtime facts into frozen target data.
//! This module holds no runtime ownership and grants no installation authority.

use pakro_index::python_markers::{MarkerSemantics, PythonMarkerEnvironment, PythonMarkerScope};
use thiserror::Error;

use crate::packages::python_resolution::source::locked_graph::{
    FrozenRegistryTarget, FrozenTargetLimits,
};
use crate::python_runtime::PythonRuntimeDescriptor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum RuntimeTargetError {
    #[error("selected runtime has incomplete native target observations")]
    IncompleteTarget,
    #[error("observed registry target could not be frozen")]
    FrozenTarget,
}

/// This projects already observed data only. Constructing a descriptor, or
/// freezing one, never grants ownership of a runtime or of its observations.
pub(crate) fn freeze_observed_target(
    descriptor: &PythonRuntimeDescriptor,
    root_scope: PythonMarkerScope<'_>,
    semantics: MarkerSemantics,
    limits: FrozenTargetLimits,
) -> Result<FrozenRegistryTarget, RuntimeTargetError> {
    let projected = crate::packages::python_target::wheel_target(descriptor)
        .map_err(|_| RuntimeTargetError::IncompleteTarget)?;
    if projected.observed_platform.is_none()
        || projected.capability_reason.is_some()
        || !projected
            .supported
            .ordered()
            .iter()
            .any(|tag| tag.platform() != "any")
    {
        return Err(RuntimeTargetError::IncompleteTarget);
    }
    let markers = &descriptor.markers;
    FrozenRegistryTarget::new(
        PythonMarkerEnvironment {
            os_name: &markers.os_name,
            sys_platform: &markers.sys_platform,
            platform_release: &markers.platform_release,
            platform_system: &markers.platform_system,
            platform_version: &markers.platform_version,
            platform_machine: &markers.platform_machine,
            platform_python_implementation: &markers.platform_python_implementation,
            python_version: &markers.python_version,
            python_full_version: &markers.python_full_version,
            implementation_name: &markers.implementation_name,
            implementation_version: &markers.implementation_version,
        },
        &projected.supported,
        root_scope,
        semantics,
        limits,
    )
    .map_err(|_| RuntimeTargetError::FrozenTarget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn descriptor() -> PythonRuntimeDescriptor {
        serde_json::from_value(json!({
            "schema":1,"observation_mode":"base_runtime_site_disabled",
            "probe_flags":["-E","-s","-S","-B","-c"],"executable":"/fixture/bin/python",
            "implementation":"cpython",
            "language_version":{"major":3,"minor":10,"micro":14,"release_level":"final","serial":0},
            "implementation_version":{"major":3,"minor":10,"micro":14,"release_level":"final","serial":0},
            "implementation_version_source":"sys_version_info",
            "markers":{"implementation_name":"cpython","implementation_version":"3.10.14",
                "os_name":"posix","platform_machine":"aarch64","platform_python_implementation":"CPython",
                "platform_release":"6.1.0","platform_system":"Linux","platform_version":"fixture",
                "python_full_version":"3.10.14","python_version":"3.10","sys_platform":"linux"},
            "soabi":"cpython-310-aarch64-linux-gnu","ext_suffix":".cpython-310-aarch64-linux-gnu.so",
            "abiflags":"","extension_suffixes":[".cpython-310-aarch64-linux-gnu.so",".abi3.so",".so"],
            "gil_disabled":false,"sysconfig_paths":{"stdlib":"/fixture/lib/python3.10"},
            "compatibility":{"pointer_bits":64,"debug":false,"pymalloc":true,"unicode_size":null,
                "platform":{"os":"linux","elf":{"class":2,"little_endian":true,"machine":183,"flags":0,
                    "interpreter":"/lib/ld-linux-aarch64.so.1"},"libc":{"family":"glibc","major":2,"minor":31},
                    "manylinux":{"state":"default"}}}
        }))
        .unwrap()
    }

    #[test]
    fn valid_descriptor_projects_markers_platform_and_normalized_extras() {
        let descriptor = descriptor();
        let frozen = freeze_observed_target(
            &descriptor,
            PythonMarkerScope::Dependency {
                requested_extras: &["Safe_Scope", "safe-scope"],
            },
            MarkerSemantics::Current2026,
            FrozenTargetLimits::default(),
        )
        .unwrap();
        let actual = frozen.environment();
        let markers = &descriptor.markers;
        assert_eq!(
            [
                actual.os_name,
                actual.sys_platform,
                actual.platform_release,
                actual.platform_system,
                actual.platform_version,
                actual.platform_machine,
                actual.platform_python_implementation,
                actual.python_version,
                actual.python_full_version,
                actual.implementation_name,
                actual.implementation_version,
            ],
            [
                markers.os_name.as_str(),
                markers.sys_platform.as_str(),
                markers.platform_release.as_str(),
                markers.platform_system.as_str(),
                markers.platform_version.as_str(),
                markers.platform_machine.as_str(),
                markers.platform_python_implementation.as_str(),
                markers.python_version.as_str(),
                markers.python_full_version.as_str(),
                markers.implementation_name.as_str(),
                markers.implementation_version.as_str(),
            ]
        );
        assert!(
            frozen
                .supported()
                .ordered()
                .iter()
                .any(|tag| tag.platform() != "any")
        );
        frozen.with_root_scope(|scope| {
            let PythonMarkerScope::Dependency { requested_extras } = scope else {
                panic!("root scope changed")
            };
            assert_eq!(requested_extras, ["safe-scope"]);
        });
    }

    #[test]
    fn missing_compatibility_is_rejected_as_incomplete_target() {
        let mut unknown = descriptor();
        unknown.compatibility = None;
        assert_eq!(
            freeze_observed_target(
                &unknown,
                PythonMarkerScope::Environment,
                MarkerSemantics::Current2026,
                FrozenTargetLimits::default(),
            )
            .unwrap_err(),
            RuntimeTargetError::IncompleteTarget
        );
    }
}
