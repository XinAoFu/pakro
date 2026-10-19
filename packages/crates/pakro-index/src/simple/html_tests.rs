use super::html::{SimpleHtmlLimits, parse_html_with_trace, parse_simple_html};
use super::*;

use serde_json::json;

const NAME: &str = "demo_pkg-1.0-py3-none-any.whl";
struct Fixture {
    registry: PythonRegistry,
    project: NormalizedPythonName,
    document: CanonicalProjectUrl,
}
impl Fixture {
    fn new() -> Self {
        let registry =
            PythonRegistry::new("corp", "https://index.example/tenant/simple/", None).unwrap();
        let project = NormalizedPythonName::new("demo-pkg").unwrap();
        let document = registry.project_url(&project).unwrap();
        Self {
            registry,
            project,
            document,
        }
    }
    fn context(&self, limits: SimpleLimits) -> SimpleParseContext<'_> {
        SimpleParseContext {
            registry: &self.registry,
            project: &self.project,
            canonical_document_url: &self.document,
            limits,
        }
    }
    fn parse(&self, document: &str) -> SimpleResult<SimpleProject> {
        parse_simple_html(
            document,
            self.context(SimpleLimits::default()),
            SimpleHtmlLimits::default(),
        )
    }
}
fn document(head: &str, body: &str) -> String {
    format!("<!DOCTYPE html><html><head>{head}</head><body>{body}</body></html>")
}
fn link(version: &str, attributes: &str) -> String {
    let name = format!("demo_pkg-{version}-py3-none-any.whl");
    format!(
        "<a href=\"https://files.example/{name}#sha256={}\" {attributes}>{name}</a>",
        "ab".repeat(32)
    )
}
fn invalid(result: SimpleResult<SimpleProject>) {
    assert!(
        matches!(result, Err(SimpleError::InvalidResponse(_))),
        "{result:?}"
    );
}

#[test]
fn html_and_json_have_the_same_file_facts() {
    let fixture = Fixture::new();
    let head = r#"<meta name="pypi:repository-version" content="1.0">"#;
    let html = fixture
        .parse(&document(
            head,
            &link("1.0", r#"data-requires-python=">=3.8""#),
        ))
        .unwrap();
    let expected = json!({"meta":{"api-version":"1.0"},"name":"demo-pkg","files":[{
        "filename":NAME,"url":format!("https://files.example/{NAME}"),
        "hashes":{"sha256":"ab".repeat(32)},"requires-python":">=3.8"
    }]});
    let expected = parse_simple_json(
        &serde_json::to_vec(&expected).unwrap(),
        fixture.context(SimpleLimits::default()),
    )
    .unwrap();
    assert_eq!(html, expected);
}

#[test]
fn html_repository_version_has_explicit_fail_warn_semantics() {
    let fixture = Fixture::new();
    let old = fixture.parse(&document("", &link("1.0", ""))).unwrap();
    assert_eq!(
        (old.api_version().major(), old.api_version().minor()),
        (1, 0)
    );
    let future = fixture
        .parse(&document(
            r#"<meta name="pypi:repository-version" content="1.99">"#,
            &link("1.0", ""),
        ))
        .unwrap();
    assert!(future.warnings().contains(&SimpleWarning::NewerApiMinor {
        major: 1,
        minor: 99
    }));
    assert!(
        future.versions().is_empty(),
        "HTML does not acquire JSON-only versions requirements"
    );
    assert_eq!(future.files()[0].size(), None);
    let error = fixture
        .parse(&document(
            r#"<meta name="pypi:repository-version" content="2.0">"#,
            &link("1.0", ""),
        ))
        .unwrap_err();
    assert!(matches!(error, SimpleError::UnsupportedApiMajor(2)));
    invalid(fixture.parse(&document(
        r#"<meta name="pypi:repository-version" content="1.x">"#,
        &link("1.0", ""),
    )));
}

#[test]
fn html5_attributes_and_text_are_decoded_in_context() {
    let fixture = Fixture::new();
    let body = format!(
        "<A\n HREF='https://files.example/{NAME}#sha256={}' class=download\n DATA-REQUIRES-PYTHON='&gt;=3.8,&lt;4' data-yanked='private &amp; reason'>\n <span>demo_pkg-1.0-</span>py3-none-any.whl \n</A>",
        "ab".repeat(32)
    );
    let project = fixture.parse(&document("", &body)).unwrap();
    assert_eq!(project.files()[0].filename(), NAME);
    assert_eq!(project.files()[0].requires_python(), Some(">=3.8,<4"));
    assert_eq!(
        project.files()[0].yanked().reason(),
        Some("private & reason")
    );
    let numeric = link("1.0", r#"data-requires-python="&#x3e;&#61;3.8""#);
    assert_eq!(
        fixture.parse(&document("", &numeric)).unwrap().files()[0].requires_python(),
        Some(">=3.8")
    );
}

#[test]
fn html_requires_python_is_unfiltered_file_metadata() {
    let fixture = Fixture::new();
    let project = fixture
        .parse(&document(
            "",
            &link("1.0", r#"data-requires-python="&gt;=3.99""#),
        ))
        .unwrap();
    assert_eq!(project.files()[0].requires_python(), Some(">=3.99"));
    invalid(fixture.parse(&document(
        "",
        &link("1.0", r#"data-requires-python="&gt;=3.invalid""#),
    )));
}

#[test]
fn html_yanked_presence_does_not_use_json_truthiness() {
    let fixture = Fixture::new();
    for attrs in ["data-yanked", "data-yanked=''", "data-yanked=false"] {
        let project = fixture.parse(&document("", &link("1.0", attrs))).unwrap();
        assert!(project.files()[0].yanked().is_yanked());
    }
    let project = fixture
        .parse(&document(
            "",
            &link("1.0", r#"data-yanked="synthetic-yank-secret &amp; reason""#),
        ))
        .unwrap();
    let state = project.files()[0].yanked();
    assert_eq!(state.reason(), Some("synthetic-yank-secret & reason"));
    assert!(!format!("{state:?} {project:?}").contains("synthetic-yank-secret"));
    assert!(
        !fixture
            .parse(&document("", &link("1.0", "")))
            .unwrap()
            .files()[0]
            .yanked()
            .is_yanked()
    );
}

#[test]
fn html_core_metadata_uses_the_new_attribute() {
    let fixture = Fixture::new();
    let project = fixture
        .parse(&document("", &link("1.0", "data-core-metadata=true")))
        .unwrap();
    assert!(
        matches!(project.files()[0].core_metadata(), CoreMetadataAvailability::Available { hashes } if hashes.is_empty())
    );
    let attrs = format!(
        "data-core-metadata='sha256={}' data-dist-info-metadata='sha256={}'",
        "ab".repeat(32),
        "cd".repeat(32)
    );
    let project = fixture.parse(&document("", &link("1.0", &attrs))).unwrap();
    assert!(
        matches!(project.files()[0].core_metadata(), CoreMetadataAvailability::Available { hashes } if hashes.preferred_digest()==Some(Digest::Sha256([0xab;32])))
    );
    let attrs = format!("data-dist-info-metadata='sha512={}'", "cd".repeat(64));
    let project = fixture.parse(&document("", &link("1.0", &attrs))).unwrap();
    assert!(
        matches!(project.files()[0].core_metadata(), CoreMetadataAvailability::Available { hashes } if hashes.preferred_digest()==Some(Digest::Sha512([0xcd;64])))
    );
    invalid(fixture.parse(&document("", &link("1.0", "data-core-metadata=false"))));
    invalid(fixture.parse(&document(
        "",
        &link(
            "1.0",
            "data-core-metadata='sha256=bad' data-dist-info-metadata=true",
        ),
    )));
}

#[test]
fn first_base_href_changes_locators_without_rebinding_the_registry() {
    let fixture = Fixture::new();
    let head = r#"<template><base href="https://inert.example/"></template><base href="https://cdn.example/packages/"><base href="https://unused.example/">"#;
    let body = format!(
        "<a href='{NAME}?token=synthetic-query-secret#sha256={}'>{NAME}</a>",
        "ab".repeat(32)
    );
    let project = fixture.parse(&document(head, &body)).unwrap();
    assert_eq!(project.source().as_str(), "corp");
    assert_eq!(project.name().as_str(), "demo-pkg");
    assert_eq!(
        project.files()[0].locator().url().as_str(),
        format!("https://cdn.example/packages/{NAME}?token=synthetic-query-secret")
    );
    assert!(!format!("{:?}", project.files()[0].locator()).contains("synthetic-query-secret"));
    let body = format!(
        "<a href='../../files/%64emo_pkg-1.0-py3-none-any.whl#sha256={}'>{NAME}</a>",
        "ab".repeat(32)
    );
    assert_eq!(
        fixture.parse(&document("", &body)).unwrap().files()[0]
            .locator()
            .url()
            .path(),
        "/tenant/files/%64emo_pkg-1.0-py3-none-any.whl"
    );
    let relative = format!("<a href='{NAME}'>{NAME}</a>");
    for first in ["http://[", "data:text/plain,ignored", "javascript:ignored"] {
        let head = format!(
            "<base target='_blank'><base href='{first}'><base href='https://unused.example/'>"
        );
        let project = fixture.parse(&document(&head, &relative)).unwrap();
        assert_eq!(
            project.files()[0].locator().url().as_str(),
            format!("https://index.example/tenant/simple/demo-pkg/{NAME}")
        );
    }
}

#[test]
fn inert_html_content_cannot_invent_distribution_links() {
    let fixture = Fixture::new();
    let fake = r#"<a href="file:///synthetic-inert-secret/bad.whl#sha256=bad">bad.whl</a>"#;
    let head = format!(
        "<!--{fake}--><style>fake {{ content:'{fake}'; }}</style><script>document.write('{fake}');</script>"
    );
    let body = format!(
        "<template>{fake}<template>{fake}</template></template>{}",
        link("1.0", "")
    );
    let project = fixture.parse(&document(&head, &body)).unwrap();
    assert_eq!(project.files().len(), 1);
    assert_eq!(project.files()[0].filename(), NAME);
    assert!(!format!("{project:?}").contains("synthetic-inert-secret"));
}

#[test]
fn foreign_namespace_anchors_do_not_hide_html_integration_points() {
    let fixture = Fixture::new();
    let fake = r#"<a href="file:///not-a-distribution/bad.whl">bad.whl</a>"#;
    let body = format!(
        "<svg>{fake}<foreignObject>{}</foreignObject></svg><math>{fake}</math>{}",
        link("2.0", ""),
        link("1.0", "")
    );
    let project = fixture.parse(&document("", &body)).unwrap();
    assert_eq!(project.files().len(), 2);
    assert!(
        project
            .files()
            .iter()
            .any(|file| file.version() == Some("2.0"))
    );
    assert!(
        project
            .files()
            .iter()
            .any(|file| file.version() == Some("1.0"))
    );
}

#[test]
fn duplicate_critical_html_attributes_are_rejected() {
    let fixture = Fixture::new();
    for attrs in [
        r#"href="https://files.example/demo_pkg-1.0-py3-none-any.whl""#,
        r#"data-requires-python=">=3" data-requires-python=">=3""#,
        "data-core-metadata=true data-core-metadata=true",
    ] {
        invalid(fixture.parse(&document("", &link("1.0", attrs))));
    }
}

#[test]
fn malformed_html_file_facts_never_fall_back_to_sdist() {
    let fixture = Fixture::new();
    let source = r#"<a href="https://files.example/demo_pkg-1.0.tar.gz">demo_pkg-1.0.tar.gz</a>"#;
    for bad in [
        r#"<a href="https://files.example/bad.whl" data-requires-python="&lt;3">bad.whl</a>"#,
        r#"<a href="https://files.example/foreign-1.0-py3-none-any.whl">foreign-1.0-py3-none-any.whl</a>"#,
        r#"<a href="https://user:synthetic-url-secret@files.example/demo_pkg-1.0-py3-none-any.whl">demo_pkg-1.0-py3-none-any.whl</a>"#,
        r#"<a href="https://files.example/demo_pkg-1.0-py3-none-any.whl">different.whl</a>"#,
    ] {
        let error = fixture
            .parse(&document("", &format!("{source}{bad}")))
            .unwrap_err();
        assert!(matches!(error, SimpleError::InvalidResponse(_)));
        assert!(!format!("{error:?} {error}").contains("synthetic-url-secret"));
    }
}

#[test]
fn html_missing_or_unknown_hashes_remain_discovery_facts() {
    let fixture = Fixture::new();
    for fragment in ["", "#customhash=abcd", "#md5=abcd"] {
        let body = format!("<a href='https://files.example/{NAME}{fragment}'>{NAME}</a>");
        let project = fixture.parse(&document("", &body)).unwrap();
        assert_eq!(project.files()[0].hashes().preferred_digest(), None);
    }
    let body = format!("<a href='https://files.example/{NAME}#sha256=bad'>{NAME}</a>");
    invalid(fixture.parse(&document("", &body)));
}

#[test]
fn html_extension_facts_do_not_add_json_only_requirements() {
    let fixture = Fixture::new();
    let head = r#"<meta name="pypi:repository-version" content="1.4"><meta name="pypi:tracks" content="https://upstream.example/simple/demo-pkg/"><meta name="pypi:alternate-locations" content="https://alternate.example/simple/demo-pkg/"><meta name="pypi:project-status" content="archived"><meta name="pypi:project-status-reason" content="synthetic-status-secret &amp; reason">"#;
    let project = fixture
        .parse(&document(
            head,
            &link(
                "1.0",
                r#"data-provenance="https://proof.example/statement?token=synthetic-proof-secret""#,
            ),
        ))
        .unwrap();
    assert_eq!(project.api_version().minor(), 4);
    assert!(project.versions().is_empty());
    assert_eq!(project.files()[0].size(), None);
    assert_eq!(
        project.status().unwrap().kind(),
        ProjectStatusKind::Archived
    );
    assert_eq!(
        project.status().unwrap().reason(),
        Some("synthetic-status-secret & reason")
    );
    assert_eq!(project.relations().tracks().len(), 1);
    assert_eq!(project.relations().alternate_locations().len(), 1);
    assert_eq!(
        project.files()[0].provenance().unwrap().url().host_str(),
        Some("proof.example")
    );
    for output in [
        format!("{project:?}"),
        format!("{:?}", project.status()),
        format!("{:?}", project.files()[0].provenance()),
    ] {
        assert!(!output.contains("synthetic-status-secret"));
        assert!(!output.contains("synthetic-proof-secret"));
    }
}

#[test]
fn duplicate_html_file_facts_are_order_independent() {
    let fixture = Fixture::new();
    let first = link("1.0", "");
    let project = fixture
        .parse(&document("", &format!("{first}{first}")))
        .unwrap();
    assert_eq!(project.files().len(), 1);
    let other = link("1.0", "data-yanked");
    for body in [format!("{first}{other}"), format!("{other}{first}")] {
        assert!(matches!(
            fixture.parse(&document("", &body)),
            Err(SimpleError::ConflictingFile)
        ));
    }
}

#[test]
fn html_common_limits_count_decoded_utf8_bytes() {
    let fixture = Fixture::new();
    let text = document("", &link("1.0", "data-yanked='中文'"));
    let exact = SimpleLimits {
        max_response_bytes: text.len() as u64,
        ..SimpleLimits::default()
    };
    assert!(parse_simple_html(&text, fixture.context(exact), SimpleHtmlLimits::default()).is_ok());
    let chars = SimpleLimits {
        max_response_bytes: text.chars().count() as u64,
        ..SimpleLimits::default()
    };
    assert!(matches!(
        parse_simple_html(&text, fixture.context(chars), SimpleHtmlLimits::default()),
        Err(SimpleError::LimitExceeded(SimpleLimitKind::ResponseBytes))
    ));
    let zero = SimpleLimits {
        max_response_bytes: 0,
        ..SimpleLimits::default()
    };
    assert!(matches!(
        parse_simple_html("", fixture.context(zero), SimpleHtmlLimits::default()),
        Err(SimpleError::InvalidLimits)
    ));
    for field in 0..8 {
        let mut limits = SimpleHtmlLimits::default();
        match field {
            0 => limits.max_tokens = 0,
            1 => limits.max_tree_nodes = 0,
            2 => limits.max_tree_depth = 0,
            3 => limits.max_tree_text_bytes = 0,
            4 => limits.max_tree_operations = 0,
            5 => limits.max_attributes_per_tag = 0,
            6 => limits.max_anchors = 0,
            _ => limits.feed_chunk_bytes = 0,
        }
        assert!(matches!(
            parse_simple_html("", fixture.context(SimpleLimits::default()), limits),
            Err(SimpleError::InvalidLimits)
        ));
    }
}

#[test]
fn html_tree_and_token_budgets_return_typed_errors() {
    let fixture = Fixture::new();
    for kind in [
        SimpleLimitKind::HtmlTokens,
        SimpleLimitKind::HtmlTreeNodes,
        SimpleLimitKind::HtmlTreeDepth,
        SimpleLimitKind::HtmlTreeTextBytes,
        SimpleLimitKind::HtmlTreeOperations,
        SimpleLimitKind::HtmlAttributes,
        SimpleLimitKind::HtmlAnchors,
        SimpleLimitKind::Files,
        SimpleLimitKind::FieldBytes,
    ] {
        let mut limits = SimpleHtmlLimits::default();
        let mut common = SimpleLimits::default();
        let body = format!(
            "{}{}",
            link("1.0", r#"data-requires-python=">=3""#),
            link("2.0", "")
        );
        match kind {
            SimpleLimitKind::HtmlTokens => limits.max_tokens = 1,
            SimpleLimitKind::HtmlTreeNodes => limits.max_tree_nodes = 1,
            SimpleLimitKind::HtmlTreeDepth => limits.max_tree_depth = 1,
            SimpleLimitKind::HtmlTreeTextBytes => limits.max_tree_text_bytes = 4,
            SimpleLimitKind::HtmlTreeOperations => limits.max_tree_operations = 1,
            SimpleLimitKind::HtmlAttributes => limits.max_attributes_per_tag = 1,
            SimpleLimitKind::HtmlAnchors => limits.max_anchors = 1,
            SimpleLimitKind::Files => common.max_files = 1,
            SimpleLimitKind::FieldBytes => common.max_field_bytes = 8,
            _ => unreachable!(),
        }
        let result = parse_simple_html(&document("", &body), fixture.context(common), limits);
        assert!(
            matches!(result, Err(SimpleError::LimitExceeded(observed)) if observed==kind),
            "wrong budget error for {kind:?}"
        );
    }
}

#[test]
fn html_budget_failure_stops_future_feeds_and_tree_delegation() {
    let fixture = Fixture::new();
    let text = document("", &link("1.0", "").repeat(100));
    let limits = SimpleHtmlLimits {
        max_tokens: 2,
        feed_chunk_bytes: 16,
        ..SimpleHtmlLimits::default()
    };
    let (result, trace) =
        parse_html_with_trace(&text, fixture.context(SimpleLimits::default()), limits);
    assert!(matches!(
        result,
        Err(SimpleError::LimitExceeded(SimpleLimitKind::HtmlTokens))
    ));
    assert!(trace.fed_chunks > 0);
    assert_eq!(Some(trace.fed_chunks), trace.fed_chunks_at_error);
    assert_eq!(
        Some(trace.delegated_tokens),
        trace.delegated_tokens_at_error
    );
    assert!(
        !trace.ended_after_error,
        "do not end/finish a failed document"
    );
    assert!(
        trace.fed_chunks < text.len().div_ceil(limits.feed_chunk_bytes),
        "driver consumed the whole page after its error"
    );
}

#[test]
fn html_static_client_keeps_noscript_fallback_links() {
    let fixture = Fixture::new();
    let body = format!(
        "<script>throw new Error('must not execute')</script><noscript>{}</noscript>",
        link("1.0", "")
    );
    let project = fixture.parse(&document("", &body)).unwrap();
    assert_eq!(project.files().len(), 1);
    assert_eq!(project.files()[0].filename(), NAME);
}

#[test]
fn html_anchor_filenames_still_match_the_decoded_url_path_component() {
    let fixture = Fixture::new();
    for locator in [
        "https://files.example/download?id=opaque-object",
        "https://files.example/foreign-1.0-py3-none-any.whl",
        "https://files.example/demo%2Fpkg-1.0-py3-none-any.whl",
    ] {
        invalid(fixture.parse(&document("", &format!("<a href='{locator}'>{NAME}</a>"))));
    }
}
