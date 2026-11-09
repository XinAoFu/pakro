use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::language::repository::LanguageInstallSpec;
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::packages::python_resolution::lock_model::{CondaLockGraph, PythonLockModel};
use crate::plugins::core::python::native::pipeline::RuntimeInstallPlan;
use rattler_conda_types::{Channel, ChannelConfig, MatchSpec, ParseStrictness};
use std::str::FromStr;

struct Case {
    root: PathBuf,
    binary: PathBuf,
    storage: PathBuf,
    evidence: PathBuf,
    records: Vec<RepoDataRecord>,
    graph: CondaLockGraph,
    routes: BTreeMap<String, PathBuf>,
    rewrites: BTreeMap<String, String>,
}

impl Case {
    fn new() -> Self {
        let binary = PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").unwrap()).canonicalize().unwrap();
        let inputs = PathBuf::from(std::env::var_os("PAKRO_TEST_CONDA_SCRIPT_INPUTS").unwrap()).canonicalize().unwrap();
        let evidence = PathBuf::from(std::env::var_os("PAKRO_TEST_CONDA_SCRIPT_EVIDENCE").unwrap());
        fs::create_dir(&evidence).unwrap();
        let root = tempfile::Builder::new().prefix("p8-script-").tempdir().unwrap().keep().canonicalize().unwrap();
        let metadata = fs::metadata(&root).unwrap();
        fs::write(evidence.join("owner.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "root": root, "uid": metadata.uid(), "device": metadata.dev(), "inode": metadata.ino(),
        })).unwrap()).unwrap();
        let input: serde_json::Value = serde_json::from_slice(&fs::read(&inputs).unwrap()).unwrap();
        assert_eq!(input["complete"], true);
        let records: Vec<RepoDataRecord> = serde_json::from_value(input["records"].clone()).unwrap();
        let source: serde_json::Value = serde_json::from_slice(&fs::read(inputs.parent().unwrap().join("records.json")).unwrap()).unwrap();
        let graph: CondaLockGraph = serde_json::from_value(source["graph"].clone()).unwrap();
        assert_eq!(records, graph.records);
        let mut routes = BTreeMap::new();
        let mut rewrites = BTreeMap::new();
        for record in &records {
            let filename = record.identifier.to_string();
            let item = input["artifacts"].as_array().unwrap().iter().find(|item| item["filename"] == filename).unwrap();
            let file = inputs.parent().unwrap().join(item["relative_path"].as_str().unwrap());
            let digest: [u8; 32] = record.package_record.sha256.unwrap().into();
            assert_eq!(sha256(&file), hex::encode(digest));
            assert_eq!(fs::metadata(&file).unwrap().len(), record.package_record.size.unwrap());
            let route = format!("/scripts/{filename}");
            routes.insert(route.clone(), file);
            rewrites.insert(record.url.to_string(), route);
        }
        let storage = root.join("storage");
        fs::create_dir(&storage).unwrap();
        Self { root, binary, storage, evidence, records, graph, routes, rewrites }
    }

    fn package(&mut self, version: &str, dependencies: &[&str], scripts: &[(&str, &[u8])]) {
        let name = "pakro-script-case";
        let filename = format!("{name}-{version}-test_0.tar.bz2");
        let index = serde_json::json!({"name":name,"version":version,"build":"test_0","build_number":0,"subdir":"osx-arm64","depends":dependencies});
        let mut files = vec![("share/pakro-script-case/value".to_owned(), version.as_bytes().to_vec())];
        files.extend(scripts.iter().map(|(path, bytes)| ((*path).to_owned(), bytes.to_vec())));
        let paths = files.iter().map(|(path, bytes)| serde_json::json!({
            "_path":path,"path_type":"hardlink","size_in_bytes":bytes.len(),"sha256":hex::encode(Sha256::digest(bytes)),
        })).collect::<Vec<_>>();
        files.push(("info/index.json".into(), serde_json::to_vec(&index).unwrap()));
        files.push(("info/paths.json".into(), serde_json::to_vec(&serde_json::json!({"paths_version":1,"paths":paths})).unwrap()));
        let encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        for (path, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, path, bytes.as_slice()).unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let path = self.root.join(&filename);
        fs::write(&path, &bytes).unwrap();
        let url = format!("https://conda.anaconda.org/conda-forge/osx-arm64/{filename}");
        let mut record = index;
        record["fn"] = filename.clone().into();
        record["url"] = url.clone().into();
        record["channel"] = "https://conda.anaconda.org/conda-forge/".into();
        record["size"] = bytes.len().into();
        record["sha256"] = hex::encode(Sha256::digest(&bytes)).into();
        self.records.push(serde_json::from_value(record).unwrap());
        let route = format!("/scripts/{filename}");
        self.routes.insert(route.clone(), path);
        self.rewrites.insert(url, route);
    }

    fn project(&self, name: &str, package: &str, version: &str) -> PathBuf {
        let project = self.root.join(name);
        fs::create_dir_all(&project).unwrap();
        let python = self.records.iter().find(|record| record.package_record.name.as_normalized() == "python").unwrap();
        let selected = self.records.iter().find(|record| record.package_record.name.as_normalized() == package && record.package_record.version.to_string() == version).unwrap();
        let declaration = format!(
            "[tools]\npython = {{ version = '{}', source = 'conda', channel = 'conda-forge', build = '{}' }}\n[settings.python.conda]\nchannels = ['conda-forge']\n[packages.'python@{}']\n{} = {{ version = '{}', source = 'conda', channel = 'conda-forge', build = '{}' }}\n",
            python.package_record.version, python.package_record.build, python.package_record.version,
            package, selected.package_record.version, selected.package_record.build,
        );
        fs::write(project.join("pakro.toml"), declaration).unwrap();
        let mut by_name = self.records.iter().filter(|r| r.package_record.name.as_normalized() != package)
            .map(|r| (r.package_record.name.as_normalized().to_owned(), r)).collect::<BTreeMap<_, _>>();
        by_name.insert(package.into(), selected);
        let mut reached = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        let mut order = Vec::new();
        for name in super::super::dependencies(python).unwrap().into_iter().chain([package.into()]) {
            super::super::visit(&name, &by_name, &mut visiting, &mut reached, &mut order).unwrap();
        }
        let mut graph = self.graph.clone();
        graph.records = by_name.values().filter(|r| r.package_record.name.as_normalized() == "python" || reached.contains(r.package_record.name.as_normalized())).map(|r| (**r).clone()).collect();
        let spec = LanguageInstallSpec::from_plan(&RuntimeInstallPlan::from_conda_record(python).unwrap(), false).unwrap();
        graph.target_fingerprint = spec.key().target();
        let channel = std::sync::Arc::new(Channel::from_str("conda-forge", &ChannelConfig::default_with_root_dir(PathBuf::from("/"))).unwrap());
        graph.roots = [python, selected].into_iter().map(|record| {
            let mut request = MatchSpec::from_str(&format!("{} =={}", record.package_record.name.as_normalized(), record.package_record.version), ParseStrictness::Strict).unwrap();
            request.channel = Some(channel.clone());
            request.build = Some(rattler_conda_types::match_spec::matcher::StringMatcher::Exact(record.package_record.build.clone()));
            request.to_canonical_string().unwrap()
        }).collect();
        let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
        discovery.ceiling_paths = vec![self.root.clone()];
        let observed = observe_project(&project, ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(30))).unwrap();
        let fingerprint = crate::programs::install::lock_document::declaration_fingerprint(&observed).unwrap();
        let model = PythonLockModel::from_parts(&[], &[], Default::default()).unwrap()
            .with_conda(graph, Default::default()).unwrap().with_declaration_fingerprint(fingerprint);
        let mut document = model.encode_projection(Default::default()).unwrap().parse::<toml_edit::DocumentMut>().unwrap();
        document.insert("lockfile_version", toml_edit::value(4));
        let digest: [u8; 32] = python.package_record.sha256.unwrap().into();
        let document = format!("{}\n[[tools.python]]\nversion='{}'\nbackend='core:python'\nspecifiers=['{}']\n[tools.python.options]\nsource='conda'\nchannel='conda-forge'\nbuild='{}'\n[tools.python.platforms.macos-arm64]\ninstall='conda'\nurl='{}'\nchecksum='sha256:{}'\n",
            document, python.package_record.version, python.package_record.version, python.package_record.build,
            python.url, hex::encode(digest));
        crate::lockfile::Lockfile::parse_observed(document.as_bytes()).unwrap();
        fs::write(project.join("pakro.lock"), document).unwrap();
        project
    }

    fn environment(&self, server: &http_fixture::Server, proxy: &http_fixture::Server) -> BTreeMap<String, OsString> {
        let mut env = environment(&self.root, &self.storage, false);
        reject_network(&mut env, proxy);
        env.insert("PAKRO_LOCKED".into(), "1".into());
        let replacements = self.rewrites.iter().map(|(url, route)| (url.clone(), format!("{}{route}", server.url()))).collect::<BTreeMap<_, _>>();
        env.insert("PAKRO_URL_REPLACEMENTS".into(), serde_json::to_string(&replacements).unwrap().into());
        env
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires original verified Conda script inputs and a freshly built CLI"]
async fn p8_original_proj_notice_and_libxml_rejection_are_explicit() {
    let case = Case::new();
    let deadline = Instant::now() + Duration::from_secs(530);
    let server = http_fixture::Server::start(Some(case.routes.clone()));
    let proxy = http_fixture::Server::start(None);
    let env = case.environment(&server, &proxy);
    let proj = case.project("proj", "proj", "9.9.0");
    let installed = run(&case.binary, &proj, &["install"], &env, deadline).await;
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    let output = format!("{}{}", String::from_utf8_lossy(&installed.stdout), String::from_utf8_lossy(&installed.stderr));
    assert!(output.contains("包 proj 的 fish/csh 版本脚本不会执行"), "{output}");
    let catalog = case.storage.join("data/projects/.control/catalog/projects");
    let before_catalog = fs::read(&catalog).unwrap();
    let xml = case.project("xml", "libxml2-16", "2.15.4");
    let before_lock = fs::read(xml.join("pakro.lock")).unwrap();
    let failed = run(&case.binary, &xml, &["install"], &env, deadline).await;
    assert!(!failed.status.success());
    let error = String::from_utf8_lossy(&failed.stderr);
    for expected in ["libxml2-16", "libxml2-split_activate.sh", "deactivation", "found 1 activation and 0 deactivation"] {
        assert!(error.contains(expected), "missing {expected}: {error}");
    }
    assert_eq!(fs::read(xml.join("pakro.lock")).unwrap(), before_lock);
    assert_eq!(fs::read(&catalog).unwrap(), before_catalog);
    assert!(proxy.requests().is_empty());
    assert!(!case.storage.join("manager-invocations").exists());
    fs::write(case.evidence.join("proj-output.txt"), output).unwrap();
    fs::write(case.evidence.join("libxml-stderr.txt"), failed.stderr).unwrap();
    eprintln!("P8_SCRIPT_ORIGINALS proj_installed=true notice=true libxml_rejected=true preserved=true");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires verified Conda Python inputs and a freshly built CLI; executes the real 60-second limit"]
async fn p8_script_failure_timeout_and_sigint_preserve_committed_project() {
    let mut case = Case::new();
    let deadline = Instant::now() + Duration::from_secs(530);
    let post = "bin/.pakro-script-case-post-link.sh";
    let active = "etc/conda/activate.d/demo.sh";
    let inactive = "etc/conda/deactivate.d/demo.sh";
    case.package("1.0", &[], &[]);
    let child = concat!(
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$PREFIX/parent.pid\"\n",
        "sh -c 'printf \"%s\" \"$$\" > \"$PREFIX/child.pid\"; while :; do :; done' &\n",
        "printf 'before-timeout-stdout\\n'\nprintf 'before-timeout-stderr\\n' >&2\n",
        "while [ ! -f \"$PREFIX/release\" ]; do :; done\n",
    );
    let failure = format!("{child}printf 'intentional script failure' >&2\nexit 7\n");
    case.package("2.0", &[], &[(post, failure.as_bytes())]);
    case.package("3.0", &[], &[(post, child.as_bytes())]);
    case.package("4.0", &[], &[(post, child.as_bytes())]);
    case.package("5.0", &[], &[("bin/.pakro-script-case-pre-link.sh", b"#!/bin/sh\nexit 0\n")]);
    case.package("6.0", &[], &[("bin/.pakro-script-case-pre-unlink.sh", b"#!/bin/sh\nexit 0\n")]);
    case.package("7.0", &[], &[("bin/.pakro-script-case-post-unlink.sh", b"#!/bin/sh\nexit 0\n")]);
    case.package("8.0", &[], &[(active, b"export DEMO_SETTING=ok\n")]);
    case.package("9.0", &[], &[(active, b"export DEMO_SETTING=ok\n"), (inactive, b"unset DEMO_SETTING\n"), (post, b"#!/bin/sh\nexit 0\n")]);
    case.package("10.0", &["bzip2 >=1"], &[(post, b"#!/bin/sh\nexit 0\n")]);
    case.package("11.0", &[], &[("etc/conda/activate.d/demo.fish", b"set -gx DEMO_SETTING ok\n"), ("etc/conda/deactivate.d/demo.fish", b"set -e DEMO_SETTING\n")]);
    case.package("12.0", &[], &[(active, b"echo unsupported\n"), (inactive, b"unset DEMO_SETTING\n")]);
    let server = http_fixture::Server::start(Some(case.routes.clone()));
    let proxy = http_fixture::Server::start(None);
    let env = case.environment(&server, &proxy);
    let project = case.project("project", "pakro-script-case", "1.0");
    success(&case.binary, &project, &["install"], &env, deadline).await;
    let executable = success(&case.binary, &project, &["which", "python"], &env, deadline).await;
    let old_value = Path::new(executable.trim()).parent().unwrap().parent().unwrap().join("share/pakro-script-case/value");
    assert_eq!(fs::read_to_string(&old_value).unwrap(), "1.0");
    let catalog = case.storage.join("data/projects/.control/catalog/projects");
    let original_catalog = fs::read(&catalog).unwrap();

    for (version, script, reason) in [
        ("5.0", "pre-link.sh", "pre-link scripts are not supported"),
        ("6.0", "pre-unlink.sh", "pre-unlink scripts are not supported"),
        ("7.0", "post-unlink.sh", "post-unlink scripts are not supported"),
        ("8.0", "demo.sh", "found 1 activation and 0 deactivation"),
        ("9.0", "post-link.sh", "cannot be combined"),
        ("10.0", "post-link.sh", "post-link dependency bzip2"),
        ("11.0", "demo.fish", "found 0 activation and 0 deactivation"),
        ("12.0", "demo.sh", "unsupported Conda activation shell statement"),
    ] {
        case.project("project", "pakro-script-case", version);
        let lock = fs::read(project.join("pakro.lock")).unwrap();
        let declaration = fs::read(project.join("pakro.toml")).unwrap();
        let output = run(&case.binary, &project, &["install"], &env, deadline).await;
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "unsupported script {version} succeeded");
        for expected in ["pakro-script-case", script, reason] { assert!(error.contains(expected), "missing {expected}: {error}"); }
        assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), lock);
        assert_eq!(fs::read(project.join("pakro.toml")).unwrap(), declaration);
        assert_eq!(fs::read(&catalog).unwrap(), original_catalog);
    }

    for (version, kind) in [("2.0", "failure"), ("3.0", "timeout"), ("4.0", "sigint")] {
        case.project("project", "pakro-script-case", version);
        let lock = fs::read(project.join("pakro.lock")).unwrap();
        let declaration = fs::read(project.join("pakro.toml")).unwrap();
        let mut command = tokio::process::Command::new(&case.binary);
        command.arg("install").current_dir(&project).env_clear().envs(&env)
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        let mut process = command.spawn().unwrap();
        let main_pid = process.id().unwrap() as i32;
        let stage = loop {
            if let Some(status) = process.try_wait().unwrap() {
                let output = process.wait_with_output().await.unwrap();
                panic!("script ended before it started: {status}: {}", String::from_utf8_lossy(&output.stderr));
            }
            let stage = fs::read_dir(case.storage.join("cache")).unwrap().filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("conda-script-"))
                .map(|entry| entry.path().join("work/prefix"))
                .find(|path| ["parent.pid", "child.pid"].iter().all(|name| fs::read_to_string(path.join(name)).ok().is_some_and(|s| s.parse::<i32>().is_ok())));
            if let Some(stage) = stage { break stage; }
            assert!(Instant::now() < deadline, "script did not start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let started = Instant::now();
        let mut pids = BTreeSet::from([main_pid]);
        for file in ["parent.pid", "child.pid"] { pids.insert(fs::read_to_string(stage.join(file)).unwrap().parse::<i32>().unwrap()); }
        let snapshot = std::process::Command::new("/bin/ps").args(["-axo", "pid=,ppid="]).output().unwrap();
        assert!(snapshot.status.success());
        let pairs = String::from_utf8(snapshot.stdout).unwrap().lines().map(|line| {
            let p = line.split_whitespace().map(|s| s.parse::<i32>().unwrap()).collect::<Vec<_>>(); (p[0], p[1])
        }).collect::<Vec<_>>();
        loop {
            let old = pids.len();
            for (pid, parent) in &pairs { if pids.contains(parent) { pids.insert(*pid); } }
            if pids.len() == old { break; }
        }
        if kind == "failure" { fs::write(stage.join("release"), b"continue").unwrap(); }
        if kind == "sigint" { nix::sys::signal::kill(nix::unistd::Pid::from_raw(main_pid), nix::sys::signal::Signal::SIGINT).unwrap(); }
        let output = tokio::time::timeout_at(deadline.into(), process.wait_with_output()).await.unwrap().unwrap();
        fs::write(case.evidence.join(format!("{kind}-stderr.txt")), &output.stderr).unwrap();
        fs::write(case.evidence.join(format!("{kind}-stdout.txt")), &output.stdout).unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        if kind == "timeout" {
            assert!(started.elapsed() >= Duration::from_secs(59), "the fixed 60-second limit was shortened");
            for expected in ["pakro-script-case", "post-link.sh", "60 seconds", "not committed", "unchanged", "confirmed stopped: yes", "Review the script output", "rerun the original command", "before-timeout-stdout", "before-timeout-stderr"] {
                assert!(error.contains(expected), "missing {expected}: {error}");
            }
        } else if kind == "failure" {
            assert!(error.contains("intentional script failure") && error.contains("pakro-script-case"), "{error}");
        } else {
            assert_eq!(output.status.code(), Some(130));
            assert!(!error.contains("panicked") && !error.contains("Broken pipe"), "{error}");
        }
        let stopped_by = Instant::now() + Duration::from_secs(10);
        loop {
            let alive = pids.iter().filter(|pid| nix::sys::signal::kill(nix::unistd::Pid::from_raw(**pid), None).is_ok()).collect::<Vec<_>>();
            if alive.is_empty() { break; }
            assert!(Instant::now() < stopped_by, "owned processes remain after {kind}: {alive:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(fs::read(project.join("pakro.lock")).unwrap(), lock);
        assert_eq!(fs::read(project.join("pakro.toml")).unwrap(), declaration);
        assert_eq!(fs::read(&catalog).unwrap(), original_catalog);
        assert_eq!(fs::read_to_string(&old_value).unwrap(), "1.0");
        assert_eq!(success(Path::new(executable.trim()), &project, &["-B", "-c", "print('old-runtime-ok')"], &env, deadline).await.trim(), "old-runtime-ok");
        fs::write(case.evidence.join(format!("{kind}-processes.json")), serde_json::to_vec_pretty(&pids).unwrap()).unwrap();
        eprintln!("P8_SCRIPT_{kind} failed=true lock_unchanged=true catalog_unchanged=true processes_stopped=true");
    }
    assert!(proxy.requests().is_empty());
    assert!(!case.storage.join("manager-invocations").exists());
}
