//! Actual replay consumers must pass the caller budget to the shared kernel.
//! The existing outer timeout cannot undo a late synchronous credential call.

use super::*;
use pakro_index::python_source::{
    CredentialRef, TransportRulesFingerprint, transport_rules_fingerprint,
};
use pakro_index::simple::{
    HttpOrigin, OriginAuthorization, SourceCredentials, SourceRequestRouter,
};
use sha2::{Digest as _, Sha256};
use std::sync::atomic::AtomicUsize;

struct FiniteRoute {
    calls: AtomicUsize,
    slow_call: Option<usize>,
}
impl SourceRequestRouter for FiniteRoute {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        transport_rules_fingerprint(None)
    }
    fn request_url(&self, canonical: &Url) -> Result<Url, SimpleClientError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.slow_call == Some(call) {
            std::thread::sleep(Duration::from_millis(120));
        }
        Ok(canonical.clone())
    }
}
struct DeadlineCredentials {
    deadline: Instant,
    calls: AtomicUsize,
    late: AtomicUsize,
}
impl SourceCredentials for DeadlineCredentials {
    fn authorization_for(
        &self,
        _: &CredentialRef,
        origin: &HttpOrigin,
        network: NetworkPolicy,
    ) -> Result<Option<OriginAuthorization>, SimpleClientError> {
        assert_eq!(network, NetworkPolicy::Allowed);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if Instant::now() >= self.deadline {
            self.late.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Some(
            OriginAuthorization::new(
                origin.clone(),
                reqwest::header::HeaderValue::from_static("Bearer actual-replay-fixture-secret"),
            )
            .unwrap(),
        ))
    }
}

async fn exercise(project_phase: bool) {
    // Control and target use distinct actual locks and empty caches. A
    // successful control must not warm away the target's cold HTTP branch.
    for slow in [false, true] {
        let peer = Peer::new(|target| {
            if target.starts_with("/simple/") {
                Reply::page(page(vec![file_json(
                    FILENAME,
                    "/files/exact",
                    serde_json::json!({"sha256":hex::encode(Sha256::digest(PAYLOAD))}),
                )]))
            } else {
                Reply::file(PAYLOAD)
            }
        });
        let mut fixture = Fixture::new(&peer);
        let reference = CredentialRef::new(if project_phase {
            "index-ref"
        } else {
            "file-ref"
        })
        .unwrap();
        if project_phase {
            fixture.registry = PythonRegistry::new(
                "corp",
                &format!("{}/simple/", peer.url),
                Some(reference.clone()),
            )
            .unwrap();
        }
        let file = fixture.file(FILENAME);
        let root = fixture.lock(
            &file,
            Digest::from_sha256_bytes(PAYLOAD),
            PAYLOAD.len() as u64,
        );
        let before = fs::read(&fixture.lock_path).unwrap();
        let deadline = Instant::now()
            + if slow {
                Duration::from_millis(60)
            } else {
                Duration::from_secs(5)
            };
        let provider = DeadlineCredentials {
            deadline,
            calls: AtomicUsize::new(0),
            late: AtomicUsize::new(0),
        };
        let target_call = if project_phase { 2 } else { 4 };
        let route = FiniteRoute {
            calls: AtomicUsize::new(0),
            slow_call: slow.then_some(target_call),
        };
        let mut context = fixture.context(NetworkPolicy::Allowed, &route);
        context.deadline = deadline;
        if project_phase {
            context.project_session.credentials = &provider;
        } else {
            context.file_session.credential_ref = Some(&reference);
            context.file_session.credentials = &provider;
        }
        let result = acquire_locked_registry_file(slot(&root, &file), context).await;
        if slow {
            assert_eq!(
                route.calls.load(Ordering::SeqCst),
                target_call,
                "target must reach the actual selected inner route"
            );
            assert_eq!(
                provider.late.load(Ordering::SeqCst),
                0,
                "replay consumer invoked credentials after the original deadline"
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            assert_eq!(result.unwrap_err(), LockedReplayError::Deadline);
            assert_eq!(
                peer.requests().len(),
                usize::from(!project_phase),
                "file phase must complete real Simple first; neither phase may send the expired authenticated request"
            );
        } else {
            let mut result = result
                .expect("legacy default-budget control must obtain genuine expected replay bytes");
            assert_eq!(read(&mut result), PAYLOAD);
            result.recheck_for(&root).unwrap();
            assert_eq!(result.digest(), Digest::from_sha256_bytes(PAYLOAD));
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            assert_eq!(provider.late.load(Ordering::SeqCst), 0);
            assert_eq!(peer.requests().len(), 2);
        }
        assert_eq!(fs::read(&fixture.lock_path).unwrap(), before);
        fixture.assert_no_authority_writes();
    }
}

#[tokio::test]
async fn standalone_replay_project_stage_does_not_call_credentials_after_its_caller_deadline() {
    exercise(true).await;
}

#[tokio::test]
async fn standalone_replay_file_stage_does_not_call_credentials_after_real_simple_and_deadline() {
    exercise(false).await;
}
