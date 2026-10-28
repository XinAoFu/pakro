//! One explicitly selected real source-build lifecycle in a wholly private library.
use super::*;
use crate::artifact_snapshot::{publish_verified_artifact, spool_verified_reader};
use crate::libraries::language::repository::{LanguageInstallSpec, LanguageRepository};
use crate::plugins::core::python::native::pipeline::RuntimeInstallPlan;
use pakro_util::cmd::CmdLineRunner;
use std::{collections::BTreeMap, ffi::OsString, fs, io::Read, os::unix::fs::MetadataExt,
    process::Output, time::{Duration, Instant}};

const ARCHIVE: &str = "Python-3.13.16.tar.xz";
const ARCHIVE_SHA: &str = "f4b1bfb3c79b5bb11b8d228a12504163b4c0dab4d679828d8f5f26b6cb6ab35d";

#[test]
fn p6_source_recipes_preserve_legacy_request_and_bind_runtime_test_policy() {
    use crate::packages::python_environment::model::NativeProgramRecord;
    let mut toolchain=NativeToolchainRecord::test_record();
    toolchain.platform="macos-aarch64-none".into();
    toolchain.programs=["ar","cc","cxx","linker","make"].into_iter().enumerate()
        .map(|(index,role)|NativeProgramRecord {role:role.into(),sha256:[index as u8+31;32],version:"portable fixture 1".into()}).collect();
    let source=SourceArtifact::new("Python-3.15.0.tar.xz","https://www.python.org/ftp/python/3.15.0/Python-3.15.0.tar.xz",pakro_store::Digest::Sha256([71;32])).unwrap();
    let current=SourcePythonBuildInput::new("3.15.0",source,RuntimeArchiveTarget::new("aarch64",RuntimePlatform::Macos).unwrap(),toolchain,None).unwrap();
    assert_eq!(current.recipe,2); assert!(!current.includes_test_modules());
    let legacy=current.for_test_recipe(1).unwrap(); assert!(legacy.includes_test_modules());
    assert!(current.for_test_recipe(0).is_err()); assert!(current.for_test_recipe(3).is_err());
    // These fixed recipe1 bytes predate the runtime-only recipe. They must
    // not be recomputed with a new default or a new configuration field.
    assert_eq!(hex::encode(legacy.request.fingerprint()),"1ee26ef74963850739f0b1edbcdf493a545e991c483b78a3c17c8bc3cbf2ea8a");
    assert_eq!(legacy.language_key().unwrap().filename(),"language-c4db06132be8772555b7e372fe096321a4eb20dd17d875500f9a3452d2a23b5d");
    assert_ne!(legacy.request,current.request); assert_ne!(legacy.language_key().unwrap(),current.language_key().unwrap());
    for input in [legacy,current] {
        let record=input.record([72;32],[73;32]).unwrap();
        let decoded:SourcePythonBuildRecord=toml::from_str(&toml::to_string(&record).unwrap()).unwrap();
        assert_eq!(decoded,record);
        let restored=decoded.input().unwrap();
        assert_eq!(restored.recipe,input.recipe); assert_eq!(restored.request,input.request);
        assert_eq!(restored.includes_test_modules(),input.includes_test_modules());
        assert_eq!(restored.record([72;32],[73;32]).unwrap(),record);
        let mut changed=record; changed.recipe=if input.recipe==1 {2}else{1};
        assert!(changed.input().is_err(),"changing the declared suite policy must change its request identity");
    }
}

struct SourceCli {
    owner: Option<tempfile::TempDir>,
    root: PathBuf,
    binary: PathBuf,
    env: BTreeMap<String, OsString>,
    logs: PathBuf,
    commands: Vec<serde_json::Value>,
    started: Instant,
    deadline: Instant,
    complete: bool,
}
impl SourceCli {
    async fn run(&mut self, cwd: &Path, args: &[&str]) -> Output {
        let remaining = self.deadline.checked_duration_since(Instant::now()).expect("the complete source lifecycle exceeded 580 seconds");
        let mut command = CmdLineRunner::new(&self.binary).args(args).current_dir(cwd)
            .env_clear().with_timeout(remaining);
        for (key, value) in &self.env { command = command.env(key, value); }
        let began = Instant::now();
        let output = command.output_isolated(16 * 1024 * 1024).await.unwrap_or_else(|error| panic!("source CLI {args:?} failed to finish: {error:#}"));
        let index = self.commands.len();
        fs::write(self.logs.join(format!("{index:02}.stdout")), &output.stdout).unwrap();
        fs::write(self.logs.join(format!("{index:02}.stderr")), &output.stderr).unwrap();
        self.commands.push(serde_json::json!({"args":args,"cwd":cwd,"exit":output.status.code(),
            "elapsed_seconds":began.elapsed().as_secs_f64(),"stdout_bytes":output.stdout.len(),"stderr_bytes":output.stderr.len(),
            "stdout_sha256":hex::encode(Sha256::digest(&output.stdout)),"stderr_sha256":hex::encode(Sha256::digest(&output.stderr))}));
        eprintln!("P6_SOURCE_CLI args={args:?} exit={:?} elapsed={:.3}\n{}\n{}", output.status.code(), began.elapsed().as_secs_f64(),
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        output
    }
    async fn success(&mut self, cwd: &Path, args: &[&str]) -> String {
        let output = self.run(cwd, args).await;
        assert!(output.status.success(), "source CLI {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }
}
impl Drop for SourceCli {
    fn drop(&mut self) {
        let receipt = serde_json::json!({"schema":1,"complete":self.complete,"fixture_root":self.root,
            "binary":self.binary,"elapsed_seconds":self.started.elapsed().as_secs_f64(),"commands":self.commands,
            "marker":if self.complete {"P6_SOURCE_LIFECYCLE_COMPLETE"} else {"P6_SOURCE_LIFECYCLE_INCOMPLETE"}});
        let _ = fs::write(self.logs.join("receipt.json"), serde_json::to_vec_pretty(&receipt).unwrap());
        if !self.complete && let Some(owner) = self.owner.take() {
            let root = owner.keep();
            eprintln!("P6_SOURCE_OWNED_FIXTURE_RETAINED={} logs={}", root.display(), self.logs.display());
        }
    }
}

fn source_environment(root: &Path) -> BTreeMap<String, OsString> {
    let mut env = BTreeMap::new();
    for key in ["PATH","LD_LIBRARY_PATH","DYLD_FALLBACK_LIBRARY_PATH","DEVELOPER_DIR"] {
        if let Some(value) = std::env::var_os(key) { env.insert(key.into(), value); }
    }
    for (key, child) in [("HOME","home"),("PAKRO_DATA_DIR","data"),("PAKRO_CACHE_DIR","cache"),("PAKRO_STATE_DIR","state"),
        ("PAKRO_CONFIG_DIR","config"),("PAKRO_SYSTEM_DIR","system"),("XDG_CONFIG_HOME","xdg-config"),("XDG_DATA_HOME","xdg-data"),
        ("XDG_CACHE_HOME","xdg-cache"),("XDG_STATE_HOME","xdg-state"),("TMPDIR","tmp")] {
        fs::create_dir(root.join(child)).unwrap(); env.insert(key.into(), root.join(child).into_os_string());
    }
    for (key, value) in [("PAKRO_OFFLINE","1"),("PAKRO_AUTO_UPDATE","0"),("PAKRO_USE_VERSIONS_HOST","0"),("PAKRO_YES","1"),
        ("PAKRO_LOCKFILE","1"),("PAKRO_EXPERIMENTAL","1"),("PAKRO_COLOR","0"),("NO_COLOR","1"),
        ("PAKRO_PYTHON_GITHUB_ATTESTATIONS","0"),("PAKRO_GITHUB_ATTESTATIONS","0"),("PYTHONDONTWRITEBYTECODE","1"),
        ("LANG","C"),("LC_ALL","C"),("NO_PROXY","127.0.0.1,localhost,::1"),("no_proxy","127.0.0.1,localhost,::1")] {
        env.insert(key.into(), value.into());
    }
    env.insert("PAKRO_TRUSTED_CONFIG_PATHS".into(), root.as_os_str().to_owned());
    env
}

fn snapshot(roots: &[&Path]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for (index, root) in roots.iter().enumerate() {
        hash.update((index as u64).to_le_bytes());
        if !root.exists() { hash.update(b"missing\0"); continue; }
        let mut paths = walkdir::WalkDir::new(root).follow_links(false).into_iter().map(|entry| entry.unwrap().path().to_owned()).collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let metadata = fs::symlink_metadata(&path).unwrap();
            hash.update(path.strip_prefix(root).unwrap().as_os_str().as_encoded_bytes()); hash.update([0]);
            for number in [metadata.dev(),metadata.ino(),metadata.mode() as u64,metadata.len(),metadata.mtime() as u64,metadata.mtime_nsec() as u64] { hash.update(number.to_le_bytes()); }
            if metadata.is_file() {
                let mut file = fs::File::open(&path).unwrap(); let mut buffer = [0u8; 32768];
                loop { let size = file.read(&mut buffer).unwrap(); if size == 0 { break; } hash.update(&buffer[..size]); }
            } else if metadata.file_type().is_symlink() { hash.update(fs::read_link(path).unwrap().as_os_str().as_encoded_bytes()); }
        }
    }
    hash.finalize().into()
}

fn source_record(value: &toml::Value, found: &mut Vec<SourcePythonBuildRecord>) {
    match value {
        toml::Value::Table(table) => for (key, value) in table {
            if key == "source_python" { found.push(value.clone().try_into().unwrap()); }
            else { source_record(value, found); }
        },
        toml::Value::Array(values) => for value in values { source_record(value, found); },
        _ => {},
    }
}

fn verify_ready(root: &Path, record: &SourcePythonBuildRecord, python: &Path, remaining: Duration) -> (PathBuf, String) {
    let context = crate::libraries::language::repository::tests::operation_context(root, false, remaining);
    let input = record.input().unwrap();
    let spec = LanguageInstallSpec::from_plan(&RuntimeInstallPlan::from_source(&input).unwrap(), false).unwrap();
    let lease = LanguageRepository::open_readonly(&context).unwrap().acquire_existing(&spec).unwrap().unwrap();
    assert_eq!(lease.entrypoint().canonicalize().unwrap(), python.canonicalize().unwrap());
    assert_eq!(lease.key(), input.language_key().unwrap());
    assert_eq!(lease.inner.manifest.schema, 2);
    assert_eq!(lease.inner.manifest.source_build.as_ref(), Some(record));
    assert!(lease.inner.manifest.source_layout.is_some());
    let system = lease.inner.manifest.source_layout.as_ref().unwrap().system_runtime.as_ref().unwrap();
    assert_eq!(system.policy,1);
    assert!(!system.libraries.is_empty());
    assert!(system.libraries.iter().all(|name|name.starts_with("@payload/") || !name.contains("source-build-")));
    assert_ne!(lease.inner.manifest.inventory.inventory_sha256(), &record.output_sha256,
        "portable source output and physical inode-bound inventory are distinct facts");
    (lease.root().to_owned(), lease.key().filename())
}

async fn verify_python_config(payload: &Path, deadline: Instant, logs: &Path) {
    let config=fs::read_dir(payload.join("lib/python3.13")).unwrap().map(|entry|entry.unwrap().path())
        .filter(|path|path.file_name().unwrap().to_string_lossy().starts_with("config-")).collect::<Vec<_>>();
    assert_eq!(config.len(),1);
    let script=config[0].join("python-config.py");
    let output=CmdLineRunner::new(&script).arg("--includes").env_clear().env("PYTHONDONTWRITEBYTECODE","1")
        .with_timeout(deadline.saturating_duration_since(Instant::now())).output_isolated(64*1024).await.unwrap();
    assert!(output.status.success(),"direct python-config.py --includes failed: {}",String::from_utf8_lossy(&output.stderr));
    let includes=String::from_utf8(output.stdout.clone()).unwrap();
    let expected=format!("-I{}",payload.join("include/python3.13").display());
    assert!(!includes.trim().is_empty()&&includes.split_whitespace().all(|word|word==expected),"python-config.py returned non-final include paths: {includes}");
    fs::write(logs.join("python-config-includes.stdout"),output.stdout).unwrap();
    fs::write(logs.join("python-config-includes.stderr"),output.stderr).unwrap();
}

#[tokio::test]
#[ignore = "requires the newly built CLI, official CPython 3.13.16 source and installed native tools; one <=600s source-build group"]
async fn p6_real_project_source_build_shared_warm_query_and_reclaim() {
    let binary = PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").expect("PAKRO_TEST_BINARY must name the new main")).canonicalize().unwrap();
    let archive = PathBuf::from(std::env::var_os("PAKRO_TEST_CPYTHON_SOURCE_ARCHIVE").expect("official CPython 3.13.16 tar.xz is required"));
    assert_eq!(archive.file_name().unwrap(), ARCHIVE);
    assert_eq!(fs::metadata(&archive).unwrap().len(), 23_225_704);
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(580);
    let logs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("p6-source-cli-{}",uuid::Uuid::now_v7().simple()));
    fs::create_dir_all(&logs).unwrap();
    let env = source_environment(&root);
    let alias = root.join("tool-aliases"); fs::create_dir(&alias).unwrap();
    let search = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into())).collect::<Vec<_>>();
    let mut declarations = String::from("[tools]\npython = { version='3.13.16', source='source' }\n");
    let mut real_inputs = Vec::new();
    let mut tool_declarations = Vec::new();
    for (name, suffix, names, alias_name) in [
        ("cc","CC",&["cc","clang","gcc"][..],"cc"),("cxx","CXX",&["c++","clang++","g++"][..],"c++"),
        ("ar","AR",&["ar"][..],"ar"),("linker","LINKER",&["ld"][..],"ld"),("make","MAKE",&["make","gmake"][..],"make"),
        ("pkg_config","PKG_CONFIG",&["pkg-config"][..],"pkg-config"),
    ] {
        let input = std::env::var_os(format!("PAKRO_TEST_CPYTHON_{suffix}")).map(PathBuf::from)
            .or_else(|| names.iter().find_map(|name| search.iter().map(|path| path.join(name)).find(|path| path.is_file())));
        let Some(input) = input else { assert_eq!(name,"pkg_config","required installed source compiler/tool is missing: {name}"); continue; };
        let input = input.canonicalize().unwrap(); real_inputs.push(input.clone());
        let path = alias.join(alias_name); std::os::unix::fs::symlink(input, &path).unwrap();
        tool_declarations.push(format!("{name} = {}\n",serde_json::to_string(path.to_str().unwrap()).unwrap()));
    }
    let mut layout_declarations = Vec::new();
    for (name, suffix) in [("sdk","SDK"),("sysroot","SYSROOT")] {
        if let Some(input) = std::env::var_os(format!("PAKRO_TEST_CPYTHON_{suffix}")) {
            let input = PathBuf::from(input).canonicalize().unwrap(); real_inputs.push(input.clone());
            let path = alias.join(name); std::os::unix::fs::symlink(input,&path).unwrap();
            layout_declarations.push(format!("{name} = {}\n",serde_json::to_string(path.to_str().unwrap()).unwrap()));
        }
    }
    if !layout_declarations.is_empty() { declarations.push_str("[settings.python.build]\n"); declarations.extend(layout_declarations); }
    declarations.push_str("[settings.python.build.tools]\n");
    declarations.extend(tool_declarations);
    for project in ["a","b","empty"] { fs::create_dir(root.join(project)).unwrap(); }
    let a = root.join("a"); let b = root.join("b"); let empty = root.join("empty");
    fs::write(a.join("pakro.toml"), &declarations).unwrap(); fs::write(b.join("pakro.toml"), &declarations).unwrap();
    let cache = root.join("cache/python/cpython-source-v1"); fs::create_dir_all(&cache).unwrap();
    let digest: pakro_store::Digest = format!("sha256-{ARCHIVE_SHA}").parse().unwrap();
    let mut verified = spool_verified_reader(fs::File::open(&archive).unwrap(),ARCHIVE,digest,256*1024*1024,&cache).unwrap();
    publish_verified_artifact(&mut verified,&cache,256*1024*1024).unwrap(); drop(verified);
    let mut cli = SourceCli { owner:Some(owner), root:root.clone(), binary, env, logs, commands:Vec::new(), started, deadline, complete:false };
    eprintln!("P6_SOURCE_FIXTURE={} LOGS={}",root.display(),cli.logs.display());
    cli.success(&a,&["install"]).await;
    let data=root.join("data");let state=root.join("state");let config=root.join("config");let home=root.join("home");let cache=root.join("cache");
    let roots=[data.as_path(),cache.as_path(),state.as_path(),config.as_path(),home.as_path(),a.as_path(),b.as_path()];
    let before_which=snapshot(&roots);
    let python = PathBuf::from(cli.success(&a,&["which","python"]).await.trim());
    assert_eq!(snapshot(&roots),before_which,"readonly source path query changed persisted state");
    assert!(python.starts_with(root.join("data/languages")), "source project selected an external interpreter");
    let lock = fs::read(a.join("pakro.lock")).unwrap();
    let mut records = Vec::new(); source_record(&toml::from_str(std::str::from_utf8(&lock).unwrap()).unwrap(),&mut records);
    assert_eq!(records.len(),1,"the exact source language result must be stored once in its project lock");
    let record = records.pop().unwrap();
    assert_eq!(record.version,"3.13.16"); assert_eq!(record.filename,ARCHIVE); assert_eq!(record.recipe,2);
    assert_eq!(record.toolchain.configuration.get("cpython-test-modules").map(String::as_str),Some("disabled"));
    assert_eq!(record.artifact,digest.to_string()); record.toolchain.validate().unwrap();
    assert_ne!(record.output_sha256,[0;32]); assert_ne!(record.runtime_target,[0;32]);
    assert_ne!(record.output_sha256,record.request_id); assert_ne!(hex::encode(record.output_sha256),ARCHIVE_SHA);
    let mut wrong = record.clone(); wrong.url="https://invalid.example/Python-3.13.16.tar.xz".into(); assert!(wrong.input().is_err());
    wrong=record.clone(); wrong.request_id[0]^=1; assert!(wrong.input().is_err());
    let remaining = deadline.saturating_duration_since(Instant::now());
    let (payload,instance) = verify_ready(&root,&record,&python,remaining);
    let main_identity = { let meta=fs::metadata(&python).unwrap(); (meta.dev(),meta.ino(),meta.len(),meta.mtime(),meta.mtime_nsec()) };
    let functional = r#"import sys,sysconfig,ssl,hashlib,sqlite3,ctypes,zlib,bz2,lzma,importlib.util,unittest,unittest.mock,doctest
assert sys.version_info[:3]==(3,13,16)
assert sys.prefix==sys.base_prefix and sysconfig.get_config_var('prefix')==sys.prefix
assert ssl.OPENSSL_VERSION and len(ssl.RAND_bytes(16))==16
assert hashlib.sha256(b'abc').hexdigest()=='ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'
assert sqlite3.connect(':memory:').execute('select 40+2').fetchone()[0]==42
libc=ctypes.CDLL(None);libc.abs.argtypes=[ctypes.c_int];libc.abs.restype=ctypes.c_int
assert libc.abs(-42)==42
for module in (zlib,bz2,lzma): assert module.decompress(module.compress(b'pakro'))==b'pakro'
assert importlib.util.find_spec('pip') is None
assert importlib.util.find_spec('test') is None
assert importlib.util.find_spec('_testcapi') is None
result=unittest.TestResult(); unittest.FunctionTestCase(lambda:None).run(result); assert result.wasSuccessful()
mock=unittest.mock.Mock(return_value=42); assert mock()==42; mock.assert_called_once_with()
example=doctest.DocTestParser().get_doctest('>>> 40 + 2\n42\n',{},'pakro',None,0)
assert doctest.DocTestRunner().run(example).failed==0
print('P6_SOURCE_FUNCTIONS_OK',sys.version.split()[0])"#;
    let output = CmdLineRunner::new(&python).args(["-I","-B","-c",functional]).env_clear()
        .env("PYTHONDONTWRITEBYTECODE","1").env("OPENSSL_CONF","/dev/null").with_timeout(deadline.saturating_duration_since(Instant::now()))
        .output_isolated(64*1024).await.unwrap();
    assert!(output.status.success(),"actual built interpreter functions failed: {}",String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("P6_SOURCE_FUNCTIONS_OK 3.13.16"));
    fs::write(cli.logs.join("functions-first.stdout"),&output.stdout).unwrap();
    fs::write(cli.logs.join("functions-first.stderr"),&output.stderr).unwrap();
    let pydoc = payload.join("bin/pydoc3.13");
    verify_python_config(&payload,deadline,&cli.logs).await;
    let documentation = CmdLineRunner::new(&pydoc).arg("sys").env_clear()
        .env("PYTHONDONTWRITEBYTECODE","1").with_timeout(deadline.saturating_duration_since(Instant::now()))
        .output_isolated(256 * 1024).await.unwrap();
    assert!(documentation.status.success(),"installed pydoc script failed: {}",String::from_utf8_lossy(&documentation.stderr));
    assert!(String::from_utf8_lossy(&documentation.stdout).contains("sys"));
    fs::write(cli.logs.join("pydoc.stdout"),&documentation.stdout).unwrap();
    for name in ["pydoc3.13", "idle3.13"] {
        let script = fs::read(payload.join("bin").join(name)).unwrap();
        let shebang = std::str::from_utf8(&script).unwrap().lines().next().unwrap();
        assert_eq!(shebang,format!("#!{}",python.canonicalize().unwrap().display()),"installed source script retains a different interpreter");
    }
    // Remove only this fixture's symlink aliases, never the actual tools/SDK.
    for path in fs::read_dir(&alias).unwrap() { let path=path.unwrap().path(); assert!(fs::symlink_metadata(&path).unwrap().file_type().is_symlink()); fs::remove_file(path).unwrap(); }
    fs::write(b.join("pakro.lock"),&lock).unwrap();
    let languages=data.join("languages");
    let before_warm_language=snapshot(&[languages.as_path()]);
    cli.env.insert("PAKRO_LOCKED".into(),"1".into());
    cli.success(&b,&["install"]).await; cli.env.remove("PAKRO_LOCKED");
    let warm_python=PathBuf::from(cli.success(&b,&["which","python"]).await.trim());
    assert_eq!(warm_python,python);
    assert_eq!(fs::read(b.join("pakro.lock")).unwrap(),lock,"strict warm installation changed the complete source result");
    let warm = CmdLineRunner::new(&python).args(["-I","-B","-c",functional]).env_clear()
        .env("PYTHONDONTWRITEBYTECODE","1").env("OPENSSL_CONF","/dev/null").with_timeout(deadline.saturating_duration_since(Instant::now()))
        .output_isolated(64*1024).await.unwrap();
    assert!(warm.status.success(),"warm source interpreter failed without the fixture tool/SDK aliases: {}",String::from_utf8_lossy(&warm.stderr));
    assert!(String::from_utf8_lossy(&warm.stdout).contains("P6_SOURCE_FUNCTIONS_OK 3.13.16"));
    fs::write(cli.logs.join("functions-warm.stdout"),&warm.stdout).unwrap();
    fs::write(cli.logs.join("functions-warm.stderr"),&warm.stderr).unwrap();
    let meta=fs::metadata(&python).unwrap(); assert_eq!((meta.dev(),meta.ino(),meta.len(),meta.mtime(),meta.mtime_nsec()),main_identity);
    let warm_payload=warm_python.parent().unwrap().parent().unwrap();
    assert_eq!(warm_payload,payload);
    let warm_instance=warm_payload.parent().unwrap().file_name().unwrap().to_str().unwrap();
    assert_eq!(warm_instance,instance);
    assert_eq!(snapshot(&[languages.as_path()]),before_warm_language,"strict warm installation changed the shared source language or its manifest");
    let before=snapshot(&roots);
    let environment=cli.success(&b,&["env","--json"]).await; let _:serde_json::Value=serde_json::from_str(&environment).unwrap();
    let listing=cli.success(&a,&["installs","ls","--json"]).await;
    let listing:serde_json::Value=serde_json::from_str(&listing).unwrap();
    let row=listing.as_array().unwrap().iter().find(|row|row["instance"].as_str()==Some(instance.as_str())).unwrap();
    assert_eq!(row["source_recipe"].as_u64(),Some(2));
    assert_eq!(row["runtime_configuration"].as_str(),Some("不含CPython自身测试套件及测试扩展"));
    assert_eq!(snapshot(&roots),before,"readonly source queries changed persisted state");
    let denied=cli.run(&empty,&["uninstall","python@3.13.16","--instance",&instance]).await;
    assert!(!denied.status.success());
    assert!(format!("{}{}",String::from_utf8_lossy(&denied.stdout),String::from_utf8_lossy(&denied.stderr)).contains("object is still referenced"),"source removal was not blocked by the actual project references");
    assert_eq!(snapshot(&roots),before,"referenced source removal changed persisted facts");
    let catalog:serde_json::Value=serde_json::from_slice(&fs::read(data.join("projects/.control/catalog/projects")).unwrap()).unwrap();
    let projects=catalog["projects"].as_array().unwrap(); assert_eq!(projects.len(),2);
    let mut ids=Vec::new();
    for project in projects {
        assert!([a.to_str().unwrap(),b.to_str().unwrap()].contains(&project["root"].as_str().unwrap()));
        ids.push(project["project"].as_str().unwrap().to_owned());
    }
    for id in ids { cli.success(&empty,&["project","forget",&id]).await; }
    assert_eq!(fs::read(a.join("pakro.toml")).unwrap(),declarations.as_bytes()); assert_eq!(fs::read(b.join("pakro.toml")).unwrap(),declarations.as_bytes());
    assert_eq!(fs::read(a.join("pakro.lock")).unwrap(),lock); assert_eq!(fs::read(b.join("pakro.lock")).unwrap(),lock);
    let preview=cli.success(&empty,&["prune","--dry-run"]).await; assert!(preview.contains(&instance)&&preview.contains("would remove")); assert!(payload.is_dir());
    let mut removed=cli.run(&empty,&["uninstall","python@3.13.16","--instance",&instance]).await;
    let mut reclaim_resumed=false;
    if !removed.status.success() {
        let interrupted = |output: &Output| {
            let text=std::str::from_utf8(&output.stdout).unwrap();
            let raw=text.lines().find_map(|line|line.strip_prefix("operation: ")).expect("the CLI must expose its original operation ID");
            let operation=uuid::Uuid::parse_str(raw).unwrap();
            assert_eq!(operation.to_string(),raw);
            assert!(text.contains(&format!("pending recovery: {instance}\n"))
                && (text.contains("controlled deletion interrupted: store operation deadline exceeded")
                    || text.contains("controlled deletion interrupted: the operation deadline has elapsed"))
                && text.contains(&format!("Continue with: pakro prune --resume {operation}")),
                "only the original deadline/PendingRecovery result may be explicitly resumed: {text}");
            operation
        };
        let operation=interrupted(&removed);
        let progress=|| {
            use pakro_store::control::{ControlNamespace,ReadStore};
            let remaining=deadline.checked_duration_since(Instant::now()).expect("source reclaim exceeded its original parent deadline");
            let context=crate::libraries::language::repository::tests::operation_context(&root,false,remaining);
            let operations=crate::libraries::reclaim::pending(&context).unwrap();
            assert_eq!(operations.len(),1,"recovery must not create another operation");
            let original=&operations[0]; assert_eq!(original.operation_id,operation);
            assert_eq!(original.objects.len(),1); assert_eq!(original.objects[0].label(),instance);
            let names=original.journals.iter().filter(|(store,name)|store=="projects" && name.starts_with(&format!("reclaim-cursor-{operation}-"))).collect::<Vec<_>>();
            assert_eq!(names.len(),1);
            let read=ReadStore::open_readonly(&root.join("data/projects"),nix::unistd::geteuid().as_raw(),Default::default(),deadline).unwrap().unwrap();
            let control=read.control(Default::default()).unwrap();
            let pin=control.read(ControlNamespace::Journals,&names[0].1).unwrap().unwrap();
            let cursor:serde_json::Value=serde_json::from_slice(pin.bytes()).unwrap();
            (original.complete,cursor["next"].as_u64().unwrap(),cursor["payload_removed"].as_bool().unwrap(),cursor["allocation_removed"].as_bool().unwrap())
        };
        let mut before=progress();
        eprintln!("P6_SOURCE_RECLAIM_FIRST_DEADLINE operation={operation} cursor_next={}",before.1);
        loop {
            removed=cli.run(&empty,&["prune","--resume",&operation.to_string()]).await;
            if !removed.status.success() { assert_eq!(interrupted(&removed),operation); }
            let after=progress();
            assert!(after>before,"same-operation continuation made no actual cursor or terminal progress");
            eprintln!("P6_SOURCE_RECLAIM_RESUME operation={operation} before_next={} after_next={} complete={}",before.1,after.1,after.0);
            before=after;
            if removed.status.success() {
                assert!(after.0&&after.2&&after.3,"successful continuation has no complete physical cursor");
                reclaim_resumed=true;
                break;
            }
        }
    }
    assert!(removed.status.success());
    let removed=std::str::from_utf8(&removed.stdout).unwrap(); assert!(removed.contains("removed")&&removed.contains(&instance));
    assert!(!payload.exists(),"exact source language removal left its actual payload");
    cli.success(&empty,&["prune"]).await;
    let inventory=cli.success(&empty,&["installs","ls","--json"]).await;
    let inventory:serde_json::Value=serde_json::from_str(&inventory).unwrap(); assert!(inventory.as_array().unwrap().iter().all(|row|row["instance"].as_str()!=Some(instance.as_str())));
    assert_eq!(fs::read(a.join("pakro.toml")).unwrap(),declarations.as_bytes()); assert_eq!(fs::read(b.join("pakro.toml")).unwrap(),declarations.as_bytes());
    assert_eq!(fs::read(a.join("pakro.lock")).unwrap(),lock); assert_eq!(fs::read(b.join("pakro.lock")).unwrap(),lock);
    for input in real_inputs { assert!(input.exists(),"fixture reclamation touched an actual user-prepared tool/SDK"); }
    cli.complete=true;
    eprintln!("P6_SOURCE_LIFECYCLE_COMPLETE version=3.13.16 instance={instance} actual_functions=true strict_warm_same_inode=true readonly=true exact_reclaimed=true reclaim_resumed={reclaim_resumed} elapsed={:.3}",started.elapsed().as_secs_f64());
}

#[tokio::test]
#[ignore = "requires the new CLI, official CPython source and an owned SDK without OpenSSL; one <=600s real negative build"]
async fn p6_real_project_source_without_openssl_rejects_before_publication() {
    let binary = PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").expect("PAKRO_TEST_BINARY must name the new main")).canonicalize().unwrap();
    let archive = PathBuf::from(std::env::var_os("PAKRO_TEST_CPYTHON_SOURCE_ARCHIVE").expect("official CPython 3.13.16 tar.xz is required"));
    assert_eq!(archive.file_name().unwrap(), ARCHIVE);
    assert_eq!(fs::metadata(&archive).unwrap().len(), 23_225_704);
    let sdk = PathBuf::from(std::env::var_os("PAKRO_TEST_CPYTHON_MISSING_OPENSSL_SDK").expect("an owned SDK without OpenSSL is required")).canonicalize().unwrap();
    for relative in ["usr/include/openssl", "include/openssl", "usr/lib/libssl.a", "usr/lib/libcrypto.a", "lib/libssl.a", "lib/libcrypto.a",
        "usr/lib/pkgconfig/openssl.pc", "usr/lib/pkgconfig/libssl.pc", "usr/lib/pkgconfig/libcrypto.pc"] {
        assert!(!sdk.join(relative).exists(), "the negative SDK still contains {relative}");
    }
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(580);
    let logs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("p6-source-missing-openssl-cli-{}",uuid::Uuid::now_v7().simple()));
    fs::create_dir_all(&logs).unwrap();
    let env = source_environment(&root);
    let project = root.join("project"); fs::create_dir(&project).unwrap();
    let declarations = format!("[tools]\npython = {{ version='3.13.16', source='source' }}\n[settings.python.build]\nsdk = {}\n", serde_json::to_string(sdk.to_str().unwrap()).unwrap());
    fs::write(project.join("pakro.toml"), &declarations).unwrap();
    let cache = root.join("cache/python/cpython-source-v1"); fs::create_dir_all(&cache).unwrap();
    let digest: pakro_store::Digest = format!("sha256-{ARCHIVE_SHA}").parse().unwrap();
    let mut verified = spool_verified_reader(fs::File::open(&archive).unwrap(),ARCHIVE,digest,256*1024*1024,&cache).unwrap();
    publish_verified_artifact(&mut verified,&cache,256*1024*1024).unwrap(); drop(verified);
    let mut cli = SourceCli { owner:Some(owner), root:root.clone(), binary, env, logs, commands:Vec::new(), started, deadline, complete:false };
    eprintln!("P6_SOURCE_MISSING_OPENSSL_FIXTURE={} LOGS={}",root.display(),cli.logs.display());
    let output = cli.run(&project,&["install"]).await;
    assert!(!output.status.success(),"source CPython unexpectedly published without its required SSL module");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("source CPython required standard-library verification failed or is missing modules")
        && error.contains("\"missing\"") && error.contains("\"ssl\""),
        "the real negative build must reach the required-standard-library gate, not fail during configure or compilation: {error}");
    assert_eq!(fs::read(project.join("pakro.toml")).unwrap(),declarations.as_bytes());
    assert!(!project.join("pakro.lock").exists(),"a failed required-module gate published a project lock");
    let language_catalog = root.join("data/languages/.control/catalog");
    if language_catalog.exists() {
        assert!(fs::read_dir(language_catalog).unwrap().next().is_none(),"a failed required-module gate published a Ready language");
    }
    let project_catalog = root.join("data/projects/.control/catalog/projects");
    if project_catalog.exists() {
        let catalog: serde_json::Value = serde_json::from_slice(&fs::read(project_catalog).unwrap()).unwrap();
        assert!(catalog["projects"].as_array().unwrap().is_empty(),"a failed source build committed a project selection");
    }
    assert!(sdk.is_dir(),"negative verification changed its user-prepared SDK input");
    cli.complete=true;
    eprintln!("P6_SOURCE_MISSING_OPENSSL_REJECTED required_module_gate=true missing_ssl=true ready=false project_selection=false lock=false declaration_unchanged=true elapsed={:.3}",started.elapsed().as_secs_f64());
}

#[tokio::test]
#[ignore = "requires the final CLI, original CPython source and fixed SDK; two actual source builds share one <=600s parent"]
async fn p6_real_project_source_locked_cold_rebuild_verifies_actual_output() {
    let binary=PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").expect("the final CLI is required")).canonicalize().unwrap();
    let archive=PathBuf::from(std::env::var_os("PAKRO_TEST_CPYTHON_SOURCE_ARCHIVE").expect("official source is required"));
    assert_eq!(archive.file_name().unwrap(),ARCHIVE);assert_eq!(fs::metadata(&archive).unwrap().len(),23_225_704);
    let sdk=PathBuf::from(std::env::var_os("PAKRO_TEST_CPYTHON_SDK").expect("the same fixed SDK is required")).canonicalize().unwrap();
    let owner=tempfile::tempdir().unwrap();let root=owner.path().canonicalize().unwrap();
    let started=Instant::now();let deadline=started+Duration::from_secs(580);
    let logs=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("p6-source-cold-cli-{}",uuid::Uuid::now_v7().simple()));fs::create_dir(&logs).unwrap();
    let a=root.join("a-context");let b=root.join("b-context");fs::create_dir(&a).unwrap();fs::create_dir(&b).unwrap();
    let env_a=source_environment(&a);let mut env_b=source_environment(&b);env_b.insert("PAKRO_LOCKED".into(),"1".into());
    let project_a=a.join("project");let project_b=b.join("project");fs::create_dir(&project_a).unwrap();fs::create_dir(&project_b).unwrap();
    let declarations=format!("[tools]\npython = {{ version='3.13.16', source='source' }}\n[settings.python.build]\nsdk = {}\n",serde_json::to_string(sdk.to_str().unwrap()).unwrap());
    for context in [&a,&b] {
        fs::write(context.join("project/pakro.toml"),&declarations).unwrap();
        let cache=context.join("cache/python/cpython-source-v1");fs::create_dir_all(&cache).unwrap();
        let digest:pakro_store::Digest=format!("sha256-{ARCHIVE_SHA}").parse().unwrap();
        let mut verified=spool_verified_reader(fs::File::open(&archive).unwrap(),ARCHIVE,digest,256*1024*1024,&cache).unwrap();
        publish_verified_artifact(&mut verified,&cache,256*1024*1024).unwrap();
    }
    let mut cli=SourceCli {owner:Some(owner),root:root.clone(),binary,env:env_a,logs,commands:Vec::new(),started,deadline,complete:false};
    eprintln!("P6_SOURCE_COLD_FIXTURE={} LOGS={}",root.display(),cli.logs.display());
    cli.success(&project_a,&["install"]).await;
    let python=PathBuf::from(cli.success(&project_a,&["which","python"]).await.trim());
    let lock=fs::read(project_a.join("pakro.lock")).unwrap();let mut records=Vec::new();source_record(&toml::from_str(std::str::from_utf8(&lock).unwrap()).unwrap(),&mut records);
    assert_eq!(records.len(),1);let expected=records.pop().unwrap();assert_eq!(expected.recipe,2);
    let (payload,_)=verify_ready(&a,&expected,&python,deadline.saturating_duration_since(Instant::now()));
    verify_python_config(&payload,deadline,&cli.logs).await;
    fs::write(project_b.join("pakro.lock"),&lock).unwrap();
    assert!(fs::read_dir(b.join("data")).unwrap().next().is_none(),"cold context must have no Ready language or package fallback");
    let before=snapshot(&[a.as_path()]);cli.env=env_b;
    let rebuilt=cli.run(&project_b,&["install"]).await;
    assert_eq!(fs::read(project_a.join("pakro.lock")).unwrap(),lock);assert_eq!(fs::read(project_b.join("pakro.lock")).unwrap(),lock);
    assert_eq!(fs::read(project_a.join("pakro.toml")).unwrap(),declarations.as_bytes());assert_eq!(fs::read(project_b.join("pakro.toml")).unwrap(),declarations.as_bytes());
    assert_eq!(snapshot(&[a.as_path()]),before,"cold preparation changed the original committed context");
    if rebuilt.status.success() {
        let cold_python=PathBuf::from(cli.success(&project_b,&["which","python"]).await.trim());
        assert!(cold_python.starts_with(b.join("data/languages"))&&!cold_python.starts_with(a.join("data")));
        verify_ready(&b,&expected,&cold_python,deadline.saturating_duration_since(Instant::now()));
        let first=fs::metadata(&python).unwrap();let second=fs::metadata(&cold_python).unwrap();assert_ne!((first.dev(),first.ino()),(second.dev(),second.ino()));
        eprintln!("P6_SOURCE_COLD_COMPLETE actual_rebuild=true actual_output_matches=true output_sha256={} original_context_unchanged=true locks_unchanged=true",hex::encode(expected.output_sha256));
    } else {
        let error=String::from_utf8_lossy(&rebuilt.stderr);
        let marker="rebuilt CPython output differs from its exact original lock result: expected ";
        let rest=error.split_once(marker).expect("cold source rejection must be the actual rebuilt-output gate").1;
        let (old,rest)=rest.split_once(", observed ").unwrap();let actual=rest.split_whitespace().next().unwrap();
        assert_eq!(old,hex::encode(expected.output_sha256));assert_eq!(actual.len(),64);let actual_bytes=hex::decode(actual).unwrap();assert_ne!(actual_bytes,expected.output_sha256);
        let catalog=b.join("data/languages/.control/catalog");assert!(!catalog.exists()||fs::read_dir(catalog).unwrap().next().is_none());
        let projects=b.join("data/projects/.control/catalog/projects");if projects.exists() {let value:serde_json::Value=serde_json::from_slice(&fs::read(projects).unwrap()).unwrap();assert!(value["projects"].as_array().unwrap().is_empty());}
        eprintln!("P6_SOURCE_COLD_COMPLETE actual_rebuild=true actual_output_matches=false actual_changed_output_rejected=true expected_sha256={} observed_sha256={actual} no_ready=true original_context_unchanged=true locks_unchanged=true",hex::encode(expected.output_sha256));
    }
    cli.complete=true;
}
