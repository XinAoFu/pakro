//! Real public backend wheels and native compilers, through the project kernel.
use super::*;
use futures_util::FutureExt;
use crate::config::{Config, FrozenConfigDiscovery, Settings};
use crate::libraries::language::{lease::LanguageLease, repository::LanguageInstallSpec};
use crate::libraries::project::{
    identity::ProjectIdentity, package_graph::CommittedGraph, repository::ProjectRepository,
    snapshot::{ReadPolicy, observe_project},
};
use crate::packages::python_resolution::lock_model::PythonLockModel;
use crate::plugins::core::python::native::pipeline::RuntimeInstallPlan;
use crate::programs::*;
use std::{path::{Path, PathBuf}, time::Instant};

fn context_for(settings: &Settings, registry: &pakro_index::python_source::PythonRegistry,
    stage: &Path, deadline: Instant) -> OperationContext {
    capture_operation_context(OperationContextInputs {
        settings, policy: OperationPolicy {
            locked: false, offline: false, deadline,
            budget: RequestBudget { max_arguments: 16, max_total_argument_bytes: 4096, max_declarations: 32, max_runtime_candidates: 16 },
            writes: AllowedWrites { libraries: true, project_selection: true, lockfile: true, declarations: false },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: stage.join("data"), cache_root: stage.join("cache"),
        python_sources: Some(PythonSourceInputs { registries: vec![registry.clone()],
            policy: PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap()) }),
        python_target_description: None,
    }).unwrap()
}

async fn actual_language(context: &OperationContext) -> LanguageLease {
    let archive = PathBuf::from(std::env::var_os("PAKRO_TEST_PYTHON_ARCHIVE").expect("a real official PBS archive is required"));
    let filename = archive.file_name().unwrap().to_str().unwrap().to_owned();
    let digest = std::env::var("PAKRO_TEST_PYTHON_SHA256").expect("the official PBS SHA256 is required");
    let digest: pakro_store::Digest = format!("sha256-{digest}").parse().unwrap();
    let url = std::env::var("PAKRO_TEST_PYTHON_URL").expect("the official PBS release URL is required");
    let (version, _, target, flavor) = crate::plugins::core::python::native::pbs_filename(&filename).unwrap().expect("runtime input must be an actual supported PBS archive");
    let platform = crate::lockfile::PlatformInfo { url: Some(url), checksum: Some(digest.to_string().replacen('-', ":", 1)), ..Default::default() };
    let spec = LanguageInstallSpec::from_plan(&RuntimeInstallPlan::from_locked(&version, &target, flavor, &platform).unwrap(), false).unwrap();
    crate::programs::install::runtime::prepare_language(&spec, context, || async {
        crate::artifact_snapshot::spool_verified_reader(std::fs::File::open(&archive)?, &filename, digest, 128 * 1024 * 1024, context.requested_cache_root())
    }).await.unwrap()
}

fn observe(project: &Path, deadline: Instant) -> crate::libraries::project::snapshot::ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![project.parent().unwrap().to_owned()];
    observe_project(project, ReadPolicy::new(discovery, deadline)).unwrap()
}

fn committed(context: &OperationContext, project: &Path) -> CommittedGraph {
    let observed = observe(project, context.policy().deadline);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    ProjectRepository::open_readonly(context).unwrap().read_views().package_graph(&identity).unwrap().unwrap()
}

fn catalog_bytes(context: &OperationContext) -> Option<Vec<u8>> {
    use pakro_store::control::{ControlLimits, ControlNamespace, ReadStore, RootLimits};
    let store = ReadStore::open_readonly(&context.requested_data_root().join("projects"),
        nix::unistd::geteuid().as_raw(), RootLimits::default(), context.policy().deadline).unwrap()?;
    let control = store.control(ControlLimits::default()).unwrap();
    control.read(ControlNamespace::Catalog, "projects").unwrap().map(|snapshot| snapshot.bytes().to_vec())
}

fn import(context: &OperationContext, graph: &CommittedGraph, code: &str) {
    let layout = crate::libraries::package::lease::acquire_layout(context, *graph.layout()).unwrap().unwrap();
    let output = std::process::Command::new(layout.bin().join("python"))
        .args(["-B", "-c", &format!("{code}; print('P6_NATIVE_IMPORT_OK')")])
        .output().unwrap();
    assert!(output.status.success(), "actual native import failed: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "P6_NATIVE_IMPORT_OK");
}

async fn native_backend(case_name: &str) {
    let parent_started = Instant::now();
    let parent_deadline = parent_started + Duration::from_secs(580);
    let manifest = PathBuf::from(std::env::var_os("PAKRO_TEST_NATIVE_ASSETS").expect("run the fixed native input preparation first"));
    let (fixture, case) = ProjectRegistryFixture::native_assets(&manifest, case_name);
    let registry = (*fixture.endpoint().project.registry).clone();
    let owner = tempfile::tempdir().unwrap();
    let stage = owner.path().canonicalize().unwrap();
    let request_started = Instant::now();
    let deadline = request_started + Duration::from_secs(480);
    let deadline = if case_name == "scikit" { deadline.min(parent_deadline) } else { deadline };
    if case_name == "scikit" {
        eprintln!("P6_NATIVE_REQUEST_BUDGET_CONTRACT case=scikit each_request_max_s=480 fixed_parent_max_s=580 operations=prepare_language_and_a,b,cold,changed_cc_zip,changed_cc_no_zip");
        eprintln!("P6_NATIVE_REQUEST_START phase=a parent_elapsed_s={:.3} deadline_after_parent_start_s={:.3}", request_started.duration_since(parent_started).as_secs_f64(), deadline.duration_since(parent_started).as_secs_f64());
    }
    let mut settings = Settings::default();
    settings.http_timeout = "30s".into();
    settings.http_download_timeout = "300s".into();
    settings.python.github_attestations = Some(false);
    settings.python.build.sdk = std::env::var_os("PAKRO_TEST_NATIVE_SDK").map(PathBuf::from);
    settings.python.build.sysroot = std::env::var_os("PAKRO_TEST_NATIVE_SYSROOT").map(PathBuf::from);
    for (slot, key) in [
        (&mut settings.python.build.tools.cc, "PAKRO_TEST_NATIVE_CC"),
        (&mut settings.python.build.tools.cxx, "PAKRO_TEST_NATIVE_CXX"),
        (&mut settings.python.build.tools.ar, "PAKRO_TEST_NATIVE_AR"),
        (&mut settings.python.build.tools.linker, "PAKRO_TEST_NATIVE_LINKER"),
        (&mut settings.python.build.tools.cmake, "PAKRO_TEST_NATIVE_CMAKE"),
        (&mut settings.python.build.tools.ninja, "PAKRO_TEST_NATIVE_NINJA"),
        (&mut settings.python.build.tools.meson, "PAKRO_TEST_NATIVE_MESON"),
        (&mut settings.python.build.tools.cargo, "PAKRO_TEST_NATIVE_CARGO"),
        (&mut settings.python.build.tools.rustc, "PAKRO_TEST_NATIVE_RUSTC"),
        (&mut settings.python.build.tools.pkg_config, "PAKRO_TEST_NATIVE_PKG_CONFIG"),
        (&mut settings.python.build.tools.make, "PAKRO_TEST_NATIVE_MAKE"),
    ] {
        if let Some(path) = std::env::var_os(key) { *slot = Some(path.into()); }
    }
    let context = context_for(&settings, &registry, &stage, deadline);
    if matches!(case_name, "maturin" | "setuptools-rust") { case.seed_crate_cache(&context); }
    let language = actual_language(&context).boxed_local().await;
    let language_key = language.key();
    let language_target = *language.target().fingerprint();
    let version = language.lock_version().unwrap();
    drop(language);
    let first = fixture.project_path().parent().unwrap().canonicalize().unwrap();
    let second = stage.join("project-b");
    std::fs::create_dir(&second).unwrap();
    let declaration = format!("[tools]\npython='{version}'\n[packages.\"python@{version}\"]\n{}='{}'\n", case.name, case.version);
    std::fs::write(first.join("pakro.toml"), &declaration).unwrap();
    std::fs::write(second.join("pakro.toml"), &declaration).unwrap();
    let config = Config::load_from_config_files(Default::default(), false).boxed_local().await.unwrap();
    crate::programs::python::project_plan(context.clone(), observe(&first, deadline), &[]).unwrap().unwrap()
        .install(config.clone()).boxed_local().await.unwrap();
    let first_graph = committed(&context, &first);
    import(&context, &first_graph, &case.import_code);
    let lock_path = first.join("pakro.lock");
    let first_bytes = std::fs::read(&lock_path).unwrap();
    let root = crate::packages::python_resolution::lock_session::BoundPythonLock::acquire(&lock_path, Default::default(), deadline).unwrap();
    let model = root.model().unwrap().clone();
    let node = model.graphs()[0].nodes().iter().find(|node| node.name() == case.name).unwrap();
    let expected = root.source_build(node.source_pin()).unwrap().clone();
    assert_eq!(expected.recipe, 2);
    assert_eq!(expected.backend, case.backend);
    expected.native.as_ref().expect("actual native recipe must retain its fixed toolchain").validate().unwrap();
    if matches!(case_name, "maturin" | "setuptools-rust") {
        let configuration = &expected.native.as_ref().unwrap().configuration;
        assert!(configuration.keys().any(|key| key.contains("cargo") && key.contains("lock")), "Cargo.lock identity was not retained");
        assert!(configuration.keys().any(|key| key.contains("cargo") && key.contains("input")), "fixed registry crate identity was not retained");
        #[cfg(target_os = "macos")]
        {
            assert_eq!(configuration.get("env.CURL_SSL_BACKEND").map(String::as_str), Some("Secure-Transport"));
            assert_eq!(configuration.get("env.CARGO_PROFILE_RELEASE_STRIP").map(String::as_str), Some("none"));
        }
    }
    let encoded = model.encode_projection(Default::default()).unwrap();
    assert!(encoded.contains("schema = 4\n"));
    assert_eq!(PythonLockModel::decode_projection(&encoded, Default::default()).unwrap(), model);
    drop(root);
    let (context, deadline, request_started) = if case_name == "scikit" {
        context.checkpoint().unwrap();
        eprintln!("P6_NATIVE_REQUEST_COMPLETE phase=a request_elapsed_s={:.3} parent_elapsed_s={:.3} deadline_after_parent_start_s={:.3}", request_started.elapsed().as_secs_f64(), parent_started.elapsed().as_secs_f64(), deadline.duration_since(parent_started).as_secs_f64());
        drop(context);
        let request_started = Instant::now();
        let deadline = (request_started + Duration::from_secs(480)).min(parent_deadline);
        eprintln!("P6_NATIVE_REQUEST_START phase=b parent_elapsed_s={:.3} deadline_after_parent_start_s={:.3}", request_started.duration_since(parent_started).as_secs_f64(), deadline.duration_since(parent_started).as_secs_f64());
        (context_for(&settings, &registry, &stage, deadline), deadline, request_started)
    } else { (context, deadline, request_started) };
    std::fs::write(second.join("pakro.lock"), &first_bytes).unwrap();
    let before = fixture.requests().len();
    crate::programs::python::project_plan(context.clone(), observe(&second, deadline), &[]).unwrap().unwrap()
        .install(config).boxed_local().await.unwrap();
    let warm_requests = fixture.requests();
    assert_eq!(warm_requests.len(), before, "project B with the same complete lock must reuse the published graph without HTTP; new requests: {:?}", &warm_requests[before..]);
    let second_graph = committed(&context, &second);
    assert_eq!(first_graph.layout(), second_graph.layout(), "two projects did not share the same actual native layout");
    assert_eq!(first_graph.nodes(), second_graph.nodes());
    import(&context, &second_graph, &case.import_code);
    assert_eq!(std::fs::read(&lock_path).unwrap(), first_bytes);
    let second_bytes = std::fs::read(second.join("pakro.lock")).unwrap();
    assert_eq!(second_bytes, first_bytes, "strict warm recovery must preserve the copied complete lock");
    if matches!(case_name, "setuptools" | "scikit") {
        let original_catalog = catalog_bytes(&context);
        let (context, deadline, request_started) = if case_name == "scikit" {
            context.checkpoint().unwrap();
            eprintln!("P6_NATIVE_REQUEST_COMPLETE phase=b request_elapsed_s={:.3} parent_elapsed_s={:.3} deadline_after_parent_start_s={:.3}", request_started.elapsed().as_secs_f64(), parent_started.elapsed().as_secs_f64(), deadline.duration_since(parent_started).as_secs_f64());
            drop(context);
            let request_started = Instant::now();
            let deadline = (request_started + Duration::from_secs(480)).min(parent_deadline);
            eprintln!("P6_NATIVE_REQUEST_START phase=cold parent_elapsed_s={:.3} deadline_after_parent_start_s={:.3}", request_started.duration_since(parent_started).as_secs_f64(), deadline.duration_since(parent_started).as_secs_f64());
            (context_for(&settings, &registry, &stage, deadline), deadline, request_started)
        } else { (context, deadline, request_started) };
        // Cold package artifacts under the existing verified language: only
        // package/output/source caches are empty, not the interpreter itself.
        let cold_stage = stage.join("cold");
        std::fs::create_dir(&cold_stage).unwrap();
        let cold = context_for(&settings, &registry, &cold_stage, deadline);
        assert!(!cold.requested_data_root().exists() && !cold.requested_cache_root().exists());
        let language = crate::libraries::language::repository::LanguageRepository::open_readonly(&context)
            .unwrap().acquire_by_key(&language_key).unwrap().expect("the actual original language must remain Ready");
        assert_eq!(language.key(), language_key);
        assert_eq!(*language.target().fingerprint(), language_target);
        assert!(crate::libraries::package::lease::acquire_package(&cold, first_graph.nodes()[0].instance).unwrap().is_none(), "the cold package library must have no Ready package fallback");
        let cold_catalog = catalog_bytes(&cold);
        let root = crate::packages::python_resolution::lock_session::BoundPythonLock::acquire(&lock_path, Default::default(), deadline).unwrap();
        if case_name == "scikit" {
            assert_eq!(root.model(), Some(&model));
            assert_eq!(root.source_build(node.source_pin()), Some(&expected));
        }
        let endpoints = [fixture.endpoint()];
        let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
        let mut registry_context = fixture.context(&endpoints, &policy);
        registry_context.environment = language.target().environment();
        registry_context.supported = language.target().supported();
        registry_context.semantics = language.target().semantics();
        registry_context.deadline = deadline;
        let cache = cold_stage.join("strict-cache");
        let scratch = cold_stage.join("strict-scratch");
        assert!(!cache.exists() && !scratch.exists(), "cold package source/output caches must start empty");
        std::fs::create_dir_all(&scratch).unwrap();
        registry_context.cache_root = &cache;
        registry_context.scratch_root = &scratch;
        let mut budget = BuildBudget::new();
        let before_cold_http = fixture.requests().len();
        let replayed = replay_registry(&cold, &language, &root, vec![format!("{}=={}", case.name, case.version)], registry_context.clone(), &mut budget, 0).await;
        if case_name == "setuptools" {
            let error = match replayed {
                Ok(_) => panic!("the actual C nonce output must not replace the locked artifact"),
                Err(error) => error,
            };
            assert!(error.chain().any(|cause| cause.to_string() == "strict source rebuilt output differs from the locked artifact"), "another error is not output-change evidence: {error:#}");
        } else {
            let closed = close_registry_plan(replayed.expect("deterministic real C++ output must recover from original locked inputs"), Default::default()).unwrap();
            closed.recheck_for(&root).unwrap();
            assert_eq!(closed.plan().selected.len(), 1);
            let output = closed.owner(closed.plan().selected[0].key).unwrap();
            assert_eq!(output.output_digest().to_string(), expected.output_digest);
            assert_eq!(output.build_record().unwrap().fingerprint().unwrap(), expected.fingerprint().unwrap());
            assert_eq!(budget.builds, 1, "the cold C++ backend must actually build once");
            drop(closed);
            let zip = cache.join("artifacts").join(&expected.output_digest).join(&expected.output_filename);
            assert!(zip.is_file(), "successful cold rebuild must retain its verified output ZIP");
            assert!(crate::libraries::package::lease::acquire_package(&cold, first_graph.nodes()[0].instance).unwrap().is_none(), "preparing a graph must not publish a Ready package");
        }
        assert!(budget.builds > 0 && budget.active.is_empty() && budget.workspace_reserved == 0);
        assert!(fixture.requests().len() > before_cold_http, "cold package recovery did not acquire original source/build inputs");
        assert_eq!(budget.policy_work, 0, "cold exact replay must not solve a new graph");
        assert_eq!(root.source_build(node.source_pin()), Some(&expected));
        assert_eq!(std::fs::read(&lock_path).unwrap(), first_bytes);
        assert_eq!(std::fs::read(second.join("pakro.lock")).unwrap(), second_bytes);
        assert_eq!(committed(&context, &first).layout(), first_graph.layout());
        assert_eq!(committed(&context, &second).layout(), second_graph.layout());
        assert_eq!(catalog_bytes(&context), original_catalog);
        assert_eq!(catalog_bytes(&cold), cold_catalog);
        import(&context, &first_graph, &case.import_code);
        eprintln!("P6_NATIVE_PACKAGE_COLD_COMPLETE case={case_name} existing_language=true actual_http=true builds={}", budget.builds);
        if case_name == "scikit" {
            context.checkpoint().unwrap();
            cold.checkpoint().unwrap();
            root.recheck().unwrap();
            eprintln!("P6_NATIVE_REQUEST_COMPLETE phase=cold request_elapsed_s={:.3} parent_elapsed_s={:.3} deadline_after_parent_start_s={:.3}", request_started.elapsed().as_secs_f64(), parent_started.elapsed().as_secs_f64(), deadline.duration_since(parent_started).as_secs_f64());
            // Each refusal is another distinct request under this parent's
            // fixed deadline; none of the previous request's owners is renewed.
            drop(registry_context);
            drop(budget);
            drop(root);
            drop(language);
            drop(cold);
            drop(context);
            let zip = cache.join("artifacts").join(&expected.output_digest).join(&expected.output_filename);
            // This is a real configured compiler wrapper, not a forged receipt:
            // Its bytes differ; probes still use the original C compiler layout.
            use std::os::unix::fs::PermissionsExt;
            let compiler = settings.python.build.tools.cc.clone().or_else(|| {
                let paths = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
                ["cc", "clang", "gcc"].iter().find_map(|name| paths.iter().map(|path| path.join(name)).find(|path| path.is_file()))
            }).expect("the successful C++ build must retain its original C compiler").canonicalize().unwrap();
            let changed_tool = stage.join("changed-compiler");
            std::fs::create_dir(&changed_tool).unwrap();
            let changed_cc = changed_tool.join("clang");
            let shim = format!("#!/bin/sh\nexec '{}' \"$@\"\n", compiler.to_str().unwrap().replace('\'', "'\\''"));
            std::fs::write(&changed_cc, &shim).unwrap();
            std::fs::set_permissions(&changed_cc, std::fs::Permissions::from_mode(0o755)).unwrap();
            use sha2::{Digest as _, Sha256};
            let changed_sha: [u8; 32] = Sha256::digest(shim.as_bytes()).into();
            assert_ne!(changed_sha, expected.native.as_ref().unwrap().programs.iter().find(|program| program.role == "cc").unwrap().sha256);
            let mut changed_settings = settings.clone();
            changed_settings.python.build.tools.cc = Some(changed_cc);
            let hooks = crate::packages::python_source_build::observed_hook_calls();
            let http = fixture.requests().len();
            for with_zip in [true, false] {
                let request_started = Instant::now();
                let request_deadline = (request_started + Duration::from_secs(480)).min(parent_deadline);
                eprintln!("P6_NATIVE_TOOLCHAIN_REQUEST_START with_zip={with_zip} parent_elapsed_s={:.3} deadline_after_parent_start_s={:.3}", request_started.duration_since(parent_started).as_secs_f64(), request_deadline.duration_since(parent_started).as_secs_f64());
                let original = context_for(&changed_settings, &registry, &stage, request_deadline);
                let changed = context_for(&changed_settings, &registry, &cold_stage, request_deadline);
                let language = crate::libraries::language::repository::LanguageRepository::open_readonly(&original)
                    .unwrap().acquire_by_key(&language_key).unwrap().expect("each refusal must reacquire the actual original language");
                assert_eq!(language.key(), language_key);
                assert_eq!(*language.target().fingerprint(), language_target);
                let root = crate::packages::python_resolution::lock_session::BoundPythonLock::acquire(&lock_path, Default::default(), request_deadline).unwrap();
                assert_eq!(root.model(), Some(&model));
                assert_eq!(root.source_build(node.source_pin()), Some(&expected));
                let mut registry_context = fixture.context(&endpoints, &policy);
                registry_context.environment = language.target().environment();
                registry_context.supported = language.target().supported();
                registry_context.semantics = language.target().semantics();
                registry_context.deadline = request_deadline;
                registry_context.cache_root = &cache;
                registry_context.scratch_root = &scratch;
                if !with_zip { std::fs::remove_file(&zip).unwrap(); }
                let mut refused = BuildBudget::new();
                let error = match replay_registry(&changed, &language, &root, vec![format!("{}=={}", case.name, case.version)], registry_context, &mut refused, 0).await {
                    Ok(_) => panic!("changed actual CC input must not reuse or rebuild a locked native result"),
                    Err(error) => error,
                };
                eprintln!("P6_NATIVE_TOOLCHAIN_REQUEST_RESULT with_zip={with_zip} request_elapsed_s={:.3} parent_elapsed_s={:.3} error={error:#}", request_started.elapsed().as_secs_f64(), parent_started.elapsed().as_secs_f64());
                assert!(error.chain().any(|cause| cause.to_string() == "locked native toolchain, SDK, driver or Cargo inputs changed"), "another error is not toolchain gate evidence: {error:#}");
                assert_eq!(crate::packages::python_source_build::observed_hook_calls(), hooks, "a changed toolchain must be rejected before PEP 517 hooks");
                assert_eq!(fixture.requests().len(), http, "the toolchain gate must use the already verified original source");
                assert!(refused.active.is_empty() && refused.workspace_reserved == 0 && refused.completed.is_empty());
                assert_eq!(refused.policy_work, 0);
                if with_zip { assert!(zip.is_file()); }
                assert_eq!(catalog_bytes(&original), original_catalog);
                assert_eq!(catalog_bytes(&changed), cold_catalog);
                assert_eq!(std::fs::read(&lock_path).unwrap(), first_bytes);
                assert_eq!(std::fs::read(second.join("pakro.lock")).unwrap(), second_bytes);
                if !with_zip {
                    assert_eq!(committed(&original, &first).layout(), first_graph.layout());
                    assert_eq!(committed(&original, &second).layout(), second_graph.layout());
                    import(&original, &first_graph, &case.import_code);
                }
                root.recheck().unwrap();
                original.checkpoint().unwrap();
                changed.checkpoint().unwrap();
                eprintln!("P6_NATIVE_TOOLCHAIN_REQUEST_COMPLETE with_zip={with_zip} request_elapsed_s={:.3} parent_elapsed_s={:.3} deadline_after_parent_start_s={:.3}", request_started.elapsed().as_secs_f64(), parent_started.elapsed().as_secs_f64(), request_deadline.duration_since(parent_started).as_secs_f64());
            }
            eprintln!("P6_NATIVE_STRICT_COLD_COMPLETE deterministic_cpp=true changed_cc_zip_and_cold_denied=true hooks_unchanged=true budget_contract=distinct_requests_each_max_480s_under_fixed_580s_parent");
        }
    }
    if case_name == "scikit" {
        assert!(Instant::now() < parent_deadline, "the complete C++ parent exceeded its fixed 580 second deadline");
    }
    eprintln!("P6_NATIVE_BACKEND_COMPLETE case={case_name} recipe=2 shared=true warm_http=0 actual_import=true");
}

#[ignore = "requires the fixed native input preparation (PAKRO_TEST_NATIVE_ASSETS, PAKRO_TEST_PYTHON_ARCHIVE/SHA256/URL, real toolchain)"]
#[tokio::test]
async fn p6_pure_setuptools_sdist_installs_without_a_compiler_and_keeps_recipe1() {
    async {
        let manifest = PathBuf::from(std::env::var_os("PAKRO_TEST_NATIVE_ASSETS")
            .expect("the existing fixed backend wheels are required"));
        let (fixture, case) = ProjectRegistryFixture::native_assets(&manifest, "pure-setuptools");
        let registry = (*fixture.endpoint().project.registry).clone();
        let owner = tempfile::tempdir().unwrap();
        let stage = owner.path().canonicalize().unwrap();
        let deadline = Instant::now() + Duration::from_secs(480);
        let missing_cc = stage.join("compiler-does-not-exist");
        assert!(!missing_cc.exists());
        let mut settings = Settings::default();
        settings.http_timeout = "30s".into();
        settings.http_download_timeout = "300s".into();
        settings.python.github_attestations = Some(false);
        settings.python.build.tools.cc = Some(missing_cc.clone());
        let context = context_for(&settings, &registry, &stage, deadline);
        let language = actual_language(&context).boxed_local().await;
        let version = language.lock_version().unwrap();
        drop(language);
        let project = fixture.project_path().parent().unwrap().canonicalize().unwrap();
        std::fs::write(project.join("pakro.toml"), format!(
            "[tools]\npython='{version}'\n[packages.\"python@{version}\"]\n{}='{}'\n",case.name,case.version)).unwrap();
        let config = Config::load_from_config_files(Default::default(), false).boxed_local().await.unwrap();
        crate::programs::python::project_plan(context.clone(), observe(&project, deadline), &[])
            .unwrap().unwrap().install(config).boxed_local().await.unwrap();
        let graph = committed(&context, &project);
        import(&context, &graph, &case.import_code);
        let root = crate::packages::python_resolution::lock_session::BoundPythonLock::acquire(
            &project.join("pakro.lock"), Default::default(), deadline).unwrap();
        let model = root.model().unwrap();
        let node = model.graphs()[0].nodes().iter().find(|node| node.name() == case.name).unwrap();
        let build = root.source_build(node.source_pin()).expect("real setuptools source build must be locked");
        assert_eq!(build.backend, "setuptools.build_meta");
        assert_eq!(build.recipe, 1, "a pure setuptools package must retain the original no-fork recipe");
        assert!(build.native.is_none(), "pure builds must not discover or pin a native toolchain");
        assert!(build.inputs.iter().any(|input| input.name == "setuptools" && input.version == "80.9.0"
            && input.output_filename == "setuptools-80.9.0-py3-none-any.whl"),
            "this must execute the real fixed setuptools backend rather than a synthetic backend");
        assert!(build.output_filename.ends_with("-py3-none-any.whl"));
        let encoded = model.encode_projection(Default::default()).unwrap();
        assert!(encoded.contains("schema = 3\n"));
        assert_eq!(PythonLockModel::decode_projection(&encoded, Default::default()).unwrap(), *model);
        assert!(fixture.requests().iter().any(|request| request == "/files/setuptools-80.9.0-py3-none-any.whl"));
        assert!(!missing_cc.exists());
        eprintln!("P6_REAL_PURE_SETUPTOOLS_COMPLETE setuptools=80.9.0 recipe=1 native=false actual_import=true missing_cc=true");
    }.boxed_local().await;
}

#[ignore = "requires the fixed native input preparation (PAKRO_TEST_NATIVE_ASSETS, PAKRO_TEST_PYTHON_ARCHIVE/SHA256/URL, real toolchain)"]
#[tokio::test]
async fn p6_native_setuptools_c_shared_warm_and_changed_cold_output_rejected() { native_backend("setuptools").boxed_local().await; }
#[ignore = "requires the fixed native input preparation (PAKRO_TEST_NATIVE_ASSETS, PAKRO_TEST_PYTHON_ARCHIVE/SHA256/URL, real toolchain)"]
#[tokio::test]
async fn p6_native_scikit_cpp_cmake_try_run_shared_warm() {
    const TEST: &str = "programs::install::build::tests::native::p6_native_scikit_cpp_cmake_try_run_shared_warm";
    const CHILD: &str = "PAKRO_TEST_P6_CPP_COLD_CHILD";
    // One exact child keeps the existing global hook counter independent of
    // other libtest cases; it is not a test-group or process supervisor.
    if std::env::var(CHILD).ok().as_deref() != Some(TEST) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"]).env(CHILD, TEST).output().unwrap();
        assert!(output.status.success(), "C++ strict native fixture failed:\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    native_backend("scikit").boxed_local().await;
}
#[ignore = "requires the fixed native input preparation (PAKRO_TEST_NATIVE_ASSETS, PAKRO_TEST_PYTHON_ARCHIVE/SHA256/URL, real toolchain)"]
#[tokio::test]
async fn p6_native_meson_c_try_run_shared_warm() { native_backend("meson").boxed_local().await; }
#[ignore = "requires the fixed native input preparation (PAKRO_TEST_NATIVE_ASSETS, PAKRO_TEST_PYTHON_ARCHIVE/SHA256/URL, real toolchain)"]
#[tokio::test]
async fn p6_native_maturin_rust_registry_inputs_shared_warm() { native_backend("maturin").boxed_local().await; }
#[ignore = "requires the fixed native input preparation (PAKRO_TEST_NATIVE_ASSETS, PAKRO_TEST_PYTHON_ARCHIVE/SHA256/URL, real toolchain)"]
#[tokio::test]
async fn p6_native_setuptools_rust_registry_inputs_shared_warm() { native_backend("setuptools-rust").boxed_local().await; }
#[ignore = "requires the fixed native input preparation (PAKRO_TEST_NATIVE_ASSETS, PAKRO_TEST_PYTHON_ARCHIVE/SHA256/URL, real toolchain)"]
#[tokio::test]
async fn p6_native_public_markupsafe_sdist_imports_actual_speedups_shared_warm() { native_backend("markupsafe").boxed_local().await; }
