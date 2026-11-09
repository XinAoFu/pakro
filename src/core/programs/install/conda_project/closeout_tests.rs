use super::*;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires fresh CLI and retained complete mixed Conda/PyPI inputs"]
async fn mixed_failed_update_preserves_lock_registration_and_runtime() {
    let deadline = Instant::now() + Duration::from_secs(500);
    let binary = PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").unwrap()).canonicalize().unwrap();
    let inputs = PathBuf::from(std::env::var_os("PAKRO_TEST_MIXED_CONDA_INPUTS").unwrap()).canonicalize().unwrap();
    let original_lock = PathBuf::from(std::env::var_os("PAKRO_TEST_MIXED_ORIGINAL_LOCK").unwrap());
    let pypi = PathBuf::from(std::env::var_os("PAKRO_TEST_MIXED_PYPI_DIR").unwrap()).canonicalize().unwrap();
    let evidence = PathBuf::from(std::env::var_os("PAKRO_TEST_MIXED_EVIDENCE_DIR").unwrap());
    fs::create_dir(&evidence).unwrap();
    let root = tempfile::Builder::new().prefix("pakro-p8-closeout-").tempdir().unwrap().keep().canonicalize().unwrap();
    let metadata = fs::metadata(&root).unwrap();
    fs::write(evidence.join("fixture-owner.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "root": root, "uid": metadata.uid(), "device": metadata.dev(), "inode": metadata.ino(),
    })).unwrap()).unwrap();
    let input: serde_json::Value = serde_json::from_slice(&fs::read(&inputs).unwrap()).unwrap();
    assert_eq!(input["complete"], true);
    let records: Vec<RepoDataRecord> = serde_json::from_value(input["records"].clone()).unwrap();
    let lock = fs::read(original_lock).unwrap();
    let model = crate::packages::python_resolution::lock_session::decode_root(&lock, Default::default()).unwrap().unwrap();
    assert_eq!(model.conda()[0].records, records);
    let python = records.iter().find(|record| record.package_record.name.as_normalized() == "python").unwrap();
    let numpy = records.iter().find(|record| record.package_record.name.as_normalized() == "numpy").unwrap();
    let declaration = format!(
        "[tools]\npython = {{ version = '{}', source = 'conda', channel = 'conda-forge', build = '{}' }}\n[settings.python.conda]\nchannels = ['conda-forge']\n[packages.'python@{}']\nnumpy = {{ version = '{}', source = 'conda', channel = 'conda-forge', build = '{}' }}\nnumpy-financial = {{ version = '1.0.0', source = 'pypi' }}\n",
        python.package_record.version, python.package_record.build, python.package_record.version,
        numpy.package_record.version, numpy.package_record.build,
    );
    let project = root.join("a");
    let empty = root.join("empty");
    let storage = root.join("storage");
    for path in [&project, &empty, &storage] { fs::create_dir(path).unwrap(); }
    fs::write(project.join("pakro.toml"), &declaration).unwrap();
    fs::write(project.join("pakro.lock"), &lock).unwrap();
    fs::write(evidence.join("original-pakro.toml"), &declaration).unwrap();
    fs::write(evidence.join("original-pakro.lock"), &lock).unwrap();
    let mut routes = BTreeMap::new();
    let mut rewrites = BTreeMap::new();
    for record in &records {
        let filename = record.identifier.to_string();
        let item = input["artifacts"].as_array().unwrap().iter().find(|item| item["filename"] == filename).unwrap();
        let file = inputs.parent().unwrap().join(item["relative_path"].as_str().unwrap());
        let digest: [u8; 32] = record.package_record.sha256.unwrap().into();
        assert_eq!(sha256(&file), hex::encode(digest));
        assert_eq!(fs::metadata(&file).unwrap().len(), record.package_record.size.unwrap());
        let route = format!("/conda/{filename}");
        routes.insert(route.clone(), file);
        rewrites.insert(record.url.to_string(), route);
    }
    let financial_page = pypi.join("numpy-financial-simple.json");
    let financial: serde_json::Value = serde_json::from_slice(&fs::read(&financial_page).unwrap()).unwrap();
    let fin = financial["files"].as_array().unwrap().iter().find(|file| file["filename"] == "numpy_financial-1.0.0-py3-none-any.whl").unwrap();
    let financial_wheel = pypi.join(fin["filename"].as_str().unwrap());
    assert_eq!(sha256(&financial_wheel), fin["hashes"]["sha256"].as_str().unwrap());
    routes.insert("/mixed/financial-simple.json".into(), financial_page);
    routes.insert("/mixed/financial.whl".into(), financial_wheel);
    routes.insert("/mixed/financial.whl.metadata".into(), pypi.join("numpy-financial-1.0.0.METADATA"));
    rewrites.insert("https://pypi.org/simple/numpy-financial/".into(), "/mixed/financial-simple.json".into());
    rewrites.insert(fin["url"].as_str().unwrap().into(), "/mixed/financial.whl".into());

    // This local registry advertises only the intact upstream macOS 14 wheel.
    // Its URL, digest, metadata and archive bytes remain the original values.
    let mut scipy: serde_json::Value = serde_json::from_slice(&fs::read(pypi.join("scipy-simple.json")).unwrap()).unwrap();
    let intact_name = "scipy-1.14.1-cp313-cp313-macosx_14_0_arm64.whl";
    scipy["files"].as_array_mut().unwrap().retain(|file| file["filename"] == intact_name);
    assert_eq!(scipy["files"].as_array().unwrap().len(), 1);
    let intact = &scipy["files"][0];
    let wheel = pypi.join("scipy-wheels").join(intact_name);
    assert_eq!(sha256(&wheel), intact["hashes"]["sha256"].as_str().unwrap());
    assert_eq!(fs::metadata(&wheel).unwrap().len(), intact["size"].as_u64().unwrap());
    let scipy_page = root.join("scipy-simple.json");
    fs::write(&scipy_page, serde_json::to_vec(&scipy).unwrap()).unwrap();
    routes.insert("/mixed/scipy-simple.json".into(), scipy_page);
    routes.insert("/mixed/scipy.whl".into(), wheel);
    routes.insert("/mixed/scipy.whl.metadata".into(), pypi.join("scipy-wheels").join(format!("{intact_name}.METADATA")));
    rewrites.insert("https://pypi.org/simple/scipy/".into(), "/mixed/scipy-simple.json".into());
    rewrites.insert(intact["url"].as_str().unwrap().into(), "/mixed/scipy.whl".into());
    let mirror = http_fixture::Server::start(Some(routes));
    let replacements = rewrites.into_iter().map(|(url, path)| (url, format!("{}{path}", mirror.url()))).collect::<BTreeMap<_, _>>();
    let mut env = environment(&root, &storage, false);
    env.insert("PAKRO_URL_REPLACEMENTS".into(), serde_json::to_string(&replacements).unwrap().into());
    let mut locked = env.clone();
    locked.insert("PAKRO_LOCKED".into(), "1".into());
    success(&binary, &project, &["install"], &locked, deadline).await;
    let executable = success(&binary, &project, &["which", "python"], &env, deadline).await;
    let code = "import numpy as n, numpy_financial as f; assert n.__version__ == '2.3.3'; assert abs(f.fv(0.05, 1, 0, -100)-105) < 1e-8; assert (n.ones((8,8)) @ n.ones((8,8)) == 8).all(); print('mixed-runtime-ok')";
    assert_eq!(success(Path::new(executable.trim()), &project, &["-B", "-c", code], &env, deadline).await.trim(), "mixed-runtime-ok");
    let catalog = storage.join("data/projects/.control/catalog/projects");
    let original_catalog = fs::read(&catalog).unwrap();
    let registered = success(&binary, &empty, &["project", "ls"], &env, deadline).await;
    assert!(registered.contains(project.to_str().unwrap()) && registered.contains("registered"), "{registered}");

    let edited = format!("{declaration}scipy = {{ version = '1.14.1', source = 'pypi' }}\n");
    fs::write(project.join("pakro.toml"), &edited).unwrap();
    let failure = run(&binary, &project, &["install", "--update-lock"], &env, deadline).await;
    assert!(!failure.status.success());
    let error = String::from_utf8_lossy(&failure.stderr);
    assert!(error.contains("scipy==1.14.1") && error.contains("<2.3") && error.contains(">=1.23.5") && error.contains("fixed numpy==2.3.3"), "{error}");
    assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), lock);
    assert_eq!(fs::read(&catalog).unwrap(), original_catalog);
    assert_eq!(fs::read_to_string(project.join("pakro.toml")).unwrap(), edited);
    let after = success(&binary, &empty, &["project", "ls"], &env, deadline).await;
    assert_eq!(after, registered);
    assert_eq!(success(Path::new(executable.trim()), &project, &["-B", "-c", code], &env, deadline).await.trim(), "mixed-runtime-ok");
    assert_eq!(fs::read_to_string(project.join("pakro.toml")).unwrap(), edited);
    assert!(!storage.join("manager-invocations").exists());
    fs::write(evidence.join("selected-python.txt"), executable.trim()).unwrap();
    fs::write(evidence.join("project-catalog.json"), original_catalog).unwrap();
    fs::write(evidence.join("update-stderr.txt"), failure.stderr).unwrap();
    fs::write(evidence.join("mirror-requests.json"), serde_json::to_vec_pretty(&mirror.requests()).unwrap()).unwrap();
    eprintln!("MIXED_FAILED_UPDATE_PRESERVED lock=true registration=true runtime=true user_declaration_unchanged=true");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the owned mixed installation produced by the failed-update test"]
async fn mixed_forget_preserves_other_project_then_reclaims_every_object() {
    let deadline = Instant::now() + Duration::from_secs(500);
    let binary = PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").unwrap()).canonicalize().unwrap();
    let previous = PathBuf::from(std::env::var_os("PAKRO_TEST_MIXED_INSTALLED_EVIDENCE").unwrap());
    let evidence = PathBuf::from(std::env::var_os("PAKRO_TEST_MIXED_EVIDENCE_DIR").unwrap());
    fs::create_dir(&evidence).unwrap();
    let owner_bytes = fs::read(previous.join("fixture-owner.json")).unwrap();
    let owner: serde_json::Value = serde_json::from_slice(&owner_bytes).unwrap();
    let root = PathBuf::from(owner["root"].as_str().unwrap());
    assert_eq!(root.canonicalize().unwrap(), root);
    let metadata = fs::symlink_metadata(&root).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.uid() as u64, owner["uid"].as_u64().unwrap());
    assert_eq!(metadata.dev(), owner["device"].as_u64().unwrap());
    assert_eq!(metadata.ino(), owner["inode"].as_u64().unwrap());
    fs::write(evidence.join("fixture-owner.json"), owner_bytes).unwrap();
    let a = root.join("a");
    let b = root.join("b");
    let empty = root.join("empty");
    let storage = root.join("storage");
    let catalog = storage.join("data/projects/.control/catalog/projects");
    assert_eq!(fs::read(&catalog).unwrap(), fs::read(previous.join("project-catalog.json")).unwrap());
    let original_lock = fs::read(previous.join("original-pakro.lock")).unwrap();
    assert_eq!(fs::read(a.join("pakro.lock")).unwrap(), original_lock);
    let edited = fs::read(a.join("pakro.toml")).unwrap();
    fs::create_dir(&b).unwrap();
    fs::write(b.join("pakro.toml"), fs::read(previous.join("original-pakro.toml")).unwrap()).unwrap();
    fs::write(b.join("pakro.lock"), &original_lock).unwrap();
    let mut env = environment(&root, &storage, true);
    env.insert("PAKRO_LOCKED".into(), "1".into());
    let proxy = http_fixture::Server::start(None);
    reject_network(&mut env, &proxy);
    success(&binary, &b, &["install"], &env, deadline).await;
    let a_python = fs::read_to_string(previous.join("selected-python.txt")).unwrap();
    let b_python = success(&binary, &b, &["which", "python"], &env, deadline).await;
    assert_eq!(b_python.trim(), a_python);
    let code = "import numpy as n, numpy_financial as f; assert n.__version__ == '2.3.3'; assert abs(f.fv(0.05, 1, 0, -100)-105) < 1e-8; assert (n.ones((8,8)) @ n.ones((8,8)) == 8).all(); print('shared-mixed-ok')";
    assert_eq!(success(Path::new(b_python.trim()), &b, &["-B", "-c", code], &env, deadline).await.trim(), "shared-mixed-ok");

    let projects: serde_json::Value = serde_json::from_slice(&fs::read(&catalog).unwrap()).unwrap();
    let ids = projects["projects"].as_array().unwrap().iter().map(|project| (
        project["root"].as_str().unwrap().to_owned(), project["project"].as_str().unwrap().to_owned(),
    )).collect::<BTreeMap<_, _>>();
    assert_eq!(ids.len(), 2);
    let mut allocations = Vec::new();
    let mut conda_count = 0;
    let mut wheel_count = 0;
    let mut layout_count = 0;
    let mut language_count = 0;
    let mut wheel_instance = None;
    for kind in ["packages", "languages"] {
        let library = storage.join("data").join(kind);
        for entry in fs::read_dir(library.join(".control/catalog")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") { continue; }
            let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            let instance = path.file_stem().unwrap().to_str().unwrap();
            let allocation = if kind == "languages" {
                language_count += 1;
                library.join(instance)
            } else {
                if instance.starts_with("layout-") {
                    layout_count += 1;
                } else if manifest["conda"].is_object() {
                    conda_count += 1;
                } else {
                    wheel_count += 1;
                    assert_eq!(manifest["facts"]["name"], "numpy-financial");
                    wheel_instance = Some(instance.to_owned());
                }
                library.join(manifest["storage"]["allocation"].as_str().unwrap())
            };
            let metadata = fs::metadata(&allocation).unwrap();
            allocations.push((path, allocation, metadata.dev(), metadata.ino()));
        }
    }
    assert_eq!((conda_count, wheel_count, layout_count, language_count), (25, 1, 1, 1));
    let protected = run(&binary, &empty, &["uninstall", "--instance", wheel_instance.as_deref().unwrap(), "--dry-run"], &env, deadline).await;
    assert!(!protected.status.success());
    assert!(String::from_utf8_lossy(&protected.stderr).contains("object is still referenced"));
    success(&binary, &empty, &["project", "forget", &ids[a.to_str().unwrap()]], &env, deadline).await;
    env.insert("PAKRO_LOG_LEVEL".into(), "debug".into());
    eprintln!("PAKRO_PRUNE_BEGIN after_forget_a");
    let prune_a_started = Instant::now();
    success(&binary, &empty, &["prune"], &env, deadline).await;
    let prune_a_seconds = prune_a_started.elapsed().as_secs_f64();
    eprintln!("PAKRO_PRUNE_TIMING after_forget_a_seconds={prune_a_seconds:.6}");
    fs::write(evidence.join("prune-timing.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "after_forget_a_seconds": prune_a_seconds,
    })).unwrap()).unwrap();
    for (manifest, allocation, dev, ino) in &allocations {
        assert!(manifest.exists());
        let metadata = fs::metadata(allocation).unwrap();
        assert_eq!((metadata.dev(), metadata.ino()), (*dev, *ino));
    }
    assert_eq!(success(Path::new(b_python.trim()), &b, &["-B", "-c", code], &env, deadline).await.trim(), "shared-mixed-ok");
    let remaining = success(&binary, &empty, &["project", "ls"], &env, deadline).await;
    assert!(!remaining.contains(&ids[a.to_str().unwrap()]) && remaining.contains(&ids[b.to_str().unwrap()]));
    success(&binary, &empty, &["project", "forget", &ids[b.to_str().unwrap()]], &env, deadline).await;
    eprintln!("PAKRO_PRUNE_BEGIN after_forget_b");
    let prune_b_started = Instant::now();
    success(&binary, &empty, &["prune"], &env, deadline).await;
    let prune_b_seconds = prune_b_started.elapsed().as_secs_f64();
    eprintln!("PAKRO_PRUNE_TIMING after_forget_b_seconds={prune_b_seconds:.6}");
    fs::write(evidence.join("prune-timing.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "after_forget_a_seconds": prune_a_seconds,
        "after_forget_b_seconds": prune_b_seconds,
    })).unwrap()).unwrap();
    for (manifest, allocation, _, _) in &allocations {
        assert!(!manifest.exists(), "retired object manifest remains: {}", manifest.display());
        assert!(!allocation.exists(), "unused object remains: {}", allocation.display());
    }
    assert!(success(&binary, &empty, &["project", "ls"], &env, deadline).await.trim().is_empty());
    assert_eq!(fs::read(a.join("pakro.toml")).unwrap(), edited);
    assert_eq!(fs::read(a.join("pakro.lock")).unwrap(), original_lock);
    assert_eq!(fs::read(b.join("pakro.lock")).unwrap(), original_lock);
    assert!(proxy.requests().is_empty());
    assert!(!storage.join("manager-invocations").exists());
    fs::write(evidence.join("reclaimed-objects.json"), serde_json::to_vec_pretty(&allocations).unwrap()).unwrap();
    eprintln!("MIXED_FORGET_RECLAIMED conda=25 wheel=1 language=1 layout=1 other_project_preserved=true");
}
