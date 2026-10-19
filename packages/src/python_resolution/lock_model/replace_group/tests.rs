use super::*;
use crate::packages::python_resolution::lock_model::{
    EdgeInput, GroupKey, LockLimit, LockMarkerSemantics, NodeInput, PinEvidence,
    RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput, RootInput, RuntimePin,
};
use pakro_store::Digest;
use std::collections::BTreeMap;

// These fixtures are portable data, never Source/C/Observed/Installed owners.
fn pin(name: &str, digest: Digest) -> ContentPin {
    let filename = format!("{}-1.0-py3-none-any.whl", name.replace('-', "_"));
    let file = RegistryFileRef::new(
        RegistryFileRefInput {
            registry_id: "corp",
            canonical_base_fingerprint: [7; 32],
            project: name,
            version: "1.0",
            filename: &filename,
            kind: RegistryArtifactKind::Wheel,
        },
        Default::default(),
    )
    .unwrap();
    ContentPin::new(
        &file,
        digest,
        123,
        PinEvidence::Advertised,
        Default::default(),
    )
    .unwrap()
}
struct Graph {
    key: GroupKey,
    pins: Vec<ContentPin>,
    raw: Vec<String>,
    roots: Vec<(usize, String)>,
    edges: Vec<(String, String, String)>,
}
impl Graph {
    fn new(group: &str, pins: Vec<ContentPin>) -> Self {
        let raw = pins
            .first()
            .map(|p| vec![format!("{}==1.0", p.file().project())])
            .unwrap_or_default();
        let roots = pins
            .first()
            .map(|p| vec![(0, p.file().project().to_owned())])
            .unwrap_or_default();
        let edges = pins
            .windows(2)
            .map(|pair| {
                (
                    pair[0].file().project().to_owned(),
                    pair[1].file().project().to_owned(),
                    format!("{}==1.0", pair[1].file().project()),
                )
            })
            .collect();
        Self {
            key: GroupKey::new("", group, [44; 32], Default::default()).unwrap(),
            pins,
            raw,
            roots,
            edges,
        }
    }
    fn with_input<T>(&self, use_input: impl FnOnce(LockGraphInput<'_>) -> T) -> T {
        let raw = self.raw.iter().map(String::as_str).collect::<Vec<_>>();
        let roots = self
            .roots
            .iter()
            .map(|(index, name)| RootInput {
                declaration_index: *index,
                node: name,
            })
            .collect::<Vec<_>>();
        let nodes = self
            .pins
            .iter()
            .map(|pin| NodeInput {
                name: pin.file().project(),
                version: pin.file().version(),
                source_pin: pin.id(),
            })
            .collect::<Vec<_>>();
        let edges = self
            .edges
            .iter()
            .map(|(from, to, raw)| EdgeInput {
                from,
                to,
                declaration: raw,
            })
            .collect::<Vec<_>>();
        use_input(LockGraphInput {
            key: &self.key,
            semantics: LockMarkerSemantics::Current2026,
            runtime: RuntimePin::new([55; 32], 1, [44; 32]).unwrap(),
            root_declarations: &raw,
            roots: &roots,
            nodes: &nodes,
            edges: &edges,
        })
    }
    fn model(&self) -> PythonLockModel {
        self.with_input(|input| {
            PythonLockModel::from_parts(&self.pins, &[input], Default::default()).unwrap()
        })
    }
    fn replace(
        &self,
        old: &PythonLockModel,
        limits: LockModelLimits,
    ) -> LockModelResult<PythonLockModel> {
        self.with_input(|input| old.replace_group(&self.pins, &input, limits))
    }
}
fn model(groups: &[&Graph]) -> PythonLockModel {
    assert_eq!(groups.len(), 2);
    let pins = groups
        .iter()
        .flat_map(|g| g.pins.iter())
        .map(|p| (p.id(), p.clone()))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    groups[0].with_input(|first| {
        groups[1].with_input(|second| {
            PythonLockModel::from_parts(&pins, &[first, second], Default::default()).unwrap()
        })
    })
}

#[test]
fn replacement_preserves_other_groups_and_releases_only_unreferenced_pins() {
    let shared = pin("shared", Digest::Sha512([2; 64]));
    let current = Graph::new(
        "main",
        vec![pin("old", Digest::Sha256([1; 32])), shared.clone()],
    );
    let other = Graph::new(
        "other",
        vec![pin("other", Digest::Sha256([3; 32])), shared.clone()],
    );
    let old = model(&[&current, &other]);
    let original = old.clone();
    let replacement = Graph::new(
        "main",
        vec![pin("new", Digest::Sha256([4; 32])), shared.clone()],
    );
    let result = replacement.replace(&old, Default::default()).unwrap();
    assert_eq!(result.graph(&other.key), old.graph(&other.key));
    assert_eq!(result.graphs().len(), 2);
    assert_eq!(result.pins().len(), 3);
    assert!(!result.pins().iter().any(|p| p.file().project() == "old"));
    assert!(result.pins().iter().any(|p| p == &shared));
    assert_eq!(old, original);
}

#[test]
fn complete_original_roots_inactive_indices_and_duplicate_edges_survive() {
    let old = Graph::new("main", vec![pin("old", Digest::Sha256([1; 32]))]).model();
    let mut replacement = Graph::new(
        "main",
        vec![
            pin("new", Digest::Sha256([4; 32])),
            pin("dep", Digest::Sha512([5; 64])),
        ],
    );
    replacement.raw = vec![
        "inactive ; python_version < '2'".into(),
        " New [ui] == 1.0 ; python_version >= '3' ".into(),
    ];
    replacement.roots = vec![(1, "new".into())];
    replacement.edges = vec![
        (
            "new".into(),
            "dep".into(),
            "dep == 1.0 ; extra == 'ui'".into(),
        ),
        (
            "new".into(),
            "dep".into(),
            "dep == 1.0 ; extra == 'ui'".into(),
        ),
    ];
    replacement.model(); // Independently valid portable-input control.
    let result = replacement.replace(&old, Default::default()).unwrap();
    let graph = result.graph(&replacement.key).unwrap();
    assert_eq!(graph.root_declarations(), replacement.raw);
    assert_eq!(graph.roots()[0].declaration_index(), 1);
    assert_eq!(graph.edges().len(), 2);
    assert_eq!(graph.edges()[0].declaration(), replacement.edges[0].2);
    assert_eq!(graph.edges()[1].declaration(), replacement.edges[1].2);
}

#[test]
fn insertion_preserves_existing_graph_and_exact_sha512_pin_facts() {
    let shared = pin("shared", Digest::Sha512([9; 64]));
    let original_group = Graph::new("main", vec![shared.clone()]);
    let old = original_group.model();
    let new = Graph::new("dev", vec![shared.clone()]);
    let result = new.replace(&old, Default::default()).unwrap();
    assert_eq!(result.graphs().len(), 2);
    assert_eq!(
        result.graph(&original_group.key),
        old.graph(&original_group.key)
    );
    assert_eq!(result.pins(), &[shared]);
}

#[test]
fn an_empty_replacement_drops_exclusive_pins_and_preserves_other_empty_groups() {
    let current = Graph::new("main", vec![pin("old", Digest::Sha256([1; 32]))]);
    let other = Graph::new("other", vec![]);
    let old = model(&[&current, &other]);
    let new = Graph::new("main", vec![]);
    let result = new.replace(&old, Default::default()).unwrap();
    assert!(result.pins().is_empty());
    assert_eq!(result.graphs().len(), 2);
    assert_eq!(result.graph(&other.key), old.graph(&other.key));
}

#[test]
fn a_shared_pin_with_conflicting_size_or_evidence_cannot_be_overwritten() {
    let original_pin = pin("shared", Digest::Sha256([1; 32]));
    let current = Graph::new("main", vec![original_pin.clone()]);
    let other = Graph::new("other", vec![original_pin.clone()]);
    let old = model(&[&current, &other]);
    let control = old.clone();
    for (size, evidence) in [
        (124, PinEvidence::Advertised),
        (123, PinEvidence::ObservedDuringFreshResolution),
    ] {
        let conflicting = ContentPin::new(
            original_pin.file(),
            original_pin.digest(),
            size,
            evidence,
            Default::default(),
        )
        .unwrap();
        assert_eq!(conflicting.id(), original_pin.id());
        let new = Graph::new("main", vec![conflicting]);
        new.model();
        assert_eq!(
            new.replace(&old, Default::default()).unwrap_err(),
            LockModelError::IdentityMismatch
        );
        assert_eq!(old, control);
    }
}

#[test]
fn zero_and_combined_output_budgets_are_enforced_without_changing_the_input() {
    let current = Graph::new("main", vec![pin("a", Digest::Sha256([1; 32]))]);
    let new = Graph::new("other", vec![pin("b", Digest::Sha256([2; 32]))]);
    let old = current.model();
    let control = old.clone();
    let mut zero = LockModelLimits::default();
    zero.max_groups = 0;
    assert_eq!(
        new.replace(&old, zero).unwrap_err(),
        LockModelError::InvalidLimits
    );
    for (limits, expected) in [
        (
            LockModelLimits {
                max_groups: 1,
                ..Default::default()
            },
            LockLimit::Groups,
        ),
        (
            LockModelLimits {
                max_pins: 1,
                ..Default::default()
            },
            LockLimit::Pins,
        ),
        (
            LockModelLimits {
                max_total_nodes: 1,
                ..Default::default()
            },
            LockLimit::TotalNodes,
        ),
        (
            LockModelLimits {
                max_total_references: 4,
                ..Default::default()
            },
            LockLimit::References,
        ),
    ] {
        old.check_limits(limits).unwrap();
        new.model().check_limits(limits).unwrap();
        assert_eq!(
            new.replace(&old, limits).unwrap_err(),
            LockModelError::Limit(expected)
        );
    }
    let mut large_current = current;
    let mut large_new = new;
    large_current.raw = vec![format!("a==1.0; os_name == '{}'", "x".repeat(6000))];
    large_new.raw = vec![format!("b==1.0; os_name == '{}'", "x".repeat(6000))];
    let old_large = large_current.model();
    let limits = LockModelLimits {
        max_total_utf8_bytes: 8000,
        ..Default::default()
    };
    old_large.check_limits(limits).unwrap();
    large_new.model().check_limits(limits).unwrap();
    assert_eq!(
        large_new.replace(&old_large, limits).unwrap_err(),
        LockModelError::Limit(LockLimit::TotalUtf8Bytes)
    );
    assert_eq!(old, control);
}
