use super::*;
use pakro_index::python_source::{NormalizedPythonName, PythonRegistry};
use pakro_index::simple::{SimpleLimits, SimpleParseContext, parse_simple_json};
use pakro_index::{PythonMarkerContextBuilder, WheelTag};
use serde_json::{Value, json};

/// Diagnose only explicitly supplied captures/observations. This reads no
/// executable and grants no runtime, registry or installation authority.
#[test]
#[ignore = "requires explicit Simple-page directory and language manifest inputs"]
fn s4_isat_candidate_policy_probe() {
    let directory =
        std::env::var_os("PAKRO_ISAT_SIMPLE_PROBE_DIR").expect("set PAKRO_ISAT_SIMPLE_PROBE_DIR");
    let manifest =
        std::env::var_os("PAKRO_ISAT_LANGUAGE_MANIFEST").expect("set PAKRO_ISAT_LANGUAGE_MANIFEST");
    let manifest: Value = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    let descriptor: crate::python_runtime::PythonRuntimeDescriptor =
        serde_json::from_value(manifest["descriptor"].clone()).unwrap();
    let target = crate::packages::python_target::wheel_target(&descriptor).unwrap();
    let markers = &descriptor.markers;
    let python = PythonMarkerContext::new(PythonMarkerContextBuilder {
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
    })
    .unwrap();
    let mut paths = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    assert!(!paths.is_empty() && paths.len() <= 128);
    let limits = CandidatePolicyLimits::default();
    let mut usage = CandidatePolicyUsage::default();
    eprintln!(
        "actual observed target tags={} work_limit={} text_limit={}",
        target.supported.ordered().len(),
        limits.max_work_units,
        limits.max_total_utf8_bytes
    );
    for path in paths {
        let requested = path.file_stem().unwrap().to_str().unwrap();
        let name = NormalizedPythonName::new(requested).unwrap();
        assert_eq!(name.as_str(), requested);
        let registry = PythonRegistry::new("pypi", "https://pypi.org/simple/", None).unwrap();
        let document = registry.project_url(&name).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let project = parse_simple_json(
            &bytes,
            SimpleParseContext {
                registry: &registry,
                project: &name,
                canonical_document_url: &document,
                limits: SimpleLimits::default(),
            },
        )
        .unwrap();
        let before_work = usage.work;
        let before_utf8 = usage.utf8;
        let count = project.files().len();
        let height = if count <= 1 {
            0
        } else {
            (usize::BITS - (count - 1).leading_zeros()) as usize
        };
        let prospective_work =
            before_work + count * (target.supported.ordered().len() + height + 1);
        let result = order_fresh_candidate_universe(
            &project,
            &target.supported,
            &python,
            CandidatePolicy {
                prereleases: Prereleases::IfNoMatchingFinal,
                yanked: Yanked::Disallow,
                sdists: Sdists::Include,
                installed_version: None,
            },
            limits,
            &mut usage,
        );
        match result {
            Ok(candidates) => eprintln!(
                "{requested}: files={count} candidates={} work={} (+{}) utf8={} (+{})",
                candidates.candidates().len(),
                usage.work,
                usage.work - before_work,
                usage.utf8,
                usage.utf8 - before_utf8
            ),
            Err(error) => panic!(
                "{requested}: {error}; files={count}; observed_work={} prospective_work={prospective_work} work_limit={}; utf8={} text_limit={} before_utf8={before_utf8}",
                usage.work, limits.max_work_units, usage.utf8, limits.max_total_utf8_bytes
            ),
        }
    }
}

#[test]
fn universe_and_known_metadata_gateway_do_not_guess_unknown_final_eligibility() {
    let p = project(vec![wheel("2.0rc1"), wheel("1.0")]);
    let supported = target();
    let python = python();
    let mut usage = CandidatePolicyUsage::default();
    let universe = order_fresh_candidate_universe(
        &p,
        &supported,
        &python,
        policy(),
        Default::default(),
        &mut usage,
    )
    .unwrap();
    assert_eq!(
        names(&universe),
        [
            "demo_pkg-2.0rc1-py3-none-any.whl",
            "demo_pkg-1.0-py3-none-any.whl"
        ]
    );
    assert!(
        universe
            .candidates()
            .iter()
            .all(|choice| choice.notices().is_empty())
    );
    let selected = p
        .files()
        .iter()
        .position(|file| file.filename() == "demo_pkg-2.0rc1-py3-none-any.whl")
        .unwrap();
    let alternative = p
        .files()
        .iter()
        .position(|file| file.filename() == "demo_pkg-1.0-py3-none-any.whl")
        .unwrap();
    let check = |own, other, usage: &mut CandidatePolicyUsage| {
        let mut knowledge = vec![CandidateKnowledge::Unknown; p.files().len()];
        knowledge[selected] = own;
        knowledge[alternative] = other;
        assess_active_candidate(
            ActiveCandidateContext {
                project: &p,
                supported: &supported,
                python: &python,
                policy: policy(),
                limits: Default::default(),
            },
            selected,
            &[">=1"],
            &knowledge,
            usage,
        )
    };
    assert!(
        matches!(check(CandidateKnowledge::Compatible,CandidateKnowledge::Unknown,&mut usage).unwrap(),ActiveCandidateAssessment::NeedMetadata(index) if index==alternative)
    );
    let ActiveCandidateAssessment::Eligible(notices) = check(
        CandidateKnowledge::Compatible,
        CandidateKnowledge::Incompatible,
        &mut usage,
    )
    .unwrap() else {
        panic!("actually rejected final must not block fallback")
    };
    assert_eq!(notices, [PolicyNotice::PrereleaseFallback]);
    assert!(matches!(
        check(
            CandidateKnowledge::Compatible,
            CandidateKnowledge::Compatible,
            &mut usage
        )
        .unwrap(),
        ActiveCandidateAssessment::RejectWithAlternative(index) if index == alternative
    ));
    assert_eq!(
        check(
            CandidateKnowledge::Unknown,
            CandidateKnowledge::Compatible,
            &mut usage
        )
        .unwrap_err(),
        CandidatePolicyError::InvalidInput
    );
}

fn assert_known_final_before_unknown_same_version_source(use_cached: bool) {
    const PRE: &str = "demo_pkg-1.8.0.dev0-py3-none-any.whl";
    const SOURCE: &str = "demo_pkg-1.8.0.dev0.tar.gz";
    const FINAL: &str = "demo_pkg-1.7.0-py3-none-any.whl";
    // Original file indices intentionally differ from policy-order positions.
    let p = project(vec![file(FINAL), file(SOURCE), file(PRE)]);
    let supported = target();
    let python = python();
    let policy = CandidatePolicy {
        yanked: Yanked::Disallow,
        ..policy()
    };
    let selected = p
        .files()
        .iter()
        .position(|file| file.filename() == PRE)
        .unwrap();
    let source = p
        .files()
        .iter()
        .position(|file| file.filename() == SOURCE)
        .unwrap();
    let alternative = p
        .files()
        .iter()
        .position(|file| file.filename() == FINAL)
        .unwrap();
    let mut knowledge = vec![CandidateKnowledge::Unknown; p.files().len()];
    knowledge[selected] = CandidateKnowledge::Compatible;
    knowledge[alternative] = CandidateKnowledge::Compatible;
    assert_eq!(knowledge[source], CandidateKnowledge::Unknown);

    let (ordered, cached) = classify_candidate_universe(
        &p,
        &supported,
        &python,
        policy,
        Default::default(),
        &mut CandidatePolicyUsage::default(),
    )
    .unwrap();
    assert_eq!(p.files().len(), 3);
    assert_eq!(cached.len(), 3);
    assert_eq!(names(&ordered), [PRE, SOURCE, FINAL]);
    let selected_position = (0..cached.len())
        .find(|position| cached.file_index(*position) == Some(selected))
        .unwrap();
    let alternative_position = (0..cached.len())
        .find(|position| cached.file_index(*position) == Some(alternative))
        .unwrap();
    assert_ne!(alternative, alternative_position);
    let cached_knowledge = (0..cached.len())
        .map(|position| knowledge[cached.file_index(position).unwrap()])
        .collect::<Vec<_>>();

    let actual = if use_cached {
        assess_cached_candidate(
            &cached,
            selected_position,
            &[">=1"],
            &cached_knowledge,
            Default::default(),
            &mut CandidatePolicyUsage::default(),
        )
    } else {
        assess_active_candidate(
            ActiveCandidateContext {
                project: &p,
                supported: &supported,
                python: &python,
                policy,
                limits: Default::default(),
            },
            selected,
            &[">=1"],
            &knowledge,
            &mut CandidatePolicyUsage::default(),
        )
    }
    .unwrap();
    assert_eq!(actual, ActiveCandidateAssessment::RejectWithAlternative(alternative));
}

#[test]
fn s4_reject_alternative_uncached_suggests_known_final_before_unknown_same_version_source() {
    assert_known_final_before_unknown_same_version_source(false);
}

#[test]
fn s4_reject_alternative_cached_suggests_known_final_before_unknown_same_version_source() {
    assert_known_final_before_unknown_same_version_source(true);
}

#[test]
fn exact_yanked_gateway_emits_notices_only_after_alternative_knowledge_is_complete() {
    let mut yanked = wheel("1.0");
    yanked["yanked"] = json!("retired");
    let p = project(vec![yanked, file("demo_pkg-1.0-1-py3-none-any.whl")]);
    let supported = target();
    let python = python();
    let mut usage = CandidatePolicyUsage::default();
    let selected = p
        .files()
        .iter()
        .position(|file| file.filename() == "demo_pkg-1.0-py3-none-any.whl")
        .unwrap();
    let alternative = p
        .files()
        .iter()
        .position(|file| file.filename() == "demo_pkg-1.0-1-py3-none-any.whl")
        .unwrap();
    let check = |raw: &[&str], other, usage: &mut CandidatePolicyUsage| {
        let mut knowledge = vec![CandidateKnowledge::Unknown; p.files().len()];
        knowledge[selected] = CandidateKnowledge::Compatible;
        knowledge[alternative] = other;
        assess_active_candidate(
            ActiveCandidateContext {
                project: &p,
                supported: &supported,
                python: &python,
                policy: policy(),
                limits: Default::default(),
            },
            selected,
            raw,
            &knowledge,
            usage,
        )
    };
    assert!(
        matches!(check(&["==1.0"],CandidateKnowledge::Unknown,&mut usage).unwrap(),ActiveCandidateAssessment::NeedMetadata(index) if index==alternative)
    );
    let ActiveCandidateAssessment::Eligible(notices) =
        check(&["==1.0"], CandidateKnowledge::Incompatible, &mut usage).unwrap()
    else {
        panic!("exact exception should retain the actual yanked file")
    };
    assert_eq!(notices, [PolicyNotice::YankedExactPin]);
    assert!(matches!(
        check(&[">=1"], CandidateKnowledge::Unknown, &mut usage).unwrap(),
        ActiveCandidateAssessment::Reject
    ));
}

#[test]
fn original_parenthesized_literal_constraints_reach_the_shared_matcher() {
    let p = project(vec![wheel("1.0")]);
    assert_eq!(
        names(&order(&p, &["===1.0"], policy())),
        ["demo_pkg-1.0-py3-none-any.whl"]
    );
    let result = order_fresh_candidates(
        &p,
        &["(===1.0,)"],
        &target(),
        &python(),
        policy(),
        Default::default(),
    )
    .expect("validated original requirement parentheses must remain usable policy input");
    assert_eq!(names(&result), ["demo_pkg-1.0-py3-none-any.whl"]);
    assert!(order(&p, &["===01.0"], policy()).candidates().is_empty());
    assert!(
        order(&p, &["===01.0,===1.0"], policy())
            .candidates()
            .is_empty()
    );
}

pub(super) fn file(filename: &str) -> Value {
    json!({"filename":filename,"url":format!("https://files.example/{filename}?token=private-locator"),"hashes":{"sha256":"ab".repeat(32)}})
}
pub(super) fn wheel(version: &str) -> Value {
    file(&format!("demo_pkg-{version}-py3-none-any.whl"))
}
fn project_from(files: Vec<Value>, source: &str) -> SimpleProject {
    project_page(
        json!({"meta":{"api-version":"1.0"},"name":"demo-pkg","files":files}),
        source,
    )
}
fn project_page(page: Value, source: &str) -> SimpleProject {
    let registry = PythonRegistry::new(source, "https://index.example/simple/", None).unwrap();
    let name = NormalizedPythonName::new("demo-pkg").unwrap();
    let document = registry.project_url(&name).unwrap();
    parse_simple_json(
        &serde_json::to_vec(&page).unwrap(),
        SimpleParseContext {
            registry: &registry,
            project: &name,
            canonical_document_url: &document,
            limits: SimpleLimits::default(),
        },
    )
    .unwrap()
}
pub(super) fn project(files: Vec<Value>) -> SimpleProject {
    project_from(files, "private")
}
pub(super) fn target() -> SupportedWheelTags {
    SupportedWheelTags::new(vec![
        WheelTag::new("cp311", "cp311", "any").unwrap(),
        WheelTag::new("py3", "none", "any").unwrap(),
    ])
    .unwrap()
}
pub(super) fn python() -> PythonMarkerContext {
    PythonMarkerContext::new(PythonMarkerContextBuilder {
        os_name: "posix",
        sys_platform: "darwin",
        platform_release: "25.0.0",
        platform_system: "Darwin",
        platform_version: "explicit fixture",
        platform_machine: "arm64",
        platform_python_implementation: "CPython",
        python_version: "3.11",
        python_full_version: "3.11.0rc2",
        implementation_name: "cpython",
        implementation_version: "3.11.0rc2",
    })
    .unwrap()
}
pub(super) fn policy() -> CandidatePolicy<'static> {
    CandidatePolicy {
        prereleases: Prereleases::IfNoMatchingFinal,
        yanked: Yanked::ExactPinIfNoUnyanked,
        sdists: Sdists::Include,
        installed_version: None,
    }
}
fn order<'a>(
    p: &'a SimpleProject,
    constraints: &[&str],
    policy: CandidatePolicy<'_>,
) -> OrderedCandidates<'a> {
    order_fresh_candidates(
        p,
        constraints,
        &target(),
        &python(),
        policy,
        CandidatePolicyLimits::default(),
    )
    .unwrap()
}
fn names(result: &OrderedCandidates<'_>) -> Vec<String> {
    result
        .candidates()
        .iter()
        .map(|c| c.file().filename().to_owned())
        .collect()
}

#[test]
fn matching_final_and_post_exclude_higher_remote_pre_and_dev() {
    let p = project(vec![
        wheel("2.0rc1"),
        wheel("1.0"),
        wheel("3.0.dev1"),
        wheel("1.0.post2"),
    ]);
    assert_eq!(
        names(&order(&p, &[">=1"], policy())),
        [
            "demo_pkg-1.0.post2-py3-none-any.whl",
            "demo_pkg-1.0-py3-none-any.whl"
        ]
    );
}

#[test]
fn only_an_eligible_matching_final_can_suppress_the_prerelease_fallback() {
    let mut wrong_python = wheel("2.0");
    wrong_python["requires-python"] = json!(">=3.11.0");
    let p = project(vec![
        wrong_python,
        file("demo_pkg-1.9-cp39-cp39-any.whl"),
        wheel("1.8rc1"),
        wheel("4.0"),
    ]);
    let result = order(&p, &[">=1,<3"], policy());
    assert_eq!(names(&result), ["demo_pkg-1.8rc1-py3-none-any.whl"]);
    assert_eq!(
        result.candidates()[0].notices(),
        [PolicyNotice::PrereleaseFallback]
    );
}

#[test]
fn explicit_pre_policy_and_positive_pre_specifiers_do_not_treat_not_equal_as_opt_in() {
    let p = project(vec![wheel("2.0rc1"), wheel("1.0")]);
    let allow = CandidatePolicy {
        prereleases: Prereleases::Allow,
        ..policy()
    };
    assert_eq!(names(&order(&p, &[">=1"], allow)).len(), 2);
    assert_eq!(
        names(&order(&p, &[">=1.0rc1"], policy()))[0],
        "demo_pkg-2.0rc1-py3-none-any.whl"
    );
    assert_eq!(
        names(&order(&p, &[">=1,!=1.5rc1"], policy())),
        ["demo_pkg-1.0-py3-none-any.whl"]
    );
    assert_eq!(
        names(&order(&p, &["<2.0rc2"], policy()))[0],
        "demo_pkg-2.0rc1-py3-none-any.whl"
    );
    let deny = CandidatePolicy {
        prereleases: Prereleases::Disallow,
        ..policy()
    };
    assert_eq!(
        names(&order(&p, &[">=1.0rc1"], deny)),
        ["demo_pkg-1.0-py3-none-any.whl"]
    );
    assert_eq!(
        names(&order(&p, &["<2.0"], allow)),
        ["demo_pkg-1.0-py3-none-any.whl"]
    );
}

#[test]
fn an_explicit_installed_pre_does_not_admit_another_remote_pre_or_override_disallow() {
    let p = project(vec![wheel("3.0rc1"), wheel("2.0rc1"), wheel("1.0")]);
    let installed = CandidatePolicy {
        installed_version: Some("2.0rc1"),
        ..policy()
    };
    assert_eq!(
        names(&order(&p, &[">=1"], installed)),
        [
            "demo_pkg-2.0rc1-py3-none-any.whl",
            "demo_pkg-1.0-py3-none-any.whl"
        ]
    );
    let deny = CandidatePolicy {
        prereleases: Prereleases::Disallow,
        ..installed
    };
    assert_eq!(
        names(&order(&p, &[">=1"], deny)),
        ["demo_pkg-1.0-py3-none-any.whl"]
    );
    assert_eq!(
        names(&order(&p, &[">=1"], policy())),
        ["demo_pkg-1.0-py3-none-any.whl"]
    );
}

#[test]
fn fresh_yanked_requires_a_syntactic_exact_pin_and_a_typed_notice() {
    let mut yank = wheel("1.0");
    yank["yanked"] = json!(true);
    let p = project(vec![yank, wheel("2.0")]);
    assert_eq!(
        names(&order(&p, &[">=1"], policy())),
        ["demo_pkg-2.0-py3-none-any.whl"]
    );
    for constraints in [["==1.0"], ["===1.0"]] {
        let r = order(&p, &constraints, policy());
        assert_eq!(names(&r), ["demo_pkg-1.0-py3-none-any.whl"]);
        assert_eq!(r.candidates()[0].notices(), [PolicyNotice::YankedExactPin]);
    }
    for constraints in [["==1.*"], [">=1,<=1"]] {
        assert!(order(&p, &constraints, policy()).candidates().is_empty());
    }
    let deny = CandidatePolicy {
        yanked: Yanked::Disallow,
        ..policy()
    };
    assert!(order(&p, &["==1.0"], deny).candidates().is_empty());
}

#[test]
fn same_version_non_yanked_files_win_only_if_they_really_match_target_and_python() {
    let mut yank = wheel("1.0");
    yank["yanked"] = json!("private-yank-reason");
    let compatible = file("demo_pkg-1.0-cp311-cp311-any.whl");
    let p = project(vec![yank.clone(), compatible.clone()]);
    let r = order(&p, &["==1"], policy());
    assert_eq!(names(&r), ["demo_pkg-1.0-cp311-cp311-any.whl"]);
    assert!(r.candidates()[0].notices().is_empty());
    let mut wrong = compatible;
    wrong["requires-python"] = json!(">=99");
    let p = project(vec![yank, wrong]);
    assert_eq!(
        names(&order(&p, &["==1"], policy())),
        ["demo_pkg-1.0-py3-none-any.whl"]
    );
}

#[test]
fn same_version_files_keep_tag_rank_numeric_build_and_a_transitive_group_order() {
    let p = project(vec![
        file("demo_pkg-1.0-2-py3-none-any.whl"),
        file("demo_pkg-1.0-2a-py2.py3-none-any.whl"),
        file("demo_pkg-1.0-3-py3-none-any.whl"),
        file("demo_pkg-1.0-10-py3-none-any.whl"),
        file("demo_pkg-1.0-cp311-cp311-any.whl"),
    ]);
    let result = order(&p, &["==1"], policy());
    assert_eq!(
        names(&result),
        [
            "demo_pkg-1.0-cp311-cp311-any.whl",
            "demo_pkg-1.0-2a-py2.py3-none-any.whl",
            "demo_pkg-1.0-10-py3-none-any.whl",
            "demo_pkg-1.0-3-py3-none-any.whl",
            "demo_pkg-1.0-2-py3-none-any.whl",
        ]
    );
    let mut reversed = p
        .files()
        .iter()
        .map(|f| file(f.filename()))
        .collect::<Vec<_>>();
    reversed.reverse();
    assert_eq!(
        names(&order(&project(reversed), &["==1"], policy())),
        names(&result)
    );
}

#[test]
fn artifact_mode_is_explicit_and_no_advertised_or_historical_version_becomes_a_file() {
    let advertised = project_page(
        json!({"meta":{"api-version":"1.1"},"name":"demo-pkg","versions":["99.0"],"files":[]}),
        "private",
    );
    assert_eq!(advertised.versions(), ["99.0"]);
    assert!(
        order(&advertised, &[">=1"], policy())
            .candidates()
            .is_empty()
    );
    let p = project(vec![
        file("demo_pkg-1.0.tar.gz"),
        wheel("1.0"),
        file("demo_pkg-2.0-py3.egg"),
    ]);
    assert_eq!(
        names(&order(&p, &[">=1"], policy())),
        ["demo_pkg-1.0-py3-none-any.whl", "demo_pkg-1.0.tar.gz"]
    );
    let wheels = CandidatePolicy {
        sdists: Sdists::Exclude,
        ..policy()
    };
    assert_eq!(
        names(&order(&p, &[">=1"], wheels)),
        ["demo_pkg-1.0-py3-none-any.whl"]
    );
    assert!(
        order(&project(vec![]), &[">=1"], policy())
            .candidates()
            .is_empty()
    );
}

#[test]
fn all_constraint_inputs_are_checked_before_filtering_and_errors_do_not_echo_private_text() {
    let p = project(vec![wheel("1.0+local"), wheel("1.0.0")]);
    assert_eq!(order(&p, &["==1"], policy()).candidates().len(), 2);
    assert_eq!(
        names(&order(&p, &["==1.0+local"], policy())),
        ["demo_pkg-1.0+local-py3-none-any.whl"]
    );
    let error = order_fresh_candidates(
        &p,
        &[">99", "@ https://private.invalid/?token=policy-secret"],
        &target(),
        &python(),
        policy(),
        Default::default(),
    )
    .unwrap_err();
    assert_eq!(error, CandidatePolicyError::InvalidInput);
    assert!(!format!("{error:?} {error}").contains("policy-secret"));
    let invalid = CandidatePolicy {
        installed_version: Some("private-installed-secret"),
        ..policy()
    };
    assert_eq!(
        order_fresh_candidates(&p, &[], &target(), &python(), invalid, Default::default())
            .unwrap_err(),
        CandidatePolicyError::InvalidInput
    );
}

#[test]
fn budgets_are_explicit_and_results_retain_the_original_project_and_file_indices() {
    let p = project(vec![wheel("1.0"), wheel("2.0")]);
    let supported = target();
    // Target validation + one constraint/classification/sort envelope per
    // file + the very same adaptive rank plan executed by the matcher.
    let exact_work = supported.ordered().len()
        + p.files().len() * 3
        + p.files()
            .iter()
            .map(|file| {
                let PythonDistributionKind::Wheel(wheel) = file.kind() else {
                    unreachable!()
                };
                supported.plan_rank(&wheel.tags).unwrap().work_bound()
            })
            .sum::<usize>();
    for zero in [
        CandidatePolicyLimits {
            max_files: 0,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_constraints: 0,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_supported_tags: 0,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_field_bytes: 0,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_total_utf8_bytes: 0,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_work_units: 0,
            ..Default::default()
        },
    ] {
        assert_eq!(
            order_fresh_candidates(&p, &[">=1"], &target(), &python(), policy(), zero).unwrap_err(),
            CandidatePolicyError::InvalidLimits
        );
    }
    for tight in [
        CandidatePolicyLimits {
            max_files: 1,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_constraints: 1,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_supported_tags: 1,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_field_bytes: 1,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_total_utf8_bytes: 1,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_work_units: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            order_fresh_candidates(&p, &[">=1", "<3"], &target(), &python(), policy(), tight)
                .unwrap_err(),
            CandidatePolicyError::Limit
        );
    }
    let result = order_fresh_candidates(
        &p,
        &[">=1"],
        &target(),
        &python(),
        policy(),
        CandidatePolicyLimits {
            max_files: 2,
            max_constraints: 1,
            max_supported_tags: 2,
            max_field_bytes: 29,
            max_total_utf8_bytes: 99,
            max_work_units: exact_work,
        },
    )
    .unwrap();
    // Text and work retain exact adjacent successful/failing boundaries.
    for tight in [
        CandidatePolicyLimits {
            max_field_bytes: 28,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_total_utf8_bytes: 98,
            ..Default::default()
        },
        CandidatePolicyLimits {
            max_work_units: exact_work - 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            order_fresh_candidates(&p, &[">=1"], &target(), &python(), policy(), tight)
                .unwrap_err(),
            CandidatePolicyError::Limit
        );
    }
    assert!(std::ptr::eq(result.project(), &p));
    for choice in result.candidates() {
        assert!(std::ptr::eq(choice.file(), &p.files()[choice.file_index()]));
    }
    let other = project_from(vec![wheel("1.0"), wheel("2.0")], "another-source");
    let other_result = order(&other, &[">=1"], policy());
    assert_ne!(result.project().source(), other_result.project().source());
    assert!(!std::ptr::eq(result.project(), other_result.project()));
}
