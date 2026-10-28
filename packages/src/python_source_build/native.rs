//! Native recipe 2. The old no-fork recipe and driver remain independent.
use super::*;
use super::toolchain::{FrozenNativeToolchain, NativeToolRole};
use crate::packages::python_environment::model::{NativeProgramRecord, NativeToolchainRecord};
use crate::packages::python_environment::prefix::verify::TreeInventoryKind;
use crate::programs::OperationContext;
use pakro_util::native_build::{NativeBuildConfig, NativeBuildSession, NativeProgramIdentity};
use pakro_util::{native_exec, sandbox::SandboxConfig};
use std::{collections::BTreeSet, os::unix::fs::{OpenOptionsExt, PermissionsExt}};

const CPYTHON_TOOLS: &[&str] = &["sh", "env", "awk", "basename", "cat", "chmod", "cmp", "cp", "cut", "date", "diff", "dirname", "expr", "find", "grep", "head", "install", "ln", "ls", "mkdir", "mv", "od", "pwd", "rm", "rmdir", "sed", "sort", "tail", "touch", "tr", "uname", "wc", "xargs", "nm", "ranlib", "pkg-config"];
const PACKAGE_TOOLS: &[&str] = &[
    "sh", "env", "uname", "ranlib", "strip", "nm", "pkg-config",
    #[cfg(target_os = "macos")] "lipo",
    #[cfg(target_os = "macos")] "file",
    #[cfg(target_os = "macos")] "otool",
    #[cfg(target_os = "macos")] "install_name_tool",
    #[cfg(target_os = "linux")] "patchelf",
];

pub(crate) struct NativeTools {
    pub(crate) bin: PathBuf,
    pub(crate) gate: PathBuf,
    pub(crate) programs: Vec<PathBuf>,
    pub(crate) read_roots: Vec<PathBuf>,
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) record: NativeToolchainRecord,
    pub(crate) bootstrap: PathBuf,
    pub(crate) unshare: Option<PathBuf>,
}

fn quoted(value: &str) -> String { format!("'{}'", value.replace('\'', "'\\''")) }

fn write_tool(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn capture_program(context: &OperationContext, path: &Path, captured: &mut BTreeMap<PathBuf,[u8;32]>) -> Result<[u8;32]> {
    if let Some(hash) = captured.get(path) { return Ok(*hash); }
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new(); let mut buffer = [0u8; 32768];
    loop { context.checkpoint()?; let n = file.read(&mut buffer)?; if n == 0 { break; } hash.update(&buffer[..n]); }
    let hash = hash.finalize().into(); captured.insert(path.to_owned(), hash); Ok(hash)
}

/// Shared with the CPython source driver: fixed local tools and portable bytes.
pub(crate) fn prepare_tools(root: &Path, toolchain: &FrozenNativeToolchain, context: &OperationContext) -> Result<NativeTools> {
    prepare_tools_using(root, toolchain, context, PACKAGE_TOOLS, false)
}

pub(crate) fn prepare_cpython_tools(root: &Path, toolchain: &FrozenNativeToolchain, context: &OperationContext) -> Result<NativeTools> {
    let mut required = CPYTHON_TOOLS.to_vec();
    required.push(if cfg!(target_os = "macos") { "otool" } else { "readelf" });
    #[cfg(target_os = "macos")]
    required.push("sw_vers");
    prepare_tools_using(root, toolchain, context, &required, true)
}

fn prepare_tools_using(root: &Path, toolchain: &FrozenNativeToolchain, context: &OperationContext,
    required: &[&str], all_required: bool) -> Result<NativeTools> {
    ensure!(native_exec::SUPPORTED, "native compilation is unavailable on this platform");
    fs::create_dir(root)?;
    let bin = root.join("bin"); fs::create_dir(&bin)?;
    let gate = root.join(native_exec::GATE_LIBRARY_NAME);
    write_tool(&gate, native_exec::GATE_LIBRARY_BYTES, 0o444)?;
    let mut programs = BTreeSet::new();
    let mut captured = toolchain.programs.values().map(|program| (program.path.clone(), program.record.sha256)).collect::<BTreeMap<_,_>>();
    let mut extra_programs = Vec::new();
    let mut reads = toolchain.read_roots.iter().cloned().collect::<BTreeSet<_>>();
    reads.insert(root.to_owned());
    let mut helpers = Vec::new();
    let mut shell_selector_sha256 = None;
    let mut missing = Vec::new();
    for name in required {
        let roots = toolchain.environment.get("PATH").into_iter().flat_map(std::env::split_paths)
            .chain([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
        let found = roots.map(|root| root.join(name))
            .find(|path| path.is_file());
        if let Some(path) = found {
            let path = path.canonicalize()?;
            #[cfg(target_os = "macos")]
            let path = if *name == "otool" && path.file_name().is_some_and(|name| name == "llvm-otool") {
                let classic = path.with_file_name("otool-classic");
                if classic.is_file() { classic.canonicalize()? } else { path }
            } else { path };
            if !captured.contains_key(&path) { extra_programs.push(path.clone()); }
            let hash = capture_program(context, &path, &mut captured)?;
            helpers.push(NativeProgramRecord { role: format!("posix.{name}"), sha256: hash, version: "fixed-posix-tool-v1".into() });
            programs.insert(path.clone());
            reads.insert(path.clone());
            if *name == "sh" {
                let chain = pakro_util::native_build::native_shell_inputs()?;
                for (index, shell) in chain.programs.into_iter().enumerate() {
                    if shell == path { continue; }
                    let sha256 = capture_program(context, &shell, &mut captured)?;
                    helpers.push(NativeProgramRecord { role: format!("posix.sh-variant-{index}"), sha256, version: "system-sh-selector-v1".into() });
                    programs.insert(shell);
                }
                reads.extend(chain.read_inputs);
                shell_selector_sha256 = chain.selector_sha256;
            }
            std::os::unix::fs::symlink(&path, bin.join(name))?;
        } else if (all_required && *name != "pkg-config") || matches!(*name, "sh" | "env") { missing.push(*name); }
    }
    ensure!(missing.is_empty(), "missing installed native helper programs: {}", missing.join(", "));
    for program in toolchain.programs.values() {
        programs.insert(program.path.clone());
        let name = &program.invocation_name;
        if name.is_empty() || name.contains(['/', '\\']) { continue; }
        let alias = bin.join(name);
        if alias.symlink_metadata().is_ok() { fs::remove_file(&alias)?; }
        std::os::unix::fs::symlink(&program.path, alias)?;
    }
    let mut environment = toolchain.environment.clone();
    environment.insert("PAKRO_NATIVE_GATE_LIBRARY".into(), gate.to_string_lossy().into_owned());
    #[cfg(target_os = "linux")]
    environment.insert("LD_PRELOAD".into(), gate.to_string_lossy().into_owned());
    #[cfg(target_os = "macos")]
    environment.insert("DYLD_INSERT_LIBRARIES".into(), gate.to_string_lossy().into_owned());
    for (role, name, variable) in [("cc", "cc", "CC"), ("cxx", "c++", "CXX"), ("rustc", "rustc", "RUSTC")] {
        let Some(program) = toolchain.programs.get(role) else { continue; };
        let wrapper = bin.join(name);
        if wrapper.symlink_metadata().is_ok() { fs::remove_file(&wrapper)?; }
        write_tool(&wrapper, native_exec::COMPILER_WRAPPER_BYTES, 0o555)?;
        programs.insert(wrapper.clone());
        environment.insert(variable.into(), if role == "rustc" { wrapper.to_string_lossy().into_owned() }
            else { quoted(&wrapper.to_string_lossy()) });
        environment.insert(format!("PAKRO_NATIVE_REAL_{variable}"), program.path.to_string_lossy().into_owned());
        let mut arguments = program.arguments.clone();
        let workspace = root.parent().context("native tools have no workspace")?;
        if role != "rustc" {
            arguments.extend(toolchain.compiler_arguments.clone());
            arguments.extend([format!("-ffile-prefix-map={}=/pakro/build", workspace.display()),
                format!("-fdebug-prefix-map={}=/pakro/build", workspace.display())]);
        } else { arguments.push(format!("--remap-path-prefix={}=/pakro/build", workspace.display())); }
        ensure!(arguments.iter().all(|arg| !arg.contains(['\0', '\u{1f}'])), "native fixed argument cannot be encoded");
        environment.insert(format!("PAKRO_NATIVE_{variable}_ARGS"), arguments.join("\u{1f}"));
    }
    if let Some(cargo) = toolchain.programs.get("cargo") {
        let path = bin.join("cargo");
        if path.symlink_metadata().is_ok() { fs::remove_file(&path)?; }
        let injection = if cfg!(target_os="macos") { "DYLD_INSERT_LIBRARIES" } else { "LD_PRELOAD" };
        let script = format!("#!/bin/sh\nexport {injection}=\"$PAKRO_NATIVE_GATE_LIBRARY\"\nexec \"$PAKRO_NATIVE_REAL_CARGO\" --offline --frozen --config \"$PAKRO_NATIVE_CARGO_CONFIG\" \"$@\"\n");
        write_tool(&path, script.as_bytes(), 0o555)?;
        programs.insert(path.clone());
        environment.insert("CARGO".into(), path.to_string_lossy().into_owned());
        environment.insert("PAKRO_NATIVE_REAL_CARGO".into(), cargo.path.to_string_lossy().into_owned());
    }
    let mut paths = vec![bin.clone()];
    if let Some(path) = toolchain.environment.get("PATH") { paths.extend(std::env::split_paths(path)); }
    environment.insert("PATH".into(), std::env::join_paths(paths)?.to_string_lossy().into_owned());
    #[cfg(test)]
    let bootstrap = PathBuf::from(std::env::var_os("PAKRO_TEST_BINARY").context("PAKRO_TEST_BINARY must name the new native main")?).canonicalize()?;
    #[cfg(not(test))]
    let bootstrap = std::env::current_exe()?.canonicalize()?;
    let unshare = if cfg!(target_os="linux") { Some(Path::new("/usr/bin/unshare").canonicalize().context("installed util-linux unshare is required")?) } else { None };
    let mut record = toolchain.record().clone();
    if let Some(selector) = shell_selector_sha256 {
        record.configuration.insert("shell-selector-sha256".into(), hex::encode(selector));
    }
    record.configuration.insert("native-driver-sha256".into(), hex::encode(Sha256::digest(include_bytes!("hook_native.py"))));
    record.configuration.insert("native-gate-sha256".into(), hex::encode(Sha256::digest(native_exec::GATE_LIBRARY_BYTES)));
    record.configuration.insert("compiler-wrapper-sha256".into(), hex::encode(Sha256::digest(native_exec::COMPILER_WRAPPER_BYTES)));
    record.configuration.insert("supervision-sha256".into(), hex::encode(pakro_util::native_build::supervision_fingerprint()));
    record.configuration.insert("native-adapter-sha256".into(), hex::encode(Sha256::digest(include_bytes!("native.rs"))));
    record.configuration.insert("native-prefix-map".into(), "workspace-to-build-v1".into());
    record.programs.extend(helpers);
    extra_programs.push(bootstrap.clone());
    record.programs.push(NativeProgramRecord { role: "supervisor.bootstrap".into(), sha256: capture_program(context, &bootstrap, &mut captured)?, version: "pakro-native-v1".into() });
    if let Some(path) = &unshare {
        extra_programs.push(path.clone());
        record.programs.push(NativeProgramRecord { role: "supervisor.unshare".into(), sha256: capture_program(context, path, &mut captured)?, version: "util-linux-kill-child-pidns-v1".into() });
    }
    #[cfg(target_os = "linux")]
    for (index, loader) in pakro_util::sandbox::executable_runtime_loaders(&extra_programs)?.into_iter().enumerate() {
        if toolchain.programs.values().any(|program| program.path == loader) { continue; }
        let sha256 = capture_program(context, &loader, &mut captured)?;
        record.programs.push(NativeProgramRecord { role: format!("runtime-loader.extra-{index}"), sha256, version: "elf-pt-interp-v1".into() });
        programs.insert(loader.clone()); reads.insert(loader);
    }
    record.programs.sort_by(|a,b| a.role.cmp(&b.role));
    record.validate()?;
    Ok(NativeTools { bin, gate, programs: programs.into_iter().collect(), read_roots: reads.into_iter().collect(), environment, record, bootstrap, unshare })
}

pub(super) struct NativeSourceContext {
    context: OperationContext,
    tools: NativeTools,
    cargo: Option<cargo::CargoBuildInputs>,
    cargo_prepared: bool,
    session: Option<NativeBuildSession>,
    workspace: PathBuf,
    work: PathBuf,
    pool: PathBuf,
    requests: PathBuf,
    responses: PathBuf,
    driver: PathBuf,
    writable: Vec<PathBuf>,
    known_environments: BTreeSet<PathBuf>,
    registered_scripts: BTreeMap<String,String>,
}

pub(super) struct NativeSourceInputs { pub native: bool, pub cxx: bool }

/// Recipe selection comes from the original source, never installed tools.
pub(super) fn inspect_source(source: &ExtractedSource, context: &OperationContext,
    limits: SourceBuildLimits) -> Result<NativeSourceInputs> {
    let mut facts = NativeSourceInputs { native: false, cxx: false };
    for requirement in &source.initial_requirements {
        let name = requirement.trim_start().split(|value: char| !value.is_ascii_alphanumeric() && !matches!(value, '-' | '_' | '.')).next().unwrap_or_default();
        let name = name.split(['-', '_', '.']).filter(|value| !value.is_empty()).collect::<Vec<_>>().join("-").to_ascii_lowercase();
        if matches!(name.as_str(), "setuptools-rust" | "cython" | "cffi") { facts.native = true; }
    }
    let project = source.root.join("pyproject.toml");
    if project.is_file() {
        let raw = archive::read_bounded(&source.reader, &project, limits.max_metadata_bytes)?;
        let value: toml::Value = toml::from_str(std::str::from_utf8(&raw)?)?;
        if let Some(tool) = value.get("tool") {
            for backend in ["setuptools", "setuptools-rust"] {
                if tool.get(backend).and_then(|value| value.get("ext-modules")).and_then(toml::Value::as_array).is_some_and(|modules| !modules.is_empty()) { facts.native = true; }
            }
        }
    }
    for (index, entry) in walkdir::WalkDir::new(&source.root).follow_links(false).into_iter().enumerate() {
        context.checkpoint()?;
        ensure!(index <= limits.max_entries, "native source entry budget exceeded");
        let entry = entry?;
        if !entry.file_type().is_file() { continue; }
        match entry.path().extension().and_then(|value| value.to_str()) {
            Some("cpp" | "cxx" | "cc" | "C" | "c++" | "mm") => { facts.native = true; facts.cxx = true; },
            Some("c" | "m" | "rs" | "pyx") => facts.native = true,
            _ => {},
        }
    }
    Ok(facts)
}

impl NativeSourceContext {
    pub(super) async fn prepare(context: &OperationContext, language: &LanguageLease, source: &ExtractedSource,
        limits: SourceBuildLimits, expected: Option<&SourceBuildRecord>, source_inputs: NativeSourceInputs) -> Result<Self> {
        use NativeToolRole::*;
        context.checkpoint()?;
        let cargo = cargo::CargoBuildInputs::parse(&source.root, limits)?;
        let mut required = vec![Cc, Ar, Linker];
        if cargo.is_some() { required.extend([Cargo, Rustc]); }
        let backend = source.backend.split(':').next().unwrap_or_default();
        let settings = &context.settings().python.build.tools;
        let search = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
        // CMake's default project languages are C and CXX. Other backends
        // need CXX only for actual C++ sources, or can use an installed one.
        let needs_cxx = backend == "scikit_build_core.build" || source_inputs.cxx;
        let installed_cxx = search.iter().any(|root| ["c++", "clang++", "g++"].iter().any(|name|
            fs::metadata(root.join(name)).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)));
        if needs_cxx || settings.cxx.is_some() || installed_cxx { required.push(Cxx); }
        for (role, name, configured, relevant) in [
            (Cmake, "cmake", settings.cmake.as_ref(), backend == "scikit_build_core.build"),
            (Ninja, "ninja", settings.ninja.as_ref(), matches!(backend, "scikit_build_core.build" | "mesonpy")),
            (Meson, "meson", settings.meson.as_ref(), backend == "mesonpy"),
            (PkgConfig, "pkg-config", settings.pkg_config.as_ref(), true),
        ] {
            if relevant && (configured.is_some() || search.iter().any(|root| root.join(name).is_file())) { required.push(role); }
        }
        ensure!(source.backend.split(':').next() != Some("maturin") || cargo.is_some(), "maturin needs an archive-contained Cargo manifest and lock");
        let toolchain = FrozenNativeToolchain::discover(context, language, &required, expected.and_then(|record| record.native.as_ref())).await?;
        let workspace = source.workspace.path().canonicalize()?;
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700))?;
        let mut tools = prepare_tools(&workspace.join("native-tools"), &toolchain, context)?;
        for key in ["PAKRO_NATIVE_CC_ARGS", "PAKRO_NATIVE_CXX_ARGS"] {
            if let Some(arguments) = tools.environment.get_mut(key) {
                for flag in ["-ffile-prefix-map", "-fdebug-prefix-map"] {
                    arguments.push('\u{1f}'); arguments.push_str(&format!("{flag}={}=/pakro/python", language.root().display()));
                }
            }
        }
        tools.record.configuration.insert("native-python-prefix-map".into(), "language-root-v1".into());
        if let Some(cargo) = &cargo {
            tools.record.configuration.insert("cargo-lock-sha256".into(), hex::encode(cargo.lock_digest));
            tools.record.configuration.insert("cargo-inputs-sha256".into(), hex::encode(cargo.inputs_digest));
            tools.record.configuration.insert("cargo-manifest".into(), cargo.manifest_relative().to_string_lossy().into_owned());
            tools.record.configuration.insert("cargo-adapter-sha256".into(), hex::encode(Sha256::digest(include_bytes!("cargo.rs"))));
            if backend == "setuptools.build_meta" {
                tools.record.configuration.insert("setuptools-metadata".into(), "frontend-egg-base-v1".into());
            }
        }
        tools.record.validate()?;
        if let Some(expected) = expected {
            ensure!(expected.recipe == 2 && expected.native.as_ref() == Some(&tools.record), "locked native toolchain, SDK, driver or Cargo inputs changed");
        }
        let work = workspace.join("native-work"); fs::create_dir(&work)?;
        let pool = workspace.join("native-exec"); fs::create_dir(&pool)?;
        let requests = workspace.join("native-requests"); fs::create_dir(&requests)?;
        let responses = workspace.join("native-responses"); fs::create_dir(&responses)?;
        let driver = workspace.join("native-tools/hook_native.py");
        write_tool(&driver, include_bytes!("hook_native.py"), 0o444)?;
        let mut writable = vec![work.clone()];
        if cargo.is_none() { writable.push(source.root.clone()); }
        else {
            for name in ["build", "target"] { let path = source.root.join(name); fs::create_dir_all(&path)?; writable.push(path); }
            fs::create_dir(work.join("metadata"))?;
        }
        tools.environment.insert("PYO3_PYTHON".into(), language.entrypoint().to_string_lossy().into_owned());
        tools.environment.insert("PYTHON_SYS_EXECUTABLE".into(), language.entrypoint().to_string_lossy().into_owned());
        Ok(Self { context: context.clone(), tools, cargo, cargo_prepared: false, session: None, workspace,
            work, pool, requests, responses, driver, writable, known_environments: BTreeSet::new(), registered_scripts: BTreeMap::new() })
    }

    pub(super) fn record(&self) -> &NativeToolchainRecord { &self.tools.record }
    pub(super) async fn close(&mut self) -> Result<()> { if let Some(session) = &mut self.session { session.close().await?; } Ok(()) }

    pub(super) async fn run(&mut self, language: &LanguageLease, source: &ExtractedSource, environment: &BuildEnvironment,
        hook: &str, metadata: Option<&Path>, deadline: Instant, limits: SourceBuildLimits) -> Result<worker::HookReply> {
        self.context.checkpoint()?;
        ensure!(matches!(hook, "get_requires_for_build_wheel" | "prepare_metadata_for_build_wheel" | "build_wheel"), "unsupported native hook");
        if !self.cargo_prepared && let Some(cargo) = &mut self.cargo {
            use crate::config::SettingsExt;
            let used = worker::workspace_usage(source, limits, deadline)?;
            // A crate's verified raw tree and its vendor copy coexist until
            // that crate's import finishes, like the existing wheel builder.
            cargo.constrain_workspace(limits.max_workspace_bytes.checked_sub(used.bytes).context("no Cargo workspace bytes remain")? / 2,
                limits.max_workspace_entries.checked_sub(used.entries).context("no Cargo workspace entries remain")? / 2)?;
            let transport = pakro_index::artifact::ArtifactTransport::new(reqwest::Client::builder()
                .connect_timeout(self.context.settings().http_timeout()).read_timeout(self.context.settings().http_timeout())
                .timeout(self.context.settings().http_download_timeout()))?;
            let home = self.workspace.join("native-cargo-inputs");
            cargo.prepare(&self.context, &transport, &self.workspace.join("native-vendor"), &home, &self.work.join("cargo-scratch")).await?;
            let state = self.work.join("cargo-state"); fs::create_dir(&state)?;
            self.tools.environment.insert("CARGO_HOME".into(), state.to_string_lossy().into_owned());
            self.tools.environment.insert("PAKRO_NATIVE_CARGO_CONFIG".into(), home.join("config.toml").to_string_lossy().into_owned());
            self.tools.environment.insert("CARGO_TARGET_DIR".into(), source.root.join("target").to_string_lossy().into_owned());
            if self.tools.environment.contains_key("CC") {
                self.tools.environment.insert("CARGO_ENCODED_RUSTFLAGS".into(), format!("-C\u{1f}linker={}", self.tools.bin.join("cc").display()));
            }
            self.cargo_prepared = true;
            worker::workspace_usage(source, limits, deadline)?;
        }
        if self.session.is_none() {
            let mut reads = self.tools.read_roots.clone(); reads.extend([self.workspace.clone(), language.root().to_owned()]);
            for path in ["/System/Library", "/usr/lib", "/lib", "/lib64", "/dev/null", "/dev/urandom"] {
                if Path::new(path).exists() { reads.push(PathBuf::from(path)); }
            }
            self.session = Some(NativeBuildSession::start_owned(NativeBuildConfig { bootstrap: self.tools.bootstrap.clone(),
                fixed_programs: self.tools.programs.clone(), sandbox: SandboxConfig { allow_read: reads, allow_write: self.writable.clone(), ..Default::default() },
                workspace: self.workspace.clone(), exec_pool: self.pool.clone(), generated_roots: self.writable.clone(),
                requests: self.requests.clone(), responses: self.responses.clone(), unshare: self.tools.unshare.clone() }, source.native_cleanup_owner()).await?);
        }
        let prefix = environment.prefix.final_path();
        if self.known_environments.insert(prefix.to_owned()) {
            let mut programs = Vec::new();
            for entry in environment.inventory.entries() {
                if let TreeInventoryKind::File { sha256, .. } = &entry.kind && entry.mode & 0o111 != 0 {
                    let path = prefix.join(&entry.relative);
                    programs.push(NativeProgramIdentity { path: path.clone(), sha256: *sha256 });
                    if entry.relative.starts_with("bin") {
                        let mut file = fs::File::open(&path)?; let mut head = vec![0u8; limits.max_path_bytes.min(65536)]; let size = file.read(&mut head)?;
                        let text = String::from_utf8_lossy(&head[..size]);
                        if head[..size].starts_with(b"#!") && (text.lines().next().is_some_and(|line| line.contains("python"))
                            || (text.contains("'''exec'") && text.contains("python"))) {
                            self.registered_scripts.insert(path.to_string_lossy().into_owned(), environment.site.to_string_lossy().into_owned());
                        }
                    }
                }
            }
            self.session.as_mut().unwrap().register_fixed_inputs(&programs).await?;
        }
        let private = self.work.join(format!("hook-{}", uuid::Uuid::now_v7().simple())); fs::create_dir(&private)?;
        for name in ["output", "home", "tmp", "response"] { fs::create_dir(private.join(name))?; }
        let output = private.join("output"); let response = private.join("response/result.json");
        let request = self.responses.join(format!("hook-{}.json", uuid::Uuid::now_v7().simple()));
        let settings = if source.backend.split(':').next() == Some("mesonpy") {
            serde_json::json!({"build-dir":source.root.join("build")})
        } else if self.cargo.is_some() && source.backend.split(':').next() == Some("setuptools.build_meta") {
            // Setuptools' documented command escape hatch supplies egg_info's
            // frontend output location, including src-layout first installs.
            serde_json::json!({"--global-option":["egg_info","--egg-base",self.work.join("metadata")]})
        } else { serde_json::Value::Null };
        fs::write(&request, serde_json::to_vec(&serde_json::json!({ "protocol":2, "backend":source.backend,
            "backend_paths":source.backend_paths, "dependency_sites":[environment.site], "hook":hook,
            "directory":output, "metadata_directory":metadata, "max_file_bytes":limits.max_file_bytes.min(limits.max_generated_wheel_bytes),
            "console_scripts":self.registered_scripts, "native_driver":self.driver, "config_settings":settings }))?)?;
        let mut env = self.tools.environment.clone();
        let mut path = vec![self.tools.bin.clone(), prefix.join("bin")];
        if let Some(value) = env.get("PATH") { path.extend(std::env::split_paths(value)); }
        env.insert("PATH".into(), std::env::join_paths(path)?.to_string_lossy().into_owned());
        for (key, value) in [("HOME", private.join("home")), ("TMPDIR", private.join("tmp")), ("TMP", private.join("tmp")), ("TEMP", private.join("tmp"))] { env.insert(key.into(), value.to_string_lossy().into_owned()); }
        env.insert("PYTHONNOUSERSITE".into(), "1".into());
        env.insert("PAKRO_NATIVE_HOOK_REQUEST".into(), request.to_string_lossy().into_owned());
        let args = vec!["-I".into(), "-S".into(), "-B".into(), self.driver.to_string_lossy().into_owned(), request.to_string_lossy().into_owned(), response.to_string_lossy().into_owned()];
        worker::workspace_usage(source, limits, deadline)?;
        let cancellation = self.context.cancellation_token();
        let execute = self.session.as_mut().unwrap().run(language.entrypoint(), &args, &source.root, &env,
            deadline, limits.max_hook_output_bytes, &cancellation);
        tokio::pin!(execute);
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(25));
        let result = loop {
            tokio::select! {
                result = &mut execute => break result?,
                _ = interval.tick() => { worker::workspace_usage(source, limits, deadline)?; },
            }
        };
        worker::workspace_usage(source, limits, deadline)?;
        let reply: worker::WireReply = serde_json::from_slice(&archive::read_bounded(&source.reader, &response, limits.max_hook_output_bytes as u64)?)?;
        let error = reply.error.as_deref().filter(|value| value.len() <= 128 && !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')).unwrap_or("BackendFailure");
        if !result.status.success() || !reply.ok {
            let diagnostic = |bytes: &[u8]| {
                String::from_utf8_lossy(&bytes[..bytes.len().min(16 * 1024)]).chars().flat_map(|value| {
                    if value.is_control() && !matches!(value, '\n' | '\t') { value.escape_debug().collect::<Vec<_>>() }
                    else { vec![value] }
                }).collect::<String>()
            };
            eyre::bail!("native source {hook} hook failed ({error})\nstdout (bounded):\n{}\nstderr (bounded):\n{}",
                diagnostic(&result.stdout), diagnostic(&result.stderr));
        }
        Ok(worker::HookReply { missing: reply.missing, value: reply.value, directory: output })
    }
}
