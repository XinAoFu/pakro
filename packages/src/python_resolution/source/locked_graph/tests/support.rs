use super::*;
use base64::Engine;
use std::io::{Cursor, Write};

pub(super) const ORIGINALS: &[&str] = &[
    "demo-pkg[fast]==1.0",
    "inactive; sys_platform == 'win32'",
    "demo-pkg==1.0",
];
pub(super) const DEPENDENCY: &str = "dep[slow]>=1; extra == 'fast'";
pub(super) fn environment() -> PythonMarkerEnvironment<'static> {
    PythonMarkerEnvironment {
        os_name: "posix",
        sys_platform: "linux",
        platform_release: "6.8",
        platform_system: "Linux",
        platform_version: "2026",
        platform_machine: "aarch64",
        platform_python_implementation: "CPython",
        python_version: "3.12",
        python_full_version: "3.12.5",
        implementation_name: "cpython",
        implementation_version: "3.12.5",
    }
}
pub(super) fn supported() -> SupportedWheelTags {
    SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap()
}
pub(super) fn target() -> FrozenRegistryTarget {
    FrozenRegistryTarget::new(
        environment(),
        &supported(),
        PythonMarkerScope::Environment,
        MarkerSemantics::Current2026,
        Default::default(),
    )
    .expect("actual explicit frozen-target prerequisite")
}
pub(super) struct Spec {
    pub name: String,
    pub version: String,
    pub filename: String,
    pub bytes: Vec<u8>,
}
impl Spec {
    pub(super) fn wheel(name: &str, version: &str, dependencies: &[&str], extra: &str) -> Self {
        let escaped = name.replace('-', "_");
        let filename = format!("{escaped}-{version}-py3-none-any.whl");
        let mut metadata = format!(
            "Metadata-Version: 2.6\nName: {name}\nVersion: {version}\nProvides-Extra: fast\nProvides-Extra: slow\n{extra}"
        );
        for raw in dependencies {
            metadata.push_str(&format!("Requires-Dist: {raw}\n"));
        }
        metadata.push('\n');
        let info = format!("{escaped}-{version}.dist-info");
        let mut entries = vec![
            (format!("{escaped}/__init__.py"), b"VALUE=1\n".to_vec()),
            (
                format!("{info}/WHEEL"),
                b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_vec(),
            ),
            (format!("{info}/METADATA"), metadata.into_bytes()),
        ];
        let mut record = String::new();
        for (path, bytes) in &entries {
            record.push_str(&format!(
                "{path},sha256={},{}\n",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(sha2::Sha256::digest(bytes)),
                bytes.len()
            ));
        }
        record.push_str(&format!("{info}/RECORD,,\n"));
        entries.push((format!("{info}/RECORD"), record.into_bytes()));
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, bytes) in entries {
            writer
                .start_file(
                    path,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        Self {
            name: name.into(),
            version: version.into(),
            filename,
            bytes: writer.finish().unwrap().into_inner(),
        }
    }
    pub(super) fn file(&self, fixture: &Fixture) -> RegistryFileRef {
        RegistryFileRef::new(
            RegistryFileRefInput {
                registry_id: fixture.registry.id().as_str(),
                canonical_base_fingerprint: canonical_registry_base_fingerprint(
                    fixture.registry.canonical_base(),
                ),
                project: &self.name,
                version: &self.version,
                filename: &self.filename,
                kind: if self.filename.ends_with(".whl") {
                    RegistryArtifactKind::Wheel
                } else {
                    RegistryArtifactKind::Sdist
                },
            },
            Default::default(),
        )
        .unwrap()
    }
    pub(super) fn pin(&self, fixture: &Fixture) -> ContentPin {
        ContentPin::new(
            &self.file(fixture),
            Digest::from_sha256_bytes(&self.bytes),
            self.bytes.len() as u64,
            PinEvidence::Advertised,
            Default::default(),
        )
        .unwrap()
    }
}
pub(super) fn specs() -> Vec<Spec> {
    vec![
        Spec::wheel("demo-pkg", "1.0", &[DEPENDENCY], "Requires-Python: >=3.8\n"),
        Spec::wheel("dep", "1.0", &[], ""),
    ]
}
pub(super) fn write_graph(
    fixture: &Fixture,
    target: &FrozenRegistryTarget,
    originals: &[&str],
    specs: &[Spec],
    roots: &[(usize, &str)],
    edges: &[(&str, &str, &str)],
) -> (BoundPythonLock, GroupKey) {
    write_graph_with_builds(fixture, target, originals, specs, roots, edges, &[])
}
pub(super) fn write_graph_with_builds(
    fixture: &Fixture,
    target: &FrozenRegistryTarget,
    originals: &[&str],
    specs: &[Spec],
    roots: &[(usize, &str)],
    edges: &[(&str, &str, &str)],
    builds: &[(
        PinId,
        crate::packages::python_environment::model::SourceBuildRecord,
    )],
) -> (BoundPythonLock, GroupKey) {
    let limits = LockModelLimits::default();
    let key = GroupKey::new("", "main", *target.fingerprint(), limits).unwrap();
    let pins = specs
        .iter()
        .map(|spec| spec.pin(fixture))
        .collect::<Vec<_>>();
    let nodes = specs
        .iter()
        .zip(&pins)
        .map(|(spec, pin)| NodeInput {
            name: &spec.name,
            version: &spec.version,
            source_pin: pin.id(),
        })
        .collect::<Vec<_>>();
    let roots = roots
        .iter()
        .map(|(declaration_index, node)| RootInput {
            declaration_index: *declaration_index,
            node,
        })
        .collect::<Vec<_>>();
    let edges = edges
        .iter()
        .map(|(from, to, declaration)| EdgeInput {
            from,
            to,
            declaration,
        })
        .collect::<Vec<_>>();
    let graph = LockGraphInput {
        key: &key,
        semantics: LockMarkerSemantics::Current2026,
        runtime: RuntimePin::new([9; 32], 1, *target.fingerprint()).unwrap(),
        root_declarations: originals,
        roots: &roots,
        nodes: &nodes,
        edges: &edges,
    };
    let model = PythonLockModel::from_parts_with_builds(&pins, &[graph], builds, limits)
        .expect("the independent locked graph must be structurally closed");
    fs::write(
        &fixture.lock_path,
        format!(
            "lockfile_version=4\n{}",
            model.encode_projection(limits).unwrap()
        ),
    )
    .unwrap();
    let root = BoundPythonLock::acquire(
        &fixture.lock_path,
        limits,
        Instant::now() + Duration::from_secs(15),
    )
    .expect("actual schema2 root/image prerequisite");
    for pin in &pins {
        assert_eq!(
            root.existing_pin(pin.id()).unwrap().unwrap().pin().id(),
            pin.id()
        );
    }
    (root, key)
}
pub(super) fn ordinary_graph(
    fixture: &Fixture,
    target: &FrozenRegistryTarget,
    specs: &[Spec],
) -> (BoundPythonLock, GroupKey) {
    write_graph(
        fixture,
        target,
        ORIGINALS,
        specs,
        &[(0, "demo-pkg"), (2, "demo-pkg")],
        &[("demo-pkg", "dep", DEPENDENCY)],
    )
}
pub(super) async fn prelude(
    fixture: &Fixture,
    root: &BoundPythonLock,
    target: &FrozenRegistryTarget,
    specs: &[Spec],
) {
    for spec in specs {
        let pin = spec.pin(fixture);
        fixture.seed_cache(&spec.filename, pin.digest(), &spec.bytes);
        let source = acquire_locked_registry_file(
            root.existing_pin(pin.id()).unwrap().unwrap(),
            fixture.context(NetworkPolicy::Offline, &NO_ROUTE),
        )
        .await
        .expect("actual expected replay must pass before the new strict constructor");
        let observed = route_locked_metadata(
            source,
            target.supported(),
            Default::default(),
            Default::default(),
        )
        .expect("actual A/C1/M1 must pass before strict graph behavior is counted");
        match observed {
            LockedMetadataState::Wheel(owner) => {
                owner.recheck_for(root).unwrap();
                assert_eq!(owner.facts().name().as_str(), spec.name);
                assert_eq!(owner.pin().id(), pin.id());
            }
            LockedMetadataState::NeedsBuild(source) => assert_eq!(source.pin().id(), pin.id()),
        }
    }
}
pub(super) fn endpoints<'a>(
    fixture: &'a Fixture,
    route: &'a dyn SourceRequestRouter,
) -> [LockedRegistryEndpoint<'a>; 1] {
    let old = fixture.context(NetworkPolicy::Allowed, route);
    [LockedRegistryEndpoint {
        project: old.project_session,
        file: old.file_session,
    }]
}
pub(super) fn context<'a>(
    fixture: &'a Fixture,
    endpoints: &'a [LockedRegistryEndpoint<'a>],
    policy: &'a PythonSourcePolicy,
    network: NetworkPolicy,
) -> LockedGraphContext<'a> {
    let old = fixture.context(network, &ROUTE);
    LockedGraphContext {
        client: old.client,
        endpoints,
        source_policy: policy,
        fetch: old.policy,
        limits: LockedGraphLimits {
            source: old.source_limits,
            ..Default::default()
        },
        cache_root: &fixture.cache,
        scratch_root: &fixture.scratch,
        deadline: Instant::now() + Duration::from_secs(5),
    }
}
pub(super) fn ready<'lock, 'config>(
    result: Result<LockedGraphPreparation<'lock, 'config>, LockedGraphError>,
) -> LockedPreparedPlan<'lock> {
    match result.unwrap() {
        LockedGraphPreparation::Ready(plan) => plan,
        _ => panic!("a complete locked wheel graph must be Ready"),
    }
}
