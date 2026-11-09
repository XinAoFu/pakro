//! Public Conda inputs, recorded once and replayed by a local verified mirror.
#[path = "http_fixture.rs"]
mod http_fixture;
#[path = "link_fixture_tests.rs"]
mod link_fixture_tests;
#[path = "closeout_tests.rs"]
mod closeout_tests;
#[path = "script_cli_tests.rs"]
mod script_cli_tests;
use pakro_util::cmd::CmdLineRunner;
use rattler_conda_types::RepoDataRecord;
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Output,
    time::{Duration, Instant},
};

const CONFIG: &str = r#"[tools]
python = { version = "3.13.13", source = "conda", channel = "conda-forge", build = "h20e6be0_100_cp313" }

[settings.python.conda]
channels = ["conda-forge"]

[packages."python@3.13.13"]
idna = { version = "3.10", source = "conda", channel = "conda-forge", build = "pyhd8ed1ab_1" }
"#;

/// Resolve the exact mixed-test Conda roots using public repodata. This creates
/// only a new owned directory with the gateway cache and descriptive records;
/// a separate preparation step must obtain and verify the selected archives.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires live Conda repodata and a new owned input directory"]
async fn p8_prepare_real_mixed_conda_records() {
    use crate::libraries::language::source::{PythonRuntimeRequest, PythonRuntimeSource};
    use crate::packages::python_resolution::lock_model::{LockModelLimits, PythonLockModel};
    use crate::packages::request::{PackageInstallRequest, PackageSource};
    use crate::programs::{
        AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
        capture_operation_context,
    };

    let stage = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_RECORDS_DIR")
            .expect("new absolute owned records directory required"),
    );
    assert!(stage.is_absolute());
    fs::create_dir(&stage).expect("records directory must not exist");
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o700)).unwrap();
    let stage = stage.canonicalize().unwrap();
    let receipt_path = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_SEED_RECEIPT")
            .expect("verified mixed seed receipt required"),
    );
    let receipt_bytes = fs::read(receipt_path).unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
    let python_version = std::env::var("PAKRO_TEST_MIXED_PYTHON_VERSION")
        .expect("exact Conda Python version required");
    let python_build =
        std::env::var("PAKRO_TEST_MIXED_PYTHON_BUILD").expect("exact Conda Python build required");
    assert_eq!(python_version, "3.13.13");
    assert_eq!(python_build, "h20e6be0_100_cp313");
    let numpy = &receipt["conda_numpy"];
    let host = super::super::conda::native_target().unwrap();
    assert_eq!(numpy["subdir"].as_str(), Some(host.0.as_str()));
    let version = numpy["version"].as_str().unwrap().to_owned();
    let build = numpy["build"].as_str().unwrap().to_owned();
    let runtime = PythonRuntimeRequest {
        version: python_version.clone(),
        source: PythonRuntimeSource::Conda,
        channel: Some("conda-forge".into()),
        build: Some(python_build),
    };
    let request = PackageInstallRequest {
        runtime_name: "python".into(),
        runtime_version: python_version,
        name: "numpy".into(),
        version,
        source: PackageSource::Conda,
        channel: Some("conda-forge".into()),
        build: Some(build),
        extras: vec![],
        marker: None,
    };
    let mut settings = crate::config::Settings::default();
    settings.http_timeout = "30s".into();
    settings.http_download_timeout = "30m".into();
    settings.python.conda.channels = vec!["conda-forge".into()];
    let context = capture_operation_context(OperationContextInputs {
        settings: &settings,
        policy: OperationPolicy {
            locked: false,
            offline: false,
            deadline: Instant::now() + Duration::from_secs(580),
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 16,
                max_runtime_candidates: 8,
            },
            writes: AllowedWrites {
                libraries: false,
                project_selection: false,
                lockfile: false,
                declarations: false,
            },
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: stage.join("data"),
        cache_root: stage.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap();
    let limits = LockModelLimits::default();
    let empty = PythonLockModel::from_parts(&[], &[], limits).unwrap();
    let model = super::super::conda::resolve_requests_into_lock(
        &context,
        &host,
        Some(&runtime),
        &[request],
        vec![],
        vec![],
        empty,
        String::new(),
        "default".into(),
        [0; 32],
        limits,
    )
    .await
    .unwrap();
    let graph = &model.conda()[0];
    let selected = graph
        .records
        .iter()
        .find(|record| record.package_record.name.as_normalized() == "numpy")
        .unwrap();
    let selected_sha: [u8; 32] = selected.package_record.sha256.unwrap().into();
    assert_eq!(hex::encode(selected_sha), numpy["sha256"].as_str().unwrap());
    assert_eq!(
        selected.url.as_str(),
        numpy["canonical_channel_url"].as_str().unwrap()
    );
    assert!(
        !stage.join("data").exists(),
        "metadata-only resolve wrote a data root"
    );
    let mut cache_hashes = BTreeMap::new();
    for entry in walkdir::WalkDir::new(stage.join("cache")) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let name = entry
                .path()
                .strip_prefix(&stage)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            cache_hashes.insert(name, sha256(entry.path()));
        }
    }
    assert!(!cache_hashes.is_empty(), "gateway did not retain metadata");
    fs::write(stage.join("seed-receipt.json"), &receipt_bytes).unwrap();
    fs::write(
        stage.join("records.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1, "artifacts_prepared": false, "graph": graph,
            "metadata_cache_sha256": cache_hashes,
            "seed_receipt_sha256": hex::encode(Sha256::digest(&receipt_bytes)),
        }))
        .unwrap(),
    )
    .unwrap();
}

fn sha256(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    hex::encode(hash.finalize())
}

fn reject_network(env: &mut BTreeMap<String, OsString>, proxy: &http_fixture::Server) {
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        env.insert(key.into(), proxy.url().into());
    }
    for key in ["NO_PROXY", "no_proxy"] {
        env.insert(key.into(), "127.0.0.1,localhost".into());
    }
}

fn save_metadata(source: &Path, destination: &Path) -> BTreeMap<String, String> {
    let mut checksums = BTreeMap::new();
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(source).unwrap();
        let output = destination.join(relative);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::copy(entry.path(), &output).unwrap();
        let digest = sha256(&output);
        assert_eq!(digest, sha256(entry.path()));
        checksums.insert(relative.to_string_lossy().into_owned(), digest);
    }
    assert!(
        !checksums.is_empty(),
        "initial solve did not save original repodata"
    );
    checksums
}

fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, u64, i64, i64)> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            (
                entry.path().strip_prefix(root).unwrap().to_owned(),
                (
                    metadata.ino(),
                    metadata.len(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                ),
            )
        })
        .collect()
}

fn environment(root: &Path, storage: &Path, offline: bool) -> BTreeMap<String, OsString> {
    let mut env = BTreeMap::new();
    for (key, child) in [
        ("HOME", "home"),
        ("PAKRO_DATA_DIR", "data"),
        ("PAKRO_CACHE_DIR", "cache"),
        ("PAKRO_STATE_DIR", "state"),
        ("PAKRO_CONFIG_DIR", "config"),
        ("PAKRO_SYSTEM_DIR", "system"),
        ("XDG_CONFIG_HOME", "xdg-config"),
        ("XDG_DATA_HOME", "xdg-data"),
        ("XDG_CACHE_HOME", "xdg-cache"),
        ("XDG_STATE_HOME", "xdg-state"),
        ("TMPDIR", "tmp"),
    ] {
        let path = storage.join(child);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        env.insert(key.into(), path.into_os_string());
    }
    let sentinels = storage.join("managers");
    fs::create_dir_all(&sentinels).unwrap();
    for name in ["conda", "mamba", "micromamba", "pip", "pip3", "uv"] {
        let path = sentinels.join(name);
        fs::write(
            &path,
            b"#!/bin/sh\nprintf '%s\\n' \"$0\" >> \"$PAKRO_TEST_MANAGER_LOG\"\nexit 91\n",
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    env.insert(
        "PATH".into(),
        format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", sentinels.display()).into(),
    );
    env.insert(
        "PAKRO_TEST_MANAGER_LOG".into(),
        storage.join("manager-invocations").into_os_string(),
    );
    env.insert(
        "PAKRO_TRUSTED_CONFIG_PATHS".into(),
        root.as_os_str().to_owned(),
    );
    for (key, value) in [
        ("PAKRO_AUTO_UPDATE", "0"),
        ("PAKRO_USE_VERSIONS_HOST", "0"),
        ("PAKRO_YES", "1"),
        ("PAKRO_LOCKFILE", "1"),
        ("PAKRO_EXPERIMENTAL", "1"),
        ("PAKRO_COLOR", "0"),
        ("NO_COLOR", "1"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("LANG", "C"),
        ("LC_ALL", "C"),
    ] {
        env.insert(key.into(), value.into());
    }
    env.insert(
        "PAKRO_OFFLINE".into(),
        if offline { "1" } else { "0" }.into(),
    );
    if !offline {
        for key in ["NO_PROXY", "no_proxy"] {
            env.insert(key.into(), "*".into());
        }
    }
    if offline {
        env.insert("HTTP_PROXY".into(), "http://127.0.0.1:9".into());
        env.insert("HTTPS_PROXY".into(), "http://127.0.0.1:9".into());
    }
    env
}

async fn run(
    binary: &Path,
    cwd: &Path,
    args: &[&str],
    env: &BTreeMap<String, OsString>,
    deadline: Instant,
) -> Output {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .expect("Conda CLI lifecycle exceeded 580 seconds");
    let mut command = CmdLineRunner::new(binary)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .with_timeout(remaining);
    for (key, value) in env {
        command = command.env(key, value);
    }
    let output = command.output_isolated(16 * 1024 * 1024).await.unwrap();
    eprintln!(
        "P8_CONDA_CLI args={args:?} exit={:?}\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

async fn success(
    binary: &Path,
    cwd: &Path,
    args: &[&str],
    env: &BTreeMap<String, OsString>,
    deadline: Instant,
) -> String {
    let output = run(binary, cwd, args, env, deadline).await;
    assert!(
        output.status.success(),
        "Conda CLI {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

async fn import_idna(
    binary: &Path,
    cwd: &Path,
    env: &BTreeMap<String, OsString>,
    deadline: Instant,
) -> String {
    let executable = success(binary, cwd, &["which", "python"], env, deadline).await;
    let path = PathBuf::from(executable.trim());
    let result = success(
        &path,
        cwd,
        &["-B", "-c", "import idna; print(idna.__version__)"],
        env,
        deadline,
    )
    .await;
    assert_eq!(result.trim(), "3.10");
    executable
}

/// Required inputs: `PAKRO_TEST_BINARY` is the freshly built CLI and
/// `PAKRO_TEST_CONDA_EVIDENCE_DIR` is a new absolute directory. The existing
/// official seeds are read from `target/p8-inputs-20261028/verification.json`;
/// `PAKRO_TEST_CONDA_SEEDS` can select the same verified input elsewhere.
/// The initial solve uses the real public HTTPS channel. Afterwards only exact,
/// digest-verified archives are routed to a local HTTP mirror; a rejecting
/// proxy counts every attempted external connection. Lock URLs stay official.
/// `PAKRO_TEST_CONDA_PREPARED_INPUTS` optionally names an existing inputs.json
/// containing the original records and verified archives for the first install.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a fresh CLI, public conda-forge access, and an empty evidence directory"]
async fn p8_real_conda_cli_install_warm_cold_shared_forget_prune() {
    let binary =
        PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").expect("fresh CLI is required"))
            .canonicalize()
            .unwrap();
    let evidence = PathBuf::from(
        std::env::var_os("PAKRO_TEST_CONDA_EVIDENCE_DIR")
            .expect("a new absolute evidence directory is required"),
    );
    assert!(evidence.is_absolute());
    fs::create_dir(&evidence).expect("evidence directory must not already exist");
    fs::write(evidence.join("project.toml"), CONFIG).unwrap();
    fs::write(evidence.join("binary.sha256"), sha256(&binary)).unwrap();
    let seeds_path = std::env::var_os("PAKRO_TEST_CONDA_SEEDS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/p8-inputs-20261028/verification.json")
        });
    let seed_bytes = fs::read(seeds_path).expect("verified official seed receipt is required");
    let seeds: serde_json::Value = serde_json::from_slice(&seed_bytes).unwrap();
    assert_eq!(seeds["complete"], true);
    fs::write(evidence.join("original-seeds.json"), &seed_bytes).unwrap();
    fs::write(
        evidence.join("original-seeds.sha256"),
        hex::encode(Sha256::digest(&seed_bytes)),
    )
    .unwrap();

    // Keep the isolated workspace on both success and failure so the review
    // session can inspect or clean up the exact owned directory afterwards.
    let root = tempfile::Builder::new()
        .prefix("pakro-p8-conda-")
        .tempdir()
        .unwrap()
        .keep()
        .canonicalize()
        .unwrap();
    let marker = root.join(".pakro-p8-conda-fixture-owner");
    let marker_bytes = b"pakro-p8-conda-cli-fixture-v1\n";
    fs::write(&marker, marker_bytes).unwrap();
    let root_metadata = fs::metadata(&root).unwrap();
    let marker_metadata = fs::metadata(&marker).unwrap();
    let receipt = serde_json::json!({
        "root": root,
        "uid": root_metadata.uid(),
        "gid": root_metadata.gid(),
        "device": root_metadata.dev(),
        "inode": root_metadata.ino(),
        "mode": root_metadata.mode() & 0o7777,
        "process_id": std::process::id(),
        "marker": marker,
        "marker_device": marker_metadata.dev(),
        "marker_inode": marker_metadata.ino(),
        "marker_sha256": hex::encode(Sha256::digest(marker_bytes)),
    });
    fs::write(
        evidence.join("fixture-owner.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    eprintln!("P8_CONDA_FIXTURE_ROOT {}", root.display());
    let warm = root.join("warm");
    let cold = root.join("cold");
    let probe_storage = root.join("probe-storage");
    for directory in [&warm, &cold, &probe_storage] {
        fs::create_dir(directory).unwrap();
    }
    let mut online = environment(&root, &warm, false);
    let empty = root.join("empty");
    fs::create_dir(&empty).unwrap();
    let a = root.join("a");
    let b = root.join("b");
    let c = root.join("cold-project");
    let probe_project = root.join("proxy-probe");
    for project in [&a, &b, &c, &probe_project] {
        fs::create_dir(project).unwrap();
        fs::write(project.join("pakro.toml"), CONFIG).unwrap();
    }
    let started = Instant::now();
    let deadline = started + Duration::from_secs(580);
    let proxy = http_fixture::Server::start(None);
    let mut probe_env = environment(&root, &probe_storage, false);
    reject_network(&mut probe_env, &proxy);
    assert!(
        !run(&binary, &probe_project, &["install"], &probe_env, deadline)
            .await
            .status
            .success(),
        "rejecting proxy probe must not install"
    );
    let probe_requests = proxy.requests();
    assert!(
        !probe_requests.is_empty(),
        "the actual CLI did not use the configured rejecting proxy"
    );
    fs::write(
        evidence.join("proxy-probe.json"),
        serde_json::to_vec_pretty(&probe_requests).unwrap(),
    )
    .unwrap();

    let mut prepared_records: Option<Vec<RepoDataRecord>> = None;
    let mut prepared_mirror = None;
    let mut prepared_paths = BTreeSet::new();
    if let Some(path) = std::env::var_os("PAKRO_TEST_CONDA_PREPARED_INPUTS") {
        let path = PathBuf::from(path)
            .canonicalize()
            .expect("prepared inputs.json is missing");
        let input_bytes = fs::read(&path).expect("prepared inputs.json cannot be read");
        let input: serde_json::Value = serde_json::from_slice(&input_bytes).unwrap();
        assert_eq!(input["schema"], 1);
        assert_eq!(input["schema_kind"], "pakro-conda-preserved-inputs");
        assert_eq!(input["complete"], true);
        let expected: Vec<RepoDataRecord> =
            serde_json::from_value(input["records"].clone()).unwrap();
        let artifacts = input["artifacts"]
            .as_array()
            .expect("prepared artifacts are missing");
        assert!(!expected.is_empty());
        assert_eq!(
            expected.len(),
            artifacts.len(),
            "prepared record/artifact count differs"
        );
        let directory = path.parent().expect("prepared inputs have no parent");
        let mut routes = BTreeMap::new();
        let mut replacements = BTreeMap::new();
        let mut names = BTreeSet::new();
        for record in &expected {
            let name = record.identifier.to_string();
            assert!(
                names.insert(name.clone()),
                "duplicate prepared artifact {name}"
            );
            let item = artifacts
                .iter()
                .find(|item| item["filename"].as_str() == Some(name.as_str()))
                .expect("prepared record has no matching artifact");
            let digest: [u8; 32] = record
                .package_record
                .sha256
                .expect("prepared SHA256 is missing")
                .into();
            let digest = hex::encode(digest);
            let size = record
                .package_record
                .size
                .expect("prepared size is missing");
            assert_eq!(item["original_url"].as_str(), Some(record.url.as_str()));
            assert_eq!(item["sha256"].as_str(), Some(digest.as_str()));
            assert_eq!(item["size"].as_u64(), Some(size));
            assert_eq!(item["verified"], true);
            let relative = Path::new(
                item["relative_path"]
                    .as_str()
                    .expect("prepared artifact path is missing"),
            );
            assert!(
                relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
            );
            let file = directory.join(relative);
            assert_eq!(
                fs::metadata(&file).unwrap().len(),
                size,
                "prepared artifact size differs: {name}"
            );
            assert_eq!(
                sha256(&file),
                digest,
                "prepared artifact digest differs: {name}"
            );
            let route = format!("/initial/{digest}/{name}");
            assert!(routes.insert(route.clone(), file).is_none());
            assert!(
                replacements
                    .insert(record.url.to_string(), route.clone())
                    .is_none()
            );
            prepared_paths.insert(route);
        }
        fs::write(evidence.join("prepared-inputs.json"), &input_bytes).unwrap();
        fs::write(
            evidence.join("prepared-inputs.sha256"),
            hex::encode(Sha256::digest(&input_bytes)),
        )
        .unwrap();
        let server = http_fixture::Server::start(Some(routes));
        let mut replacements: BTreeMap<_, _> = replacements
            .into_iter()
            .map(|(url, route)| (url, format!("{}{route}", server.url())))
            .collect();
        // A newly selected archive is input drift, never an unrecorded public download.
        replacements.insert(
            r"regex:^https?://[^?#]+\.(?:conda|tar\.bz2)(?:\?.*)?$".into(),
            format!("{}/input-drift", server.url()),
        );
        online.insert(
            "PAKRO_URL_REPLACEMENTS".into(),
            serde_json::to_string(&replacements).unwrap().into(),
        );
        prepared_records = Some(expected);
        prepared_mirror = Some(server);
    }

    let first_install = run(&binary, &a, &["install"], &online, deadline).await;
    if let Some(server) = &prepared_mirror {
        fs::write(
            evidence.join("initial-mirror-requests.json"),
            serde_json::to_vec_pretty(&server.requests()).unwrap(),
        )
        .unwrap();
    }
    if !first_install.status.success() {
        let error = String::from_utf8_lossy(&first_install.stderr);
        if prepared_mirror.as_ref().is_some_and(|server| {
            server
                .requests()
                .iter()
                .any(|request| request.contains("/input-drift"))
        }) {
            panic!(
                "Conda prepared input drift: the public solver selected an unrecorded artifact: {error}"
            );
        }
        if prepared_mirror.is_some() && error.contains("artifact digest mismatch") {
            panic!(
                "Conda prepared input drift: the selected artifact digest differs from the fixed input: {error}"
            );
        }
        if prepared_mirror.is_some()
            && error.contains("Conda archive size differs from locked record")
        {
            panic!(
                "Conda prepared input drift: the selected artifact size differs from the fixed input: {error}"
            );
        }
        panic!("Conda CLI install failed: {error}");
    }
    let lock = fs::read(a.join("pakro.lock")).unwrap();
    fs::write(evidence.join("original-pakro.lock"), &lock).unwrap();
    fs::write(
        evidence.join("original-pakro.lock.sha256"),
        hex::encode(Sha256::digest(&lock)),
    )
    .unwrap();
    let model =
        crate::packages::python_resolution::lock_session::decode_root(&lock, Default::default())
            .unwrap()
            .unwrap();
    assert_eq!(model.conda().len(), 1);
    let records = model.conda()[0].records.clone();
    assert!(!records.is_empty());
    if let Some(expected_records) = &prepared_records {
        let actual: BTreeMap<_, _> = records
            .iter()
            .map(|record| (record.package_record.name.as_normalized(), record))
            .collect();
        let expected: BTreeMap<_, _> = expected_records
            .iter()
            .map(|record| (record.package_record.name.as_normalized(), record))
            .collect();
        assert_eq!(
            actual.len(),
            records.len(),
            "duplicate selected Conda records"
        );
        assert_eq!(
            expected.len(),
            expected_records.len(),
            "duplicate prepared Conda records"
        );
        assert_eq!(
            actual, expected,
            "Conda prepared input drift: public solver records differ"
        );
        let calls = prepared_mirror.as_ref().unwrap().requests();
        let requested: BTreeSet<_> = calls
            .iter()
            .map(|line| {
                let mut words = line.split_whitespace();
                assert_eq!(words.next(), Some("GET"));
                words.next().unwrap().to_owned()
            })
            .collect();
        assert_eq!(
            requested, prepared_paths,
            "initial install must fetch exactly the prepared archives"
        );
    }
    fs::write(
        evidence.join("records.json"),
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();
    fs::write(
        evidence.join("records.sha256"),
        sha256(&evidence.join("records.json")),
    )
    .unwrap();
    for name in ["python", "idna"] {
        let seed = seeds["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["name"] == name
                    && item["version"] == if name == "python" { "3.13.13" } else { "3.10" }
            })
            .unwrap();
        let actual = records
            .iter()
            .find(|r| r.package_record.name.as_normalized() == name)
            .unwrap();
        let digest: [u8; 32] = actual.package_record.sha256.unwrap().into();
        assert_eq!(
            actual.identifier.to_string(),
            seed["filename"].as_str().unwrap()
        );
        assert_eq!(actual.url.as_str(), seed["url"].as_str().unwrap());
        assert_eq!(
            hex::encode(digest),
            seed["expected_sha256"].as_str().unwrap()
        );
        assert_eq!(actual.package_record.size, seed["expected_size"].as_u64());
    }
    let metadata = save_metadata(&warm.join("cache/conda"), &evidence.join("repodata"));
    fs::write(
        evidence.join("repodata-sha256.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let mut routes = BTreeMap::new();
    let mut replay_paths = BTreeMap::new();
    let mut artifacts = Vec::new();
    let channel_config = rattler_conda_types::ChannelConfig::default_with_root_dir(root.clone());
    let official_channel =
        rattler_conda_types::Channel::from_str("conda-forge", &channel_config).unwrap();
    for record in &records {
        let digest: [u8; 32] = record.package_record.sha256.unwrap().into();
        let digest = hex::encode(digest);
        let size = record.package_record.size.unwrap();
        let name = record.identifier.to_string();
        let channel = record
            .channel
            .as_deref()
            .expect("fixed Conda source is required");
        assert_eq!(
            rattler_conda_types::Channel::from_str(channel, &channel_config)
                .unwrap()
                .base_url,
            official_channel.base_url
        );
        assert_eq!(
            record.url.scheme(),
            "https",
            "initial artifacts must use official HTTPS source records"
        );
        let channel_hash = hex::encode(Sha256::digest(channel.as_bytes()));
        let cached = warm
            .join("cache/conda-artifacts")
            .join(channel_hash)
            .join("artifacts")
            .join(format!("sha256-{digest}"))
            .join(&name);
        assert_eq!(fs::metadata(&cached).unwrap().len(), size);
        assert_eq!(
            sha256(&cached),
            digest,
            "initial cached archive differs from official lock"
        );
        let copied = evidence.join("artifacts").join(&digest).join(&name);
        fs::create_dir_all(copied.parent().unwrap()).unwrap();
        fs::copy(&cached, &copied).unwrap();
        assert_eq!(sha256(&copied), digest);
        let route = format!("/locked/{digest}/{name}");
        assert!(routes.insert(route.clone(), copied.clone()).is_none());
        assert!(replay_paths.insert(record.url.to_string(), route).is_none());
        artifacts.push(serde_json::json!({"name": record.package_record.name.as_normalized(),
            "version": record.package_record.version.to_string(), "build": record.package_record.build.as_str(),
            "channel": channel, "url": record.url.as_str(), "size": size, "sha256": digest,
            "saved": copied.strip_prefix(&evidence).unwrap()}));
    }
    fs::write(
        evidence.join("artifacts.json"),
        serde_json::to_vec_pretty(&artifacts).unwrap(),
    )
    .unwrap();
    fs::write(
        evidence.join("artifacts.sha256"),
        sha256(&evidence.join("artifacts.json")),
    )
    .unwrap();
    let mirror = http_fixture::Server::start(Some(routes));
    let replacements: BTreeMap<_, _> = replay_paths
        .iter()
        .map(|(original, path)| (original.clone(), format!("{}{path}", mirror.url())))
        .collect();
    let catalog = warm.join("data/projects/.control/catalog/projects");
    let before = fs::read(&catalog).unwrap();
    let invalid = CONFIG.replace(
        "version = \"3.10\", source = \"conda\"",
        "version = \"3.11\", source = \"conda\"",
    );
    assert_ne!(invalid, CONFIG);
    fs::write(a.join("pakro.toml"), invalid).unwrap();
    assert!(
        !run(&binary, &a, &["install"], &online, deadline)
            .await
            .status
            .success()
    );
    assert_eq!(fs::read(a.join("pakro.lock")).unwrap(), lock);
    assert_eq!(fs::read(&catalog).unwrap(), before);
    fs::write(a.join("pakro.toml"), CONFIG).unwrap();
    let first = success(&binary, &a, &["where", "python"], &online, deadline).await;
    let payload = PathBuf::from(first.trim());
    let instance = payload
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    import_idna(&binary, &a, &online, deadline).await;
    let language_manifest = warm
        .join("data/languages/.control/catalog")
        .join(format!("{instance}.json"));
    let language_bytes = fs::read(&language_manifest).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&language_bytes).unwrap();
    let runtime_dependencies: Vec<[u8; 32]> =
        serde_json::from_value(manifest["runtime_dependencies"].clone()).unwrap();
    let runtime_key = *runtime_dependencies
        .first()
        .expect("real Conda Python has runtime packages");
    assert!(
        records
            .iter()
            .filter(|r| r.package_record.name.as_normalized() != "python")
            .any(
                |r| crate::libraries::package::conda::package_key(r, &records).unwrap()
                    == runtime_key
            )
    );
    let runtime_instance = format!("package-{}", hex::encode(runtime_key));

    // Network remains enabled: any swallowed network error is still counted.
    let mut blocked = online.clone();
    reject_network(&mut blocked, &proxy);
    blocked.insert("PAKRO_LOCKED".into(), "1".into());
    if prepared_mirror.is_some() {
        blocked.insert(
            "PAKRO_URL_REPLACEMENTS".into(),
            serde_json::to_string(&replacements).unwrap().into(),
        );
    }
    let empty_cache = warm.join("empty-cache");
    fs::create_dir(&empty_cache).unwrap();
    fs::set_permissions(&empty_cache, fs::Permissions::from_mode(0o700)).unwrap();
    blocked.insert("PAKRO_CACHE_DIR".into(), empty_cache.into_os_string());
    let proxy_before_warm = proxy.requests().len();
    let mirror_before_warm = mirror.requests().len();
    let prepared_before_warm = prepared_mirror
        .as_ref()
        .map(|server| server.requests().len());
    success(&binary, &a, &["install"], &blocked, deadline).await;
    fs::write(b.join("pakro.lock"), &lock).unwrap();
    success(&binary, &b, &["install"], &blocked, deadline).await;
    assert_eq!(
        success(&binary, &b, &["where", "python"], &blocked, deadline)
            .await
            .trim(),
        first.trim()
    );
    import_idna(&binary, &b, &blocked, deadline).await;
    assert_eq!(
        proxy.requests().len(),
        proxy_before_warm,
        "warm installation attempted external network"
    );
    assert_eq!(
        mirror.requests().len(),
        mirror_before_warm,
        "warm installation requested an archive"
    );
    assert_eq!(
        prepared_mirror
            .as_ref()
            .map(|server| server.requests().len()),
        prepared_before_warm,
        "warm installation requested a prepared archive"
    );

    // Only exact fixed artifact URLs are rewritten. Any attempt to re-read
    // metadata or contact another external URL reaches the rejecting proxy.
    fs::write(c.join("pakro.lock"), &lock).unwrap();
    let mut cold_env = environment(&root, &cold, false);
    reject_network(&mut cold_env, &proxy);
    cold_env.insert("PAKRO_LOCKED".into(), "1".into());
    cold_env.insert(
        "PAKRO_URL_REPLACEMENTS".into(),
        serde_json::to_string(&replacements).unwrap().into(),
    );
    let proxy_before_cold = proxy.requests().len();
    let mirror_before_cold = mirror.requests().len();
    success(&binary, &c, &["install"], &cold_env, deadline).await;
    import_idna(&binary, &c, &cold_env, deadline).await;
    assert_eq!(fs::read(c.join("pakro.lock")).unwrap(), lock);
    assert_eq!(
        proxy.requests().len(),
        proxy_before_cold,
        "cold restore tried to contact external metadata"
    );
    assert!(
        !cold.join("cache/conda").exists(),
        "cold restore created the repodata cache"
    );
    let calls = mirror.requests();
    let calls = &calls[mirror_before_cold..];
    let fetched: BTreeSet<_> = calls
        .iter()
        .map(|line| {
            let mut words = line.split_whitespace();
            assert_eq!(words.next(), Some("GET"));
            words.next().unwrap().to_owned()
        })
        .collect();
    assert_eq!(
        fetched,
        replay_paths.values().cloned().collect(),
        "cold restore must fetch exactly the locked archives"
    );
    for record in &records {
        let digest: [u8; 32] = record.package_record.sha256.unwrap().into();
        let digest = hex::encode(digest);
        let channel_hash = hex::encode(Sha256::digest(
            record.channel.as_deref().unwrap().as_bytes(),
        ));
        let cached = cold
            .join("cache/conda-artifacts")
            .join(channel_hash)
            .join("artifacts")
            .join(format!("sha256-{digest}"))
            .join(record.identifier.to_string());
        assert_eq!(
            fs::metadata(&cached).unwrap().len(),
            record.package_record.size.unwrap()
        );
        assert_eq!(
            sha256(&cached),
            digest,
            "cold archive differs from the fixed source"
        );
    }
    fs::write(
        evidence.join("mirror-requests.json"),
        serde_json::to_vec_pretty(&mirror.requests()).unwrap(),
    )
    .unwrap();
    fs::write(
        evidence.join("proxy-requests.json"),
        serde_json::to_vec_pretty(&proxy.requests()).unwrap(),
    )
    .unwrap();

    let catalog_before_reference = fs::read(&catalog).unwrap();
    let data_before_preview = tree_snapshot(&warm.join("data"));
    let referenced = run(
        &binary,
        &empty,
        &["uninstall", "--instance", &runtime_instance, "--dry-run"],
        &blocked,
        deadline,
    )
    .await;
    assert!(!referenced.status.success());
    assert!(String::from_utf8_lossy(&referenced.stderr).contains("object is still referenced"));
    assert_eq!(
        fs::read(&catalog).unwrap(),
        catalog_before_reference,
        "protected preview changed the project catalog"
    );
    assert_eq!(
        fs::read(&language_manifest).unwrap(),
        language_bytes,
        "protected preview changed the language manifest"
    );
    assert_eq!(
        tree_snapshot(&warm.join("data")),
        data_before_preview,
        "protected preview changed the shared data tree"
    );
    let projects: serde_json::Value = serde_json::from_slice(&fs::read(&catalog).unwrap()).unwrap();
    let ids = projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| {
            (
                project["root"].as_str().unwrap().to_owned(),
                project["project"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    success(
        &binary,
        &empty,
        &["project", "forget", &ids[a.to_str().unwrap()]],
        &blocked,
        deadline,
    )
    .await;
    success(&binary, &empty, &["prune"], &blocked, deadline).await;
    import_idna(&binary, &b, &blocked, deadline).await;
    success(
        &binary,
        &empty,
        &["project", "forget", &ids[b.to_str().unwrap()]],
        &blocked,
        deadline,
    )
    .await;
    for args in [
        vec![
            "uninstall",
            "--instance",
            runtime_instance.as_str(),
            "--dry-run",
        ],
        vec!["uninstall", "--instance", runtime_instance.as_str()],
    ] {
        let rejected = run(&binary, &empty, &args, &blocked, deadline).await;
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("object is still referenced"));
    }
    success(&binary, &empty, &["prune"], &blocked, deadline).await;
    assert!(!payload.exists());
    assert_eq!(
        proxy.requests().len(),
        proxy_before_cold,
        "later local actions attempted external network"
    );
    assert_eq!(
        mirror.requests().len(),
        mirror_before_cold + calls.len(),
        "later local actions requested archives"
    );
    assert_eq!(
        prepared_mirror
            .as_ref()
            .map(|server| server.requests().len()),
        prepared_before_warm,
        "later local actions requested prepared archives"
    );
    assert!(
        !warm.join("manager-invocations").exists() && !cold.join("manager-invocations").exists()
    );
    fs::write(evidence.join("result.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "complete": true,
        "elapsed_seconds": started.elapsed().as_secs_f64(),
        "official_source": "https://conda.anaconda.org/conda-forge/",
        "prepared_inputs": prepared_records.is_some(),
        "initial_records": records.len(),
        "proxy_probe_requests": probe_requests.len(),
        "warm_external_requests": 0,
        "warm_mirror_requests": 0,
        "cold_external_requests": 0,
        "cold_locked_artifacts": fetched.len(),
        "runtime_dependency_instance": runtime_instance,
        "shared_python": instance,
        "preview_unchanged": true,
        "cold_lock_unchanged": true,
        "manager_invocations": 0,
        "proxy_visibility": "rejecting proxy records attempted HTTP request lines and HTTPS CONNECT host:port; it does not inspect TLS content"
    })).unwrap()).unwrap();
    eprintln!(
        "P8_CONDA_REAL_CLI_COMPLETE shared=true warm_zero_requests=true cold_only_locked_artifacts=true failure_preserved=true forget_prune=true"
    );
}

/// One explicitly supplied PBS release and one real PyPI wheel on the current
/// platform. Run one case per process; no platform or binary identity is guessed.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a fresh CLI and explicit official PBS and PyPI inputs"]
async fn p9_pbs_wheel_single_platform_lifecycle() {
    let deadline = Instant::now() + Duration::from_secs(580);
    let binary = PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").expect("fresh CLI required"))
        .canonicalize()
        .unwrap();
    let version = std::env::var("PAKRO_TEST_P9_PYTHON_VERSION").expect("Python version required");
    let build = std::env::var("PAKRO_TEST_P9_PBS_BUILD").expect("PBS build required");
    let archive =
        PathBuf::from(std::env::var_os("PAKRO_TEST_P9_PBS_ARCHIVE").expect("PBS archive required"));
    let api = PathBuf::from(
        std::env::var_os("PAKRO_TEST_P9_PBS_API").expect("original PBS API JSON required"),
    );
    let simple = PathBuf::from(
        std::env::var_os("PAKRO_TEST_P9_SIMPLE_JSON").expect("original PyPI Simple JSON required"),
    );
    let wheel =
        PathBuf::from(std::env::var_os("PAKRO_TEST_P9_WHEEL").expect("packaging wheel required"));
    let wheel_metadata = PathBuf::from(
        std::env::var_os("PAKRO_TEST_P9_WHEEL_METADATA").expect("wheel metadata required"),
    );
    assert_eq!(
        sha256(&api),
        std::env::var("PAKRO_TEST_P9_PBS_API_SHA256").expect("PBS API SHA256 required")
    );
    assert_eq!(
        sha256(&simple),
        std::env::var("PAKRO_TEST_P9_SIMPLE_SHA256").expect("Simple JSON SHA256 required")
    );
    let release: serde_json::Value = serde_json::from_slice(&fs::read(&api).unwrap()).unwrap();
    assert_eq!(release["tag_name"].as_str(), Some(build.as_str()));
    let archive_name = archive.file_name().unwrap().to_str().unwrap();
    let asset = release["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|asset| asset["name"].as_str() == Some(archive_name))
        .expect("PBS asset absent from official API");
    let archive_sha = asset["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    assert_eq!(sha256(&archive), archive_sha);
    assert_eq!(
        fs::metadata(&archive).unwrap().len(),
        asset["size"].as_u64().unwrap()
    );
    let archive_url = asset["browser_download_url"].as_str().unwrap();
    let page: serde_json::Value = serde_json::from_slice(&fs::read(&simple).unwrap()).unwrap();
    assert_eq!(page["name"].as_str(), Some("packaging"));
    let wheel_name = wheel.file_name().unwrap().to_str().unwrap();
    assert_eq!(wheel_name, "packaging-25.0-py3-none-any.whl");
    let file = page["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["filename"].as_str() == Some(wheel_name))
        .expect("wheel absent from official Simple metadata");
    assert_eq!(sha256(&wheel), file["hashes"]["sha256"].as_str().unwrap());
    assert_eq!(
        fs::metadata(&wheel).unwrap().len(),
        file["size"].as_u64().unwrap()
    );
    assert_eq!(
        sha256(&wheel_metadata),
        file["core-metadata"]["sha256"].as_str().unwrap()
    );
    let wheel_url = file["url"].as_str().unwrap();
    let api_url = format!("https://api.github.com/repositories/162334160/releases/tags/{build}");

    let evidence = PathBuf::from(
        std::env::var_os("PAKRO_TEST_P9_EVIDENCE_DIR").expect("new evidence directory required"),
    );
    assert!(evidence.is_absolute());
    fs::create_dir(&evidence).expect("evidence directory must not exist");
    fs::set_permissions(&evidence, fs::Permissions::from_mode(0o700)).unwrap();
    let root = tempfile::Builder::new()
        .prefix("pakro-p9-pbs-")
        .tempdir()
        .unwrap()
        .keep()
        .canonicalize()
        .unwrap();
    let metadata = fs::metadata(&root).unwrap();
    fs::write(
        evidence.join("fixture-owner.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "root": root, "uid": metadata.uid(), "device": metadata.dev(), "inode": metadata.ino(),
            "binary_sha256": sha256(&binary), "pbs_api_sha256": sha256(&api),
            "pbs_archive_sha256": archive_sha, "simple_sha256": sha256(&simple),
            "wheel_sha256": sha256(&wheel), "wheel_metadata_sha256": sha256(&wheel_metadata),
        }))
        .unwrap(),
    )
    .unwrap();
    let warm = root.join("warm");
    let cold = root.join("cold");
    let probe_storage = root.join("probe-storage");
    fs::create_dir(&warm).unwrap();
    fs::create_dir(&cold).unwrap();
    fs::create_dir(&probe_storage).unwrap();
    let a = root.join("a");
    let b = root.join("b");
    let c = root.join("c");
    let empty = root.join("empty");
    let probe = root.join("proxy-probe");
    let exact = format!("{version}+{build}");
    let declaration = format!(
        "[tools]\npython = '{}'\n[settings.python]\nprecompiled_flavor = 'install_only_stripped'\n[packages.'python@{}']\npackaging = '25.0'\n",
        exact, exact
    );
    for project in [&a, &b, &c] {
        fs::create_dir(project).unwrap();
        fs::write(project.join("pakro.toml"), &declaration).unwrap();
    }
    fs::create_dir(&probe).unwrap();
    fs::write(probe.join("pakro.toml"), &declaration).unwrap();
    fs::create_dir(&empty).unwrap();
    let server = http_fixture::Server::start(Some(BTreeMap::from([
        ("/p9/tag.json".into(), api),
        ("/p9/python.tar.gz".into(), archive),
        ("/p9/simple.json".into(), simple),
        ("/p9/wheel.whl".into(), wheel),
        ("/p9/wheel.whl.metadata".into(), wheel_metadata),
    ])));
    let proxy = http_fixture::Server::start(None);
    let mut probe_env = environment(&root, &probe_storage, false);
    reject_network(&mut probe_env, &proxy);
    assert!(
        !run(&binary, &probe, &["install"], &probe_env, deadline)
            .await
            .status
            .success()
    );
    let probe_requests = proxy.requests().len();
    assert!(
        probe_requests > 0,
        "the CLI did not use the rejecting proxy"
    );
    let replacements = BTreeMap::from([
        (api_url, format!("{}/p9/tag.json", server.url())),
        (
            archive_url.to_owned(),
            format!("{}/p9/python.tar.gz", server.url()),
        ),
        (
            "https://pypi.org/simple/packaging/".to_owned(),
            format!("{}/p9/simple.json", server.url()),
        ),
        (
            wheel_url.to_owned(),
            format!("{}/p9/wheel.whl", server.url()),
        ),
    ]);
    let mut warm_env = environment(&root, &warm, false);
    reject_network(&mut warm_env, &proxy);
    warm_env.insert(
        "PAKRO_URL_REPLACEMENTS".into(),
        serde_json::to_string(&replacements).unwrap().into(),
    );
    let mut cold_env = environment(&root, &cold, false);
    reject_network(&mut cold_env, &proxy);
    cold_env.insert(
        "PAKRO_URL_REPLACEMENTS".into(),
        serde_json::to_string(&replacements).unwrap().into(),
    );
    success(&binary, &a, &["install"], &warm_env, deadline).await;
    assert_eq!(
        proxy.requests().len(),
        probe_requests,
        "initial install attempted external network"
    );
    let lock = fs::read(a.join("pakro.lock")).unwrap();
    fs::write(evidence.join("pakro.lock"), &lock).unwrap();
    let payload = success(&binary, &a, &["where", "python"], &warm_env, deadline).await;
    let executable = success(&binary, &a, &["which", "python"], &warm_env, deadline).await;
    let actual = success(
        Path::new(executable.trim()),
        &a,
        &["-B", "-c", "import packaging; print(packaging.__version__)"],
        &warm_env,
        deadline,
    )
    .await;
    assert_eq!(actual.trim(), "25.0");
    let before = server.requests().len();
    let external = proxy.requests().len();
    let empty_cache = warm.join("empty-cache");
    fs::create_dir(&empty_cache).unwrap();
    let mut locked_warm = warm_env.clone();
    locked_warm.insert("PAKRO_CACHE_DIR".into(), empty_cache.into_os_string());
    locked_warm.insert("PAKRO_LOCKED".into(), "1".into());
    success(&binary, &a, &["install"], &locked_warm, deadline).await;
    fs::write(b.join("pakro.lock"), &lock).unwrap();
    success(&binary, &b, &["install"], &locked_warm, deadline).await;
    let b_payload = success(&binary, &b, &["where", "python"], &locked_warm, deadline).await;
    assert_eq!(b_payload.trim(), payload.trim());
    let a_metadata = fs::metadata(payload.trim()).unwrap();
    let b_metadata = fs::metadata(b_payload.trim()).unwrap();
    assert_eq!(
        (a_metadata.dev(), a_metadata.ino()),
        (b_metadata.dev(), b_metadata.ino())
    );
    let b_executable = success(&binary, &b, &["which", "python"], &locked_warm, deadline).await;
    assert_eq!(
        success(
            Path::new(b_executable.trim()),
            &b,
            &["-B", "-c", "import packaging; print(packaging.__version__)"],
            &locked_warm,
            deadline
        )
        .await
        .trim(),
        "25.0"
    );
    assert_eq!(
        server.requests().len(),
        before,
        "warm restore requested an input"
    );
    assert_eq!(
        proxy.requests().len(),
        external,
        "warm restore attempted external network"
    );

    fs::write(c.join("pakro.lock"), &lock).unwrap();
    cold_env.insert("PAKRO_LOCKED".into(), "1".into());
    success(&binary, &c, &["install"], &cold_env, deadline).await;
    let cold_executable = success(&binary, &c, &["which", "python"], &cold_env, deadline).await;
    let cold_payload = success(&binary, &c, &["where", "python"], &cold_env, deadline).await;
    assert!(Path::new(cold_payload.trim()).starts_with(cold.join("data/languages")));
    assert_ne!(cold_payload.trim(), payload.trim());
    assert_eq!(
        success(
            Path::new(cold_executable.trim()),
            &c,
            &["-B", "-c", "import packaging; print(packaging.__version__)"],
            &cold_env,
            deadline
        )
        .await
        .trim(),
        "25.0"
    );
    assert_eq!(fs::read(c.join("pakro.lock")).unwrap(), lock);
    assert_eq!(
        proxy.requests().len(),
        external,
        "cold restore attempted external network"
    );
    let cold_requests = server.requests();
    assert!(
        cold_requests[before..]
            .iter()
            .any(|line| line.contains("/p9/python.tar.gz"))
    );
    assert!(
        cold_requests[before..]
            .iter()
            .any(|line| line.contains("/p9/wheel.whl"))
    );
    assert!(
        cold_requests[before..]
            .iter()
            .all(|line| line.contains("/p9/python.tar.gz") || line.contains("/p9/wheel.whl")),
        "cold restore re-read source metadata"
    );
    let payload_path = PathBuf::from(payload.trim());
    let instance = payload_path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        !run(
            &binary,
            &empty,
            &["uninstall", "--instance", instance, "--dry-run"],
            &locked_warm,
            deadline
        )
        .await
        .status
        .success()
    );
    let catalog = warm.join("data/projects/.control/catalog/projects");
    let projects: serde_json::Value = serde_json::from_slice(&fs::read(&catalog).unwrap()).unwrap();
    let ids = projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| {
            (
                project["root"].as_str().unwrap().to_owned(),
                project["project"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    success(
        &binary,
        &empty,
        &["project", "forget", &ids[a.to_str().unwrap()]],
        &locked_warm,
        deadline,
    )
    .await;
    success(&binary, &empty, &["prune"], &locked_warm, deadline).await;
    assert!(payload_path.exists());
    assert_eq!(
        success(
            Path::new(b_executable.trim()),
            &b,
            &["-B", "-c", "import packaging; print(packaging.__version__)"],
            &locked_warm,
            deadline
        )
        .await
        .trim(),
        "25.0"
    );
    success(
        &binary,
        &empty,
        &["project", "forget", &ids[b.to_str().unwrap()]],
        &locked_warm,
        deadline,
    )
    .await;
    success(&binary, &empty, &["prune"], &locked_warm, deadline).await;
    assert!(!payload_path.exists());
    fs::write(
        evidence.join("mirror-requests.json"),
        serde_json::to_vec_pretty(&server.requests()).unwrap(),
    )
    .unwrap();
    eprintln!(
        "P9_PBS_WHEEL_SINGLE_CASE_COMPLETE version={exact} shared=true warm_zero_requests=true cold_fixed=true reclaim=true"
    );
}

/// The caller supplies a complete, verified Conda closure and original PyPI
/// Simple/metadata responses. The current three seed archives alone are not a closure.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires complete original Conda/PyPI inputs and a freshly built CLI"]
async fn p8_real_mixed_numpy_financial_update_conflict_lifecycle() {
    let deadline = Instant::now() + Duration::from_secs(580);
    let binary = PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").expect("fresh CLI required"))
        .canonicalize()
        .unwrap();
    let original_lock = fs::read(
        std::env::var_os("PAKRO_TEST_MIXED_ORIGINAL_LOCK")
            .expect("saved original mixed lock required"),
    )
    .unwrap();
    let inputs = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_CONDA_INPUTS")
            .expect("complete Conda inputs.json required"),
    )
    .canonicalize()
    .unwrap();
    let receipt = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_SEED_RECEIPT")
            .expect("verified mixed seed receipt required"),
    );
    let financial_simple = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_FINANCIAL_SIMPLE")
            .expect("original numpy-financial Simple JSON required"),
    );
    let scipy_simple = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_SCIPY_SIMPLE")
            .expect("original SciPy Simple JSON required"),
    );
    let financial_wheel = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_FINANCIAL_WHEEL")
            .expect("verified numpy-financial wheel required"),
    );
    let financial_metadata = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_FINANCIAL_METADATA")
            .expect("verified numpy-financial METADATA required"),
    );
    let scipy_metadata = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_SCIPY_METADATA")
            .expect("verified SciPy METADATA required"),
    );
    let scipy_wheels: BTreeMap<String, PathBuf> = serde_json::from_str(
        &std::env::var("PAKRO_TEST_MIXED_SCIPY_WHEELS")
            .expect("JSON map of original SciPy wheel filenames to absolute paths required"),
    )
    .expect("SciPy wheel paths must be a JSON object");
    let evidence = PathBuf::from(
        std::env::var_os("PAKRO_TEST_MIXED_EVIDENCE_DIR")
            .expect("new absolute evidence directory required"),
    );
    assert!(evidence.is_absolute());
    fs::create_dir(&evidence).expect("evidence directory must not exist");
    fs::set_permissions(&evidence, fs::Permissions::from_mode(0o700)).unwrap();
    let receipt_bytes = fs::read(&receipt).unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
    let input_bytes = fs::read(&inputs).unwrap();
    let input: serde_json::Value = serde_json::from_slice(&input_bytes).unwrap();
    assert_eq!(input["schema"], 1);
    assert_eq!(input["complete"], true);
    let records: Vec<RepoDataRecord> = serde_json::from_value(input["records"].clone()).unwrap();
    let artifacts = input["artifacts"].as_array().unwrap();
    assert_eq!(records.len(), artifacts.len());
    let numpy = records
        .iter()
        .find(|record| record.package_record.name.as_normalized() == "numpy")
        .expect("complete Conda closure must include numpy");
    assert_eq!(
        numpy.package_record.version.to_string(),
        receipt["conda_numpy"]["version"].as_str().unwrap()
    );
    assert_eq!(
        numpy.package_record.build,
        receipt["conda_numpy"]["build"].as_str().unwrap()
    );
    let numpy_sha: [u8; 32] = numpy.package_record.sha256.unwrap().into();
    assert_eq!(
        hex::encode(numpy_sha),
        receipt["conda_numpy"]["sha256"].as_str().unwrap()
    );
    let financial: serde_json::Value =
        serde_json::from_slice(&fs::read(&financial_simple).unwrap()).unwrap();
    let mut scipy: serde_json::Value =
        serde_json::from_slice(&fs::read(&scipy_simple).unwrap()).unwrap();
    assert_eq!(financial["name"], "numpy-financial");
    assert_eq!(scipy["name"], "scipy");
    let fin_name = receipt["numpy_financial"]["filename"].as_str().unwrap();
    let fin = financial["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["filename"].as_str() == Some(fin_name))
        .expect("financial wheel absent from original Simple JSON");
    let fin_url = fin["url"].as_str().unwrap();
    assert_eq!(fin_url, receipt["numpy_financial"]["url"].as_str().unwrap());
    assert_eq!(
        sha256(&financial_wheel),
        fin["hashes"]["sha256"].as_str().unwrap()
    );
    assert_eq!(
        fs::metadata(&financial_wheel).unwrap().len(),
        fin["size"].as_u64().unwrap()
    );
    assert_eq!(
        sha256(&financial_metadata),
        fin["core-metadata"]["sha256"].as_str().unwrap()
    );
    assert_eq!(
        sha256(&scipy_metadata),
        receipt["scipy"]["expected_metadata_sha256"]
            .as_str()
            .unwrap()
    );
    let original_candidates = receipt["scipy"]["candidates"].as_array().unwrap();
    assert_eq!(original_candidates.len(), 2);
    // Advertise only the intact upstream macOS 14 wheel; retain every original field.
    let intact_name = "scipy-1.14.1-cp313-cp313-macosx_14_0_arm64.whl";
    scipy["files"].as_array_mut().unwrap().retain(|file| file["filename"] == intact_name);
    assert_eq!(scipy["files"].as_array().unwrap().len(), 1);
    let scipy_candidates = original_candidates.iter().filter(|file| file["filename"] == intact_name).collect::<Vec<_>>();
    assert_eq!(scipy_candidates.len(), 1);
    assert_eq!(scipy_wheels.len(), scipy_candidates.len());
    let mut scipy_wheel_facts = BTreeMap::new();
    for &candidate in &scipy_candidates {
        assert!(
            scipy["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == candidate),
            "SciPy candidate differs from original Simple JSON"
        );
        assert_eq!(
            candidate["core-metadata"]["sha256"].as_str(),
            Some(sha256(&scipy_metadata).as_str())
        );
        let filename = candidate["filename"].as_str().unwrap();
        let wheel = scipy_wheels.get(filename).expect("SciPy wheel input missing");
        assert!(wheel.is_absolute(), "SciPy wheel input must be an absolute path");
        let digest = sha256(wheel);
        let size = fs::metadata(wheel).unwrap().len();
        assert_eq!(candidate["hashes"]["sha256"].as_str(), Some(digest.as_str()));
        assert_eq!(candidate["size"].as_u64(), Some(size));
        assert!(scipy_wheel_facts.insert(filename.to_owned(), (digest, size)).is_none());
    }
    let root = tempfile::Builder::new()
        .prefix("pakro-p8-mixed-")
        .tempdir()
        .unwrap()
        .keep()
        .canonicalize()
        .unwrap();
    let root_meta = fs::metadata(&root).unwrap();
    fs::write(evidence.join("fixture-owner.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "root": root, "uid": root_meta.uid(), "device": root_meta.dev(), "inode": root_meta.ino(),
        "binary_sha256": sha256(&binary), "inputs_sha256": hex::encode(Sha256::digest(&input_bytes)),
        "financial_simple_sha256": sha256(&financial_simple), "scipy_simple_sha256": sha256(&scipy_simple),
            "scipy_wheels": scipy_wheel_facts,
    })).unwrap()).unwrap();
    fs::write(evidence.join("original-inputs.json"), &input_bytes).unwrap();
    fs::write(evidence.join("original-seed-receipt.json"), &receipt_bytes).unwrap();
    fs::copy(&financial_simple, evidence.join("financial-simple.json")).unwrap();
    fs::copy(&scipy_simple, evidence.join("scipy-simple.json")).unwrap();
    let local_scipy_simple = evidence.join("scipy-local-simple.json");
    fs::write(&local_scipy_simple, serde_json::to_vec(&scipy).unwrap()).unwrap();
    let warm = root.join("warm");
    let cold = root.join("cold");
    let probe_storage = root.join("probe-storage");
    for path in [&warm, &cold, &probe_storage] {
        fs::create_dir(path).unwrap();
    }
    let a = root.join("a");
    let b = root.join("b");
    let c = root.join("c");
    let empty = root.join("empty");
    let probe = root.join("probe");
    let python = records
        .iter()
        .find(|record| record.package_record.name.as_normalized() == "python")
        .expect("Conda closure lacks Python");
    let python_version = python.package_record.version.to_string();
    let declaration = format!(
        "[tools]\npython = {{ version = '{}', source = 'conda', channel = 'conda-forge', build = '{}' }}\n[settings.python.conda]\nchannels = ['conda-forge']\n[packages.'python@{}']\nnumpy = {{ version = '{}', source = 'conda', channel = 'conda-forge', build = '{}' }}\nnumpy-financial = {{ version = '1.0.0', source = 'pypi' }}\n",
        python_version,
        python.package_record.build,
        python_version,
        numpy.package_record.version,
        numpy.package_record.build
    );
    for project in [&a, &b, &c, &probe] {
        fs::create_dir(project).unwrap();
        fs::write(project.join("pakro.toml"), &declaration).unwrap();
    }
    fs::create_dir(&empty).unwrap();
    let mut routes = BTreeMap::new();
    let mut rewrites = BTreeMap::new();
    let mut archive_paths = BTreeSet::new();
    for record in &records {
        let name = record.identifier.to_string();
        let item = artifacts
            .iter()
            .find(|item| item["filename"].as_str() == Some(name.as_str()))
            .expect("Conda artifact missing");
        let digest: [u8; 32] = record.package_record.sha256.unwrap().into();
        let digest = hex::encode(digest);
        assert_eq!(item["original_url"].as_str(), Some(record.url.as_str()));
        assert_eq!(item["sha256"].as_str(), Some(digest.as_str()));
        assert_eq!(item["size"].as_u64(), record.package_record.size);
        let relative = Path::new(item["relative_path"].as_str().unwrap());
        assert!(
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        );
        let file = inputs.parent().unwrap().join(relative);
        assert_eq!(sha256(&file), digest);
        assert_eq!(
            fs::metadata(&file).unwrap().len(),
            record.package_record.size.unwrap()
        );
        let route = format!("/mixed/conda/{digest}/{name}");
        archive_paths.insert(route.clone());
        assert!(routes.insert(route.clone(), file).is_none());
        assert!(rewrites.insert(record.url.to_string(), route).is_none());
    }
    routes.insert("/mixed/financial-simple.json".into(), financial_simple);
    routes.insert("/mixed/scipy-simple.json".into(), local_scipy_simple);
    routes.insert("/mixed/financial.whl".into(), financial_wheel);
    routes.insert("/mixed/financial.whl.metadata".into(), financial_metadata);
    rewrites.insert(
        "https://pypi.org/simple/numpy-financial/".into(),
        "/mixed/financial-simple.json".into(),
    );
    rewrites.insert(
        "https://pypi.org/simple/scipy/".into(),
        "/mixed/scipy-simple.json".into(),
    );
    rewrites.insert(fin_url.into(), "/mixed/financial.whl".into());
    for (index, candidate) in scipy_candidates.iter().enumerate() {
        let filename = candidate["filename"].as_str().unwrap();
        routes.insert(
            format!("/mixed/scipy-{index}.whl"),
            scipy_wheels[filename].clone(),
        );
        let route = format!("/mixed/scipy-{index}.whl.metadata");
        routes.insert(route, scipy_metadata.clone());
        rewrites.insert(
            candidate["url"].as_str().unwrap().into(),
            format!("/mixed/scipy-{index}.whl"),
        );
    }
    let server = http_fixture::Server::start(Some(routes));
    let mut replacements: BTreeMap<_, _> = rewrites
        .into_iter()
        .map(|(url, route)| (url, format!("{}{route}", server.url())))
        .collect();
    replacements.insert(
        r"regex:^https?://[^?#]+\.(?:conda|tar\.bz2)(?:\?.*)?$".into(),
        format!("{}/input-drift", server.url()),
    );
    let proxy = http_fixture::Server::start(None);
    let mut probe_env = environment(&root, &probe_storage, false);
    reject_network(&mut probe_env, &proxy);
    assert!(
        !run(&binary, &probe, &["install"], &probe_env, deadline)
            .await
            .status
            .success()
    );
    assert!(
        !proxy.requests().is_empty(),
        "rejecting proxy did not observe the CLI"
    );
    let mut online = environment(&root, &warm, false);
    online.insert(
        "PAKRO_URL_REPLACEMENTS".into(),
        serde_json::to_string(&replacements).unwrap().into(),
    );
    // Restore the original lock from retained artifacts; only the later update resolves again.
    fs::write(a.join("pakro.lock"), &original_lock).unwrap();
    let mut initial_locked = online.clone();
    initial_locked.insert("PAKRO_LOCKED".into(), "1".into());
    success(&binary, &a, &["install"], &initial_locked, deadline).await;
    let lock = fs::read(a.join("pakro.lock")).unwrap();
    assert_eq!(lock, original_lock, "strict initial installation changed the original lock");
    let model =
        crate::packages::python_resolution::lock_session::decode_root(&lock, Default::default())
            .unwrap()
            .unwrap();
    let actual: BTreeMap<_, _> = model.conda()[0]
        .records
        .iter()
        .map(|record| (record.package_record.name.as_normalized(), record))
        .collect();
    let expected: BTreeMap<_, _> = records
        .iter()
        .map(|record| (record.package_record.name.as_normalized(), record))
        .collect();
    assert_eq!(
        actual, expected,
        "Conda metadata selected records differing from supplied fixed inputs"
    );
    assert!(
        !model.mixed().is_empty(),
        "mixed dependency facts missing from unified lock"
    );
    fs::write(evidence.join("original-pakro.lock"), &lock).unwrap();
    let payload = success(&binary, &a, &["where", "python"], &online, deadline).await;
    let executable = success(&binary, &a, &["which", "python"], &online, deadline).await;
    link_fixture_tests::assert_real_cross_package_links(Path::new(executable.trim()));
    let smoke = concat!(
        "import numpy as n, numpy_financial as f; ",
        "import numpy._core._multiarray_umath as native; ",
        "assert n.__version__ == '2.3.3' and native.__file__.endswith('.so'); ",
        "assert abs(f.fv(0.05, 1, 0, -100) - 105) < 1e-8; ",
        "a = n.ones((128, 128), dtype=n.float64); ",
        "assert n.all((a @ a) == 128.0); ",
        "x = n.linalg.solve(n.array([[3.0, 1.0], [1.0, 2.0]]), n.array([9.0, 8.0])); ",
        "assert n.all(n.abs(x - [2.0, 3.0]) < 1e-12); ",
        "print('mixed-ok')",
    );
    assert_eq!(
        success(
            Path::new(executable.trim()),
            &a,
            &["-B", "-c", smoke],
            &online,
            deadline
        )
        .await
        .trim(),
        "mixed-ok"
    );
    let mut blocked = online.clone();
    reject_network(&mut blocked, &proxy);
    blocked.insert("PAKRO_LOCKED".into(), "1".into());
    let empty_cache = warm.join("empty-cache");
    fs::create_dir(&empty_cache).unwrap();
    fs::set_permissions(&empty_cache, fs::Permissions::from_mode(0o700)).unwrap();
    blocked.insert("PAKRO_CACHE_DIR".into(), empty_cache.into_os_string());
    let warm_mirror = server.requests().len();
    let warm_proxy = proxy.requests().len();
    success(&binary, &a, &["install"], &blocked, deadline).await;
    fs::write(b.join("pakro.lock"), &lock).unwrap();
    success(&binary, &b, &["install"], &blocked, deadline).await;
    let b_payload = success(&binary, &b, &["where", "python"], &blocked, deadline).await;
    assert_eq!(b_payload.trim(), payload.trim());
    let a_stat = fs::metadata(payload.trim()).unwrap();
    let b_stat = fs::metadata(b_payload.trim()).unwrap();
    assert_eq!((a_stat.dev(), a_stat.ino()), (b_stat.dev(), b_stat.ino()));
    let b_python = success(&binary, &b, &["which", "python"], &blocked, deadline).await;
    assert_eq!(
        success(
            Path::new(b_python.trim()),
            &b,
            &["-B", "-c", smoke],
            &blocked,
            deadline
        )
        .await
        .trim(),
        "mixed-ok"
    );
    assert_eq!(
        server.requests().len(),
        warm_mirror,
        "warm restore fetched an artifact"
    );
    assert_eq!(
        proxy.requests().len(),
        warm_proxy,
        "warm restore attempted external network"
    );
    fs::write(c.join("pakro.lock"), &lock).unwrap();
    let mut cold_env = environment(&root, &cold, false);
    reject_network(&mut cold_env, &proxy);
    cold_env.insert(
        "PAKRO_URL_REPLACEMENTS".into(),
        serde_json::to_string(&replacements).unwrap().into(),
    );
    cold_env.insert("PAKRO_LOCKED".into(), "1".into());
    success(&binary, &c, &["install"], &cold_env, deadline).await;
    let c_python = success(&binary, &c, &["which", "python"], &cold_env, deadline).await;
    link_fixture_tests::assert_real_cross_package_links(Path::new(c_python.trim()));
    assert_eq!(
        success(
            Path::new(c_python.trim()),
            &c,
            &["-B", "-c", smoke],
            &cold_env,
            deadline
        )
        .await
        .trim(),
        "mixed-ok"
    );
    assert_eq!(fs::read(c.join("pakro.lock")).unwrap(), lock);
    assert_eq!(
        proxy.requests().len(),
        warm_proxy,
        "strict cold restore attempted public metadata"
    );
    let calls = server.requests();
    let cold_calls = &calls[warm_mirror..];
    assert!(
        !cold_calls.iter().any(|call| call.contains("-simple.json")),
        "cold restore re-read PyPI metadata"
    );
    let cold_paths: BTreeSet<_> = cold_calls
        .iter()
        .map(|line| line.split_whitespace().nth(1).unwrap().to_owned())
        .collect();
    archive_paths.insert("/mixed/financial.whl".into());
    assert_eq!(
        cold_paths, archive_paths,
        "cold restore did not fetch exactly the locked artifacts"
    );
    let catalog = warm.join("data/projects/.control/catalog/projects");
    let before_catalog = fs::read(&catalog).unwrap();
    let before_lock = fs::read(a.join("pakro.lock")).unwrap();
    let conflicting = format!("{declaration}scipy = {{ version = '1.14.1', source = 'pypi' }}\n");
    fs::write(a.join("pakro.toml"), conflicting).unwrap();
    let before_update_requests = server.requests().len();
    let failure = run(
        &binary,
        &a,
        &["install", "--update-lock"],
        &online,
        deadline,
    )
    .await;
    assert!(
        !failure.status.success(),
        "SciPy conflicting with fixed numpy unexpectedly succeeded"
    );
    let update_requests = server.requests();
    assert!(
        update_requests[before_update_requests..].iter().any(|request|
            request.starts_with("GET /mixed/scipy-") && request.contains(".whl ")),
        "conflict update did not request an original SciPy wheel"
    );
    let error = String::from_utf8_lossy(&failure.stderr).to_lowercase();
    assert!(
        error.contains("numpy") && error.contains("scipy"),
        "failure did not identify actual numpy/SciPy dependency conflict: {error}"
    );
    assert_eq!(fs::read(a.join("pakro.lock")).unwrap(), before_lock);
    assert_eq!(
        fs::read(&catalog).unwrap(),
        before_catalog,
        "failed update changed the project selection"
    );
    fs::write(a.join("pakro.toml"), &declaration).unwrap();
    let after_failure = success(&binary, &a, &["which", "python"], &blocked, deadline).await;
    assert_eq!(after_failure.trim(), executable.trim());
    assert_eq!(
        success(
            Path::new(after_failure.trim()),
            &a,
            &["-B", "-c", smoke],
            &blocked,
            deadline
        )
        .await
        .trim(),
        "mixed-ok"
    );
    let payload_path = PathBuf::from(payload.trim());
    let instance = payload_path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        !run(
            &binary,
            &empty,
            &["uninstall", "--instance", instance, "--dry-run"],
            &blocked,
            deadline
        )
        .await
        .status
        .success()
    );
    let projects: serde_json::Value = serde_json::from_slice(&before_catalog).unwrap();
    let ids = projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| {
            (
                project["root"].as_str().unwrap().to_owned(),
                project["project"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    success(
        &binary,
        &empty,
        &["project", "forget", &ids[a.to_str().unwrap()]],
        &blocked,
        deadline,
    )
    .await;
    success(&binary, &empty, &["prune"], &blocked, deadline).await;
    assert_eq!(
        success(
            Path::new(b_python.trim()),
            &b,
            &["-B", "-c", smoke],
            &blocked,
            deadline
        )
        .await
        .trim(),
        "mixed-ok"
    );
    success(
        &binary,
        &empty,
        &["project", "forget", &ids[b.to_str().unwrap()]],
        &blocked,
        deadline,
    )
    .await;
    success(&binary, &empty, &["prune"], &blocked, deadline).await;
    assert!(!payload_path.exists());
    assert!(
        !warm.join("manager-invocations").exists() && !cold.join("manager-invocations").exists()
    );
    fs::write(
        evidence.join("mirror-requests.json"),
        serde_json::to_vec_pretty(&server.requests()).unwrap(),
    )
    .unwrap();
    eprintln!(
        "P8_MIXED_NUMPY_FINANCIAL_COMPLETE shared=true warm_zero=true cold_fixed=true update_conflict_preserved=true reclaim=true"
    );
}
