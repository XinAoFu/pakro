//! Pure projection from selected-process observations to ordered wheel tags.
//! No compilation-host, filesystem, or runtime-version-label detection belongs here.

use std::collections::BTreeSet;

use eyre::{Result, ensure};
use pakro_index::{
    PythonLinuxArch as LinuxArch, PythonMacArch as MacArch,
    PythonManylinuxOverride as ManylinuxOverride, PythonManylinuxPolicy, PythonTagInterpreter,
    PythonTagLibc, PythonTagPlatform, PythonTagTarget, SupportedWheelTags,
    pure_python_supported_tags, python_supported_tags,
};
use pakro_store::{Arch, Libc, Os, Platform};

use crate::python_runtime::{
    PythonElfObservation, PythonImplementation, PythonLibcObservation, PythonManylinuxObservation,
    PythonPlatformObservation, PythonRuntimeDescriptor,
};

pub(crate) struct PythonWheelTarget {
    pub supported: SupportedWheelTags,
    pub observed_platform: Option<Platform>,
    pub capability_reason: Option<String>,
}

fn pure_only(descriptor: &PythonRuntimeDescriptor, reason: &str) -> Result<PythonWheelTarget> {
    Ok(PythonWheelTarget {
        supported: pure_python_supported_tags(
            descriptor.language_version.major,
            descriptor.language_version.minor,
        )?,
        observed_platform: None,
        capability_reason: Some(format!("generic pure-wheel query only: {reason}")),
    })
}

fn linux_arch(elf: &PythonElfObservation, machine: &str) -> Option<LinuxArch> {
    match (elf.class, elf.little_endian, elf.machine) {
        (1, true, 3) => Some(LinuxArch::I686),
        (2, true, 62) => Some(LinuxArch::X86_64),
        (2, true, 183) => Some(LinuxArch::Aarch64),
        (1, true, 40) if matches!(machine, "armv8l" | "aarch64") => Some(LinuxArch::Armv8l),
        (1, true, 40) if machine == "armv7l" => Some(LinuxArch::Armv7l),
        (2, false, 21) => Some(LinuxArch::Ppc64),
        (2, true, 21) => Some(LinuxArch::Ppc64le),
        (2, false, 22) => Some(LinuxArch::S390x),
        (2, true, 258) => Some(LinuxArch::Loongarch64),
        (2, true, 243) => Some(LinuxArch::Riscv64),
        _ => None,
    }
}

fn linux_arch_name(arch: LinuxArch) -> &'static str {
    match arch {
        LinuxArch::X86_64 => "x86_64",
        LinuxArch::I686 => "i686",
        LinuxArch::Aarch64 => "aarch64",
        LinuxArch::Armv8l => "armv8l",
        LinuxArch::Armv7l => "armv7l",
        LinuxArch::Ppc64 => "ppc64",
        LinuxArch::Ppc64le => "ppc64le",
        LinuxArch::S390x => "s390x",
        LinuxArch::Loongarch64 => "loongarch64",
        LinuxArch::Riscv64 => "riscv64",
    }
}

fn policy_for_glibc(
    arch: LinuxArch,
    minor: u32,
    elf: &PythonElfObservation,
    observation: &PythonManylinuxObservation,
) -> Result<Option<PythonManylinuxPolicy>> {
    ensure!(
        minor <= 4096,
        "observed glibc version exceeds compatibility resource limits"
    );
    let abi_compatible = match arch {
        LinuxArch::I686 => elf.class == 1 && elf.little_endian && elf.machine == 3,
        LinuxArch::Armv7l | LinuxArch::Armv8l => {
            elf.flags & 0xff00_0000 == 0x0500_0000 && elf.flags & 0x400 != 0
        }
        _ => true,
    };
    let overrides = match observation {
        PythonManylinuxObservation::Default => Vec::new(),
        PythonManylinuxObservation::Unknown => return Ok(None),
        PythonManylinuxObservation::Overrides { values } => {
            let minimum = if matches!(arch, LinuxArch::X86_64 | LinuxArch::I686) {
                5
            } else {
                17
            };
            let arches = if arch == LinuxArch::Armv8l {
                vec![LinuxArch::Armv8l, LinuxArch::Armv7l]
            } else {
                vec![arch]
            };
            let mut covered = BTreeSet::new();
            let mut result = Vec::new();
            for value in values {
                let target_arch = arches
                    .iter()
                    .copied()
                    .find(|candidate| linux_arch_name(*candidate) == value.arch)
                    .ok_or_else(|| {
                        eyre::eyre!("manylinux policy describes another execution architecture")
                    })?;
                ensure!(
                    value.major == 2 && (minimum..=minor).contains(&value.minor),
                    "manylinux policy floor is outside observed libc capability"
                );
                ensure!(
                    covered.insert((target_arch, value.minor)),
                    "duplicate manylinux policy floor"
                );
                result.push(ManylinuxOverride {
                    arch: target_arch,
                    major: value.major,
                    minor: value.minor,
                    compatible: value.compatible,
                });
            }
            for target_arch in arches {
                for floor in minimum..=minor {
                    if !covered.contains(&(target_arch, floor)) {
                        return Ok(None);
                    }
                }
            }
            result
        }
    };
    Ok(Some(PythonManylinuxPolicy {
        abi_compatible,
        overrides,
    }))
}

pub(crate) fn wheel_target(descriptor: &PythonRuntimeDescriptor) -> Result<PythonWheelTarget> {
    descriptor.validate_compatibility()?;
    ensure!(
        descriptor.language_version.major == 3,
        "Python package compatibility requires Python 3"
    );
    let Some(observed) = &descriptor.compatibility else {
        return pure_only(
            descriptor,
            "the selected runtime did not report compatibility observations",
        );
    };
    let interpreter = match descriptor.implementation {
        PythonImplementation::CPython => {
            let (Some(debug), Some(gil_disabled)) = (observed.debug, descriptor.gil_disabled)
            else {
                return pure_only(
                    descriptor,
                    "CPython debug/GIL ABI observations are incomplete",
                );
            };
            if descriptor.language_version.minor < 8 && observed.pymalloc.is_none() {
                return pure_only(
                    descriptor,
                    "legacy CPython pymalloc ABI observation is missing",
                );
            }
            if descriptor.language_version.minor < 3 && observed.unicode_size.is_none() {
                return pure_only(
                    descriptor,
                    "legacy CPython Unicode ABI observation is missing",
                );
            }
            PythonTagInterpreter::CPython {
                debug,
                gil_disabled,
                pymalloc: observed.pymalloc,
                unicode_size: observed.unicode_size,
            }
        }
        PythonImplementation::PyPy => {
            let (Some(soabi), Some(ext_suffix)) = (&descriptor.soabi, &descriptor.ext_suffix)
            else {
                return pure_only(
                    descriptor,
                    "PyPy SOABI/extension suffix observations are incomplete",
                );
            };
            PythonTagInterpreter::PyPy {
                soabi: soabi.clone(),
                ext_suffix: ext_suffix.clone(),
            }
        }
    };
    let (platform, coarse) = match &observed.platform {
        PythonPlatformObservation::Macos {
            cpu_type,
            product_version,
        } => {
            let (Some(cpu_type), Some(version)) = (cpu_type, product_version) else {
                return pure_only(
                    descriptor,
                    "loaded Mach-O CPU or macOS ProductVersion is unknown",
                );
            };
            let (arch, coarse) = match *cpu_type {
                16777223 => (MacArch::X86_64, Some(Arch::X86_64)),
                16777228 => (MacArch::Arm64, Some(Arch::Aarch64)),
                7 => (MacArch::I386, None),
                18 => (MacArch::Ppc, None),
                16777234 => (MacArch::Ppc64, None),
                _ => {
                    return pure_only(
                        descriptor,
                        "loaded Mach-O execution architecture is unsupported",
                    );
                }
            };
            (
                PythonTagPlatform::Macos {
                    product_major: version.major,
                    product_minor: version.minor,
                    arch,
                },
                coarse.map(|arch| Platform {
                    os: Os::Macos,
                    arch,
                    libc: Libc::Other,
                }),
            )
        }
        PythonPlatformObservation::Linux {
            elf,
            libc,
            manylinux,
        } => {
            let Some(elf) = elf else {
                return pure_only(descriptor, "the selected process ELF header is unknown");
            };
            let Some(arch) = linux_arch(elf, &descriptor.markers.platform_machine) else {
                return pure_only(
                    descriptor,
                    "the selected process ELF architecture/ABI is unsupported",
                );
            };
            let (libc, coarse_libc, policy) = match libc {
                PythonLibcObservation::Unknown => {
                    return pure_only(descriptor, "runtime libc is unknown");
                }
                PythonLibcObservation::Glibc { major, minor } => {
                    ensure!(*major == 2, "only observed glibc major 2 is supported");
                    let Some(policy) = policy_for_glibc(arch, *minor, elf, manylinux)? else {
                        return pure_only(
                            descriptor,
                            "manylinux policy is unknown or does not cover every target floor",
                        );
                    };
                    (
                        PythonTagLibc::Glibc {
                            major: *major,
                            minor: *minor,
                        },
                        Libc::Gnu,
                        policy,
                    )
                }
                PythonLibcObservation::Musl { version, .. } => {
                    let Some(version) = version else {
                        return pure_only(descriptor, "musl loader version is unknown");
                    };
                    (
                        PythonTagLibc::Musl {
                            major: version.major,
                            minor: version.minor,
                        },
                        Libc::Musl,
                        PythonManylinuxPolicy {
                            abi_compatible: false,
                            overrides: Vec::new(),
                        },
                    )
                }
            };
            let coarse_arch = match arch {
                LinuxArch::X86_64 => Some(Arch::X86_64),
                LinuxArch::Aarch64 => Some(Arch::Aarch64),
                _ => None,
            };
            (
                PythonTagPlatform::Linux {
                    arch,
                    libc,
                    manylinux: policy,
                },
                coarse_arch.map(|arch| Platform {
                    os: Os::Linux,
                    arch,
                    libc: coarse_libc,
                }),
            )
        }
    };
    let supported = python_supported_tags(&PythonTagTarget {
        language_major: descriptor.language_version.major,
        language_minor: descriptor.language_version.minor,
        interpreter,
        platform,
    })?;
    Ok(PythonWheelTarget{
        supported,observed_platform:coarse,
        capability_reason:coarse.is_none().then(||format!("{}-bit/execution architecture is queryable but cannot be materialized by the current Store layout",observed.pointer_bits)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pakro_index::parse_wheel_filename;
    use pakro_store::{Arch, Libc, Os};
    use serde_json::{Value, json};

    fn descriptor() -> Value {
        json!({
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
        })
    }

    fn target(value: &Value) -> Result<PythonWheelTarget> {
        wheel_target(&serde_json::from_value(value.clone())?)
    }

    fn compatible(target: &PythonWheelTarget, filename: &str) -> bool {
        target
            .supported
            .best_rank(&parse_wheel_filename(filename).unwrap().tags)
            .unwrap()
            .is_some()
    }

    #[test]
    fn observed_linux_target_does_not_use_compiled_host() {
        let value = target(&descriptor()).unwrap();
        assert_eq!(
            value.observed_platform,
            Some(Platform {
                os: Os::Linux,
                arch: Arch::Aarch64,
                libc: Libc::Gnu
            })
        );
        assert!(compatible(&value, "demo-1.0-cp310-cp310-linux_aarch64.whl"));
        assert!(compatible(
            &value,
            "demo-1.0-cp310-cp310-manylinux_2_31_aarch64.whl"
        ));
        assert!(!compatible(
            &value,
            "demo-1.0-cp310-cp310-manylinux_2_32_aarch64.whl"
        ));
        assert!(!compatible(&value, "demo-1.0-cp310-cp310-linux_x86_64.whl"));
    }

    #[test]
    fn missing_observations_are_explicitly_generic_pure_only() {
        let mut value = descriptor();
        value.as_object_mut().unwrap().remove("compatibility");
        let value = target(&value).unwrap();
        assert!(value.observed_platform.is_none());
        assert!(value.capability_reason.as_deref().unwrap().contains("pure"));
        assert!(compatible(&value, "demo-1.0-py3-none-any.whl"));
        assert!(!compatible(&value, "demo-1.0-cp310-none-any.whl"));
        assert!(!compatible(
            &value,
            "demo-1.0-cp310-cp310-linux_aarch64.whl"
        ));
    }

    #[test]
    fn unknown_abi_or_manylinux_never_becomes_default_native() {
        for path in ["debug", "gil_disabled", "libc", "manylinux"] {
            let mut value = descriptor();
            match path {
                "debug" => value["compatibility"]["debug"] = Value::Null,
                "gil_disabled" => value["gil_disabled"] = Value::Null,
                "libc" => value["compatibility"]["platform"]["libc"] = json!({"family":"unknown"}),
                _ => value["compatibility"]["platform"]["manylinux"] = json!({"state":"unknown"}),
            }
            let value = target(&value).unwrap();
            assert!(value.observed_platform.is_none(), "{path}");
            assert!(value.capability_reason.is_some(), "{path}");
            assert!(
                !compatible(&value, "demo-1.0-cp310-cp310-linux_aarch64.whl"),
                "{path}"
            );
        }
    }

    #[test]
    fn elf_32bit_under_64bit_uname_has_no_64bit_or_fake_store_identity() {
        let mut value = descriptor();
        value["markers"]["platform_machine"] = json!("x86_64");
        value["compatibility"]["pointer_bits"] = json!(32);
        value["compatibility"]["platform"]["elf"]["class"] = json!(1);
        value["compatibility"]["platform"]["elf"]["machine"] = json!(3);
        let value = target(&value).unwrap();
        assert!(value.observed_platform.is_none());
        assert!(compatible(&value, "demo-1.0-cp310-cp310-linux_i686.whl"));
        assert!(!compatible(&value, "demo-1.0-cp310-cp310-linux_x86_64.whl"));
        assert!(value.capability_reason.as_deref().unwrap().contains("32"));
    }

    #[test]
    fn arm32_process_under_aarch64_uname_keeps_both_native_query_arches() {
        let mut value = descriptor();
        value["compatibility"]["pointer_bits"] = json!(32);
        value["compatibility"]["platform"]["elf"] = json!({
            "class":1,"little_endian":true,"machine":40,"flags":0x05000400,
            "interpreter":"/lib/ld-linux-armhf.so.3"
        });
        value["soabi"] = json!("cpython-310-arm-linux-gnueabihf");
        value["ext_suffix"] = json!(".cpython-310-arm-linux-gnueabihf.so");
        let value = target(&value).unwrap();
        assert!(value.observed_platform.is_none());
        for arch in ["armv8l", "armv7l"] {
            assert!(compatible(
                &value,
                &format!("demo-1.0-cp310-cp310-linux_{arch}.whl")
            ));
            assert!(compatible(
                &value,
                &format!("demo-1.0-cp310-cp310-manylinux_2_17_{arch}.whl")
            ));
        }
        assert!(!compatible(
            &value,
            "demo-1.0-cp310-cp310-linux_aarch64.whl"
        ));
    }

    #[test]
    fn macos_uses_loaded_cpu_and_product_version() {
        let mut value = descriptor();
        value["markers"]["platform_system"] = json!("Darwin");
        value["markers"]["sys_platform"] = json!("darwin");
        value["markers"]["platform_machine"] = json!("x86_64");
        value["compatibility"]["platform"] = json!({"os":"macos","cpu_type":16777228,
            "product_version":{"major":14,"minor":2,"micro":1}});
        let value = target(&value).unwrap();
        assert_eq!(
            value.observed_platform,
            Some(Platform {
                os: Os::Macos,
                arch: Arch::Aarch64,
                libc: Libc::Other
            })
        );
        assert!(compatible(
            &value,
            "demo-1.0-cp310-cp310-macosx_14_0_arm64.whl"
        ));
        assert!(!compatible(
            &value,
            "demo-1.0-cp310-cp310-macosx_14_0_x86_64.whl"
        ));
        assert!(compatible(
            &value,
            "demo-1.0-cp310-cp310-macosx_10_9_universal2.whl"
        ));
    }

    #[test]
    fn manylinux_callback_must_cover_every_floor_and_its_own_arch() {
        let mut value = descriptor();
        value["compatibility"]["platform"]["manylinux"] = json!({"state":"overrides","values":[
            {"arch":"aarch64","major":2,"minor":31,"compatible":true}
        ]});
        let partial = target(&value).unwrap();
        assert!(partial.observed_platform.is_none());
        assert!(partial.capability_reason.is_some());
        let overrides: Vec<_> = (17..=31)
            .map(|minor| json!({"arch":"aarch64","major":2,"minor":minor,"compatible":minor!=17}))
            .collect();
        value["compatibility"]["platform"]["manylinux"]["values"] = json!(overrides);
        let complete = target(&value).unwrap();
        assert!(complete.observed_platform.is_some());
        assert!(!compatible(
            &complete,
            "demo-1.0-cp310-cp310-manylinux2014_aarch64.whl"
        ));
        assert!(compatible(
            &complete,
            "demo-1.0-cp310-cp310-manylinux_2_18_aarch64.whl"
        ));
    }

    #[test]
    fn malformed_or_mutated_observations_are_errors_not_pure_fallback() {
        let mut value = descriptor();
        value["compatibility"]["pointer_bits"] = json!(16);
        assert!(target(&value).is_err());
        let mut value = descriptor();
        value["compatibility"]["platform"]["libc"] = json!({"family":"glibc","major":3,"minor":1});
        assert!(target(&value).is_err());
    }

    #[test]
    fn pypy_uses_observed_soabi_and_extension_suffix_not_implementation_version() {
        let mut value = descriptor();
        value["implementation"] = json!("pypy");
        value["implementation_version"] =
            json!({"major":7,"minor":3,"micro":17,"release_level":"final","serial":0});
        value["implementation_version_source"] = json!("pypy_version_info");
        value["markers"]["implementation_version"] = json!("7.3.17");
        value["markers"]["implementation_name"] = json!("pypy");
        value["markers"]["platform_python_implementation"] = json!("PyPy");
        value["soabi"] = json!("pypy310-pp73-aarch64-linux-gnu");
        value["ext_suffix"] = json!(".pypy310-pp73-aarch64-linux-gnu.so");
        let pypy = target(&value).unwrap();
        assert!(compatible(
            &pypy,
            "demo-1.0-pp310-pypy310_pp73-linux_aarch64.whl"
        ));
        assert!(!compatible(&pypy, "demo-1.0-cp310-cp310-linux_aarch64.whl"));
        value["ext_suffix"] = json!(".pypy39-pp73-aarch64-linux-gnu.so");
        assert!(target(&value).is_err());
    }
}
