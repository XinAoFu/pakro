use super::*;

use std::collections::BTreeSet;

use serde_json::{Value, json};

/// Read only explicitly supplied public response captures. No requests, raw
/// response bodies, locators or credentials are printed by this diagnostic.
#[test]
#[ignore = "requires explicit PAKRO_ISAT_SIMPLE_PROBE_DIR with public JSON captures"]
fn s4_isat_simple_probe() {
    use std::str::FromStr;

    let directory = std::env::var_os("PAKRO_ISAT_SIMPLE_PROBE_DIR")
        .expect("set PAKRO_ISAT_SIMPLE_PROBE_DIR to the explicit capture directory");
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
    let mut failures = Vec::new();
    for path in paths {
        let requested = path.file_stem().unwrap().to_str().unwrap();
        let project = NormalizedPythonName::new(requested).unwrap();
        assert_eq!(project.as_str(), requested);
        let file = std::fs::File::open(&path).unwrap();
        assert!(file.metadata().unwrap().len() <= SimpleLimits::default().max_response_bytes);
        let bytes = std::fs::read(&path).unwrap();
        let registry = PythonRegistry::new("pypi", "https://pypi.org/simple/", None).unwrap();
        let document = registry.project_url(&project).unwrap();
        let parse = |bytes: &[u8]| {
            parse_simple_json(
                bytes,
                SimpleParseContext {
                    registry: &registry,
                    project: &project,
                    canonical_document_url: &document,
                    limits: SimpleLimits::default(),
                },
            )
        };
        match parse(&bytes) {
            Ok(observed) => {
                eprintln!(
                    "{requested}: OK files={} versions={} warnings={:?}",
                    observed.files().len(),
                    observed.versions().len(),
                    observed.warnings()
                );
                if !observed
                    .warnings()
                    .contains(&SimpleWarning::VersionListingMismatch)
                {
                    continue;
                }
                let declared = observed
                    .versions()
                    .iter()
                    .filter_map(|version| pep440_rs::Version::from_str(version).ok())
                    .collect::<BTreeSet<_>>();
                for file in observed.files() {
                    let Some(version) = file.version() else {
                        continue;
                    };
                    let parsed = pep440_rs::Version::from_str(version).unwrap();
                    if !declared.contains(&parsed) {
                        eprintln!(
                            "{requested}: filename={:?} parsed_version={version:?} declared_member=false",
                            file.filename()
                        );
                    }
                }
            }
            Err(error) => {
                eprintln!("{requested}: {error}");
                failures.push(project.as_str().to_owned());
            }
        }
    }
    assert!(
        failures.is_empty(),
        "public response failures: {failures:?}"
    );
}

struct Fixture {
    registry: PythonRegistry,
    project: NormalizedPythonName,
    document: CanonicalProjectUrl,
}
impl Fixture {
    fn new() -> Self {
        let registry =
            PythonRegistry::new("corp", "https://index.example/tenant/simple/", None).unwrap();
        let project = NormalizedPythonName::new("Demo_Pkg").unwrap();
        let document = registry.project_url(&project).unwrap();
        Self {
            registry,
            project,
            document,
        }
    }
    fn raw(&self, body: &[u8], limits: SimpleLimits) -> SimpleResult<SimpleProject> {
        parse_simple_json(
            body,
            SimpleParseContext {
                registry: &self.registry,
                project: &self.project,
                canonical_document_url: &self.document,
                limits,
            },
        )
    }
    fn parse(&self, value: Value) -> SimpleResult<SimpleProject> {
        self.raw(
            &serde_json::to_vec(&value).unwrap(),
            SimpleLimits::default(),
        )
    }
}
fn wheel(version: &str) -> Value {
    let filename = format!("demo_pkg-{version}-py3-none-any.whl");
    json!({
        "filename": filename,
        "url": format!("https://files.example/packages/{filename}"),
        "hashes": {"sha256": "ab".repeat(32)},
        "size": 123,
    })
}
fn page(api: &str, files: Vec<Value>) -> Value {
    let mut value = json!({"meta": {"api-version": api}, "name": "demo-pkg", "files": files});
    if api != "1.0" {
        value["versions"] = json!(["1.0"]);
    }
    value
}
fn invalid_response(result: SimpleResult<SimpleProject>) {
    let error = result.unwrap_err();
    assert!(
        matches!(error, SimpleError::InvalidResponse(_)),
        "{error:?}"
    );
}

#[test]
fn json_10_preserves_file_level_identity() {
    let fixture = Fixture::new();
    let mut file = wheel("1.0");
    file.as_object_mut().unwrap().remove("size");
    let project = fixture.parse(page("1.0", vec![file])).unwrap();
    assert_eq!(project.name().as_str(), "demo-pkg");
    assert_eq!(project.source().as_str(), "corp");
    assert_eq!(
        (project.api_version().major(), project.api_version().minor()),
        (1, 0)
    );
    assert!(
        project.versions().is_empty(),
        "do not invent an advertised versions set"
    );
    assert!(project.warnings().is_empty());
    assert_eq!(project.files().len(), 1);
    let file = &project.files()[0];
    assert_eq!(file.filename(), "demo_pkg-1.0-py3-none-any.whl");
    assert_eq!(file.version(), Some("1.0"));
    assert!(
        matches!(file.kind(), PythonDistributionKind::Wheel(wheel) if wheel.name == "demo_pkg" && wheel.version == "1.0")
    );
    assert_eq!(file.size(), None);
    assert_eq!(
        file.hashes().preferred_digest(),
        Some(Digest::Sha256([0xab; 32]))
    );
    assert!(matches!(
        file.core_metadata(),
        CoreMetadataAvailability::Unspecified
    ));
}

#[test]
fn api_version_fail_warn_policy_is_explicit() {
    let fixture = Fixture::new();
    let project = fixture.parse(page("1.99", vec![wheel("1.0")])).unwrap();
    assert_eq!(
        project.warnings(),
        &[SimpleWarning::NewerApiMinor {
            major: 1,
            minor: 99
        }]
    );
    let error = fixture.parse(page("2.0", vec![wheel("1.0")])).unwrap_err();
    assert!(matches!(error, SimpleError::UnsupportedApiMajor(2)));
    let normalized = fixture.parse(page("01.0", vec![wheel("1.0")])).unwrap();
    assert_eq!(normalized.api_version().major(), 1);
    assert_eq!(normalized.api_version().minor(), 0);
    for version in ["1", "1.0.0", "1.x", "-1.0", "1.0rc1"] {
        invalid_response(fixture.parse(page(version, vec![wheel("1.0")])));
    }
}

#[test]
fn extension_facts_do_not_trigger_source_discovery() {
    let fixture = Fixture::new();
    let mut value = page("1.4", vec![wheel("1.0")]);
    value["meta"]["tracks"] = json!(["https://upstream.example/simple/demo-pkg/"]);
    value["alternate-locations"] = json!(["https://alternate.example/simple/demo-pkg/"]);
    value["project-status"] = json!({"status": "deprecated", "reason": "synthetic-reason-secret"});
    value["files"][0]["provenance"] =
        json!("https://proof.example/attestation?token=synthetic-proof-secret");
    value["_future"] = json!({"arbitrary": [false, null, {"new": "ignored"}]});
    value["files"][0]["future-minor-file-field"] = json!([1, 2, 3]);
    let project = fixture.parse(value.clone()).unwrap();
    assert_eq!(
        project.relations().tracks()[0].url().host_str(),
        Some("upstream.example")
    );
    assert_eq!(
        project.relations().alternate_locations()[0]
            .url()
            .host_str(),
        Some("alternate.example")
    );
    assert_eq!(
        project.status().unwrap().kind(),
        ProjectStatusKind::Deprecated
    );
    assert_eq!(
        project.status().unwrap().reason(),
        Some("synthetic-reason-secret")
    );
    assert!(project.files()[0].provenance().unwrap().is_ephemeral());
    for output in [
        format!("{project:?}"),
        format!("{:?}", project.files()[0]),
        format!("{:?}", project.files()[0].provenance()),
        format!("{:?}", project.status()),
    ] {
        for secret in [
            "synthetic-reason-secret",
            "synthetic-proof-secret",
            "proof.example",
        ] {
            assert!(
                !output.contains(secret),
                "private extension fact leaked: {output}"
            );
        }
    }
    value["project-status"]["status"] = json!("future-status");
    let unknown = fixture.parse(value.clone()).unwrap();
    assert_eq!(unknown.status().unwrap().kind(), ProjectStatusKind::Unknown);
    assert_eq!(unknown.status().unwrap().raw(), "future-status");
    assert!(
        unknown
            .warnings()
            .contains(&SimpleWarning::UnknownProjectStatus)
    );
    for (field, replacement) in [
        (
            "alternate-locations",
            json!(["https://alternate.example/simple/foreign/"]),
        ),
        ("project-status", json!({"status": false})),
    ] {
        let mut invalid = value.clone();
        invalid[field] = replacement;
        invalid_response(fixture.parse(invalid));
    }
    value["files"][0]["provenance"] = json!("http://proof.example/not-secure");
    invalid_response(fixture.parse(value));
}

#[test]
fn required_json_schema_is_not_a_warehouse_release_response() {
    let fixture = Fixture::new();
    for key in ["meta", "name", "files"] {
        let mut value = page("1.0", vec![wheel("1.0")]);
        value.as_object_mut().unwrap().remove(key);
        invalid_response(fixture.parse(value));
    }
    for value in [
        json!({"info": {"name": "demo-pkg", "version": "1.0"}, "urls": []}),
        json!({"meta": {}, "name": "demo-pkg", "files": []}),
        json!({"meta": {"api-version": 1}, "name": "demo-pkg", "files": []}),
        json!({"meta": {"api-version": "1.0"}, "name": "foreign", "files": []}),
        json!({"meta": {"api-version": "1.0"}, "name": "demo-pkg", "files": {}}),
    ] {
        invalid_response(fixture.parse(value));
    }
    for key in ["filename", "url", "hashes"] {
        let mut file = wheel("1.0");
        file.as_object_mut().unwrap().remove(key);
        invalid_response(fixture.parse(page("1.0", vec![file])));
    }
    for body in [b"[]".as_slice(), b"null", b"{\"name\":", b"{} {}"] {
        let error = fixture.raw(body, SimpleLimits::default()).unwrap_err();
        assert!(
            matches!(
                error,
                SimpleError::InvalidJson | SimpleError::InvalidResponse(_)
            ),
            "{error:?}"
        );
    }
}

#[test]
fn explicit_document_context_cannot_change_source() {
    let fixture = Fixture::new();
    let other = PythonRegistry::new("other", "https://other.example/simple/", None).unwrap();
    let wrong_document = other.project_url(&fixture.project).unwrap();
    let body = serde_json::to_vec(&page("1.0", vec![wheel("1.0")])).unwrap();
    let error = parse_simple_json(
        &body,
        SimpleParseContext {
            registry: &fixture.registry,
            project: &fixture.project,
            canonical_document_url: &wrong_document,
            limits: SimpleLimits::default(),
        },
    )
    .unwrap_err();
    assert!(matches!(error, SimpleError::InvalidContext));
    assert!(!format!("{error:?} {error}").contains("other.example"));
}

#[test]
fn relative_locators_preserve_queries_without_hash_fragments() {
    let fixture = Fixture::new();
    let mut relative = wheel("1.0");
    relative["url"] = json!(format!(
        "../../files/demo_pkg-1.0-py3-none-any.whl#sha256={}",
        "ab".repeat(32)
    ));
    let mut signed = wheel("2.0");
    signed["url"] =
        json!("https://cdn.example/demo_pkg-2.0-py3-none-any.whl?token=synthetic-query-secret");
    let mut encoded = wheel("3.0");
    encoded["url"] = json!("https://cdn.example/%64emo_pkg-3.0-py3-none-any.whl");
    let project = fixture
        .parse(page("1.0", vec![relative, signed, encoded]))
        .unwrap();
    let first = project
        .files()
        .iter()
        .find(|file| file.version() == Some("1.0"))
        .unwrap();
    assert_eq!(
        first.locator().url().as_str(),
        "https://index.example/tenant/files/demo_pkg-1.0-py3-none-any.whl"
    );
    assert!(first.locator().url().fragment().is_none());
    let signed = project
        .files()
        .iter()
        .find(|file| file.version() == Some("2.0"))
        .unwrap();
    assert_eq!(
        signed.locator().url().query(),
        Some("token=synthetic-query-secret")
    );
    assert!(signed.locator().is_ephemeral());
    let metadata = signed.locator().metadata_locator().unwrap();
    assert_eq!(
        metadata.url().path(),
        "/demo_pkg-2.0-py3-none-any.whl.metadata"
    );
    assert_eq!(metadata.url().query(), Some("token=synthetic-query-secret"));
    assert!(!format!("{:?} {metadata:?}", signed.locator()).contains("synthetic-query-secret"));
}

#[test]
fn unsafe_file_locator_facts_are_rejected_without_echo() {
    let fixture = Fixture::new();
    for url in [
        "file:///synthetic-locator-secret/demo_pkg-1.0-py3-none-any.whl",
        "https://user:synthetic-locator-secret@files.example/demo_pkg-1.0-py3-none-any.whl",
        "https://@files.example/demo_pkg-1.0-py3-none-any.whl",
        "https://files.example/demo_pkg-1.0-py3-none-any.whl#token=synthetic-locator-secret",
    ] {
        let mut file = wheel("1.0");
        file["url"] = json!(url);
        let error = fixture.parse(page("1.0", vec![file])).unwrap_err();
        assert!(
            matches!(error, SimpleError::InvalidResponse(_)),
            "{error:?}"
        );
        assert!(!format!("{error:?} {error}").contains("synthetic-locator-secret"));
    }
    for filename in [
        "../escape.whl",
        "one/two.whl",
        "one\\two.whl",
        "bad\n.whl",
        "bad\0.whl",
    ] {
        let mut file = wheel("1.0");
        file["filename"] = json!(filename);
        invalid_response(fixture.parse(page("1.0", vec![file])));
    }
}

#[test]
fn all_wheel_facts_are_checked_before_any_candidate_filter() {
    let fixture = Fixture::new();
    let mut sdist = wheel("1.0");
    sdist["filename"] = json!("demo_pkg-1.0.tar.gz");
    sdist["url"] = json!("https://files.example/demo_pkg-1.0.tar.gz");
    let mut historical = wheel("1.0");
    historical["filename"] = json!("demo_pkg-0.8-py3.egg");
    historical["url"] = json!("https://files.example/demo_pkg-0.8-py3.egg");
    let project = fixture
        .parse(page("1.0", vec![sdist.clone(), historical]))
        .unwrap();
    assert!(
        project
            .files()
            .iter()
            .any(|file| matches!(file.kind(), PythonDistributionKind::Sdist)
                && file.version() == Some("1.0"))
    );
    assert!(
        project
            .files()
            .iter()
            .any(|file| matches!(file.kind(), PythonDistributionKind::HistoricalFormat))
    );
    for name in [
        "bad.whl",
        "foreign-1.0-py3-none-any.whl",
        "demo_pkg-1.0-build-py3-none-any.whl",
    ] {
        let mut invalid = wheel("1.0");
        invalid["filename"] = json!(name);
        invalid["url"] = json!(format!("https://files.example/{name}"));
        invalid["requires-python"] = json!("<3.0");
        invalid_response(fixture.parse(page("1.0", vec![sdist.clone(), invalid])));
    }
}

#[test]
fn p6_zip_sdist_facts_keep_exact_project_version_and_original_basename() {
    let fixture = Fixture::new();
    for filename in ["demo_pkg-1.0.zip", "demo-pkg-1.0.zip"] {
        let mut file = wheel("1.0");
        file["filename"] = json!(filename);
        file["url"] = json!(format!("https://files.example/{filename}"));
        let project = fixture.parse(page("1.0", vec![file])).unwrap();
        let file = &project.files()[0];
        assert!(matches!(file.kind(), PythonDistributionKind::Sdist));
        assert_eq!(file.version(), Some("1.0"));
        assert_eq!(file.filename(), filename);
    }
    let mut foreign = wheel("1.0");
    foreign["filename"] = json!("foreign-1.0.zip");
    foreign["url"] = json!("https://files.example/foreign-1.0.zip");
    invalid_response(fixture.parse(page("1.0", vec![foreign])));
    for filename in ["demo_pkg-1.0-1.zip", "demo_pkg-1.0.tar.xz"] {
        let mut file = wheel("1.0");
        file["filename"] = json!(filename);
        file["url"] = json!(format!("https://files.example/{filename}"));
        let project = fixture.parse(page("1.0", vec![file])).unwrap();
        assert!(matches!(
            project.files()[0].kind(),
            PythonDistributionKind::HistoricalFormat
        ));
        assert_eq!(project.files()[0].version(), None);
    }
}

#[test]
fn supported_hashes_have_validated_digest_identities() {
    let fixture = Fixture::new();
    let mut file = wheel("1.0");
    file["hashes"] =
        json!({"sha256": "AB".repeat(32), "sha512": "CD".repeat(64), "md5": "ef".repeat(16)});
    let project = fixture.parse(page("1.0", vec![file.clone()])).unwrap();
    let hashes = project.files()[0].hashes();
    assert_eq!(hashes.len(), 3);
    assert_eq!(hashes.get("sha256"), Some("ab".repeat(32).as_str()));
    assert_eq!(hashes.get("sha512"), Some("cd".repeat(64).as_str()));
    assert_eq!(hashes.preferred_digest(), Some(Digest::Sha256([0xab; 32])));
    file["hashes"] = json!({"sha512": "CD".repeat(64)});
    assert_eq!(
        fixture.parse(page("1.0", vec![file])).unwrap().files()[0]
            .hashes()
            .preferred_digest(),
        Some(Digest::Sha512([0xcd; 64]))
    );
}

#[test]
fn absent_supported_hashes_are_valid_discovery_facts() {
    let fixture = Fixture::new();
    for hashes in [
        json!({}),
        json!({"customhash": "abcd"}),
        json!({"md5": "ab".repeat(16)}),
    ] {
        let mut file = wheel("1.0");
        file["hashes"] = hashes.clone();
        let project = fixture.parse(page("1.0", vec![file])).unwrap();
        assert_eq!(project.files()[0].hashes().preferred_digest(), None);
        assert_eq!(
            project.files()[0].hashes().len(),
            hashes.as_object().unwrap().len()
        );
    }
}

#[test]
fn malformed_hashes_cannot_become_missing_expected_integrity() {
    let fixture = Fixture::new();
    for hashes in [
        json!({"sha256": "ab"}),
        json!({"sha512": "ab".repeat(32)}),
        json!({"sha256": "z".repeat(64)}),
        json!({"sha256": false}),
        json!({"customhash": "not-hex"}),
        json!({"": "abcd"}),
        json!({"sha256": "ab".repeat(32), "SHA256": "cd".repeat(32)}),
    ] {
        let mut file = wheel("1.0");
        file["hashes"] = hashes;
        invalid_response(fixture.parse(page("1.0", vec![file])));
    }
    let mut file = wheel("1.0");
    file["url"] = json!(format!(
        "https://files.example/demo_pkg-1.0-py3-none-any.whl#sha256={}",
        "cd".repeat(32)
    ));
    invalid_response(fixture.parse(page("1.0", vec![file])));
}

#[test]
fn requires_python_is_validated_as_unfiltered_file_metadata() {
    let fixture = Fixture::new();
    for (raw, expected) in [
        (json!(">=3.13,<3.14"), Some(">=3.13,<3.14")),
        (Value::Null, None),
        (json!(""), Some("")),
    ] {
        let mut file = wheel("1.0");
        file["requires-python"] = raw;
        let project = fixture.parse(page("1.0", vec![file])).unwrap();
        assert_eq!(project.files()[0].requires_python(), expected);
    }
    for invalid in [json!(">=3.invalid"), json!(false), json!([">=3"])] {
        let mut file = wheel("1.0");
        file["requires-python"] = invalid;
        invalid_response(fixture.parse(page("1.0", vec![file])));
    }
}

#[test]
fn yanked_file_state_retains_its_reason_privately() {
    let fixture = Fixture::new();
    for (raw, yanked, reason) in [
        (json!(false), false, None),
        (json!(true), true, None),
        (
            json!("synthetic-yank-secret"),
            true,
            Some("synthetic-yank-secret"),
        ),
    ] {
        let mut file = wheel("1.0");
        file["yanked"] = raw;
        let project = fixture.parse(page("1.0", vec![file])).unwrap();
        let state = project.files()[0].yanked();
        assert_eq!(state.is_yanked(), yanked);
        assert_eq!(state.reason(), reason);
        assert!(!format!("{state:?}").contains("synthetic-yank-secret"));
    }
    for invalid in [Value::Null, json!(""), json!(1)] {
        let mut file = wheel("1.0");
        file["yanked"] = invalid;
        invalid_response(fixture.parse(page("1.0", vec![file])));
    }
}

#[test]
fn core_metadata_prefers_the_new_field_over_the_legacy_alias() {
    let fixture = Fixture::new();
    for (new, legacy, expected) in [
        (None, None, 0),
        (Some(json!(false)), None, 1),
        (Some(json!({})), None, 1),
        (Some(json!(true)), None, 2),
        (None, Some(json!({"sha256": "cd".repeat(32)})), 3),
        (Some(json!(false)), Some(json!(true)), 1),
        (
            Some(json!({"sha256": "ab".repeat(32)})),
            Some(json!({"sha256": "cd".repeat(32)})),
            3,
        ),
    ] {
        let mut file = wheel("1.0");
        if let Some(new) = new {
            file["core-metadata"] = new;
        }
        if let Some(legacy) = legacy {
            file["dist-info-metadata"] = legacy;
        }
        let project = fixture.parse(page("1.0", vec![file])).unwrap();
        match (expected, project.files()[0].core_metadata()) {
            (0, CoreMetadataAvailability::Unspecified) | (1, CoreMetadataAvailability::Absent) => {}
            (2, CoreMetadataAvailability::Available { hashes }) => assert!(hashes.is_empty()),
            (3, CoreMetadataAvailability::Available { hashes }) => {
                assert!(hashes.preferred_digest().is_some())
            }
            (_, observed) => panic!("wrong metadata availability: {observed:?}"),
        }
    }
    let mut file = wheel("1.0");
    file["core-metadata"] = json!({"sha256": "ab".repeat(32)});
    file["dist-info-metadata"] = json!({"sha256": "cd".repeat(32)});
    let project = fixture.parse(page("1.0", vec![file.clone()])).unwrap();
    assert!(
        matches!(project.files()[0].core_metadata(), CoreMetadataAvailability::Available { hashes } if hashes.preferred_digest() == Some(Digest::Sha256([0xab; 32])))
    );
    file["core-metadata"] = json!([]);
    file["dist-info-metadata"] = json!(true);
    invalid_response(fixture.parse(page("1.0", vec![file])));
}

#[test]
fn duplicate_file_conflicts_are_not_response_order_dependent() {
    let fixture = Fixture::new();
    let file = wheel("1.0");
    assert_eq!(
        fixture
            .parse(page("1.0", vec![file.clone(), file.clone()]))
            .unwrap()
            .files()
            .len(),
        1
    );
    for (field, alternative) in [
        (
            "url",
            json!("https://mirror.example/demo_pkg-1.0-py3-none-any.whl"),
        ),
        ("hashes", json!({"sha256": "cd".repeat(32)})),
        ("requires-python", json!(">=3.13")),
        ("yanked", json!(true)),
        ("core-metadata", json!(true)),
        ("size", json!(124)),
        ("upload-time", json!("2026-10-15T01:02:03Z")),
        ("provenance", json!("https://proof.example/attestation")),
    ] {
        let mut other = file.clone();
        other[field] = alternative;
        for files in [
            vec![file.clone(), other.clone()],
            vec![other.clone(), file.clone()],
        ] {
            assert!(
                matches!(
                    fixture.parse(page("1.0", files)),
                    Err(SimpleError::ConflictingFile)
                ),
                "field {field}"
            );
        }
    }
}

#[test]
fn duplicate_critical_json_keys_are_rejected() {
    let fixture = Fixture::new();
    let file = serde_json::to_string(&wheel("1.0")).unwrap();
    let cases = [
        format!(
            r#"{{"meta":{{"api-version":"1.0","api-version":"1.0"}},"name":"demo-pkg","files":[{file}]}}"#
        ),
        format!(
            r#"{{"meta":{{"api-version":"1.0"}},"name":"demo-pkg","name":"demo-pkg","files":[{file}]}}"#
        ),
        format!(
            r#"{{"meta":{{"api-version":"1.0"}},"name":"demo-pkg","files":[{file}],"files":[]}}"#
        ),
        r#"{"meta":{"api-version":"1.0"},"name":"demo-pkg","files":[{"filename":"demo_pkg-1.0-py3-none-any.whl","filename":"demo_pkg-1.0-py3-none-any.whl","url":"https://files.example/demo_pkg-1.0-py3-none-any.whl","hashes":{}}]}"#.to_owned(),
        format!(
            r#"{{"meta":{{"api-version":"1.0"}},"name":"demo-pkg","files":[{{"filename":"demo_pkg-1.0-py3-none-any.whl","url":"https://files.example/demo_pkg-1.0-py3-none-any.whl","hashes":{{"sha256":"{}","sha256":"{}"}}}}]}}"#,
            "ab".repeat(32),
            "ab".repeat(32)
        ),
    ];
    for body in cases {
        invalid_response(fixture.raw(body.as_bytes(), SimpleLimits::default()));
    }
}

#[test]
fn versioned_size_time_and_version_sets_are_consistent() {
    let fixture = Fixture::new();
    let mut file = wheel("1.0");
    file["upload-time"] = json!("2024-02-29T23:59:59.123456Z");
    let mut value = page("1.1", vec![file.clone()]);
    value["versions"] = json!(["2.0", "1.0", "opaque-legacy-version"]);
    let project = fixture.parse(value.clone()).unwrap();
    assert_eq!(project.files()[0].size(), Some(123));
    assert_eq!(
        project.files()[0].upload_time(),
        Some("2024-02-29T23:59:59.123456Z")
    );
    assert_eq!(
        project
            .versions()
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["1.0", "2.0", "opaque-legacy-version"])
    );
    let mut largest = value.clone();
    largest["files"][0]["size"] = json!(u64::MAX);
    assert_eq!(
        fixture.parse(largest).unwrap().files()[0].size(),
        Some(u64::MAX)
    );
    let overflowing = serde_json::to_string(&value)
        .unwrap()
        .replace("\"size\":123", "\"size\":18446744073709551616");
    invalid_response(fixture.raw(overflowing.as_bytes(), SimpleLimits::default()));
    let mut mismatch = value.clone();
    mismatch["versions"] = json!(["2.0"]);
    let observed = fixture.parse(mismatch).unwrap();
    assert_eq!(observed.versions(), &["2.0"]);
    assert_eq!(observed.files()[0].version(), Some("1.0"));
    assert_eq!(
        observed.files()[0].filename(),
        "demo_pkg-1.0-py3-none-any.whl"
    );
    assert_eq!(
        observed.warnings(),
        &[SimpleWarning::VersionListingMismatch]
    );
    for versions in [json!(["1.0", "1.0"]), json!([""]), json!([1]), Value::Null] {
        let mut invalid = value.clone();
        invalid["versions"] = versions;
        invalid_response(fixture.parse(invalid));
    }
    let mut invalid = value.clone();
    invalid.as_object_mut().unwrap().remove("versions");
    invalid_response(fixture.parse(invalid));
    for size in [Value::Null, json!(-1), json!(1.5), json!("123")] {
        let mut invalid = value.clone();
        invalid["files"][0]["size"] = size;
        invalid_response(fixture.parse(invalid));
    }
    let mut invalid = value.clone();
    invalid["files"][0].as_object_mut().unwrap().remove("size");
    invalid_response(fixture.parse(invalid));
    for timestamp in [
        "2023-02-29T01:02:03Z",
        "2024-01-01T25:00:00Z",
        "2024-01-01T01:02:03.1234567Z",
        "2024-01-01T01:02:03+00:00",
    ] {
        let mut invalid = value.clone();
        invalid["files"][0]["upload-time"] = json!(timestamp);
        invalid_response(fixture.parse(invalid));
    }
}

#[test]
fn s4_version_listing_mismatch_preserves_original_artifact_identity() {
    let fixture = Fixture::new();
    for (filename, version, is_wheel) in [
        ("demo_pkg-1.0-py3-none-any.whl", "1.0", true),
        ("demo_pkg-1.0.post2.tar.gz", "1.0.post2", false),
    ] {
        let mut file = wheel("1.0");
        file["filename"] = json!(filename);
        let mut value = page("1.1", vec![file]);
        value["versions"] = json!(["2.0", "opaque-legacy-release"]);
        let observed = fixture.parse(value.clone()).unwrap();
        assert_eq!(observed.versions(), &["2.0", "opaque-legacy-release"]);
        let actual = &observed.files()[0];
        assert_eq!(actual.filename(), filename);
        assert_eq!(actual.version(), Some(version));
        assert_eq!(actual.size(), Some(123));
        assert_eq!(
            actual.hashes().preferred_digest(),
            Some(Digest::Sha256([0xab; 32]))
        );
        assert!(if is_wheel {
            matches!(actual.kind(), PythonDistributionKind::Wheel(_))
        } else {
            matches!(actual.kind(), PythonDistributionKind::Sdist)
        });
        assert_eq!(
            observed.warnings(),
            &[SimpleWarning::VersionListingMismatch]
        );

        for filename in ["bad.whl", "foreign-1.0-py3-none-any.whl"] {
            let mut invalid = value.clone();
            invalid["files"][0]["filename"] = json!(filename);
            invalid_response(fixture.parse(invalid));
        }
        let mut invalid = value.clone();
        invalid["files"][0]["hashes"]["sha256"] = json!("not-a-valid-digest");
        invalid_response(fixture.parse(invalid));
    }
}

#[test]
fn json_resource_limits_cover_unknown_raw_structures() {
    let fixture = Fixture::new();
    let value = page("1.0", vec![wheel("1.0")]);
    let bytes = serde_json::to_vec(&value).unwrap();
    let mut invalid_limits = vec![];
    for field in 0..9 {
        let mut limits = SimpleLimits::default();
        match field {
            0 => limits.max_response_bytes = 0,
            1 => limits.max_files = 0,
            2 => limits.max_versions = 0,
            3 => limits.max_hashes_per_file = 0,
            4 => limits.max_relations = 0,
            5 => limits.max_field_bytes = 0,
            6 => limits.max_object_fields = 0,
            7 => limits.max_json_depth = 0,
            _ => limits.max_json_values = 0,
        }
        invalid_limits.push(limits);
    }
    for limits in invalid_limits {
        assert!(matches!(
            fixture.raw(b"not JSON", limits),
            Err(SimpleError::InvalidLimits)
        ));
    }
    let mut limits = SimpleLimits::default();
    limits.max_response_bytes = bytes.len() as u64;
    assert!(
        fixture.raw(&bytes, limits).is_ok(),
        "exact raw limit must work"
    );
    limits.max_response_bytes -= 1;
    assert!(matches!(
        fixture.raw(&bytes, limits),
        Err(SimpleError::LimitExceeded(SimpleLimitKind::ResponseBytes))
    ));
    for limits in [
        SimpleLimits {
            max_json_values: 64,
            ..SimpleLimits::default()
        },
        SimpleLimits {
            max_json_depth: 4,
            ..SimpleLimits::default()
        },
        SimpleLimits {
            max_object_fields: 8,
            ..SimpleLimits::default()
        },
    ] {
        assert!(
            fixture.raw(&bytes, limits).is_ok(),
            "known fixture must fit before unknown structures are added"
        );
    }
    let mut too_many_files = page("1.0", vec![wheel("1.0"), wheel("2.0")]);
    too_many_files["files"][1]["filename"] = json!("bad.whl");
    for (mut value, expected) in [
        (too_many_files, SimpleLimitKind::Files),
        (
            {
                let mut v = page("1.1", vec![wheel("1.0")]);
                v["versions"] = json!(["1.0", "2.0"]);
                v
            },
            SimpleLimitKind::Versions,
        ),
        (
            {
                let mut v = value.clone();
                v["files"][0]["hashes"] =
                    json!({"sha256":"ab".repeat(32),"sha512":"cd".repeat(64)});
                v
            },
            SimpleLimitKind::Hashes,
        ),
        (
            {
                let mut v = value.clone();
                v["meta"]["tracks"] = json!([
                    "https://one.example/simple/demo-pkg/",
                    "https://two.example/simple/demo-pkg/"
                ]);
                v
            },
            SimpleLimitKind::Relations,
        ),
        (value.clone(), SimpleLimitKind::FieldBytes),
        (value.clone(), SimpleLimitKind::ObjectFields),
        (value.clone(), SimpleLimitKind::JsonValues),
        (value.clone(), SimpleLimitKind::JsonDepth),
    ] {
        let mut limits = SimpleLimits::default();
        match expected {
            SimpleLimitKind::Files => limits.max_files = 1,
            SimpleLimitKind::Versions => limits.max_versions = 1,
            SimpleLimitKind::Hashes => limits.max_hashes_per_file = 1,
            SimpleLimitKind::Relations => limits.max_relations = 1,
            SimpleLimitKind::FieldBytes => limits.max_field_bytes = 8,
            SimpleLimitKind::ObjectFields => {
                limits.max_object_fields = 8;
                value["_unknown"] = Value::Object(
                    (0..9)
                        .map(|index| (format!("field-{index}"), json!(0)))
                        .collect(),
                );
            }
            SimpleLimitKind::JsonValues => {
                limits.max_json_values = 64;
                value["_unknown"] = Value::Array((0..100).map(|value| json!(value)).collect());
            }
            SimpleLimitKind::JsonDepth => {
                limits.max_json_depth = 4;
                value["_unknown"] = json!([[[[[[0]]]]]]);
            }
            SimpleLimitKind::ResponseBytes
            | SimpleLimitKind::HtmlTokens
            | SimpleLimitKind::HtmlTreeNodes
            | SimpleLimitKind::HtmlTreeDepth
            | SimpleLimitKind::HtmlTreeTextBytes
            | SimpleLimitKind::HtmlTreeOperations
            | SimpleLimitKind::HtmlAttributes
            | SimpleLimitKind::HtmlAnchors => unreachable!(),
        }
        assert!(
            matches!(fixture.raw(&serde_json::to_vec(&value).unwrap(), limits), Err(SimpleError::LimitExceeded(observed)) if observed==expected),
            "wrong limit result for {expected:?}"
        );
    }
}

#[test]
fn legacy_hyphenated_sdist_versions_remain_historical_facts() {
    let fixture = Fixture::new();
    let mut file = wheel("1.0");
    file["filename"] = json!("demo_pkg-1.0-1.tar.gz");
    file["url"] = json!("https://files.example/demo_pkg-1.0-1.tar.gz");
    let project = fixture.parse(page("1.0", vec![file])).unwrap();
    assert!(matches!(
        project.files()[0].kind(),
        PythonDistributionKind::HistoricalFormat
    ));
    assert_eq!(project.files()[0].filename(), "demo_pkg-1.0-1.tar.gz");
}

#[test]
fn s3_legacy_source_filename_known_hyphenated_project_remains_exact() {
    let registry = PythonRegistry::new("pypi", "https://pypi.org/simple/", None).unwrap();
    let name = NormalizedPythonName::new("antlr4-python3-runtime").unwrap();
    let document = registry.project_url(&name).unwrap();
    let fixture = Fixture {
        registry,
        project: name,
        document,
    };
    for filename in [
        "antlr4-python3-runtime-4.9.3.tar.gz",
        "antlr4_python3_runtime-4.9.3.tar.gz",
    ] {
        let body = json!({
            "meta":{"api-version":"1.0"},"name":"antlr4-python3-runtime",
            "files":[{"filename":filename,"url":format!("https://files.example/{filename}"),"hashes":{"sha256":"ab".repeat(32)}}]
        });
        let project = fixture.parse(body).unwrap();
        let file = &project.files()[0];
        assert!(
            matches!(file.kind(), PythonDistributionKind::Sdist),
            "{filename}"
        );
        assert_eq!(file.version(), Some("4.9.3"));
        assert_eq!(file.filename(), filename);
    }
    let body = |filename: &str| {
        json!({"meta":{"api-version":"1.0"},"name":"antlr4-python3-runtime",
        "files":[{"filename":filename,"url":format!("https://files.example/{filename}"),"hashes":{}}]})
    };
    invalid_response(fixture.parse(body("foreign-4.9.3.tar.gz")));
    let historical = fixture
        .parse(body("antlr4-python3-runtime-4.9.3-1.tar.gz"))
        .unwrap();
    assert!(matches!(
        historical.files()[0].kind(),
        PythonDistributionKind::HistoricalFormat
    ));
    assert_eq!(historical.files()[0].version(), None);
}

#[test]
fn historical_file_facts_have_an_explicit_adapter_warning() {
    let fixture = Fixture::new();
    let mut file = wheel("1.0");
    file["filename"] = json!("demo_pkg-0.8-py3.egg");
    file["url"] = json!("https://files.example/demo_pkg-0.8-py3.egg");
    let project = fixture.parse(page("1.0", vec![file])).unwrap();
    assert!(
        project
            .warnings()
            .contains(&SimpleWarning::HistoricalDistribution)
    );
}

#[test]
fn declared_json_filenames_are_independent_of_download_endpoint_paths() {
    let fixture = Fixture::new();
    for locator in [
        "https://files.example/download?id=opaque-object",
        "https://files.example/foreign-1.0-py3-none-any.whl?token=synthetic-endpoint-secret",
        "https://files.example/demo%2Fpkg-1.0-py3-none-any.whl",
        "../../download?id=opaque-object",
    ] {
        let mut file = wheel("1.0");
        file["url"] = json!(locator);
        file["requires-python"] = json!(">=99");
        let observed = fixture.parse(page("1.0", vec![file])).unwrap();
        let file = &observed.files()[0];
        assert_eq!(file.filename(), "demo_pkg-1.0-py3-none-any.whl");
        assert_eq!(file.version(), Some("1.0"));
        assert_eq!(file.requires_python(), Some(">=99"));
        let expected = fixture.document.as_url().join(locator).unwrap();
        assert_eq!(file.locator().url(), &expected);
        assert!(!format!("{file:?}").contains("synthetic-endpoint-secret"));
        assert_eq!(
            file.hashes().preferred_digest(),
            Some(Digest::Sha256([0xab; 32]))
        );
    }
}
