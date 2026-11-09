use super::*;
use crate::packages::python_resolution::lock_model::{ContentPin,PinEvidence,RegistryArtifactKind,RegistryFileRef,RegistryFileRefInput};

fn input(archive: InspectedWheelArchive) -> (SourceBuildInput,InspectedWheelArchive) {
    let name=archive.manifest().name().to_owned();let version=archive.manifest().version().to_owned();
    let file=RegistryFileRef::new(RegistryFileRefInput {registry_id:"public",canonical_base_fingerprint:[9;32],project:&name,version:&version,filename:archive.filename(),kind:RegistryArtifactKind::Wheel},Default::default()).unwrap();
    let pin=ContentPin::new(&file,archive.digest(),archive.size(),PinEvidence::ObservedDuringFreshResolution,Default::default()).unwrap();
    (SourceBuildInput {name,version,source_pin:hex::encode(pin.id().as_bytes()),registry:"public".into(),base:[9;32],source_filename:archive.filename().into(),source_digest:archive.digest().to_string(),source_size:archive.size(),source_is_sdist:false,advertised:false,output_filename:archive.filename().into(),output_digest:archive.digest().to_string(),output_size:archive.size()},archive)
}

#[ignore = "requires PAKRO_TEST_PYTHON_ARCHIVE (Python 3.13.16 stripped) and PAKRO_TEST_NATIVE_ASSETS (fixed original backend wheels)"]
#[tokio::test(flavor="current_thread")]
async fn p6_native_python_children_keep_fixed_build_sites_for_code_module_and_script() {
    use crate::packages::python_wheel::{WheelExpectation,WheelLimits};
    use crate::packages::python_wheel_source::inspect_owned_wheel;
    let owner=tempfile::tempdir().unwrap();let root=owner.path().canonicalize().unwrap();
    let context=crate::libraries::language::repository::tests::operation_context(&root,true,Duration::from_secs(300));
    let language=crate::libraries::package::layouts::tests::real_language(&context).await;
    let manifest=PathBuf::from(std::env::var_os("PAKRO_TEST_NATIVE_ASSETS").expect("the fixed original backend wheels are required"));
    let assets:serde_json::Value=serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    let mut dependencies=Vec::new();
    for name in ["setuptools","setuptools-rust","semantic-version"] {
        let asset=assets["assets"].as_array().unwrap().iter().find(|asset|asset["name"].as_str()==Some(name)&&asset["kind"].as_str()==Some("wheel")).unwrap();
        let filename=asset["filename"].as_str().unwrap();let digest:pakro_store::Digest=format!("sha256-{}",asset["sha256"].as_str().unwrap()).parse().unwrap();
        let artifact=spool_verified_reader(std::fs::File::open(manifest.parent().unwrap().join(asset["path"].as_str().unwrap())).unwrap(),filename,digest,64*1024*1024,context.requested_cache_root()).unwrap();
        let archive=inspect_owned_wheel(artifact,digest,&WheelExpectation {filename,name,version:asset["version"].as_str().unwrap(),supported:language.target().supported()},WheelLimits::default()).unwrap();
        dependencies.push(input(archive));
    }
    let listener=std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).unwrap();
    let secret=root.join("outside-secret");std::fs::write(&secret,b"not a build input").unwrap();
    let outside=root.join("outside-write");
    let probe=r#"import errno,importlib.util,os,pathlib,socket,sys,setuptools_rust
assert sys.flags.isolated and sys.flags.no_user_site and sys.flags.dont_write_bytecode
assert str(pathlib.Path(setuptools_rust.__file__).resolve()).startswith(os.environ['P6_EXPECTED_SITE']+'/')
assert importlib.util.find_spec('p6_user_only') is None
try: open(os.environ['P6_SECRET'],'rb')
except PermissionError: pass
else: raise AssertionError('secondary Python read outside its inputs')
try: open(os.environ['P6_OUTSIDE'],'w')
except PermissionError: pass
else: raise AssertionError('secondary Python wrote outside its workspace')
try: socket.create_connection(('127.0.0.1',int(os.environ['P6_PORT'])),timeout=1)
except OSError as error: assert error.errno in (errno.EPERM,errno.EACCES),error
else: raise AssertionError('secondary Python connected to the live listener')
VALUE=42
if sys.argv[1]=='c':
    import __main__; assert __main__.VALUE==42
if sys.argv[1]=='script':
    import secondary_support; assert secondary_support.VALUE==42
if sys.argv[1]=='script-safe':
    assert importlib.util.find_spec('secondary_support') is None
print('P6_CHILD_'+sys.argv[1])
"#;
    let (_,probe_wheel)=crate::libraries::package::layouts::tests::wheel(&context,&language,"p6probe",[54;32],Vec::new(),vec![("p6_child_probe.py",probe.as_bytes())],None);
    dependencies.push(input(probe_wheel));
    let backend=format!(r#"import importlib.util,os,pathlib,subprocess,sys
PROBE={probe:?}
def get_requires_for_build_wheel(config_settings=None):
    source=pathlib.Path(__file__).resolve().parent
    site=pathlib.Path(importlib.util.find_spec('setuptools_rust').origin).parent.parent
    env={{'PYTHONPATH':str(source/'user-site'),'P6_EXPECTED_SITE':str(site),'P6_SECRET':{secret:?},'P6_OUTSIDE':{outside:?},'P6_PORT':{port:?}}}
    assert subprocess.check_output([sys.executable,'-c',PROBE,'c'],env=env).decode().strip()=='P6_CHILD_c'
    assert subprocess.check_output([sys.executable,'-W','ignore','-m','p6_child_probe','m'],env=env).decode().strip()=='P6_CHILD_m'
    assert subprocess.check_output([sys.executable,'-B',str(source/'secondary.py'),'script'],env=env).decode().strip()=='P6_CHILD_script'
    for options,label in [([], 'stdin'),(['-u','-O'],'stdin-options')]:
        code="import sys,local_codegen_marker; assert local_codegen_marker.VALUE==42; assert sys.argv==['']; sys.argv.append("+repr(label)+");\n"+PROBE
        assert subprocess.check_output([sys.executable,*options],input=code.encode(),env=env).decode().strip()=='P6_CHILD_'+label
    code="import local_codegen_marker; assert local_codegen_marker.VALUE==42\n"+PROBE
    assert subprocess.check_output([sys.executable,'-c',code,'c-cwd'],env=env).decode().strip()=='P6_CHILD_c-cwd'
    assert subprocess.check_output([sys.executable,'-m','local_codegen','m-cwd'],env=env).decode().strip()=='P6_CHILD_m-cwd'
    for flag in ['-I','-P']:
        code="import importlib.util; assert importlib.util.find_spec('local_codegen_marker') is None\n"+PROBE
        assert subprocess.check_output([sys.executable,flag,'-c',code,'c-safe'],env=env).decode().strip()=='P6_CHILD_c-safe'
        assert subprocess.check_output([sys.executable,flag,str(source/'secondary.py'),'script-safe'],env=env).decode().strip()=='P6_CHILD_script-safe'
    for flag in ['-h','--help','-V','--version']:
        assert subprocess.run([sys.executable,flag],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE).returncode==0
    return []
"#,secret=secret.to_str().unwrap(),outside=outside.to_str().unwrap(),port=listener.local_addr().unwrap().port().to_string());
    let original=bundle(&[("demo-1.0/pyproject.toml",b"[build-system]\nrequires=['setuptools==80.9.0','setuptools-rust==1.11.1','semantic-version==2.10.0','p6probe==1.0']\nbuild-backend='setuptools.build_meta'\nbackend-path=['backend']\n"),("demo-1.0/backend/setuptools/__init__.py",b""),("demo-1.0/backend/setuptools/build_meta.py",backend.as_bytes()),("demo-1.0/backend/setuptools/secondary.py",probe.as_bytes()),("demo-1.0/backend/setuptools/secondary_support.py",b"VALUE=42\n"),("demo-1.0/backend/setuptools/user-site/p6_user_only.py",b"VISIBLE=True\n"),("demo-1.0/local_codegen.py",probe.as_bytes()),("demo-1.0/local_codegen_marker.py",b"VALUE=42\n")]);
    let scratch=root.join("builds");std::fs::create_dir(&scratch).unwrap();
    let mut session=SourceBuildSession::prepare(source(&original,&root),"demo","1.0",language,&scratch,context.policy().deadline,Default::default()).unwrap();
    session.configure_native(&context,None).await.unwrap();assert!(session.native.is_some());
    assert!(session.get_requires(dependencies).await.unwrap().is_empty());
    let replies=session.workspace_path().join("native-responses");
    assert!(std::fs::read_dir(replies).unwrap().filter(|entry|entry.as_ref().unwrap().path().extension().is_some_and(|ext|ext=="resp")).count()>=3,"all three child interpreters must cross the execution-registration gate");
    assert!(!outside.exists());
    drop(session);
    eprintln!("P6_NATIVE_SECONDARY_PYTHON_SITES_OK code=true module=true script=true stdin=true option_stdin=true cwd=true caller_I_P=true help_version=true real_setuptools_rust=true user_site_disabled=true read_write_network_denied=true gate=true");
}
