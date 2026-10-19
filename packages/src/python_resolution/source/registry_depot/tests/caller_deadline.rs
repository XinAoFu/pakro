//! Finite synchronous routing crosses the original caller budget before auth.
//! Each default-budget positive control reaches real TCP and real owner bytes.

use super::*;
use crate::packages::python_resolution::source::caller_deadline::*;
use crate::packages::python_resolution::source::fresh::acquire_fresh_registry_file;

struct FiniteRoute(AtomicUsize);
impl SourceRequestRouter for FiniteRoute {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        transport_rules_fingerprint(None)
    }
    fn request_url(&self, canonical: &Url) -> Result<Url, SimpleClientError> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
            thread::sleep(Duration::from_millis(120));
        }
        Ok(canonical.clone())
    }
}
struct DeadlineCredentials {
    deadline: Instant,
    calls: AtomicUsize,
    late: AtomicUsize,
}
impl DeadlineCredentials {
    fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            calls: AtomicUsize::new(0),
            late: AtomicUsize::new(0),
        }
    }
}
impl SourceCredentials for DeadlineCredentials {
    fn authorization_for(
        &self,
        _: &CredentialRef,
        origin: &HttpOrigin,
        policy: NetworkPolicy,
    ) -> Result<Option<OriginAuthorization>, SimpleClientError> {
        assert_eq!(policy, NetworkPolicy::Allowed);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if Instant::now() >= self.deadline {
            self.late.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Some(
            OriginAuthorization::new(
                origin.clone(),
                reqwest::header::HeaderValue::from_static("Bearer finite-fixture-secret"),
            )
            .unwrap(),
        ))
    }
}
fn peer() -> Peer {
    let bytes = wheel("demo_pkg", "1.0", &[], "");
    Peer::new(move |p| {
        if p == "/simple/demo-pkg/" {
            Reply::page("demo-pkg", vec![wire_file(DEMO)])
        } else {
            Reply::file(&bytes)
        }
    })
}
async fn observed(f: &Fixture) -> BoundRegistryProject {
    let endpoint = f.endpoint();
    match fetch_registry_project(
        &f.client,
        endpoint.project,
        &NormalizedPythonName::new("demo-pkg").unwrap(),
        SimpleFetchPolicy {
            network: NetworkPolicy::Allowed,
            refresh: true,
            fresh_for: Duration::from_secs(60),
            now: SystemTime::UNIX_EPOCH,
        },
        Default::default(),
    )
    .await
    .unwrap()
    {
        RegistryProjectLookup::Present(value) => value,
        RegistryProjectLookup::Missing404 => panic!("real project prelude missing"),
    }
}
fn file_session<'a>(
    f: &'a Fixture,
    reference: &'a CredentialRef,
    credentials: &'a DeadlineCredentials,
    route: &'a dyn SourceRequestRouter,
) -> SourceFileSession<'a> {
    SourceFileSession {
        credential_ref: Some(reference),
        auth_scope: &f.scope,
        credentials,
        auth_origins: &f.grants,
        transport: route,
    }
}
fn positive_policy() -> SimpleFetchPolicy {
    SimpleFetchPolicy {
        network: NetworkPolicy::Allowed,
        refresh: true,
        fresh_for: Duration::from_secs(60),
        now: SystemTime::UNIX_EPOCH,
    }
}
fn authenticated_session<'a>(
    registry: &'a PythonRegistry,
    fixture: &'a Fixture,
    credentials: &'a dyn SourceCredentials,
    route: &'a dyn SourceRequestRouter,
) -> PythonSourceSession<'a> {
    PythonSourceSession {
        registry,
        auth_scope: &fixture.scope,
        credentials,
        auth_origins: &fixture.grants,
        transport: route,
    }
}
fn no_late_provider(credentials: &DeadlineCredentials, route: &FiniteRoute) {
    assert_eq!(
        route.0.load(Ordering::SeqCst),
        2,
        "target must reach the real inner route after source preflight"
    );
    assert_eq!(
        credentials.late.load(Ordering::SeqCst),
        0,
        "source provider ran after original caller deadline"
    );
    assert_eq!(credentials.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn project_caller_deadline_reaches_the_inner_provider_gate() {
    let peer = peer();
    let f = Fixture::new(&peer);
    let reference = CredentialRef::new("index-ref").unwrap();
    let registry =
        PythonRegistry::new("corp", &format!("{}/simple/", peer.url), Some(reference)).unwrap();
    let name = NormalizedPythonName::new("demo-pkg").unwrap();
    let control = DeadlineCredentials::new(Instant::now() + Duration::from_secs(5));
    assert!(matches!(
        fetch_registry_project(
            &f.client,
            authenticated_session(&registry, &f, &control, &ROUTE),
            &name,
            positive_policy(),
            Default::default()
        )
        .await
        .unwrap(),
        RegistryProjectLookup::Present(_)
    ));
    assert_eq!(control.calls.load(Ordering::SeqCst), 1);
    assert_eq!(peer.requests().len(), 1);
    let deadline = Instant::now() + Duration::from_millis(60);
    let credentials = DeadlineCredentials::new(deadline);
    let route = FiniteRoute(AtomicUsize::new(0));
    // The target is a cold page. Revalidating the positive control's cached
    // document would correctly add another finite forward-route observation.
    let target_client =
        SimpleIndexClient::new(f.root.path().join("caller-simple"), Default::default()).unwrap();
    let result = fetch_registry_project_at_deadline(
        &target_client,
        authenticated_session(&registry, &f, &credentials, &route),
        &name,
        positive_policy(),
        Default::default(),
        deadline,
    )
    .await;
    no_late_provider(&credentials, &route);
    assert!(matches!(
        result,
        Err(SourceResolutionError::Transport(SimpleClientError::Timeout))
    ));
    assert_eq!(peer.requests().len(), 1);
    f.no_publish();
}

#[tokio::test]
async fn file_caller_deadline_reaches_the_same_t_provider_gate() {
    let peer = peer();
    let f = Fixture::new(&peer);
    let project = observed(&f).await;
    let selected = project.select_exact_file(DEMO).unwrap();
    let reference = CredentialRef::new("file-ref").unwrap();
    let control = DeadlineCredentials::new(Instant::now() + Duration::from_secs(5));
    let artifact = acquire_registry_file(
        &selected,
        &f.client,
        file_session(&f, &reference, &control, &ROUTE),
        NetworkPolicy::Allowed,
        Default::default(),
        &f.scratch,
    )
    .await
    .unwrap();
    assert!(matches!(artifact, AcquiredRegistryFile::Observed(_)));
    drop(artifact);
    assert_eq!(control.calls.load(Ordering::SeqCst), 1);
    assert_eq!(peer.requests().len(), 2);
    let deadline = Instant::now() + Duration::from_millis(60);
    let credentials = DeadlineCredentials::new(deadline);
    let route = FiniteRoute(AtomicUsize::new(0));
    let result = acquire_registry_file_at_deadline(
        &selected,
        &f.client,
        file_session(&f, &reference, &credentials, &route),
        NetworkPolicy::Allowed,
        Default::default(),
        &f.scratch,
        deadline,
    )
    .await;
    no_late_provider(&credentials, &route);
    assert!(matches!(
        result,
        Err(SourceResolutionError::Transport(SimpleClientError::Timeout))
    ));
    assert_eq!(peer.requests().len(), 2);
    f.no_publish();
}

#[tokio::test]
async fn fresh_owner_propagates_the_original_budget_through_source_acquisition() {
    let peer = peer();
    let f = Fixture::new(&peer);
    let project = observed(&f).await;
    let root = f.lock();
    let reference = CredentialRef::new("file-ref").unwrap();
    let file = selected_file_ref(
        &project.select_exact_file(DEMO).unwrap(),
        Default::default(),
    )
    .unwrap();
    let slot = || match root.lookup(&file, ResolutionIntent::Resolve).unwrap() {
        FilePinLookup::Fresh(value) => value,
        other => panic!("real vacancy prelude became {other:?}"),
    };
    let control = DeadlineCredentials::new(Instant::now() + Duration::from_secs(5));
    let artifact = acquire_fresh_registry_file(
        slot(),
        project.select_exact_file(DEMO).unwrap(),
        &f.client,
        file_session(&f, &reference, &control, &ROUTE),
        NetworkPolicy::Allowed,
        Default::default(),
        Default::default(),
        &f.scratch,
    )
    .await
    .unwrap();
    artifact.recheck_for(&root).unwrap();
    drop(artifact);
    assert_eq!(control.calls.load(Ordering::SeqCst), 1);
    assert_eq!(peer.requests().len(), 2);
    let deadline = Instant::now() + Duration::from_millis(60);
    let credentials = DeadlineCredentials::new(deadline);
    let route = FiniteRoute(AtomicUsize::new(0));
    let result = acquire_fresh_registry_file_at_deadline(
        slot(),
        project.select_exact_file(DEMO).unwrap(),
        &f.client,
        file_session(&f, &reference, &credentials, &route),
        NetworkPolicy::Allowed,
        Default::default(),
        Default::default(),
        &f.scratch,
        deadline,
    )
    .await;
    no_late_provider(&credentials, &route);
    assert!(matches!(
        result,
        Err(super::super::super::fresh::FreshPinError::Source(
            SourceResolutionError::Transport(SimpleClientError::Timeout)
        ))
    ));
    assert_eq!(peer.requests().len(), 2);
    f.no_publish();
}
