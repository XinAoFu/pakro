// Fixed packaging 26.3/PEP fixtures; expected order is independent of the generator.

use pakro_index::{
    IndexError, PythonLinuxArch as L, PythonMacArch as M, PythonManylinuxOverride,
    PythonManylinuxPolicy, PythonTagLibc, PythonTagPlatform, python_platform_tags,
};

fn linux(arch: L, libc: PythonTagLibc) -> PythonTagPlatform {
    PythonTagPlatform::Linux {
        arch,
        manylinux: PythonManylinuxPolicy {
            abi_compatible: matches!(libc, PythonTagLibc::Glibc { .. }),
            overrides: Vec::new(),
        },
        libc,
    }
}

fn mac(major: u32, minor: u32, arch: M) -> PythonTagPlatform {
    PythonTagPlatform::Macos {
        product_major: major,
        product_minor: minor,
        arch,
    }
}

fn lines(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_owned).collect()
}

fn assert_invalid(platform: PythonTagPlatform) {
    assert!(matches!(
        python_platform_tags(&platform),
        Err(IndexError::InvalidPythonTagTarget(_))
    ));
}

#[test]
fn glibc_x86_floor_and_legacy_order_golden() {
    assert_eq!(
        python_platform_tags(&linux(
            L::X86_64,
            PythonTagLibc::Glibc {
                major: 2,
                minor: 17
            }
        ))
        .unwrap(),
        lines(
            "linux_x86_64 manylinux_2_17_x86_64 manylinux2014_x86_64
         manylinux_2_16_x86_64 manylinux_2_15_x86_64 manylinux_2_14_x86_64
         manylinux_2_13_x86_64 manylinux_2_12_x86_64 manylinux2010_x86_64
         manylinux_2_11_x86_64 manylinux_2_10_x86_64 manylinux_2_9_x86_64
         manylinux_2_8_x86_64 manylinux_2_7_x86_64 manylinux_2_6_x86_64
         manylinux_2_5_x86_64 manylinux1_x86_64"
        )
    );
}

#[test]
fn glibc_aarch64_floor_golden() {
    assert_eq!(
        python_platform_tags(&linux(
            L::Aarch64,
            PythonTagLibc::Glibc {
                major: 2,
                minor: 18
            }
        ))
        .unwrap(),
        lines("linux_aarch64 manylinux_2_18_aarch64 manylinux_2_17_aarch64 manylinux2014_aarch64")
    );
}

#[test]
fn manylinux_callback_veto_removes_floor_and_alias_only() {
    let mut target = linux(L::X86_64, PythonTagLibc::Glibc { major: 2, minor: 6 });
    if let PythonTagPlatform::Linux { manylinux, .. } = &mut target {
        manylinux.overrides.push(PythonManylinuxOverride {
            arch: L::X86_64,
            major: 2,
            minor: 5,
            compatible: false,
        });
    }
    assert_eq!(
        python_platform_tags(&target).unwrap(),
        lines("linux_x86_64 manylinux_2_6_x86_64")
    );
}

#[test]
fn callback_true_and_none_default_are_not_global_vetoes() {
    let mut target = linux(
        L::Aarch64,
        PythonTagLibc::Glibc {
            major: 2,
            minor: 18,
        },
    );
    if let PythonTagPlatform::Linux { manylinux, .. } = &mut target {
        manylinux.overrides.extend([
            PythonManylinuxOverride {
                arch: L::Aarch64,
                major: 2,
                minor: 18,
                compatible: true,
            },
            PythonManylinuxOverride {
                arch: L::Aarch64,
                major: 2,
                minor: 17,
                compatible: false,
            },
        ]);
    }
    assert_eq!(
        python_platform_tags(&target).unwrap(),
        lines("linux_aarch64 manylinux_2_18_aarch64")
    );
}

#[test]
fn incompatible_elf_abi_preserves_only_native() {
    let mut target = linux(
        L::Armv7l,
        PythonTagLibc::Glibc {
            major: 2,
            minor: 17,
        },
    );
    if let PythonTagPlatform::Linux { manylinux, .. } = &mut target {
        manylinux.abi_compatible = false;
    }
    assert_eq!(
        python_platform_tags(&target).unwrap(),
        lines("linux_armv7l")
    );
}

#[test]
fn i686_never_emits_x86_64() {
    assert_eq!(
        python_platform_tags(&linux(L::I686, PythonTagLibc::Glibc { major: 2, minor: 5 })).unwrap(),
        lines("linux_i686 manylinux_2_5_i686 manylinux1_i686")
    );
}

#[test]
fn armv8l_dual_arch_glibc_sequence_golden() {
    assert_eq!(
        python_platform_tags(&linux(
            L::Armv8l,
            PythonTagLibc::Glibc {
                major: 2,
                minor: 17
            }
        ))
        .unwrap(),
        lines(
            "linux_armv8l linux_armv7l manylinux_2_17_armv8l manylinux2014_armv8l
         manylinux_2_17_armv7l manylinux2014_armv7l"
        )
    );
}

#[test]
fn musl_x86_sequence_golden() {
    assert_eq!(
        python_platform_tags(&linux(
            L::X86_64,
            PythonTagLibc::Musl { major: 1, minor: 2 }
        ))
        .unwrap(),
        lines("linux_x86_64 musllinux_1_2_x86_64 musllinux_1_1_x86_64 musllinux_1_0_x86_64")
    );
}

#[test]
fn musl_armv8l_sequence_has_no_64bit_or_glibc() {
    assert_eq!(
        python_platform_tags(&linux(
            L::Armv8l,
            PythonTagLibc::Musl { major: 1, minor: 0 }
        ))
        .unwrap(),
        lines("linux_armv8l linux_armv7l musllinux_1_0_armv8l musllinux_1_0_armv7l")
    );
}

#[test]
fn non_x86_architectures_share_explicit_floor_without_host_detection() {
    for (arch, name) in [
        (L::Ppc64, "ppc64"),
        (L::Ppc64le, "ppc64le"),
        (L::S390x, "s390x"),
        (L::Loongarch64, "loongarch64"),
        (L::Riscv64, "riscv64"),
    ] {
        assert_eq!(
            python_platform_tags(&linux(
                arch,
                PythonTagLibc::Glibc {
                    major: 2,
                    minor: 17
                }
            ))
            .unwrap(),
            vec![
                format!("linux_{name}"),
                format!("manylinux_2_17_{name}"),
                format!("manylinux2014_{name}")
            ]
        );
    }
}

#[test]
fn mac_x86_10_4_binary_formats_golden() {
    assert_eq!(
        python_platform_tags(&mac(10, 4, M::X86_64)).unwrap(),
        lines(
            "macosx_10_4_x86_64 macosx_10_4_intel macosx_10_4_fat64
         macosx_10_4_fat3 macosx_10_4_universal2 macosx_10_4_universal"
        )
    );
}

#[test]
fn mac_i386_and_ppc_historical_binary_formats_golden() {
    assert_eq!(
        python_platform_tags(&mac(10, 4, M::I386)).unwrap(),
        lines(
            "macosx_10_4_i386 macosx_10_4_intel macosx_10_4_fat3 macosx_10_4_fat macosx_10_4_universal"
        )
    );
    assert_eq!(
        python_platform_tags(&mac(10, 1, M::Ppc)).unwrap(),
        lines(
            "macosx_10_1_ppc macosx_10_1_fat3 macosx_10_1_fat macosx_10_1_universal
         macosx_10_0_ppc macosx_10_0_fat3 macosx_10_0_fat macosx_10_0_universal"
        )
    );
    assert_eq!(
        python_platform_tags(&mac(10, 5, M::Ppc64)).unwrap(),
        lines(
            "macosx_10_5_ppc64 macosx_10_5_fat64 macosx_10_5_universal
         macosx_10_4_ppc64 macosx_10_4_fat64 macosx_10_4_universal"
        )
    );
}

#[test]
fn mac_arm_12_rolls_major_and_only_old_universal2_golden() {
    assert_eq!(python_platform_tags(&mac(12, 6, M::Arm64)).unwrap(), lines(
        "macosx_12_0_arm64 macosx_12_0_universal2 macosx_11_0_arm64 macosx_11_0_universal2
         macosx_10_16_universal2 macosx_10_15_universal2 macosx_10_14_universal2
         macosx_10_13_universal2 macosx_10_12_universal2 macosx_10_11_universal2
         macosx_10_10_universal2 macosx_10_9_universal2 macosx_10_8_universal2
         macosx_10_7_universal2 macosx_10_6_universal2 macosx_10_5_universal2 macosx_10_4_universal2"
    ));
}

#[test]
fn mac_x86_11_complete_old_floor_sequence_golden() {
    assert_eq!(python_platform_tags(&mac(11, 9, M::X86_64)).unwrap(), lines(
        "macosx_11_0_x86_64 macosx_11_0_intel macosx_11_0_fat64 macosx_11_0_fat3 macosx_11_0_universal2 macosx_11_0_universal
         macosx_10_16_x86_64 macosx_10_16_intel macosx_10_16_fat64 macosx_10_16_fat3 macosx_10_16_universal2 macosx_10_16_universal
         macosx_10_15_x86_64 macosx_10_15_intel macosx_10_15_fat64 macosx_10_15_fat3 macosx_10_15_universal2 macosx_10_15_universal
         macosx_10_14_x86_64 macosx_10_14_intel macosx_10_14_fat64 macosx_10_14_fat3 macosx_10_14_universal2 macosx_10_14_universal
         macosx_10_13_x86_64 macosx_10_13_intel macosx_10_13_fat64 macosx_10_13_fat3 macosx_10_13_universal2 macosx_10_13_universal
         macosx_10_12_x86_64 macosx_10_12_intel macosx_10_12_fat64 macosx_10_12_fat3 macosx_10_12_universal2 macosx_10_12_universal
         macosx_10_11_x86_64 macosx_10_11_intel macosx_10_11_fat64 macosx_10_11_fat3 macosx_10_11_universal2 macosx_10_11_universal
         macosx_10_10_x86_64 macosx_10_10_intel macosx_10_10_fat64 macosx_10_10_fat3 macosx_10_10_universal2 macosx_10_10_universal
         macosx_10_9_x86_64 macosx_10_9_intel macosx_10_9_fat64 macosx_10_9_fat3 macosx_10_9_universal2 macosx_10_9_universal
         macosx_10_8_x86_64 macosx_10_8_intel macosx_10_8_fat64 macosx_10_8_fat3 macosx_10_8_universal2 macosx_10_8_universal
         macosx_10_7_x86_64 macosx_10_7_intel macosx_10_7_fat64 macosx_10_7_fat3 macosx_10_7_universal2 macosx_10_7_universal
         macosx_10_6_x86_64 macosx_10_6_intel macosx_10_6_fat64 macosx_10_6_fat3 macosx_10_6_universal2 macosx_10_6_universal
         macosx_10_5_x86_64 macosx_10_5_intel macosx_10_5_fat64 macosx_10_5_fat3 macosx_10_5_universal2 macosx_10_5_universal
         macosx_10_4_x86_64 macosx_10_4_intel macosx_10_4_fat64 macosx_10_4_fat3 macosx_10_4_universal2 macosx_10_4_universal"
    ));
}

#[test]
fn unknown_libc_and_future_glibc_major_are_errors() {
    assert_invalid(linux(L::X86_64, PythonTagLibc::Unknown));
    assert_invalid(linux(
        L::X86_64,
        PythonTagLibc::Glibc { major: 3, minor: 0 },
    ));
    assert_invalid(linux(L::X86_64, PythonTagLibc::Musl { major: 0, minor: 1 }));
}

#[test]
fn duplicate_or_out_of_domain_manylinux_override_is_error() {
    for values in [
        vec![
            PythonManylinuxOverride {
                arch: L::X86_64,
                major: 2,
                minor: 5,
                compatible: false
            };
            2
        ],
        vec![PythonManylinuxOverride {
            arch: L::Aarch64,
            major: 2,
            minor: 5,
            compatible: false,
        }],
        vec![PythonManylinuxOverride {
            arch: L::X86_64,
            major: 2,
            minor: 6,
            compatible: false,
        }],
        vec![PythonManylinuxOverride {
            arch: L::X86_64,
            major: 3,
            minor: 5,
            compatible: false,
        }],
    ] {
        let mut target = linux(L::X86_64, PythonTagLibc::Glibc { major: 2, minor: 5 });
        if let PythonTagPlatform::Linux { manylinux, .. } = &mut target {
            manylinux.overrides = values;
        }
        assert_invalid(target);
    }
}

#[test]
fn musl_cannot_carry_glibc_policy() {
    let mut target = linux(L::X86_64, PythonTagLibc::Musl { major: 1, minor: 2 });
    if let PythonTagPlatform::Linux { manylinux, .. } = &mut target {
        manylinux.abi_compatible = true;
    }
    assert_invalid(target);
}

#[test]
fn mac_impossible_or_compat_alias_target_is_error() {
    for value in [
        mac(9, 9, M::X86_64),
        mac(10, 3, M::X86_64),
        mac(10, 16, M::X86_64),
        mac(10, 15, M::I386),
        mac(10, 15, M::Arm64),
        mac(11, 0, M::Ppc),
        mac(10, 6, M::Ppc64),
    ] {
        assert_invalid(value);
    }
}

#[test]
fn below_manylinux_minimum_is_known_native_only() {
    assert_eq!(
        python_platform_tags(&linux(
            L::X86_64,
            PythonTagLibc::Glibc { major: 2, minor: 4 }
        ))
        .unwrap(),
        lines("linux_x86_64")
    );
    assert_eq!(
        python_platform_tags(&linux(
            L::Aarch64,
            PythonTagLibc::Glibc {
                major: 2,
                minor: 16
            }
        ))
        .unwrap(),
        lines("linux_aarch64")
    );
}

#[test]
fn platform_resource_limit_is_error_not_truncated_success() {
    assert_invalid(mac(u32::MAX, 0, M::Arm64));
    assert_invalid(linux(
        L::X86_64,
        PythonTagLibc::Glibc {
            major: 2,
            minor: u32::MAX,
        },
    ));
    assert_invalid(linux(
        L::X86_64,
        PythonTagLibc::Musl {
            major: 1,
            minor: u32::MAX,
        },
    ));
}
