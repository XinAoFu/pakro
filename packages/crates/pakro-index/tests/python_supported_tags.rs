// Fixed packaging 26.3/PEP fixtures; expected order is independent of the generator.

use pakro_index::{
    IndexError, PythonLinuxArch as L, PythonManylinuxPolicy, PythonTagInterpreter as I,
    PythonTagLibc, PythonTagPlatform, PythonTagTarget, SupportedWheelTags, parse_wheel_filename,
    pure_python_supported_tags, python_supported_tags,
};

fn cpython(
    minor: u32,
    debug: bool,
    gil_disabled: bool,
    pymalloc: Option<bool>,
    unicode_size: Option<u8>,
) -> PythonTagTarget {
    PythonTagTarget {
        language_major: 3,
        language_minor: minor,
        interpreter: I::CPython {
            debug,
            gil_disabled,
            pymalloc,
            unicode_size,
        },
        platform: PythonTagPlatform::Linux {
            arch: L::X86_64,
            libc: PythonTagLibc::Glibc { major: 2, minor: 4 },
            manylinux: PythonManylinuxPolicy {
                abi_compatible: true,
                overrides: Vec::new(),
            },
        },
    }
}

fn serialized(tags: &SupportedWheelTags) -> Vec<String> {
    tags.ordered()
        .iter()
        .map(|tag| format!("{}-{}-{}", tag.interpreter(), tag.abi(), tag.platform()))
        .collect()
}

fn lines(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_owned).collect()
}

fn rank(tags: &SupportedWheelTags, filename: &str) -> Option<usize> {
    tags.best_rank(&parse_wheel_filename(filename).unwrap().tags)
        .unwrap()
}

fn assert_invalid(target: &PythonTagTarget) {
    assert!(matches!(
        python_supported_tags(target),
        Err(IndexError::InvalidPythonTagTarget(_))
    ));
}

#[test]
fn cpython_32_complete_old_mu_sequence_golden() {
    assert_eq!(
        serialized(&python_supported_tags(&cpython(2, false, false, Some(true), Some(4))).unwrap()),
        lines(
            "cp32-cp32mu-linux_x86_64 cp32-abi3-linux_x86_64 cp32-none-linux_x86_64
         py32-none-linux_x86_64 py3-none-linux_x86_64 py31-none-linux_x86_64 py30-none-linux_x86_64
         cp32-none-any py32-none-any py3-none-any py31-none-any py30-none-any"
        )
    );
}

#[test]
fn cpython_30_complete_no_stable_abi_sequence_golden() {
    assert_eq!(
        serialized(&python_supported_tags(&cpython(0, false, false, Some(true), Some(2))).unwrap()),
        lines(
            "cp30-cp30m-linux_x86_64 cp30-none-linux_x86_64 py30-none-linux_x86_64
         py3-none-linux_x86_64 cp30-none-any py30-none-any py3-none-any"
        )
    );
}

#[test]
fn cpython_38_debug_complete_order_golden() {
    assert_eq!(serialized(&python_supported_tags(&cpython(8, true, false, None, None)).unwrap()), lines(
        "cp38-cp38d-linux_x86_64 cp38-cp38-linux_x86_64 cp38-abi3-linux_x86_64 cp38-none-linux_x86_64
         cp37-abi3-linux_x86_64 cp36-abi3-linux_x86_64 cp35-abi3-linux_x86_64 cp34-abi3-linux_x86_64
         cp33-abi3-linux_x86_64 cp32-abi3-linux_x86_64
         py38-none-linux_x86_64 py3-none-linux_x86_64 py37-none-linux_x86_64 py36-none-linux_x86_64
         py35-none-linux_x86_64 py34-none-linux_x86_64 py33-none-linux_x86_64 py32-none-linux_x86_64
         py31-none-linux_x86_64 py30-none-linux_x86_64 cp38-none-any
         py38-none-any py3-none-any py37-none-any py36-none-any py35-none-any py34-none-any
         py33-none-any py32-none-any py31-none-any py30-none-any"
    ));
}

#[test]
fn cpython_313_free_threaded_complete_abi3t_order_golden() {
    assert_eq!(serialized(&python_supported_tags(&cpython(13, false, true, None, None)).unwrap()), lines(
        "cp313-cp313t-linux_x86_64 cp313-abi3t-linux_x86_64 cp313-none-linux_x86_64
         cp312-abi3t-linux_x86_64 cp311-abi3t-linux_x86_64 cp310-abi3t-linux_x86_64
         cp39-abi3t-linux_x86_64 cp38-abi3t-linux_x86_64 cp37-abi3t-linux_x86_64
         cp36-abi3t-linux_x86_64 cp35-abi3t-linux_x86_64 cp34-abi3t-linux_x86_64 cp33-abi3t-linux_x86_64 cp32-abi3t-linux_x86_64
         py313-none-linux_x86_64 py3-none-linux_x86_64 py312-none-linux_x86_64 py311-none-linux_x86_64
         py310-none-linux_x86_64 py39-none-linux_x86_64 py38-none-linux_x86_64 py37-none-linux_x86_64
         py36-none-linux_x86_64 py35-none-linux_x86_64 py34-none-linux_x86_64 py33-none-linux_x86_64
         py32-none-linux_x86_64 py31-none-linux_x86_64 py30-none-linux_x86_64 cp313-none-any
         py313-none-any py3-none-any py312-none-any py311-none-any py310-none-any py39-none-any
         py38-none-any py37-none-any py36-none-any py35-none-any py34-none-any py33-none-any
         py32-none-any py31-none-any py30-none-any"
    ));
}

#[test]
fn pypy_310_complete_generic_and_pp3_order_golden() {
    let mut target = cpython(10, false, false, None, None);
    target.interpreter = I::PyPy {
        soabi: "pypy310-pp73-x86_64-linux-gnu".into(),
        ext_suffix: ".pypy310-pp73-x86_64-linux-gnu.so".into(),
    };
    assert_eq!(serialized(&python_supported_tags(&target).unwrap()), lines(
        "pp310-pypy310_pp73-linux_x86_64 pp310-none-linux_x86_64
         py310-none-linux_x86_64 py3-none-linux_x86_64 py39-none-linux_x86_64 py38-none-linux_x86_64
         py37-none-linux_x86_64 py36-none-linux_x86_64 py35-none-linux_x86_64 py34-none-linux_x86_64
         py33-none-linux_x86_64 py32-none-linux_x86_64 py31-none-linux_x86_64 py30-none-linux_x86_64
         pp3-none-any py310-none-any py3-none-any py39-none-any py38-none-any py37-none-any
         py36-none-any py35-none-any py34-none-any py33-none-any py32-none-any py31-none-any py30-none-any"
    ));
}

#[test]
fn each_abi_block_preserves_platform_preference() {
    let mut target = cpython(2, false, false, Some(true), Some(4));
    if let PythonTagPlatform::Linux { libc, .. } = &mut target.platform {
        *libc = PythonTagLibc::Glibc { major: 2, minor: 5 };
    }
    assert_eq!(serialized(&python_supported_tags(&target).unwrap()), lines(
        "cp32-cp32mu-linux_x86_64 cp32-cp32mu-manylinux_2_5_x86_64 cp32-cp32mu-manylinux1_x86_64
         cp32-abi3-linux_x86_64 cp32-abi3-manylinux_2_5_x86_64 cp32-abi3-manylinux1_x86_64
         cp32-none-linux_x86_64 cp32-none-manylinux_2_5_x86_64 cp32-none-manylinux1_x86_64
         py32-none-linux_x86_64 py32-none-manylinux_2_5_x86_64 py32-none-manylinux1_x86_64
         py3-none-linux_x86_64 py3-none-manylinux_2_5_x86_64 py3-none-manylinux1_x86_64
         py31-none-linux_x86_64 py31-none-manylinux_2_5_x86_64 py31-none-manylinux1_x86_64
         py30-none-linux_x86_64 py30-none-manylinux_2_5_x86_64 py30-none-manylinux1_x86_64
         cp32-none-any py32-none-any py3-none-any py31-none-any py30-none-any"
    ));
}

#[test]
fn pre38_debug_has_no_release_abi_and_old_flags_are_exact() {
    for (minor, pm, width, expected) in [
        (2, true, Some(4), "cp32dmu"),
        (2, false, Some(2), "cp32d"),
        (2, true, Some(2), "cp32dm"),
        (2, false, Some(4), "cp32du"),
        (7, true, None, "cp37dm"),
    ] {
        let tags = python_supported_tags(&cpython(minor, true, false, Some(pm), width)).unwrap();
        assert_eq!(tags.ordered().first().unwrap().abi(), expected);
        let release = format!("demo-1-cp3{minor}-cp3{minor}m-linux_x86_64.whl");
        assert_eq!(rank(&tags, &release), None);
    }
}

#[test]
fn modern_ft_debug_accepts_release_ft_but_not_gil_enabled_abi() {
    let tags = python_supported_tags(&cpython(15, true, true, None, None)).unwrap();
    assert_eq!(tags.ordered()[0].abi(), "cp315td");
    assert_eq!(tags.ordered()[1].abi(), "cp315t");
    assert_eq!(tags.ordered()[2].abi(), "abi3t");
    assert_eq!(rank(&tags, "demo-1-cp315-cp315-linux_x86_64.whl"), None);
    assert_eq!(rank(&tags, "demo-1-cp314-abi3-linux_x86_64.whl"), None);
}

#[test]
fn gil_enabled_and_ft_stable_abis_are_disjoint() {
    let normal = python_supported_tags(&cpython(13, false, false, None, None)).unwrap();
    let threaded = python_supported_tags(&cpython(13, false, true, None, None)).unwrap();
    assert!(rank(&normal, "demo-1-cp32-abi3-linux_x86_64.whl").is_some());
    assert_eq!(rank(&normal, "demo-1-cp32-abi3t-linux_x86_64.whl"), None);
    assert_eq!(rank(&threaded, "demo-1-cp32-abi3-linux_x86_64.whl"), None);
    assert!(rank(&threaded, "demo-1-cp32-abi3t-linux_x86_64.whl").is_some());
    assert!(rank(&threaded, "demo-1-cp32-abi3.abi3t-linux_x86_64.whl").is_some());
    assert_eq!(rank(&normal, "demo-1-cp314-abi3-linux_x86_64.whl"), None);
}

#[test]
fn missing_historical_abi_and_pre313_threading_are_errors() {
    for target in [
        cpython(7, false, false, None, None),
        cpython(2, false, false, Some(true), None),
        cpython(2, false, false, Some(true), Some(3)),
        cpython(12, false, true, None, None),
    ] {
        assert_invalid(&target);
    }
}

#[test]
fn pypy_soabi_ext_suffix_and_language_must_agree() {
    for (soabi, suffix) in [
        ("pypy39-pp73", ".pypy39-pp73.so"),
        ("pypy310-pp73", ".pypy310-pp74.so"),
        ("pypy310-pp73", ".cpython-310-x86_64-linux-gnu.so"),
        ("pypy310-pp", ".pypy310-pp.so"),
        ("pypy310-pp73", "pypy310-pp73.so"),
        ("pypy310-pp73", ".pypy310-pp73/unsafe.so"),
        ("pypy310-pp73", ".pypy310-pp73.so.extra"),
    ] {
        let mut target = cpython(10, false, false, None, None);
        target.interpreter = I::PyPy {
            soabi: soabi.into(),
            ext_suffix: suffix.into(),
        };
        assert_invalid(&target);
    }
}

#[test]
fn pypy_does_not_accept_cpython_or_stable_native_abi() {
    let mut target = cpython(10, false, false, None, None);
    target.interpreter = I::PyPy {
        soabi: "pypy310-pp73".into(),
        ext_suffix: ".pypy310-pp73.so".into(),
    };
    let tags = python_supported_tags(&target).unwrap();
    assert_eq!(rank(&tags, "demo-1-cp310-cp310-linux_x86_64.whl"), None);
    assert_eq!(rank(&tags, "demo-1-cp32-abi3-linux_x86_64.whl"), None);
    assert_eq!(
        rank(&tags, "demo-1-pp310-pypy310_pp74-linux_x86_64.whl"),
        None
    );
    assert!(rank(&tags, "demo-1-pp3-none-any.whl").is_some());
    assert_eq!(rank(&tags, "demo-1-pp310-none-any.whl"), None);
}

#[test]
fn conservative_pure_generic_subset_exact_golden() {
    let tags = pure_python_supported_tags(3, 2).unwrap();
    assert_eq!(
        serialized(&tags),
        lines("py32-none-any py3-none-any py31-none-any py30-none-any")
    );
    assert_eq!(rank(&tags, "demo-1-cp32-none-any.whl"), None);
    assert_eq!(rank(&tags, "demo-1-pp3-none-any.whl"), None);
    assert_eq!(rank(&tags, "demo-1-py33-none-any.whl"), None);
    assert_eq!(rank(&tags, "demo-1-py3-none-linux_x86_64.whl"), None);
}

#[test]
fn unknown_native_is_error_even_when_generic_pure_is_possible() {
    let mut target = cpython(10, false, false, None, None);
    if let PythonTagPlatform::Linux { libc, .. } = &mut target.platform {
        *libc = PythonTagLibc::Unknown;
    }
    assert_invalid(&target);
    assert!(
        !pure_python_supported_tags(3, 10)
            .unwrap()
            .ordered()
            .is_empty()
    );
}

#[test]
fn language_major_and_supported_resource_limit_are_validated() {
    let mut target = cpython(10, false, false, None, None);
    target.language_major = 2;
    assert_invalid(&target);
    assert!(pure_python_supported_tags(2, 7).is_err());
    target.language_major = 3;
    target.language_minor = u32::MAX;
    assert_invalid(&target);
    assert!(pure_python_supported_tags(3, u32::MAX).is_err());
    let mut finite_target = cpython(100, false, false, None, None);
    if let PythonTagPlatform::Linux { libc, .. } = &mut finite_target.platform {
        *libc = PythonTagLibc::Glibc {
            major: 2,
            minor: 500,
        };
    }
    assert_invalid(&finite_target);
}

#[test]
fn platform_rank_beats_generic_and_rejects_other_arch_libc() {
    let mut target = cpython(10, false, false, None, None);
    if let PythonTagPlatform::Linux { libc, .. } = &mut target.platform {
        *libc = PythonTagLibc::Glibc {
            major: 2,
            minor: 17,
        };
    }
    let tags = python_supported_tags(&target).unwrap();
    let native = rank(&tags, "demo-1-cp310-cp310-linux_x86_64.whl").unwrap();
    let many = rank(&tags, "demo-1-cp310-cp310-manylinux_2_17_x86_64.whl").unwrap();
    let pure = rank(&tags, "demo-1-py3-none-any.whl").unwrap();
    assert!(native < many && many < pure);
    assert_eq!(
        rank(&tags, "demo-1-cp310-cp310-manylinux_2_18_x86_64.whl"),
        None
    );
    assert_eq!(
        rank(&tags, "demo-1-cp310-cp310-manylinux_2_17_aarch64.whl"),
        None
    );
    assert_eq!(
        rank(&tags, "demo-1-cp310-cp310-musllinux_1_2_x86_64.whl"),
        None
    );
}

#[test]
fn modern_mac_targets_share_full_abi_order_without_linux_assumptions() {
    for arch in [
        pakro_index::PythonMacArch::X86_64,
        pakro_index::PythonMacArch::Arm64,
    ] {
        let mut target = cpython(13, false, false, None, None);
        target.platform = PythonTagPlatform::Macos {
            product_major: 12,
            product_minor: 1,
            arch,
        };
        let tags = python_supported_tags(&target).unwrap();
        let expected_platform = if arch == pakro_index::PythonMacArch::Arm64 {
            "macosx_12_0_arm64"
        } else {
            "macosx_12_0_x86_64"
        };
        assert_eq!(tags.ordered()[0].interpreter(), "cp313");
        assert_eq!(tags.ordered()[0].abi(), "cp313");
        assert_eq!(tags.ordered()[0].platform(), expected_platform);
        assert!(rank(&tags, "demo-1-cp313-cp313-macosx_11_0_universal2.whl").is_some());
        assert_eq!(rank(&tags, "demo-1-cp313-cp313-linux_x86_64.whl"), None);
    }
}
