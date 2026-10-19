//! Test-only producers using the actual registry/slot/C/sidecar/closure flow.
use super::*;
use crate::packages::python_environment::runtime_target::ObservedRegistryRuntimeTarget;
use crate::packages::python_resolution::lock_model::{
    EdgeInput, GroupKey, LockGraphInput, LockMarkerSemantics, NodeInput, PinEvidence,
    PythonLockModel, RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput, RootInput,
    RuntimePin, canonical_registry_base_fingerprint,
};
use crate::packages::python_resolution::source::locked_graph::FrozenRegistryTarget;
use crate::packages::python_resolution::source::registry_depot::closed::{
    ClosedRegistryGraph, close_registry_plan,
};

pub(crate) struct RegistryBuildFixture {
    fixture: Fixture,
    peer: Peer,
    // One original request budget covers lock acquisition, inputs and build.
    request_deadline: std::time::Instant,
    expected_edges: usize,
    demo_digest: Digest,
    demo_size: u64,
    dep_digest: Digest,
    dep_size: u64,
}
impl RegistryBuildFixture {
    pub(crate) fn new(requires_python: &str) -> Self {
        Self::with_dependencies(
            requires_python,
            &["dep==1.0"],
            std::time::Instant::now() + Duration::from_secs(240),
        )
    }
    pub(crate) fn with_dependencies(
        requires_python: &str,
        requires_dist: &[&str],
        request_deadline: std::time::Instant,
    ) -> Self {
        let other = format!("Requires-Python: {requires_python}\n");
        let demo = wheel("demo_pkg", "1.0", requires_dist, &other);
        let dep = wheel("dep", "1.0", &[], &other);
        let demo_digest = Digest::from_sha512_bytes(&demo);
        let dep_digest = Digest::from_sha256_bytes(&dep);
        let demo_size = demo.len() as u64;
        let dep_size = dep.len() as u64;
        let mut demo_file = wire_file(DEMO);
        let mut dep_file = wire_file(DEP);
        let Digest::Sha512(demo_hash) = demo_digest else {
            unreachable!()
        };
        let Digest::Sha256(dep_hash) = dep_digest else {
            unreachable!()
        };
        demo_file["hashes"] = serde_json::json!({"sha512":hex::encode(demo_hash)});
        dep_file["hashes"] = serde_json::json!({"sha256":hex::encode(dep_hash)});
        let peer = Peer::new(move |target| match target {
            "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![demo_file.clone()]),
            "/simple/dep/" => Reply::page("dep", vec![dep_file.clone()]),
            value if value == format!("/files/{DEMO}") => Reply::file(&demo),
            value if value == format!("/files/{DEP}") => Reply::file(&dep),
            _ => Reply::missing(),
        });
        Self {
            fixture: Fixture::new(&peer),
            peer,
            request_deadline,
            expected_edges: requires_dist.len(),
            demo_digest,
            demo_size,
            dep_digest,
            dep_size,
        }
    }
    pub(crate) fn lock(&self) -> BoundPythonLock {
        BoundPythonLock::acquire(
            &self.fixture.root.path().join("pakro.lock"),
            Default::default(),
            self.request_deadline,
        )
        .unwrap()
    }
    pub(crate) fn lock_path(&self) -> std::path::PathBuf {
        self.fixture.root.path().join("pakro.lock")
    }
    pub(crate) fn image(&self) -> Option<Vec<u8>> {
        fs::read(self.fixture.root.path().join("pakro.lock")).ok()
    }
    pub(crate) fn requests(&self) -> usize {
        self.peer.requests().len()
    }
    pub(crate) fn remove_cache(&self) {
        if self.fixture.cache.exists() {
            fs::remove_dir_all(&self.fixture.cache).unwrap();
        }
    }
    pub(crate) fn assert_unchanged(&self, image: Option<Vec<u8>>) {
        assert_eq!(self.image(), image);
        for path in ["refs", "environment", "journal", "catalog"] {
            assert!(!self.fixture.root.path().join(path).exists());
        }
        assert_eq!(self.fixture.credentials.0.load(Ordering::SeqCst), 0);
    }
    pub(crate) fn lock_existing(&self, runtime: &ObservedRegistryRuntimeTarget) -> BoundPythonLock {
        let limits = LockModelLimits::default();
        let pins = [
            ("demo-pkg", DEMO, self.demo_digest, self.demo_size),
            ("dep", DEP, self.dep_digest, self.dep_size),
        ]
        .map(|(name, filename, digest, size)| {
            let file = RegistryFileRef::new(
                RegistryFileRefInput {
                    registry_id: self.fixture.registry.id().as_str(),
                    canonical_base_fingerprint: canonical_registry_base_fingerprint(
                        self.fixture.registry.canonical_base(),
                    ),
                    project: name,
                    version: "1.0",
                    filename,
                    kind: RegistryArtifactKind::Wheel,
                },
                limits,
            )
            .unwrap();
            ContentPin::new(&file, digest, size, PinEvidence::Advertised, limits).unwrap()
        });
        let key = GroupKey::new("", "fixture", *runtime.target().fingerprint(), limits).unwrap();
        let nodes = [
            NodeInput {
                name: "demo-pkg",
                version: "1.0",
                source_pin: pins[0].id(),
            },
            NodeInput {
                name: "dep",
                version: "1.0",
                source_pin: pins[1].id(),
            },
        ];
        let graph = LockGraphInput {
            key: &key,
            semantics: LockMarkerSemantics::Current2026,
            runtime: RuntimePin::new(
                *runtime.content_tree_sha256(),
                1,
                *runtime.target().fingerprint(),
            )
            .unwrap(),
            root_declarations: &["demo-pkg==1.0"],
            roots: &[RootInput {
                declaration_index: 0,
                node: "demo-pkg",
            }],
            nodes: &nodes,
            edges: &[EdgeInput {
                from: "demo-pkg",
                to: "dep",
                declaration: "dep==1.0",
            }],
        };
        let model = PythonLockModel::from_parts(&pins, &[graph], limits).unwrap();
        fs::write(
            self.fixture.root.path().join("pakro.lock"),
            format!(
                "lockfile_version=4\n{}",
                model.encode_projection(limits).unwrap()
            ),
        )
        .unwrap();
        self.lock()
    }
    pub(crate) async fn closed<'lock>(
        &self,
        root: &'lock BoundPythonLock,
        target: &FrozenRegistryTarget,
        raw_roots: &[&str],
    ) -> ClosedRegistryGraph<'lock> {
        let endpoints = [self.fixture.endpoint()];
        let source_policy = policy();
        let mut context = self.fixture.context(&endpoints, &source_policy);
        context.environment = target.environment();
        context.supported = target.supported();
        context.semantics = target.semantics();
        context.root_scope = target.with_root_scope(|scope| match scope {
            PythonMarkerScope::Environment => RegistryRootScope::Environment,
            PythonMarkerScope::Dependency { requested_extras } => RegistryRootScope::ProjectExtras(
                requested_extras.iter().map(|v| (*v).to_owned()).collect(),
            ),
            PythonMarkerScope::Lock {
                selected_extras,
                dependency_groups,
            } => RegistryRootScope::Lock {
                extras: selected_extras.iter().map(|v| (*v).to_owned()).collect(),
                groups: dependency_groups.iter().map(|v| (*v).to_owned()).collect(),
            },
        });
        context.deadline = self.request_deadline;
        let prepared = ready(
            ResolveRegistryDepot::new(root, raw_roots, context)
                .unwrap()
                .prepare()
                .await
                .expect("actual registry/T/O2/C/sidecar/M1 prerequisite"),
        );
        assert_eq!(prepared.plan().selected.len(), 2);
        for selected in &prepared.plan().selected {
            prepared
                .owner(selected.key)
                .unwrap()
                .recheck_for(root)
                .unwrap();
        }
        let closed = close_registry_plan(prepared, Default::default())
            .expect("actual semantic/source closure prerequisite");
        assert_eq!(closed.checked().edges.len(), self.expected_edges);
        closed.recheck_for(root).unwrap();
        closed
    }
}
