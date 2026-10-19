use super::frozen_route::{LocalRoute, RoutingArtifact, run_in_local_test_process};
use super::*;

const SIGNED_SUBJECT: &str = "b4058dece685259910d3aba5782445996eea79dbdb3cf952a6eb81aadf0373ff";

fn signed_bundle() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/github_release_attestation_jdx_pakro.json"
    ))
    .unwrap()
}

#[tokio::test]
async fn r06_digest_verification_succeeds_without_reopening_an_artifact_path() {
    if run_in_local_test_process(
        "tests::frozen_digest::r06_digest_verification_succeeds_without_reopening_an_artifact_path",
    ) {
        return;
    }
    let bundle = signed_bundle();
    let digest = Sha256Hash::from_hex(SIGNED_SUBJECT).unwrap();
    let parsed: Bundle = serde_json::from_value(bundle.clone()).unwrap();
    assert!(
        is_github_internal_certificate(&parsed),
        "fixture must use the offline GitHub embedded trust root"
    );
    verify_bundle(
        Artifact::from(&digest),
        &parsed,
        None,
        &github_embedded_trusted_root().unwrap(),
    )
    .unwrap();
    let api = LocalRoute::new(
        200,
        serde_json::json!({"attestations":[{"bundle":bundle,"bundle_url":null}]}).to_string(),
    );
    let missing = {
        let named = RoutingArtifact::new();
        named.path().to_owned()
    };
    assert!(!missing.exists());
    let retry = || RetryConfig {
        timeout: Duration::from_secs(2),
        retries: 0,
        backoff_base: Duration::ZERO,
    };
    let legacy = verify_github_attestation_with_base_url_and_digest(GithubAttestationRequest {
        artifact_path: &missing,
        owner: "jdx",
        repo: "mise",
        token: None,
        signer_workflow: None,
        base_url: Some(api.url()),
        digest: Some(SIGNED_SUBJECT),
        retry_config: retry(),
    })
    .await;
    assert!(
        matches!(legacy, Err(AttestationError::Io(ref error)) if error.kind() == std::io::ErrorKind::NotFound),
        "the path-only baseline must demonstrate its actual reopen: {legacy:?}"
    );
    assert_eq!(api.hits(), 1);
    let result = verify_github_attestation_digest_with_tuf_url(
        GithubAttestationDigestRequest {
            sha256: *digest.as_bytes(),
            owner: "jdx",
            repo: "mise",
            token: None,
            signer_workflow: None,
            base_url: Some(api.url()),
            retry_config: retry(),
        },
        None,
    )
    .await;
    assert!(
        result.unwrap(),
        "the verified subject digest does not require any named artifact path"
    );
    assert_eq!(api.hits(), 2);
    assert!(
        api.requests()
            .iter()
            .all(|request| request.contains(SIGNED_SUBJECT)),
        "both actual requests must address the fixed subject"
    );
}

#[tokio::test]
async fn r06_digest_bundle_kernel_rejects_a_valid_signature_for_a_different_subject() {
    let attestations: Vec<Attestation> =
        serde_json::from_value(serde_json::json!([{"bundle":signed_bundle(),"bundle_url":null}]))
            .unwrap();
    let mut roots = TrustRoots::default();
    roots.github_embedded = Some(github_embedded_trusted_root().unwrap());
    // Avoid remote fallback while retaining the exact same verification core.
    roots.github_tuf = Some(github_embedded_trusted_root().unwrap());
    let valid = Sha256Hash::from_hex(SIGNED_SUBJECT).unwrap();
    assert!(
        verify_attestation_bundles(&attestations, Artifact::from(&valid), None, &mut roots)
            .await
            .unwrap(),
        "the actual signature and matching subject must verify before testing a different subject"
    );
    let different = Sha256Hash::from_bytes([0; 32]);
    let error =
        verify_attestation_bundles(&attestations, Artifact::from(&different), None, &mut roots)
            .await
            .unwrap_err();
    assert!(
        is_slsa_subject_mismatch(&error),
        "a genuine signature for another artifact must fail its subject binding: {error}"
    );
}
