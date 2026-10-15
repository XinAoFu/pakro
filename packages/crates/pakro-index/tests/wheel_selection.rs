use pakro_index::{
    ArtifactDescriptor, ArtifactKind, IndexError, PackageMetadata, SupportedWheelTags, WheelTag,
    WheelTags, parse_wheel_filename, select_wheel_artifact,
};

fn support() -> SupportedWheelTags {
    SupportedWheelTags::new(vec![
        WheelTag::new("cp310", "cp310", "manylinux_2_17_x86_64").unwrap(),
        WheelTag::new("py3", "none", "any").unwrap(),
    ])
    .unwrap()
}

fn artifact(filename: &str, url: &str, digest: Option<&str>) -> ArtifactDescriptor {
    ArtifactDescriptor {
        filename: filename.into(),
        url: url.into(),
        digest: digest.map(str::to_string),
        kind: ArtifactKind::Wheel(parse_wheel_filename(filename).unwrap().tags),
    }
}

fn metadata(artifacts: Vec<ArtifactDescriptor>) -> PackageMetadata {
    PackageMetadata {
        name: "demo".into(),
        version: "1.0".into(),
        requires_python: None,
        deps: vec![],
        artifacts,
    }
}

fn selected(metadata: &PackageMetadata) -> Option<&str> {
    select_wheel_artifact(metadata, &support())
        .unwrap()
        .map(|artifact| artifact.filename.as_str())
}

#[test]
fn best_support_rank_precedes_larger_build_or_filename() {
    let native = "demo-1.0-1-cp310-cp310-manylinux_2_17_x86_64.whl";
    let pure = "demo-1.0-999-py3-none-any.whl";
    let mut expected = metadata(vec![
        artifact(native, "https://fixture/native", None),
        artifact(pure, "https://fixture/pure", None),
    ]);
    assert_eq!(selected(&expected), Some(native));
    expected.artifacts.reverse();
    assert_eq!(selected(&expected), Some(native));
}

#[test]
fn same_full_tags_use_numeric_build_and_suffix_tuple() {
    let two = "demo-1.0-2-py3-none-any.whl";
    let ten_a = "demo-1.0-10a-py3-none-any.whl";
    let ten_b = "demo-1.0-10b-py3-none-any.whl";
    let mut value = metadata(vec![
        artifact(ten_a, "https://fixture/ten-a", None),
        artifact(two, "https://fixture/two", None),
        artifact(ten_b, "https://fixture/ten-b", None),
    ]);
    assert_eq!(selected(&value), Some(ten_b));
    value.artifacts.reverse();
    assert_eq!(selected(&value), Some(ten_b));
}

#[test]
fn explicit_zero_build_precedes_absent_build_in_same_full_tags() {
    let plain = "demo-1.0-py3-none-any.whl";
    let zero = "demo-1.0-0-py3-none-any.whl";
    let value = metadata(vec![
        artifact(plain, "https://fixture/plain", None),
        artifact(zero, "https://fixture/zero", None),
    ]);
    assert_eq!(selected(&value), Some(zero));
}

#[test]
fn compressed_full_tag_sets_are_equal_after_sort_and_dedup() {
    let two = "demo-1.0-2-cp310.py3.cp310-cp310.none-any.manylinux_2_17_x86_64.whl";
    let ten = "demo-1.0-10-py3.cp310-none.cp310-manylinux_2_17_x86_64.any.whl";
    let value = metadata(vec![
        artifact(two, "https://fixture/two", None),
        artifact(ten, "https://fixture/ten", None),
    ]);
    assert_eq!(selected(&value), Some(ten));
}

#[test]
fn same_best_rank_with_different_complete_tags_is_ambiguous_before_build() {
    let exact = "demo-1.0-1-cp310-cp310-manylinux_2_17_x86_64.whl";
    let extra = "demo-1.0-999-cp310.py3-cp310-manylinux_2_17_x86_64.whl";
    let value = metadata(vec![
        artifact(exact, "https://fixture/exact", None),
        artifact(extra, "https://fixture/extra", None),
    ]);
    assert!(matches!(
        select_wheel_artifact(&value, &support()),
        Err(IndexError::AmbiguousWheelArtifacts { .. })
    ));
}

#[test]
fn worse_rank_ambiguity_does_not_block_unique_best_candidate() {
    let native = "demo-1.0-cp310-cp310-manylinux_2_17_x86_64.whl";
    let pure = "demo-1.0-py3-none-any.whl";
    let mut value = metadata(vec![
        artifact(pure, "https://fixture/pure-a", None),
        artifact(pure, "https://fixture/pure-b", Some("sha256-different")),
        artifact(native, "https://fixture/native", None),
    ]);
    assert_eq!(selected(&value), Some(native));
    value.artifacts.reverse();
    assert_eq!(selected(&value), Some(native));
}

#[test]
fn lower_build_identity_conflicts_do_not_block_unique_higher_build() {
    let low = "demo-1.0-2-py3-none-any.whl";
    let high = "demo-1.0-10-py3-none-any.whl";
    let mut value = metadata(vec![
        artifact(low, "https://fixture/low-a", None),
        artifact(low, "https://fixture/low-b", Some("sha256-low-b")),
        artifact(high, "https://fixture/high", Some("sha256-high")),
    ]);
    assert_eq!(selected(&value), Some(high));
    value.artifacts.reverse();
    assert_eq!(selected(&value), Some(high));
}

#[test]
fn distinct_winning_urls_or_digests_are_ambiguous_without_leaking_urls() {
    let filename = "demo-1.0-py3-none-any.whl";
    for (left_digest, right_digest) in [
        (None, None),
        (Some("sha256-a"), Some("sha256-b")),
        (None, Some("sha256-a")),
    ] {
        let value = metadata(vec![
            artifact(filename, "https://secret-a:token@fixture/a", left_digest),
            artifact(filename, "https://secret-b:token@fixture/b", right_digest),
        ]);
        let error = select_wheel_artifact(&value, &support()).unwrap_err();
        assert!(matches!(error, IndexError::AmbiguousWheelArtifacts { .. }));
        let message = error.to_string();
        assert!(!message.contains("secret"));
        assert!(!message.contains("token"));
        assert!(!message.contains("https://"));
    }
    let same_url = metadata(vec![
        artifact(filename, "https://fixture/same", Some("sha256-a")),
        artifact(filename, "https://fixture/same", Some("sha256-b")),
    ]);
    assert!(matches!(
        select_wheel_artifact(&same_url, &support()),
        Err(IndexError::AmbiguousWheelArtifacts { .. })
    ));
}

#[test]
fn equivalent_build_numbers_with_different_filename_identity_are_ambiguous() {
    let value = metadata(vec![
        artifact("demo-1.0-2-py3-none-any.whl", "https://fixture/a", None),
        artifact("demo-1.0-0002-py3-none-any.whl", "https://fixture/a", None),
    ]);
    assert!(matches!(
        select_wheel_artifact(&value, &support()),
        Err(IndexError::AmbiguousWheelArtifacts { .. })
    ));
}

#[test]
fn identical_identity_can_be_deduplicated_including_semantic_kind_order() {
    let filename = "demo-1.0-py3.cp310-none-any.whl";
    let first = artifact(filename, "https://fixture/a", None);
    let mut second = first.clone();
    second.kind = ArtifactKind::Wheel(WheelTags {
        python: vec!["PY3".into(), "cp310".into(), "py3".into()],
        abi: vec!["NONE".into()],
        platform: vec!["ANY".into()],
    });
    let value = metadata(vec![first, second]);
    assert_eq!(selected(&value), Some(filename));
}

#[test]
fn expected_name_normalization_and_pep440_equivalence_are_used() {
    let filename = "Zope.Interface-1.0.0-py3-none-any.whl";
    let mut value = metadata(vec![artifact(filename, "https://fixture/a", None)]);
    value.name = "zope-interface".into();
    value.version = "1.0".into();
    assert_eq!(selected(&value), Some(filename));
    let epoch_local = "demo-1!2.0+cpu.1-py3-none-any.whl";
    let mut value = metadata(vec![artifact(epoch_local, "https://fixture/a", None)]);
    value.version = "1!2.0+cpu.1".into();
    assert_eq!(selected(&value), Some(epoch_local));
}

#[test]
fn all_wheels_validate_expected_identity_before_matching_or_selection() {
    let good = artifact("demo-1.0-py3-none-any.whl", "https://fixture/good", None);
    for bad in [
        artifact("other-1.0-cp399-cp399-any.whl", "https://fixture/bad", None),
        artifact("demo-2.0-cp399-cp399-any.whl", "https://fixture/bad", None),
    ] {
        let mut value = metadata(vec![good.clone(), bad]);
        assert!(matches!(
            select_wheel_artifact(&value, &support()),
            Err(IndexError::InvalidWheelArtifact { .. })
        ));
        value.artifacts.reverse();
        assert!(matches!(
            select_wheel_artifact(&value, &support()),
            Err(IndexError::InvalidWheelArtifact { .. })
        ));
    }
}

#[test]
fn malformed_filename_kind_mismatch_and_invalid_public_tags_are_errors() {
    let good = artifact("demo-1.0-py3-none-any.whl", "https://fixture/good", None);
    let mut wrong_filename = good.clone();
    wrong_filename.filename = "demo-1.0-bad-py3-none-any.whl".into();
    let mut wrong_kind_tags = good.clone();
    wrong_kind_tags.kind = ArtifactKind::Wheel(WheelTags {
        python: vec!["cp310".into()],
        abi: vec!["cp310".into()],
        platform: vec!["any".into()],
    });
    let mut invalid_tags = good.clone();
    invalid_tags.kind = ArtifactKind::Wheel(WheelTags {
        python: vec!["py3.none".into()],
        abi: vec!["none".into()],
        platform: vec![],
    });
    let mut wrong_kind = good.clone();
    wrong_kind.kind = ArtifactKind::Sdist;
    for bad in [wrong_filename, wrong_kind_tags, invalid_tags, wrong_kind] {
        let value = metadata(vec![good.clone(), bad]);
        assert!(matches!(
            select_wheel_artifact(&value, &support()),
            Err(IndexError::InvalidWheelArtifact { .. })
        ));
    }
}

#[test]
fn sdists_do_not_become_wheels_and_missing_digest_is_not_identity_proof() {
    let source = ArtifactDescriptor {
        filename: "demo-1.0.tar.gz".into(),
        url: "https://fixture/source".into(),
        digest: None,
        kind: ArtifactKind::Sdist,
    };
    let value = metadata(vec![source]);
    assert_eq!(selected(&value), None);
    let value = metadata(vec![artifact(
        "demo-1.0-py3-none-any.whl",
        "https://fixture/a",
        None,
    )]);
    assert!(select_wheel_artifact(&value, &support()).unwrap().is_some());
}

#[test]
fn invalid_expected_metadata_is_reported_and_no_matching_wheel_is_none() {
    for name in ["", "invalid/name"] {
        let mut bad_name = metadata(vec![]);
        bad_name.name = name.into();
        assert!(
            matches!(
                select_wheel_artifact(&bad_name, &support()),
                Err(IndexError::BadResponse { .. })
            ),
            "{name:?}"
        );
    }
    for version in ["", "not_a_version"] {
        let mut bad_version = metadata(vec![]);
        bad_version.version = version.into();
        assert!(
            matches!(
                select_wheel_artifact(&bad_version, &support()),
                Err(IndexError::BadResponse { .. })
            ),
            "{version:?}"
        );
    }
    let value = metadata(vec![artifact(
        "demo-1.0-pp310-pypy310_pp73-any.whl",
        "https://fixture/a",
        None,
    )]);
    assert_eq!(selected(&value), None);
}
