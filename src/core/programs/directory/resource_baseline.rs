//! Fixed, owned 64-wheel query/apply baseline. The language archive is the
//! deterministic repository fixture; the wheel trees and catalog are real.
use super::*;
use crate::libraries::language::repository::tests::{archive, install_spec, prepare};
use crate::libraries::package::layouts::{prepare_layout, tests::wheel};
use crate::packages::python_environment::model::{
    PackageGraphNode, PackageGraphRecord, PackageSourcePin,
};
use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
use crate::packages::python_resolution::lock_model::{
    ContentPin, PinEvidence, RegistryArtifactKind, RegistryFileRef, RegistryFileRefInput,
    canonical_registry_base_fingerprint,
};
use crate::programs::query::environment::query_observed;
use crate::programs::shared::commit::{commit_package_selection, prepare_package_selection_commit};
use pakro_index::python_source::{PythonRegistry, PythonRegistryId, PythonSourcePolicy};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn descriptors() -> usize {
    std::fs::read_dir("/dev/fd").unwrap().count()
}

#[tokio::test(flavor = "current_thread")]
async fn p1_resource_sixty_four_package_query_and_apply_baseline() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    let a = root.join("a");
    let b = root.join("b");
    for path in [&stage, &a, &b] {
        std::fs::create_dir(path).unwrap();
    }
    // A reachable, counted fixture endpoint proves selection never attempts
    // acquisition even with offline disabled. TLS is unnecessary: any attempt
    // is counted and the connection is immediately closed.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let registry =
        PythonRegistry::new("fixture", &format!("https://{address}/simple/"), None).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicBool::new(false));
    let counted = requests.clone();
    let done = stopped.clone();
    let network = std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    counted.fetch_add(1, Ordering::Relaxed);
                    drop(stream);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => panic!("fixture listener failed: {error}"),
            }
        }
    });
    let initial = context(&stage);
    let mut policy = *initial.policy();
    policy.offline = false;
    policy.deadline = Instant::now() + Duration::from_secs(300);
    policy.budget.max_declarations = 4096;
    let context = capture_operation_context(OperationContextInputs {
        settings: initial.settings(),
        policy,
        cancellation: CancellationToken::new(),
        data_root: initial.requested_data_root().to_owned(),
        cache_root: initial.requested_cache_root().to_owned(),
        python_sources: Some(crate::programs::PythonSourceInputs {
            registries: vec![registry.clone()],
            policy: PythonSourcePolicy::Explicit(PythonRegistryId::new("fixture").unwrap()),
        }),
        python_target_description: None,
    })
    .unwrap();
    let bytes = archive("3.15.0");
    let language = prepare(&install_spec(&bytes), &context, &bytes)
        .await
        .unwrap();
    let language_key = language.key();
    let base = canonical_registry_base_fingerprint(registry.canonical_base());
    let mut graph = PackageGraphRecord {
        roots: vec![],
        nodes: vec![],
        edges: vec![],
    };
    let mut wheels = vec![];
    let mut declaration = String::from("[tools]\npython='3.15.0'\n[packages.\"python@3.15.0\"]\n");
    let keys: Vec<_> = (0..64u8).map(|index| [index + 80; 32]).collect();
    for (index, key) in keys.iter().enumerate() {
        let name = format!("p1pkg{index:02}");
        let module = format!("{name}.py");
        let (mut facts, archive) = wheel(
            &context,
            &language,
            &name,
            *key,
            vec![],
            vec![(&module, b"VALUE = 1\n")],
            None,
        );
        let file = RegistryFileRef::new(
            RegistryFileRefInput {
                registry_id: "fixture",
                canonical_base_fingerprint: base,
                project: &name,
                version: "1.0",
                filename: archive.filename(),
                kind: RegistryArtifactKind::Wheel,
            },
            Default::default(),
        )
        .unwrap();
        let pin = ContentPin::new(
            &file,
            archive.digest(),
            archive.size(),
            PinEvidence::Advertised,
            Default::default(),
        )
        .unwrap();
        facts.source = hex::encode(pin.id().as_bytes());
        graph.roots.push(index);
        graph.nodes.push(PackageGraphNode {
            name: name.clone(),
            version: "1.0".into(),
            instance: *key,
            source_pin: facts.source.clone(),
            artifact: facts.artifact.clone(),
            pin: PackageSourcePin {
                registry: "fixture".into(),
                base,
                filename: archive.filename().into(),
                size: archive.size(),
                advertised: true,
                build: None,
            },
        });
        declaration.push_str(&format!("{name}='1.0'\n"));
        wheels.push((facts, archive));
    }
    let layout = prepare_layout(&context, &language, graph.fingerprint().unwrap(), wheels).unwrap();
    let layout_key = layout.key();
    let mut files = 0usize;
    let mut payload_bytes = 0u64;
    for package in layout.packages() {
        for entry in package.manifest().storage.inventory.entries() {
            if let TreeInventoryKind::File { size, .. } = &entry.kind {
                files += 1;
                payload_bytes += size;
            }
        }
    }
    drop(layout);
    drop(language);
    for project in [&a, &b] {
        std::fs::write(project.join("pakro.toml"), &declaration).unwrap();
    }
    let observed = observe(&a);
    commit_package_selection(
        prepare_package_selection_commit(&context, &observed, language_key, layout_key, graph)
            .unwrap(),
    )
    .unwrap();
    drop(observed);
    let start_fd = descriptors();
    let peak = Arc::new(AtomicUsize::new(start_fd));
    let sampling_done = Arc::new(AtomicBool::new(false));
    let peak_fd = peak.clone();
    let done = sampling_done.clone();
    let sampler = std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            peak_fd.fetch_max(descriptors(), Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let package_checks = || {
        keys.iter()
            .map(|key| crate::libraries::package::lease::full_inventory_checks(*key))
            .sum::<usize>()
    };
    let request = PythonQueryRequest {
        target: None,
        scope: PythonQueryScope::Environment,
    };
    let mut samples = vec![];
    for iteration in 0..2 {
        let total = Instant::now();
        let now = Instant::now();
        let observed = observe(&b);
        let observation_ms = now.elapsed().as_secs_f64() * 1000.0;
        let package_before = package_checks();
        let language_before = crate::libraries::language::lease::full_inventory_checks();
        let now = Instant::now();
        let selected = query_observed(&context, &observed, &request).unwrap();
        let query_ms = now.elapsed().as_secs_f64() * 1000.0;
        let query_package_inventories = package_checks() - package_before;
        let query_language_inventories =
            crate::libraries::language::lease::full_inventory_checks() - language_before;
        assert!(matches!(&selected.report, PythonQueryReport::Ready(_)));
        assert_eq!(
            query_package_inventories, 128,
            "one acquire and one full graph binding check per package"
        );
        let registrations = ProjectRepository::open_readonly(&context)
            .unwrap()
            .read_views()
            .registered_projects()
            .len();
        assert_eq!(
            registrations,
            if iteration == 0 { 1 } else { 2 },
            "query must not register"
        );
        let package_before = package_checks();
        let language_before = crate::libraries::language::lease::full_inventory_checks();
        let now = Instant::now();
        let report = apply_selection(&context, &observed, selected).unwrap();
        let commit_ms = now.elapsed().as_secs_f64() * 1000.0;
        assert!(matches!(report, PythonQueryReport::Ready(_)));
        let commit_package_inventories = package_checks() - package_before;
        let commit_language_inventories =
            crate::libraries::language::lease::full_inventory_checks() - language_before;
        drop(observed);
        samples.push(serde_json::json!({"iteration": iteration, "observation_ms": observation_ms,
            "query_match_verify_ms": query_ms, "commit_revalidate_ms": commit_ms,
            "total_ms": total.elapsed().as_secs_f64() * 1000.0,
            "query_package_inventories": query_package_inventories, "query_language_inventories": query_language_inventories,
            "commit_package_inventories": commit_package_inventories, "commit_language_inventories": commit_language_inventories}));
    }
    sampling_done.store(true, Ordering::Relaxed);
    sampler.join().unwrap();
    let released_fd = descriptors();
    assert!(
        released_fd <= start_fd + 1,
        "query/apply leaked descriptors: {start_fd} -> {released_fd}"
    );
    stopped.store(true, Ordering::Relaxed);
    network.join().unwrap();
    let requests = requests.load(Ordering::Relaxed);
    assert_eq!(requests, 0);
    println!(
        "P1_RESOURCE {}",
        serde_json::json!({ "scenario": "query_apply_64_real_wheels", "runtime": "deterministic_repository_fixture",
        "packages": 64, "package_files": files, "package_bytes": payload_bytes,
        "fd_initial": start_fd, "fd_peak_sampled": peak.load(Ordering::Relaxed), "fd_released": released_fd,
        "registry_connections": requests, "offline": false, "samples": samples })
    );
}
