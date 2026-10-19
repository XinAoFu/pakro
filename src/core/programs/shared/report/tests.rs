use super::*;

const STATES: [OperationState; 6] = [
    OperationState::NoChange,
    OperationState::PreparedOnly,
    OperationState::Committed,
    OperationState::CommittedNeedsFinalization,
    OperationState::NotCommitted,
    OperationState::RecoveryRequired,
];

fn empty_effects() -> OperationEffects<()> {
    OperationEffects {
        added: Vec::new(),
        reused: Vec::new(),
        associations_changed: Vec::new(),
        retained: Vec::new(),
        removed: Vec::new(),
    }
}

#[test]
fn r01_report_preserves_each_observed_commit_state() {
    for (index, state) in STATES.into_iter().enumerate() {
        assert!(STATES[..index].iter().all(|other| *other != state));
        let report = OperationReport::new(state, empty_effects(), Vec::new());
        assert_eq!(report.state(), state);
    }
}

#[test]
fn r01_report_preserves_subject_references() {
    // These values deliberately have no Clone, Debug, or serialization trait.
    // A report must not replace caller-owned descriptions with invented IDs.
    struct Subject(u8);
    let subjects = [Subject(0), Subject(1), Subject(2), Subject(3), Subject(4)];
    let effects = OperationEffects {
        added: vec![&subjects[0]],
        reused: vec![&subjects[1]],
        associations_changed: vec![&subjects[2]],
        retained: vec![&subjects[3]],
        removed: vec![&subjects[4]],
    };
    let warnings = vec![
        "a separate optional diagnostic is unavailable".to_owned(),
        "one retained object is still referenced".to_owned(),
    ];
    let report = OperationReport::new(OperationState::Committed, effects, warnings.clone());
    let actual = report.effects();
    for (reported, original) in [
        (&actual.added, &subjects[0]),
        (&actual.reused, &subjects[1]),
        (&actual.associations_changed, &subjects[2]),
        (&actual.retained, &subjects[3]),
        (&actual.removed, &subjects[4]),
    ] {
        assert_eq!(reported.len(), 1);
        assert!(std::ptr::eq(reported[0], original));
        assert_eq!(reported[0].0, original.0);
    }
    assert_eq!(report.warnings(), warnings.as_slice());
}

fn observed_report(state: OperationState) -> OperationReport<&'static str> {
    // Describe facts already observed by a caller. Constructing this private
    // fixture directly lets termination tests reach their own RED stub without
    // depending on the separate report-construction stub.
    OperationReport {
        state,
        effects: OperationEffects {
            added: Vec::new(),
            reused: vec!["shared language"],
            associations_changed: Vec::new(),
            retained: vec!["referenced package"],
            removed: Vec::new(),
        },
        warnings: vec!["follow the recorded operation state".into()],
    }
}

fn assert_observed_facts(report: &OperationReport<&str>, state: OperationState) {
    assert_eq!(report.state(), state);
    assert!(report.effects().added.is_empty());
    assert_eq!(report.effects().reused.as_slice(), &["shared language"]);
    assert!(report.effects().associations_changed.is_empty());
    assert_eq!(
        report.effects().retained.as_slice(),
        &["referenced package"]
    );
    assert!(report.effects().removed.is_empty());
    assert_eq!(report.warnings().len(), 1);
    assert_eq!(report.warnings()[0], "follow the recorded operation state");
}

#[test]
fn r01_report_failure_preserves_observed_commit_facts() {
    struct Failure {
        reason: &'static str,
    }

    for state in STATES {
        let cause = Failure {
            reason: "later operation failed",
        };
        let interrupted =
            observed_report(state).with_termination(OperationTermination::Failed(cause));
        assert_observed_facts(interrupted.report(), state);
        match interrupted.termination() {
            OperationTermination::Failed(actual) => {
                assert_eq!(actual.reason, "later operation failed");
            }
            OperationTermination::Cancelled => panic!("failure must retain its actual cause"),
        }
    }
}

#[test]
fn r01_report_cancellation_preserves_observed_commit_facts() {
    for state in STATES {
        let interrupted =
            observed_report(state).with_termination(OperationTermination::<()>::Cancelled);
        assert_observed_facts(interrupted.report(), state);
        assert!(matches!(
            interrupted.termination(),
            OperationTermination::Cancelled
        ));
    }
}
