//! One native configure/build/install operation over an original pinned source.
use super::*;
use crate::config::SettingsExt;
use crate::libraries::language::repository::{LanguageInstallSpec, LanguageRepository, LanguageReservation};
use crate::packages::python_source_build::{SourceBuildLimits, extract_raw_source};
use crate::packages::python_source_build::toolchain::{FrozenNativeToolchain, NativeToolRole};
use crate::plugins::core::python::native::pipeline::RuntimeInstallPlan;
use crate::programs::OperationContext;
use crate::python_runtime::{self, PythonRuntimeHandle};
use eyre::{Context, ContextCompat, Result, ensure};
use pakro_index::artifact::{ArtifactTransport, NetworkPolicy};
use pakro_util::{native_build::{NativeBuildConfig, NativeBuildSession}, sandbox::SandboxConfig};
use sha2::{Digest as _, Sha256};
use std::{fs, path::{Component, Path, PathBuf}, time::Duration};

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

async fn run(session: &mut NativeBuildSession, context: &OperationContext, program: &Path,
    arguments: &[String], cwd: &Path, environment: &BTreeMap<String,String>, phase: &str) -> Result<std::process::Output> {
    context.checkpoint()?;
    let output = session.run(program, arguments, cwd, environment, context.policy().deadline,
        OUTPUT_LIMIT, &context.cancellation_token()).await.with_context(|| format!("source CPython {phase} failed"))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout[output.stdout.len().saturating_sub(8192)..]);
        let stderr = String::from_utf8_lossy(&output.stderr[output.stderr.len().saturating_sub(8192)..]);
        eyre::bail!("source CPython {phase} exited {}: {stdout}\n{stderr}", output.status);
    }
    Ok(output)
}

pub(crate) async fn install_source(context: &OperationContext, version: &str,
    pin: Option<&crate::lockfile::PlatformInfo>, update: bool) -> Result<LanguageKey> {
    context.checkpoint()?;
    ensure!(context.policy().writes.libraries, "operation policy forbids source CPython installation");
    let parts = version.split('.').map(str::parse::<u32>).collect::<std::result::Result<Vec<_>,_>>()?;
    ensure!(parts.len() == 3 && parts[0] == 3 && (11..=15).contains(&parts[1])
        && version == format!("{}.{}.{}",parts[0],parts[1],parts[2]), "source CPython requires a full ordinary supported release version");
    let original = pin.map(|pin| super::input_from_pin(version, pin)).transpose()?.flatten();
    ensure!(!context.policy().locked || original.is_some(), "strict source CPython installation needs its complete original build result");
    if !update && let Some(pin) = pin {
        let spec = crate::plugins::core::python::pinned_shared_language_spec(version, pin, false, context.settings())?;
        if let Some(ready) = LanguageRepository::open_readonly(context)?.acquire_existing(&spec)? { return Ok(ready.key()); }
    }
    let source = if let Some(original) = &original { original.source.clone() } else {
        let filename = format!("Python-{version}.tar.xz");
        let url = format!("https://www.python.org/ftp/python/{version}/{filename}");
        let digest = if version == "3.13.16" {
            // Previously fixed and checked on both supported test hosts.
            "sha256-f4b1bfb3c79b5bb11b8d228a12504163b4c0dab4d679828d8f5f26b6cb6ab35d".parse()?
        } else {
            ensure!(!context.policy().offline, "official CPython source digest metadata is not available offline");
            use base64::Engine as _;
            let canonical = url::Url::parse(&format!("{url}.sigstore"))?;
            let request = crate::plugins::core::python::native::transport::request_url(canonical, context.settings().url_replacements.as_ref())?;
            let client = reqwest::Client::builder().connect_timeout(context.settings().http_timeout())
                .timeout(context.policy().deadline.saturating_duration_since(std::time::Instant::now())).build()?;
            let fetch = async {
                let mut response = client.get(request).send().await.map_err(|error| error.without_url())?;
                ensure!(response.status().is_success(), "official CPython digest metadata HTTP status {}", response.status());
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|error| error.without_url())? {
                    ensure!(bytes.len() + chunk.len() <= 2 * 1024 * 1024, "official CPython digest metadata exceeds its budget"); bytes.extend_from_slice(&chunk);
                }
                let value: serde_json::Value = serde_json::from_slice(&bytes)?;
                let digest = &value["messageSignature"]["messageDigest"];
                ensure!(digest["algorithm"].as_str() == Some("SHA2_256"), "official CPython bundle has no supported source digest");
                let bytes = base64::engine::general_purpose::STANDARD.decode(digest["digest"].as_str().context("official CPython source digest is missing")?)?;
                ensure!(bytes.len() == 32, "official CPython source SHA256 has the wrong length");
                Ok::<_,eyre::Report>(pakro_store::Digest::Sha256(bytes.try_into().expect("checked digest size")))
            };
            tokio::select! { _ = context.cancellation_token().cancelled_owned() => eyre::bail!("source metadata acquisition cancelled"),
                result = tokio::time::timeout_at(context.policy().deadline.into(), fetch) => result.context("source metadata acquisition timed out")?? }
        };
        SourceArtifact::new(filename, &url, digest)?
    };
    let transport = ArtifactTransport::new(reqwest::Client::builder().connect_timeout(context.settings().http_timeout())
        .read_timeout(context.settings().http_timeout()).timeout(context.settings().http_download_timeout()))?;
    let request = crate::plugins::core::python::native::transport::download_source(&source, context.settings().url_replacements.as_ref())?;
    let cache = context.requested_cache_root().join("python/cpython-source-v1");
    let mut artifact = tokio::select! {
        _ = context.cancellation_token().cancelled_owned() => eyre::bail!("source CPython acquisition cancelled"),
        result = tokio::time::timeout_at(context.policy().deadline.into(), crate::artifact_snapshot::acquire_verified_artifact(
            &request, &cache, &transport, if context.policy().offline { NetworkPolicy::Offline } else { NetworkPolicy::Allowed },
            SourceBuildLimits::default().max_archive_bytes)) => result.context("source CPython acquisition timed out")??,
    };
    let raw = extract_raw_source(&mut artifact, &context.requested_cache_root().join("python/source-builds"),
        context.policy().deadline, SourceBuildLimits::default())?;
    let workspace = raw.workspace_path().to_owned();
    let frozen = FrozenNativeToolchain::discover_cpython(context, &[NativeToolRole::Cc, NativeToolRole::Cxx,
        NativeToolRole::Ar, NativeToolRole::Linker, NativeToolRole::Make]).await?;
    let mut tools = crate::packages::python_source_build::native::prepare_cpython_tools(&workspace.join("native-tools"), &frozen, context)?;
    tools.record.configuration.insert("cpython-driver-sha256".into(), hex::encode(Sha256::digest(include_bytes!("build.rs"))));
    tools.record.configuration.insert("cpython-layout-sha256".into(), hex::encode(Sha256::digest(include_bytes!("output.rs"))));
    tools.record.configuration.insert("cpython-layout".into(), "logical-prefix-staging-v1".into());
    let recipe = if update { SOURCE_RECIPE } else { original.as_ref().map_or(SOURCE_RECIPE, |input| input.recipe) };
    if recipe == 2 { tools.record.configuration.insert("cpython-test-modules".into(), "disabled".into()); }
    let target = if tools.record.platform.starts_with("macos-") { RuntimePlatform::Macos }
        else if tools.record.platform.ends_with("-musl") { RuntimePlatform::LinuxMusl } else { RuntimePlatform::LinuxGnu };
    let input = SourcePythonBuildInput::with_recipe(version, source, RuntimeArchiveTarget::new(std::env::consts::ARCH, target)?,
        tools.record.clone(), if update { None } else { original.as_ref().and_then(|original| original.expected_output) }, recipe)?;
    if !update && let Some(original) = &original {
        ensure!(input.request == original.request, "strict source CPython restore requires the original toolchain, SDK and build recipe");
    }
    let mut input = input;
    if !update { input.expected_runtime_target = original.as_ref().and_then(|original| original.expected_runtime_target); }
    let spec = LanguageInstallSpec::from_plan(&RuntimeInstallPlan::from_source(&input)?, false)?;
    if let Some(ready) = LanguageRepository::open_readonly(context)?.acquire_existing(&spec)? { return Ok(ready.key()); }
    let work = workspace.join("native-work"); fs::create_dir(&work)?;
    let build = work.join("build"); fs::create_dir(&build)?;
    let staging = work.join("staging"); fs::create_dir(&staging)?;
    let home = work.join("home"); fs::create_dir(&home)?;
    let temporary = work.join("tmp"); fs::create_dir(&temporary)?;
    let mut pending = vec![raw.root().to_owned()];
    let time = std::time::UNIX_EPOCH + Duration::from_secs(315532800);
    while let Some(path) = pending.pop() {
        context.checkpoint()?; let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() { for entry in fs::read_dir(&path)? { pending.push(entry?.path()); } }
        fs::File::open(&path)?.set_times(fs::FileTimes::new().set_modified(time))?;
    }
    let mut environment = tools.environment.clone();
    let linker = frozen.programs.get("linker").context("native linker was not frozen")?;
    let linker_alias = tools.bin.join("ld");
    if linker_alias.symlink_metadata().is_ok() { fs::remove_file(&linker_alias)?; }
    std::os::unix::fs::symlink(&linker.path,&linker_alias)?;
    for name in ["PAKRO_NATIVE_CC_ARGS","PAKRO_NATIVE_CXX_ARGS"] {
        let arguments = environment.entry(name.into()).or_default();
        if !arguments.is_empty() { arguments.push('\u{1f}'); }
        arguments.push_str(&format!("-B{}/",tools.bin.display()));
    }
    for (name,value) in [("CC","cc"),("CXX","c++"),("AR","ar"),("LD","ld"),("RANLIB","ranlib"),
        ("INSTALL","install"),("PYTHON_FOR_REGEN","false"),("OPENSSL_CONF","/dev/null")] { environment.insert(name.into(),value.into()); }
    for (name,path) in [("HOME",&home),("TMPDIR",&temporary),("TMP",&temporary),("TEMP",&temporary)] { environment.insert(name.into(),path.to_string_lossy().into_owned()); }
    environment.insert("_PYTHON_PROJECT_BASE".into(),build.to_string_lossy().into_owned());
    environment.insert("CFLAGS_NODIST".into(),format!("-O2 -g0 -ffile-prefix-map={}={LOGICAL_PREFIX}/src -ffile-prefix-map={}={LOGICAL_PREFIX}/build",raw.root().display(),build.display()));
    let mut reads = tools.read_roots.clone(); reads.push(workspace.clone());
    for path in ["/System/Library","/usr/lib","/lib","/lib64","/dev/null","/dev/urandom"] { if Path::new(path).exists() { reads.push(path.into()); } }
    // The official source's fixed install script is an input executable, not
    // an executable grant over the writable build directory.
    let mut programs = tools.programs.clone(); programs.push(raw.root().join("install-sh"));
    let mut session = NativeBuildSession::start_owned(NativeBuildConfig { bootstrap: tools.bootstrap.clone(), fixed_programs: programs,
        sandbox: SandboxConfig { allow_read: reads.clone(), allow_write: vec![work.clone()], ..Default::default() }, workspace: workspace.clone(),
        exec_pool: workspace.join("native-pool"), generated_roots: vec![work.clone()], requests: workspace.join("native-requests"),
        responses: workspace.join("native-responses"), unshare: tools.unshare.clone() }, raw.native_cleanup_owner()).await?;
    let mut configure = PathBuf::new();
    for _ in build.strip_prefix(&workspace)?.components() { configure.push(".."); }
    configure.push(raw.root().strip_prefix(&workspace)?); configure.push("configure");
    let shell = tools.bin.join("sh").canonicalize()?;
    let mut configure_arguments = vec![configure.to_string_lossy().into_owned(),format!("--prefix={LOGICAL_PREFIX}"),
        format!("--exec-prefix={LOGICAL_PREFIX}"),"--with-ensurepip=no".into(),"--disable-optimizations".into(),"--without-lto".into()];
    if !input.includes_test_modules() { configure_arguments.push("--disable-test-modules".into()); }
    // configure's OpenSSL search checks physical prefixes, whereas the C
    // compiler already resolves ordinary headers/libraries through the frozen
    // SDK. Point it at that same input when the user provided OpenSSL there.
    if let Some(sdk) = frozen.input_bindings.get("sdk") {
        let prefix = if sdk.join("usr/include/openssl/ssl.h").is_file() { Some(sdk.join("usr")) }
            else if sdk.join("include/openssl/ssl.h").is_file() { Some(sdk.clone()) } else { None };
        if let Some(prefix) = prefix { configure_arguments.push(format!("--with-openssl={}",prefix.display())); }
    }
    let mut configure_command = vec!["-c".into(),
        "\"$@\"; status=$?; if [ \"$status\" -ne 0 ]; then sed '/^## Cache variables/,$d' config.log | tail -c 8192 >&2; fi; exit \"$status\"".into(),
        "pakro-configure".into(),shell.to_string_lossy().into_owned()];
    configure_command.extend(configure_arguments);
    run(&mut session,context,&shell,&configure_command,
        &build,&environment,"configure").await?;
    let makefile = fs::read_to_string(build.join("Makefile"))?;
    let suffix = makefile.lines().filter_map(|line| line.split_once('=')).find(|(name,_)| name.trim() == "BUILDEXE").map_or("",|(_,value)|value.trim());
    ensure!(matches!(suffix,""|".exe"),"unsupported native CPython executable suffix");
    // A sealed executable can have a different realpath. Its explicit,
    // recipe-owned ._pth keeps the original build's adjacent Python resources.
    let pth_names = ["python._pth","python.exe._pth","_bootstrap_python._pth","_bootstrap_python.exe._pth"];
    let paths = format!("{}\n{}\n",raw.root().join("Lib").display(),build.display());
    for name in pth_names { fs::write(build.join(name),&paths)?; }
    let make = frozen.programs.get("make").context("Make was not frozen")?.path.clone();
    let fixed_make = ["-j2".to_owned(),"MAKE=make".into(),"SHELL=sh".into(),"PYTHON_FOR_REGEN=false".into()];
    let mut arguments = fixed_make.to_vec(); arguments.push(format!("python{suffix}"));
    run(&mut session,context,&make,&arguments,&build,&environment,"interpreter build").await?;
    let mut arguments = fixed_make.to_vec(); arguments.push("pybuilddir.txt".into());
    run(&mut session,context,&make,&arguments,&build,&environment,"build metadata").await?;
    let directory = fs::read_to_string(build.join("pybuilddir.txt"))?;
    let directory = Path::new(directory.trim());
    ensure!(!directory.is_absolute() && directory.components().all(|part| matches!(part,Component::Normal(_))),"generated CPython build directory is invalid");
    let paths = format!("{paths}{}\n",build.join(directory).display());
    for name in pth_names { fs::write(build.join(name),&paths)?; }
    let mut arguments = fixed_make.to_vec(); arguments.push("all".into());
    run(&mut session,context,&make,&arguments,&build,&environment,"standard-library build").await?;
    let mut arguments = fixed_make.to_vec(); arguments.extend(["install".into(),format!("DESTDIR={}",staging.display())]);
    run(&mut session,context,&make,&arguments,&build,&environment,"staged installation").await?;
    session.close().await?;
    let staged = staging.join(LOGICAL_PREFIX.trim_start_matches('/'));
    ensure!(staged.is_dir(),"CPython produced no staged logical prefix");
    let short = format!("{}.{}",parts[0],parts[1]);
    for name in ["python","python3"] {
        let path = staged.join("bin").join(name);
        if !path.exists() { std::os::unix::fs::symlink(format!("python{short}"),path)?; }
    }
    let mut bindings = frozen.input_bindings.iter().map(|(role,path)| (format!("input.{role}"),path.clone())).collect::<BTreeMap<_,_>>();
    bindings.extend(frozen.programs.iter().map(|(role,program)| (format!("program.{role}"),program.path.clone())));
    let mut owner = match LanguageRepository::open_writer(context)?.reserve_install(&spec)? {
        LanguageReservation::Existing(ready) => {
            let actual = super::output::stage_digest(context,&staged,raw.root(),&build,&tools.bin,&bindings)?;
            ensure!(ready.spec().plan().source_build().and_then(|input|input.expected_output) == Some(actual),
                "concurrent CPython build produced a different actual output for the same request");
            return Ok(ready.key());
        }
        LanguageReservation::Build(owner) => owner,
    };
    let (_,mut layout,_) = super::output::import_staged(context,&staged,&mut owner,raw.root(),&build,&tools.bin,bindings)?;
    let entrypoint = owner.final_path().join(format!("bin/python{short}"));
    ensure!(entrypoint.is_file(),"source CPython did not install its exact interpreter");
    for name in ["python","python3"] {
        let path = Path::new("bin").join(name);
        if !owner.final_path().join(&path).exists() { owner.create_symlink(&path,Path::new(&format!("python{short}")))?; }
    }
    let inventory = owner.capture_inventory()?;
    let digest = super::portable_output_digest(&inventory,owner.final_path(),&layout,|path|owner.read_file(path,16*1024*1024))?;
    let allocation = owner.final_path().parent().context("language allocation is missing")?.to_owned();
    let controls = allocation.join("source-validation"); fs::create_dir(&controls)?;
    let writable = controls.join("work"); fs::create_dir(&writable)?;
    let inspector = if cfg!(target_os = "macos") { "otool" } else { "readelf" };
    let inspector = tools.bin.join(inspector).canonicalize()?;
    // Publication must work with the payload and OS libraries alone. Neither
    // the build SDK nor its dynamic-library search environment is admitted.
    let mut validation_reads = vec![owner.final_path().to_owned(),controls.clone(),inspector.clone()];
    for path in ["/System/Library","/usr/lib","/lib","/lib64","/dev/null","/dev/urandom"] { if Path::new(path).exists() { validation_reads.push(path.into()); } }
    let mut programs = vec![entrypoint.clone(), inspector.clone()];
    programs.extend(frozen.programs.iter().filter(|(role,_)|role.starts_with("runtime-loader."))
        .map(|(_,program)|program.path.clone()));
    let mut validation = NativeBuildSession::start_owned(NativeBuildConfig { bootstrap: tools.bootstrap.clone(),fixed_programs:programs,
        sandbox:SandboxConfig { allow_read:validation_reads,allow_write:vec![writable.clone()],..Default::default() },workspace:allocation,
        exec_pool:controls.join("pool"),generated_roots:vec![writable.clone()],requests:controls.join("requests"),responses:controls.join("responses"),unshare:tools.unshare.clone() }, owner.native_cleanup_owner()).await?;
    let mut validation_environment = BTreeMap::from([("LANG".into(),"C".into()),("LC_ALL".into(),"C".into()),
        ("PYTHONDONTWRITEBYTECODE".into(),"1".into())]);
    validation_environment.insert("HOME".into(),writable.to_string_lossy().into_owned());
    validation_environment.insert("TMPDIR".into(),writable.to_string_lossy().into_owned());
    validation_environment.insert("OPENSSL_CONF".into(),"/dev/null".into());
    let mut libraries = std::collections::BTreeSet::new();
    for entry in inventory.entries() {
        context.checkpoint()?;
        let crate::packages::python_environment::prefix::verify::TreeInventoryKind::File { .. } = &entry.kind else { continue; };
        let name = entry.relative.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        let interpreter = entry.relative.parent() == Some(Path::new("bin")) && name.strip_prefix("python3.")
            .is_some_and(|minor| !minor.is_empty() && minor.bytes().all(|byte| byte.is_ascii_digit()));
        if !interpreter && !name.ends_with(".so") && !name.ends_with(".dylib") { continue; }
        let mut arguments = if cfg!(target_os = "macos") { vec!["-L".into()] }
            else { vec!["--dynamic".into(), "--program-headers".into(), "--wide".into()] };
        arguments.push(owner.final_path().join(&entry.relative).to_string_lossy().into_owned());
        let output = run(&mut validation,context,&inspector,&arguments,&writable,&validation_environment,"runtime library inspection").await?;
        libraries.extend(super::output::verify_system_runtime_dependencies(std::str::from_utf8(&output.stdout)?)?);
    }
    let mut arguments = python_runtime::FLAGS.iter().map(|value|value.to_string()).collect::<Vec<_>>(); arguments.push(python_runtime::SCRIPT.into());
    let output = run(&mut validation,context,&entrypoint,&arguments,&writable,&validation_environment,"installed runtime probe").await?;
    let handle = PythonRuntimeHandle::from_existing_executable(&entrypoint)?;
    let mut descriptor = python_runtime::decode(&handle,&output.stdout)?;
    if let Some(compatibility) = &mut descriptor.compatibility
        && let crate::python_runtime::PythonPlatformObservation::Linux { elf,libc:crate::python_runtime::PythonLibcObservation::Musl { version,loader },.. } = &mut compatibility.platform {
        let actual = python_runtime::read_bound_elf(&handle)?;
        ensure!(elf.as_ref() == Some(&actual) && actual.interpreter.as_ref() == Some(loader),"built CPython musl loader differs from its actual ELF");
        let output = validation.run(&loader.canonicalize()?,&[],&writable,&validation_environment,context.policy().deadline,64*1024,&context.cancellation_token()).await?;
        *version = python_runtime::musl_version(&output.stderr);
        ensure!(version.is_some(),"built CPython musl loader version could not be observed");
    }
    spec.plan().validate_observed(&descriptor)?;
    let smoke = r#"import importlib,importlib.machinery,importlib.util,json,os,sys
missing=[]
for name in ('ssl','hashlib','sqlite3','ctypes','zlib','bz2','lzma'):
    try: importlib.import_module(name)
    except ImportError: missing.append(name)
if missing:
    print(json.dumps({'missing':missing})); sys.exit(2)
import ssl,hashlib,sqlite3,ctypes,zlib,bz2,lzma
assert ssl.OPENSSL_VERSION and len(ssl.RAND_bytes(16))==16
assert hashlib.sha256(b'abc').hexdigest()=='ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'
assert sqlite3.connect(':memory:').execute('select 40+2').fetchone()[0]==42
libc=ctypes.CDLL(None); libc.abs.argtypes=[ctypes.c_int]; libc.abs.restype=ctypes.c_int
assert libc.abs(-42)==42
for module in (zlib,bz2,lzma): assert module.decompress(module.compress(b'pakro'))==b'pakro'
import unittest,unittest.mock,doctest
result=unittest.TestResult(); unittest.FunctionTestCase(lambda:None).run(result)
assert result.wasSuccessful()
mock=unittest.mock.Mock(return_value=42); assert mock()==42; mock.assert_called_once_with()
example=doctest.DocTestParser().get_doctest('>>> 40 + 2\n42\n',{},'pakro',None,0)
assert doctest.DocTestRunner().run(example).failed==0
if sys.argv[1] == 'without-self-tests':
    assert importlib.util.find_spec('test') is None
    assert importlib.util.find_spec('_testcapi') is None
# Every installed extension must resolve under the same SDK-free boundary,
# including optional modules that the seven required functional tests omit.
extension_dir=os.path.join(sys.base_prefix,'lib','python%d.%d'%sys.version_info[:2],'lib-dynload')
for filename in sorted(os.listdir(extension_dir)):
    if filename.endswith(tuple(importlib.machinery.EXTENSION_SUFFIXES)):
        importlib.import_module(filename.split('.',1)[0])
names=[]
if sys.platform == 'darwin':
    libc._dyld_image_count.restype=ctypes.c_uint32
    libc._dyld_get_image_name.argtypes=[ctypes.c_uint32]
    libc._dyld_get_image_name.restype=ctypes.c_char_p
    names=[libc._dyld_get_image_name(i).decode() for i in range(libc._dyld_image_count())]
else:
    class Image(ctypes.Structure):
        _fields_=[('address',ctypes.c_void_p),('name',ctypes.c_char_p)]
    Visit=ctypes.CFUNCTYPE(ctypes.c_int,ctypes.POINTER(Image),ctypes.c_size_t,ctypes.c_void_p)
    @Visit
    def visit(image,size,data):
        name=image.contents.name
        if name: names.append(name.decode())
        return 0
    libc.dl_iterate_phdr.argtypes=[Visit,ctypes.c_void_p]
    libc.dl_iterate_phdr(visit,None)
libraries=[]
for name in names:
    if name == 'linux-vdso.so.1': continue
    path=os.path.realpath(name)
    if path.startswith(sys.base_prefix+'/'):
        libraries.append('@payload/'+os.path.relpath(path,sys.base_prefix))
    elif any(path.startswith(root) for root in ('/usr/lib/','/usr/lib64/','/lib/','/lib64/','/System/Library/')):
        libraries.append(path)
    else:
        raise RuntimeError('unsupported non-system runtime library: '+path)
optional=('readline','_curses','_curses_panel','_tkinter','_gdbm','_dbm','_uuid','_decimal')
print(json.dumps({'ok':True,'version':sys.version.split()[0],'libraries':sorted(set(libraries)),
                  'optional_missing':[name for name in optional if importlib.util.find_spec(name) is None]}))
"#;
    let arguments = ["-I","-S","-B","-c",smoke,
        if input.includes_test_modules() { "with-self-tests" } else { "without-self-tests" }].map(str::to_owned);
    let output = validation.run(&entrypoint,&arguments,&writable,&validation_environment,context.policy().deadline,OUTPUT_LIMIT,&context.cancellation_token()).await?;
    ensure!(output.status.success(),"source CPython required standard-library verification failed or is missing modules: {} {}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RuntimeObservation { ok: bool, version: String, libraries: std::collections::BTreeSet<String>, optional_missing: std::collections::BTreeSet<String> }
    let observed: RuntimeObservation = serde_json::from_slice(&output.stdout)?;
    ensure!(observed.ok && observed.version == version && observed.libraries.len() <= 1024 && observed.optional_missing.len() <= 8,
        "source CPython system-runtime report differs from its required validation");
    libraries.extend(observed.libraries);
    ensure!(libraries.len() <= 1024,"source CPython system-runtime report exceeds its library budget");
    if !observed.optional_missing.is_empty() {
        log::info!("source CPython optional modules unavailable: {:?}",observed.optional_missing);
    }
    layout.system_runtime = Some(super::SourceSystemRuntime { policy: 1, libraries, optional_missing: observed.optional_missing });
    validation.close().await?;
    fs::remove_dir_all(&controls)?;
    let target = crate::packages::python_environment::runtime_target::freeze_observed_target(&descriptor,
        pakro_index::python_markers::PythonMarkerScope::Environment,pakro_index::python_markers::MarkerSemantics::Current2026,Default::default())?;
    let record = input.record(digest,*target.fingerprint())?;
    let receipt = SourcePythonBuildReceipt { key:spec.key(),inventory,descriptor,record,layout };
    let verified = crate::libraries::language::publication::verify_source_python(owner,receipt)?;
    Ok(crate::libraries::language::publication::publish_language(verified)?.key())
}
