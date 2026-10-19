//! Even an inactive/empty request validates every configured stage budget.
use super::*;

#[test]
fn unused_stage_zero_limits_are_rejected_before_observation_or_prepared_data() {
    let peer = Peer::new(|_| panic!("invalid stage limits must precede HTTP"));
    let f = Fixture::new(&peer);
    let root = f.lock();
    let endpoints = [f.endpoint()];
    let policy = policy();
    let cases: &[(&str, fn(&mut RegistryDepotLimits))] = &[
        ("source.wire", |l| l.source.transfer.max_wire_bytes = 0),
        ("source.header-bytes", |l| {
            l.source.transfer.max_header_bytes = 0
        }),
        ("source.header-count", |l| l.source.transfer.max_headers = 0),
        ("source.spool", |l| l.source.max_spool_bytes = 0),
        ("source.identity", |l| l.source.max_identity_bytes = 0),
        ("project.wire", |l| l.project.max_wire_bytes = 0),
        ("project.cache", |l| l.project.max_cache_bytes = 0),
        ("project.header-bytes", |l| l.project.max_header_bytes = 0),
        ("project.header-count", |l| l.project.max_headers = 0),
        ("project.decoded", |l| l.project.max_decoded_bytes = 0),
        ("json.response", |l| l.project.parser.max_response_bytes = 0),
        ("json.files", |l| l.project.parser.max_files = 0),
        ("json.versions", |l| l.project.parser.max_versions = 0),
        ("json.hashes", |l| l.project.parser.max_hashes_per_file = 0),
        ("json.relations", |l| l.project.parser.max_relations = 0),
        ("json.fields", |l| l.project.parser.max_field_bytes = 0),
        ("json.object-fields", |l| {
            l.project.parser.max_object_fields = 0
        }),
        ("json.depth", |l| l.project.parser.max_json_depth = 0),
        ("json.values", |l| l.project.parser.max_json_values = 0),
        ("html.tokens", |l| l.project.html.max_tokens = 0),
        ("html.nodes", |l| l.project.html.max_tree_nodes = 0),
        ("html.depth", |l| l.project.html.max_tree_depth = 0),
        ("html.text", |l| l.project.html.max_tree_text_bytes = 0),
        ("html.work", |l| l.project.html.max_tree_operations = 0),
        ("html.attributes", |l| {
            l.project.html.max_attributes_per_tag = 0
        }),
        ("html.anchors", |l| l.project.html.max_anchors = 0),
        ("html.feed", |l| l.project.html.feed_chunk_bytes = 0),
        ("policy.files", |l| l.candidates.max_files = 0),
        ("policy.constraints", |l| l.candidates.max_constraints = 0),
        ("policy.tags", |l| l.candidates.max_supported_tags = 0),
        ("policy.field", |l| l.candidates.max_field_bytes = 0),
        ("policy.utf8", |l| l.candidates.max_total_utf8_bytes = 0),
        ("policy.work", |l| l.candidates.max_work_units = 0),
        ("input.dependencies", |l| l.input.max_dependencies = 0),
        ("input.utf8", |l| l.input.max_retained_utf8_bytes = 0),
        ("input.truth", |l| l.input.max_total_truth_states = 0),
        ("input.nodes", |l| l.input.max_total_evaluation_nodes = 0),
        ("truth.extras", |l| l.truth.max_relevant_extras = 0),
        ("truth.states", |l| l.truth.max_truth_states = 0),
        ("truth.work", |l| l.truth.max_total_evaluation_nodes = 0),
        ("truth.names", |l| l.truth.max_name_bytes = 0),
        ("solver.projects", |l| l.solver.max_projects = 0),
        ("solver.candidates", |l| l.solver.max_candidates = 0),
        ("solver.dependencies", |l| l.solver.max_dependencies = 0),
        ("solver.fields", |l| l.solver.max_field_bytes = 0),
        ("solver.utf8", |l| l.solver.max_total_utf8_bytes = 0),
        ("solver.calls", |l| l.solver.max_solver_calls = 0),
        ("solver.requirement-extras", |l| {
            l.solver.max_extras_per_requirement = 0
        }),
        ("solver.truth-extras", |l| l.solver.max_truth_extras = 0),
        ("solver.extra-refs", |l| {
            l.solver.max_total_extra_references = 0
        }),
        ("solver.guard-states", |l| {
            l.solver.max_total_guard_states = 0
        }),
        ("solver.truth-work", |l| l.solver.max_truth_work = 0),
    ];
    assert_eq!(cases.len(), 52);
    for (axis, set) in cases {
        let mut context = f.context(&endpoints, &policy);
        set(&mut context.limits);
        let error = ResolveRegistryDepot::new(&root, &[], context).expect_err(axis);
        assert_eq!(error, RegistryDepotError::InvalidLimits, "{axis}");
    }
    assert!(peer.requests().is_empty());
    f.no_publish();
}
