//! Test-only declaration-factory producer using the existing real HTTP/A assets.
use super::*;

pub(crate) struct ProjectRegistryFixture {
    fixture: Fixture,
    peer: Peer,
}
impl ProjectRegistryFixture {
    pub(crate) fn wheels() -> Self {
        let demo = wheel("demo_pkg", "1.0", &["dep==1.0"], "");
        let dep = wheel("dep", "1.0", &[], "");
        let peer = Peer::new(move |target| match target {
            "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(DEMO)]),
            "/simple/dep/" => Reply::page("dep", vec![wire_file(DEP)]),
            value if value == format!("/files/{DEMO}") => Reply::file(&demo),
            value if value == format!("/files/{DEP}") => Reply::file(&dep),
            _ => Reply::missing(),
        });
        Self {
            fixture: Fixture::new(&peer),
            peer,
        }
    }
    pub(crate) fn sdist() -> Self {
        const SDIST: &str = "demo_pkg-1.0.tar.gz";
        let bytes = include_bytes!("../../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz");
        let peer = Peer::new(move |target| match target {
            "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(SDIST)]),
            "/files/demo_pkg-1.0.tar.gz" => Reply::file(bytes),
            _ => Reply::missing(),
        });
        Self {
            fixture: Fixture::new(&peer),
            peer,
        }
    }
    pub(crate) fn lock(&self) -> BoundPythonLock {
        self.fixture.lock()
    }
    pub(crate) fn lock_at_deadline(&self, deadline: Instant) -> BoundPythonLock {
        BoundPythonLock::acquire(
            &self.fixture.root.path().join("pakro.lock"),
            Default::default(),
            deadline,
        )
        .unwrap()
    }
    pub(crate) fn project_path(&self) -> std::path::PathBuf {
        self.fixture.root.path().join("pyproject.toml")
    }
    pub(crate) fn endpoint(&self) -> RegistryEndpoint<'_> {
        self.fixture.endpoint()
    }
    pub(crate) fn context<'a>(
        &'a self,
        endpoints: &'a [RegistryEndpoint<'a>],
        source_policy: &'a PythonSourcePolicy,
    ) -> RegistryDepotContext<'a> {
        self.fixture.context(endpoints, source_policy)
    }
    pub(crate) fn requests(&self) -> Vec<String> {
        self.peer.requests()
    }
    pub(crate) fn no_publish(&self) {
        self.fixture.no_publish();
    }
}
