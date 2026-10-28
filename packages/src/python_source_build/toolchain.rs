//! Discover one installed native toolchain, then carry its frozen local
//! bindings and portable input record through this build operation.
use crate::libraries::language::lease::LanguageLease;
use crate::programs::OperationContext;
pub(crate) use crate::packages::python_environment::model::{NativeProgramRecord, NativeSdkRecord, NativeToolchainRecord};
use eyre::{Context, ContextCompat, Result, bail, ensure};
use pakro_util::cmd::CmdLineRunner;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 262_144;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROGRAM_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NativeToolRole { Cc, Cxx, Ar, Linker, Cmake, Ninja, Meson, Cargo, Rustc, PkgConfig, Make }
impl NativeToolRole {
    pub(crate) fn name(self) -> &'static str {
        match self { Self::Cc => "cc", Self::Cxx => "cxx", Self::Ar => "ar", Self::Linker => "linker",
            Self::Cmake => "cmake", Self::Ninja => "ninja", Self::Meson => "meson", Self::Cargo => "cargo",
            Self::Rustc => "rustc", Self::PkgConfig => "pkg_config", Self::Make => "make" }
    }
}

#[derive(Debug, Clone)]
struct NativeInputRecord {
    content_sha256: [u8; 32],
    /// A search directory covered by another fixed input tree is represented
    /// relative to that tree, so nested library roots are not hashed twice.
    relative: PathBuf,
    root_kind: String,
    entries: usize,
    bytes: u64,
}

#[derive(Debug)]
pub(crate) struct FrozenNativeProgram {
    pub(crate) path: PathBuf,
    pub(crate) record: NativeProgramRecord,
    /// Original role invocation, e.g. clang++ when it resolves to clang.
    pub(crate) invocation_name: String,
    pub(crate) identity: pakro_store::control::FileIdentity,
    pub(crate) version_status: Option<i32>,
    pub(crate) arguments: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct FrozenNativeToolchain {
    pub(crate) programs: BTreeMap<String, FrozenNativeProgram>,
    pub(crate) read_roots: Vec<PathBuf>,
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) compiler_arguments: Vec<String>,
    pub(crate) input_usage: BTreeMap<String, (usize, u64)>,
    pub(crate) input_bindings: BTreeMap<String, PathBuf>,
    record: NativeToolchainRecord,
    fingerprint: [u8; 32],
}
impl FrozenNativeToolchain {
    pub(crate) fn record(&self) -> &NativeToolchainRecord { &self.record }
    pub(crate) fn fingerprint(&self) -> Result<[u8; 32]> { Ok(self.fingerprint) }

    /// Called once for the selected native recipe. Unused optional tools are
    /// neither probed nor added to that recipe's execution/input permissions.
    pub(crate) async fn discover(context: &OperationContext, language: &LanguageLease,
        required: &[NativeToolRole], expected: Option<&NativeToolchainRecord>) -> Result<Self> {
        Self::discover_inner(context, Some(language), required, expected).await
    }

    pub(crate) async fn discover_cpython(context: &OperationContext, required: &[NativeToolRole]) -> Result<Self> {
        ensure!([NativeToolRole::Cc, NativeToolRole::Cxx, NativeToolRole::Ar, NativeToolRole::Linker, NativeToolRole::Make]
            .iter().all(|role| required.contains(role)), "CPython source builds require cc, cxx, ar, linker and make");
        let mut required = required.to_vec();
        if context.settings().python.build.tools.pkg_config.is_some() && !required.contains(&NativeToolRole::PkgConfig) {
            required.push(NativeToolRole::PkgConfig);
        }
        Self::discover_inner(context, None, &required, None).await
    }

    async fn discover_inner(context: &OperationContext, language: Option<&LanguageLease>, required: &[NativeToolRole],
        expected: Option<&NativeToolchainRecord>) -> Result<Self> {
        context.checkpoint()?;
        let settings = &context.settings().python.build;
        let search = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
        let cwd = std::env::current_dir().context("native tool discovery needs a current directory")?;
        let mut probe_env = BTreeMap::from([("PATH".into(), std::env::join_paths(&search)?.to_string_lossy().into_owned()),
            ("LANG".into(), "C".into()), ("LC_ALL".into(), "C".into()),
            ("RUSTUP_AUTO_INSTALL".into(), "0".into())]);
        for name in ["HOME", "RUSTUP_HOME", "CARGO_HOME", "RUSTUP_TOOLCHAIN", "DEVELOPER_DIR"] {
            if let Some(value) = std::env::var_os(name) { probe_env.insert(name.into(), value.to_string_lossy().into_owned()); }
        }
        let required = required.iter().copied().collect::<BTreeSet<_>>();
        let mut selected = BTreeMap::<String, (PathBuf, String)>::new();
        if let Some(language) = language { selected.insert("python".into(), (language.entrypoint().canonicalize()?, "python".into())); }
        let mut missing = Vec::new();
        for (role, explicit, names) in [
            (NativeToolRole::Cc, settings.tools.cc.as_ref(), &["cc", "clang", "gcc"][..]),
            (NativeToolRole::Cxx, settings.tools.cxx.as_ref(), &["c++", "clang++", "g++"][..]),
            (NativeToolRole::Ar, settings.tools.ar.as_ref(), &["ar"][..]),
            (NativeToolRole::Linker, settings.tools.linker.as_ref(), &["ld"][..]),
            (NativeToolRole::Cmake, settings.tools.cmake.as_ref(), &["cmake"][..]),
            (NativeToolRole::Ninja, settings.tools.ninja.as_ref(), &["ninja"][..]),
            (NativeToolRole::Meson, settings.tools.meson.as_ref(), &["meson"][..]),
            (NativeToolRole::Cargo, settings.tools.cargo.as_ref(), &["cargo"][..]),
            (NativeToolRole::Rustc, settings.tools.rustc.as_ref(), &["rustc"][..]),
            (NativeToolRole::PkgConfig, settings.tools.pkg_config.as_ref(), &["pkg-config"][..]),
            (NativeToolRole::Make, settings.tools.make.as_ref(), &["make", "gmake"][..]),
        ] {
            if !required.contains(&role) { continue; }
            let requested = explicit.cloned().or_else(|| names.iter().find(|name| executable(Path::new(name), &search, &cwd).is_some()).map(|name| PathBuf::from(*name)));
            let Some(requested) = requested else { missing.push(role.name()); continue; };
            let invocation = requested.file_name().context("native tool has no basename")?.to_string_lossy().into_owned();
            let Some(mut path) = executable(&requested, &search, &cwd) else { missing.push(role.name()); continue; };
            if matches!(role, NativeToolRole::Cargo | NativeToolRole::Rustc) {
                let rustup = executable(Path::new("rustup"), &search, &cwd);
                let proxy = path.file_name().is_some_and(|name| name == "rustup") || rustup.as_ref().is_some_and(|rustup| {
                    let a = std::fs::metadata(&path).ok(); let b = std::fs::metadata(rustup).ok();
                    matches!((a,b), (Some(a),Some(b)) if (a.dev(),a.ino()) == (b.dev(),b.ino()))
                });
                if proxy {
                    let Some(rustup) = rustup else { missing.push(role.name()); continue; };
                    let result = probe(context, &rustup, &["which".into(), role.name().into()], &probe_env).await?;
                    if !result.status.success() { missing.push(role.name()); continue; }
                    let resolved = std::str::from_utf8(&result.stdout)?.trim();
                    let Some(actual) = executable(Path::new(resolved), &search, &cwd) else { missing.push(role.name()); continue; };
                    ensure!(actual != rustup, "rustup which returned a proxy instead of an installed {}", role.name());
                    path = actual;
                }
            }
            #[cfg(target_os = "macos")]
            if path.starts_with("/usr/bin") && matches!(role, NativeToolRole::Cc | NativeToolRole::Cxx | NativeToolRole::Ar | NativeToolRole::Linker | NativeToolRole::Make) {
                let xcrun = executable(Path::new("/usr/bin/xcrun"), &search, &cwd).context("native Apple tools need installed xcrun")?;
                let name = if role == NativeToolRole::Cxx { "clang++" } else if role == NativeToolRole::Cc { "clang" }
                    else if role == NativeToolRole::Make { "gnumake" } else { role.name() };
                let name = if role == NativeToolRole::Linker { "ld" } else { name };
                let output = probe(context, &xcrun, &["--find".into(), name.into()], &probe_env).await?;
                ensure!(output.status.success(), "xcrun could not locate installed {name}");
                path = executable(Path::new(std::str::from_utf8(&output.stdout)?.trim()), &search, &cwd)
                    .context("xcrun returned an unavailable native program")?;
                selected.insert("discovery.xcrun".into(), (xcrun, "xcrun".into()));
            }
            selected.insert(role.name().into(), (path, invocation));
        }
        ensure!(missing.is_empty(), "missing installed native build tools: {}; install them or set settings.python.build.tools", missing.join(", "));
        let mut inputs = Vec::<(String, PathBuf)>::new();
        let mut compiler_arguments = Vec::new();
        let mut portable_arguments = Vec::<String>::new();
        let mut environment = BTreeMap::from([("LANG".into(), "C".into()), ("LC_ALL".into(), "C".into()),
            ("TZ".into(), "UTC".into()), ("SOURCE_DATE_EPOCH".into(), "315532800".into()),
            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()), ("CARGO_NET_OFFLINE".into(), "true".into()),
            ("OPENSSL_CONF".into(), "/dev/null".into()),
            ("CCACHE_DISABLE".into(), "1".into())]);
        #[cfg(target_os = "macos")]
        if required.contains(&NativeToolRole::Cargo) {
            // Apple's libcurl defaults to LibreSSL and loads /etc/ssl even
            // for Cargo's version probe. Select its OS backend while offline.
            environment.insert("CURL_SSL_BACKEND".into(), "Secure-Transport".into());
            // Keep Mach-O string tables intact (rust-lang/rust#157750).
            environment.insert("CARGO_PROFILE_RELEASE_STRIP".into(), "none".into());
        }
        let mut portable_environment = environment.clone();
        portable_environment.insert("OPENSSL_CONF".into(), "@null-device".into());
        let native_c = required.contains(&NativeToolRole::Cc) || required.contains(&NativeToolRole::Cxx);
        let sdk = if !native_c { None } else if let Some(path) = &settings.sdk { Some(path.canonicalize().context("configured SDK is unavailable")?) } else {
            #[cfg(target_os = "macos")]
            {
                if native_c {
                    let xcrun = executable(Path::new("/usr/bin/xcrun"), &search, &cwd).context("native Apple builds need installed xcrun and SDK")?;
                    let output = probe(context, &xcrun, &["--show-sdk-path".into()], &probe_env).await?;
                    ensure!(output.status.success(), "no native Apple SDK is installed");
                    selected.insert("discovery.xcrun".into(), (xcrun, "xcrun".into()));
                    Some(Path::new(std::str::from_utf8(&output.stdout)?.trim()).canonicalize()?)
                } else { None }
            }
            #[cfg(not(target_os = "macos"))]
            { None }
        };
        if let Some(sdk) = sdk {
            ensure!(sdk.is_dir(), "SDK is not a directory");
            environment.insert("SDKROOT".into(), sdk.to_string_lossy().into_owned());
            portable_environment.insert("SDKROOT".into(), "@sdk".into());
            #[cfg(target_os = "macos")]
            { compiler_arguments.extend(["-isysroot".into(), sdk.to_string_lossy().into_owned()]); portable_arguments.extend(["-isysroot".into(), "@sdk".into()]); }
            #[cfg(target_os = "linux")]
            {
                let mut pkgconfig = vec![sdk.join("lib/pkgconfig"), sdk.join("share/pkgconfig")];
                for (directory, flag) in [("include", "-I"), ("lib", "-L")] {
                    if sdk.join(directory).is_dir() {
                        compiler_arguments.push(format!("{flag}{}", sdk.join(directory).display()));
                        portable_arguments.push(format!("{flag}@sdk/{directory}"));
                    }
                }
                if let Some((compiler, _)) = selected.get("cc").or_else(|| selected.get("cxx")) {
                    let output = probe(context, compiler, &["-print-multiarch".into()], &probe_env).await?;
                    if output.status.success() {
                        let triple = std::str::from_utf8(&output.stdout)?.trim();
                        ensure!(!triple.contains(['/', '\\']) && triple.len() <= 128, "compiler multiarch tuple is invalid");
                        if !triple.is_empty() {
                            for (directory, flag) in [("include", "-I"), ("lib", "-L")] {
                                let relative = Path::new(directory).join(triple);
                                if sdk.join(&relative).is_dir() {
                                    compiler_arguments.push(format!("{flag}{}", sdk.join(&relative).display()));
                                    portable_arguments.push(format!("{flag}@sdk/{}", relative.display()));
                                    if directory == "lib" { pkgconfig.push(sdk.join(&relative).join("pkgconfig")); }
                                }
                            }
                        }
                    }
                }
                let pkgconfig = pkgconfig.into_iter().filter(|path| path.is_dir()).collect::<Vec<_>>();
                if !pkgconfig.is_empty() {
                    environment.insert("PKG_CONFIG_PATH".into(), std::env::join_paths(pkgconfig.iter())?.to_string_lossy().into_owned());
                    portable_environment.insert("PKG_CONFIG_PATH".into(), pkgconfig.iter().map(|path| format!("@sdk/{}", path.strip_prefix(&sdk).expect("SDK pkg-config directory").display())).collect::<Vec<_>>().join(":"));
                }
            }
            inputs.push(("sdk".into(), sdk));
        }
        let compiler = selected.get("cc").or_else(|| selected.get("cxx")).map(|value| value.0.clone());
        let mut native_platform = format!("{}-{}-{}", std::env::consts::OS, std::env::consts::ARCH,
            if cfg!(target_os="linux") { if cfg!(target_env="musl") { "musl" } else { "gnu" } } else { "none" });
        let mut compiler_target = None;
        if let Some(compiler) = &compiler {
            let output = probe(context, compiler, &["-dumpmachine".into()], &probe_env).await?;
            ensure!(output.status.success(), "C compiler could not describe its native target");
            let triple = std::str::from_utf8(&output.stdout)?.trim();
            ensure!(triple.len() <= 128 && triple.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte)), "compiler target tuple is invalid");
            let arch = if triple.starts_with("aarch64") || triple.starts_with("arm64") { "aarch64" }
                else if triple.starts_with("x86_64") { "x86_64" } else { "unsupported" };
            let os = if triple.contains("darwin") { "macos" } else if triple.contains("linux") { "linux" } else { "unsupported" };
            ensure!(arch == std::env::consts::ARCH && os == std::env::consts::OS, "native source builds do not support a cross compiler");
            let libc = if os == "macos" { "none" } else if triple.contains("musl") { "musl" } else { "gnu" };
            native_platform = format!("{os}-{arch}-{libc}"); compiler_target = Some(triple.to_owned());
        }
        let sysroot = if !native_c { None } else if let Some(path) = &settings.sysroot { Some(path.canonicalize().context("configured sysroot is unavailable")?) }
            else if let Some(compiler) = &compiler {
                let output = probe(context, compiler, &["-print-sysroot".into()], &probe_env).await?;
                if output.status.success() { let value = std::str::from_utf8(&output.stdout)?.trim();
                    if value.is_empty() || value == "/" { None } else { Some(Path::new(value).canonicalize()?) }
                } else { None }
            } else { None };
        if let Some(sysroot) = sysroot.filter(|path| path != Path::new("/")) {
            ensure!(sysroot.is_dir(), "compiler sysroot is not a directory");
            environment.insert("SYSROOT".into(), sysroot.to_string_lossy().into_owned());
            portable_environment.insert("SYSROOT".into(), "@sysroot".into());
            compiler_arguments.push(format!("--sysroot={}", sysroot.display()));
            portable_arguments.push("--sysroot=@sysroot".into());
            inputs.push(("sysroot".into(), sysroot));
        }
        for role in ["cc", "cxx"] {
            let Some((compiler, _)) = selected.get(role).cloned() else { continue; };
            for helper in ["cc1", "cc1plus", "as", "ld", "collect2"] {
                let mut args = compiler_arguments.clone(); args.push(format!("-print-prog-name={helper}"));
                let output = probe(context, &compiler, &args, &probe_env).await?;
                if !output.status.success() { continue; }
                let value = std::str::from_utf8(&output.stdout)?.trim();
                if value.is_empty() { continue; }
                if matches!(helper, "cc1" | "cc1plus") && value == helper
                    && compiler.file_name().is_some_and(|name| name.to_string_lossy().contains("clang")) { continue; }
                if let Some(path) = executable(Path::new(value), &search, &cwd) {
                    selected.insert(format!("{role}.{helper}"), (path, helper.into()));
                } else if Path::new(value).is_absolute() {
                    bail!("{role} points to an unavailable compiler helper: {helper}");
                }
            }
            if native_c {
                let output = probe(context, &compiler, &["-print-resource-dir".into()], &probe_env).await?;
                if output.status.success() && let Ok(path) = Path::new(std::str::from_utf8(&output.stdout)?.trim()).canonicalize()
                    && path.is_dir() { inputs.push((format!("{role}.resource"), path)); }
                let mut args = compiler_arguments.clone(); args.extend(["-E".into(), "-x".into(), if role == "cxx" { "c++".into() } else { "c".into() }, "-".into(), "-v".into()]);
                let output = probe(context, &compiler, &args, &probe_env).await?;
                ensure!(output.status.success(), "{role} could not describe its native include search roots");
                let text = std::str::from_utf8(&output.stderr)?;
                let mut active = false; let mut count = 0;
                for line in text.lines().map(str::trim) {
                    if line == "#include <...> search starts here:" { active = true; continue; }
                    if line == "End of search list." { active = false; continue; }
                    if active && let Ok(path) = Path::new(line.strip_suffix(" (framework directory)").unwrap_or(line)).canonicalize() && path.is_dir() {
                        inputs.push((format!("{role}.include-{count}"), path)); count += 1;
                    }
                }
                ensure!(count > 0, "{role} returned no supported include search roots");
                let mut args = compiler_arguments.clone(); args.push("-print-search-dirs".into());
                let output = probe(context, &compiler, &args, &probe_env).await?;
                ensure!(output.status.success(), "{role} could not describe its native library search roots");
                let value = std::str::from_utf8(&output.stdout)?.lines().find_map(|line| line.strip_prefix("libraries: ="))
                    .context("compiler returned no library search directories")?;
                for (index, path) in std::env::split_paths(value).enumerate() {
                    if let Ok(path) = path.canonicalize() && path.is_dir() { inputs.push((format!("{role}.library-{index}"), path)); }
                }
                #[cfg(target_os = "linux")]
                {
                    let mut args = compiler_arguments.clone(); args.push("-print-file-name=liblto_plugin.so".into());
                    let output = probe(context, &compiler, &args, &probe_env).await?;
                    if output.status.success() {
                        let value = std::str::from_utf8(&output.stdout)?.trim();
                        if !value.is_empty() && value != "liblto_plugin.so" {
                            let plugin = Path::new(value).canonicalize().context("compiler LTO plugin is unavailable")?;
                            ensure!(plugin.is_file(), "compiler LTO plugin is not a regular file");
                            inputs.push((format!("{role}.lto-plugin"), plugin));
                        }
                    }
                }
            }
        }
        if let Some((rustc, _)) = selected.get("rustc").cloned() {
            let output = probe(context, &rustc, &["--print".into(), "sysroot".into()], &probe_env).await?;
            ensure!(output.status.success(), "rustc could not describe its installed sysroot");
            let sysroot = Path::new(std::str::from_utf8(&output.stdout)?.trim()).canonicalize()?;
            let output = probe(context, &rustc, &["-vV".into()], &probe_env).await?;
            ensure!(output.status.success(), "rustc could not describe its native host");
            let host = std::str::from_utf8(&output.stdout)?.lines().find_map(|line| line.strip_prefix("host: ")).context("rustc host is missing")?.to_owned();
            ensure!(!host.contains(['/', '\\']) && !host.is_empty(), "rustc host is invalid");
            let lib = sysroot.join("lib").canonicalize()?;
            for relative in [format!("rustlib/{host}/bin/rust-lld"), format!("rustlib/{host}/bin/rust-objcopy"), format!("rustlib/{host}/bin/gcc-ld/ld.lld"), format!("rustlib/{host}/bin/gcc-ld/ld64.lld")] {
                if let Some(path) = executable(&lib.join(&relative), &search, &cwd) {
                    let name = Path::new(&relative).file_name().unwrap().to_string_lossy().into_owned();
                    selected.insert(format!("rust.{name}"), (path, name));
                }
            }
            inputs.push(("rust-sysroot".into(), lib.join("rustlib").join(&host).join("lib").canonicalize()
                .context("native Rust standard-library inputs are unavailable")?));
            inputs.push(("rust.library-0".into(), lib));
        }
        if let Some((cmake, _)) = selected.get("cmake") {
            let share = cmake.parent().and_then(Path::parent).context("CMake has no installation prefix")?.join("share");
            let mut modules = std::fs::read_dir(&share).with_context(|| format!("installed CMake modules are unavailable under {}", share.display()))?
                .filter_map(|entry| match entry { Ok(entry) if entry.file_name() == "cmake" || entry.file_name().to_string_lossy().starts_with("cmake-") => Some(entry.path()), _ => None }).collect::<Vec<_>>();
            modules.sort();
            ensure!(!modules.is_empty() && modules.len() <= 8, "CMake has no bounded standard share/cmake or share/cmake-* module roots");
            for (index, path) in modules.into_iter().enumerate() {
                let path = path.canonicalize()?; ensure!(path.is_dir(), "CMake module root is not a directory");
                inputs.push((format!("cmake-modules.{index}"), path));
            }
        }
        #[cfg(target_os = "linux")]
        {
            let paths = selected.values().map(|(path, _)| path.clone()).collect::<Vec<_>>();
            for (index, path) in pakro_util::sandbox::executable_runtime_loaders(&paths)?.into_iter().enumerate() {
                let name = path.file_name().context("runtime loader has no basename")?.to_string_lossy().into_owned();
                selected.insert(format!("runtime-loader.{index}"), (path, name));
            }
        }
        #[cfg(target_os = "macos")]
        if let Some((linker, _)) = selected.get("linker") {
            let library = linker.parent().and_then(Path::parent).context("Apple linker has no installation prefix")?.join("lib");
            for name in ["libtapi.dylib", "libcodedirectory.dylib", "libLTO.dylib", "libswiftDemangle.dylib"] {
                let path = library.join(name);
                match std::fs::symlink_metadata(&path) {
                    Ok(_) => {
                        let path = path.canonicalize().context("Apple linker runtime input is unavailable")?;
                        ensure!(path.is_file(), "Apple linker runtime input is not a file");
                        inputs.push((format!("linker.runtime-{name}"), path));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let mut input_paths = BTreeSet::new();
        inputs.retain(|(_, path)| input_paths.insert(path.clone()));
        let mut programs = BTreeMap::new();
        let mut cached = BTreeMap::<PathBuf, ([u8; 32], pakro_store::control::FileIdentity, Option<String>, Option<i32>)>::new();
        for (role, (path, invocation)) in selected {
            context.checkpoint()?;
            let (sha256, identity, version, status) = if role == "python" {
                let language = language.context("Python discovery has no held language")?;
                let file = File::open(&path)?;
                (language.entrypoint_sha256()?, pakro_store::control::FileIdentity::of(&file)?,
                    Some(format!("{} {}", language.descriptor().markers.implementation_name,
                        language.descriptor().language_version.full_version())), Some(0))
            } else if let Some(value) = cached.get(&path) { value.clone() } else {
                let mut file = File::options().read(true).custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC).open(&path)?;
                let metadata = file.metadata()?;
                ensure!(metadata.is_file() && metadata.mode() & 0o111 != 0 && metadata.len() <= MAX_PROGRAM_BYTES, "native program is not a bounded executable");
                let identity = pakro_store::control::FileIdentity::of(&file)?;
                let mut hash = Sha256::new(); let mut bytes = 0u64; let mut buffer = vec![0; 64 * 1024];
                loop { context.checkpoint()?; let count = file.read(&mut buffer)?; if count == 0 { break; }
                    bytes += count as u64; ensure!(bytes <= MAX_PROGRAM_BYTES, "native program grew beyond its budget"); hash.update(&buffer[..count]); }
                let after = file.metadata()?;
                ensure!((metadata.dev(),metadata.ino(),metadata.len(),metadata.mtime(),metadata.mtime_nsec()) == (after.dev(),after.ino(),after.len(),after.mtime(),after.mtime_nsec()), "native program changed during discovery");
                let args = if cfg!(target_os = "macos") && (role == "linker" || role.ends_with(".ld")) { vec!["-v".into()] } else { vec!["--version".into()] };
                let output = probe(context, &path, &args, &probe_env).await?;
                let version = if output.status.success() {
                    let text = if output.stdout.is_empty() { &output.stderr } else { &output.stdout };
                    std::str::from_utf8(text)?.lines().find(|line| !line.trim().is_empty()).map(normalized_program_version)
                } else { None };
                let value = (hash.finalize().into(), identity, version, output.status.code()); cached.insert(path.clone(), value.clone()); value
            };
            if let Some(wanted) = expected.and_then(|record| record.programs.iter().find(|program| program.role == role)) {
                ensure!(wanted.sha256 == sha256, "locked native toolchain, SDK, driver or Cargo inputs changed");
            }
            let arguments = if role == "cxx" && invocation.ends_with("++") && path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("clang")) {
                vec!["--driver-mode=g++".into()]
            } else { Vec::new() };
            let record = NativeProgramRecord { role: role.clone(), sha256, version: version.unwrap_or_else(|| "unavailable".into()) };
            programs.insert(role, FrozenNativeProgram { path, record, invocation_name: invocation, identity, version_status: status, arguments });
        }
        let input_records = hash_inputs(context, &inputs)?;
        for (role, key) in [("python","PYTHON"),("cc","CC"),("cxx","CXX"),("ar","AR"),("linker","LD"),("cargo","CARGO"),("rustc","RUSTC"),("pkg_config","PKG_CONFIG"),("make","MAKE")] {
            if let Some(program) = programs.get(role) {
                let mut value = program.path.to_string_lossy().into_owned();
                if matches!(role, "cc" | "cxx") {
                    value = std::iter::once(value.as_str()).chain(program.arguments.iter().chain(&compiler_arguments).map(String::as_str))
                        .map(|word| if word.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"/_-.+=:".contains(&byte)) { word.to_owned() }
                            else { format!("'{}'", word.replace('\'', "'\\''")) }).collect::<Vec<_>>().join(" ");
                }
                environment.insert(key.into(), value); portable_environment.insert(key.into(), format!("@program:{role}"));
            }
        }
        let mut paths = Vec::new(); let mut seen = BTreeSet::new(); let mut portable_paths = Vec::new();
        for role in ["python", "cc", "cxx", "ar", "linker", "cmake", "ninja", "meson", "cargo", "rustc", "pkg_config", "make"]
            .into_iter().chain(programs.keys().map(String::as_str)) {
            if let Some(path) = programs.get(role).and_then(|program| program.path.parent())
                && seen.insert(path.to_owned()) {
                paths.push(path.to_owned()); portable_paths.push(format!("@program:{role}"));
            }
        }
        environment.insert("PATH".into(), std::env::join_paths(paths.iter())?.to_string_lossy().into_owned());
        portable_environment.insert("PATH".into(), portable_paths.join(":"));
        let mut read_roots = inputs.iter().map(|(_, path)| path.clone()).collect::<BTreeSet<_>>();
        if let Some(language) = language { read_roots.insert(language.root().to_owned()); }
        read_roots.extend(programs.values().filter_map(|program| program.path.parent().map(Path::to_owned)));
        let mut configuration = portable_environment.into_iter().map(|(name,value)| (format!("env.{name}"),value)).collect::<BTreeMap<_,_>>();
        if let Some(target) = compiler_target { configuration.insert("compiler-target".into(), target); }
        configuration.insert("compiler-arguments".into(), serde_json::to_string(&portable_arguments)?);
        let mut sdk_roots = BTreeMap::new();
        let input_usage = input_records.iter().map(|(role,input)| (role.clone(),(input.entries,input.bytes))).collect();
        for (role, input) in input_records {
            sdk_roots.insert(input.root_kind.clone(), input.content_sha256);
            configuration.insert(format!("input.{role}"), format!("{}:{}", input.root_kind, input.relative.display()));
        }
        for (role, program) in &programs {
            if matches!(role.as_str(), "cc" | "cxx") { configuration.insert(format!("program.{role}.invocation"), program.invocation_name.clone()); }
            if program.version_status != Some(0) { configuration.insert(format!("program.{role}.version-exit"), program.version_status.map_or_else(|| "signal".into(), |code| code.to_string())); }
            if !program.arguments.is_empty() { configuration.insert(format!("program.{role}.arguments"), serde_json::to_string(&program.arguments)?); }
        }
        let record = NativeToolchainRecord { schema: 1,
            platform: native_platform,
            programs: programs.values().map(|program| program.record.clone()).collect(),
            sdk_roots: sdk_roots.into_iter().map(|(kind,sha256)| NativeSdkRecord { kind, sha256 }).collect(), configuration };
        let fingerprint = record.fingerprint()?;
        let input_bindings = inputs.into_iter().collect();
        Ok(Self { programs, read_roots: read_roots.into_iter().collect(), environment, compiler_arguments, input_usage, input_bindings, record, fingerprint })
    }
}

fn normalized_program_version(line: &str) -> String {
    line.split_whitespace().map(|word| {
        let local_path = word.split(|ch| "\"'=,[]{}();".contains(ch)).any(|token| {
            let bytes = token.as_bytes();
            token.starts_with('/')
                || ["-I", "-L", "-isystem", "-isysroot"].iter().any(|flag| {
                    token.strip_prefix(flag).is_some_and(|rest| rest.starts_with('/'))
                })
                || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\'))
        });
        if local_path { "@path" } else { word }
    }).collect::<Vec<_>>().join(" ")
}

fn executable(path: &Path, search: &[PathBuf], cwd: &Path) -> Option<PathBuf> {
    let candidates = if path.components().count() == 1 && !path.is_absolute() { search.iter().map(|root| root.join(path)).collect::<Vec<_>>() }
        else { vec![if path.is_absolute() { path.to_owned() } else { cwd.join(path) }] };
    candidates.into_iter().find_map(|path| {
        let path = path.canonicalize().ok()?; let metadata = path.metadata().ok()?;
        (metadata.is_file() && metadata.mode() & 0o111 != 0).then_some(path)
    })
}

async fn probe(context: &OperationContext, program: &Path, arguments: &[String], environment: &BTreeMap<String, String>) -> Result<std::process::Output> {
    let remaining = context.policy().deadline.checked_duration_since(std::time::Instant::now()).context("native tool discovery deadline expired")?;
    let mut command = CmdLineRunner::new(program).args(arguments).env_clear().with_timeout(remaining);
    for (name, value) in environment { command = command.env(name, value); }
    tokio::select! {
        _ = context.cancellation_token().cancelled_owned() => bail!("native tool discovery cancelled"),
        result = command.output_isolated(64 * 1024) => result.context("installed native tool discovery failed"),
    }
}

fn hash_inputs(context: &OperationContext, inputs: &[(String, PathBuf)]) -> Result<BTreeMap<String, NativeInputRecord>> {
    let home = std::env::var_os("HOME").and_then(|path| Path::new(&path).canonicalize().ok());
    for (_, path) in inputs { ensure!(path != Path::new("/") && home.as_ref() != Some(path), "refusing to scan the filesystem or home as a native input"); }
    let covers = inputs.iter().enumerate().filter(|(index, (_, path))| !inputs.iter().enumerate().any(|(other, (_, parent))|
        other != *index && path.starts_with(parent) && (path != parent || other < *index))).map(|(index, _)| index).collect::<Vec<_>>();
    let mut observed = BTreeMap::<String, ([u8;32], usize, u64)>::new();
    let (mut total_entries, mut total_bytes, mut total_paths) = (0usize, 0u64, 0usize);
    for &index in &covers {
        let (role, root) = &inputs[index];
        let mut hash = Sha256::new_with_prefix(b"pakro.native-input-tree/v1\0");
        let mut pending = vec![PathBuf::new()]; let (mut entries, mut bytes) = (0usize, 0u64);
        while let Some(relative) = pending.pop() {
            context.checkpoint()?;
            total_entries += 1; entries += 1;
            total_paths = total_paths.checked_add(relative.as_os_str().as_encoded_bytes().len()).context("native input path count overflow")?;
            ensure!(total_entries <= MAX_ENTRIES && total_paths <= MAX_PATH_BYTES, "native SDK/search-root inventory exceeds its entry/path budget");
            let path = if relative.as_os_str().is_empty() { root.clone() } else { root.join(&relative) };
            let metadata = std::fs::symlink_metadata(&path)?;
            // A native library search directory such as /usr/lib is not an
            // SDK root: hash its link inputs, not unrelated Python/JVM data
            // or configuration-directory symlinks such as /usr/lib/ssl.
            let complete = !role.contains(".library-") || inputs.iter().any(|(kind, input)|
                !kind.contains(".library-") && path.starts_with(input));
            if !complete && !metadata.is_dir() {
                let name = path.file_name().context("native input has no basename")?.to_string_lossy();
                let link_input = name.contains(".so") || [".a", ".o", ".dylib", ".tbd", ".ld", ".pc", ".la"].iter().any(|suffix| name.ends_with(suffix))
                    || relative.components().any(|part| part.as_os_str() == "ldscripts");
                if !link_input { continue; }
            }
            hash.update(relative.as_os_str().as_encoded_bytes()); hash.update([0]); hash.update((metadata.mode() & 0o777).to_le_bytes());
            if metadata.is_dir() {
                hash.update(b"directory\0");
                let mut children = Vec::new();
                for entry in std::fs::read_dir(&path)? {
                    context.checkpoint()?;
                    ensure!(children.len() + pending.len() < MAX_ENTRIES - total_entries, "native input directory exceeds its entry budget");
                    children.push(relative.join(entry?.file_name()));
                }
                children.sort(); pending.extend(children.into_iter().rev());
            } else if metadata.file_type().is_symlink() {
                hash.update(b"symlink\0");
                let target = path.canonicalize().context("native SDK has an unavailable symlink target")?;
                let covered = inputs.iter().find_map(|(role, root)| target.strip_prefix(root).ok().map(|relative| (role, relative)))
                    .context("native SDK symlink leaves the frozen input roots")?;
                hash.update(covered.0.as_bytes()); hash.update([0]); hash.update(covered.1.as_os_str().as_encoded_bytes()); hash.update([0]);
            } else if metadata.is_file() {
                hash.update(b"file\0"); hash.update(metadata.len().to_le_bytes());
                ensure!(metadata.len() <= MAX_BYTES.saturating_sub(total_bytes), "native SDK/search-root inventory exceeds its byte budget");
                let mut file = File::options().read(true).custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC).open(&path)?;
                let mut content = Sha256::new(); let mut size = 0u64; let mut buffer = [0; 64 * 1024];
                loop { context.checkpoint()?; let count = file.read(&mut buffer)?; if count == 0 { break; }
                    size += count as u64; total_bytes += count as u64; bytes += count as u64;
                    ensure!(total_bytes <= MAX_BYTES && size <= metadata.len(), "native SDK/search-root inventory exceeds its byte budget or changed"); content.update(&buffer[..count]); }
                let after = file.metadata()?;
                ensure!(size == metadata.len() && (metadata.dev(),metadata.ino(),metadata.len(),metadata.mtime(),metadata.mtime_nsec())
                    == (after.dev(),after.ino(),after.len(),after.mtime(),after.mtime_nsec()), "native input file changed during hashing"); hash.update(content.finalize());
            } else { bail!("native input contains a special file: {}", relative.display()); }
        }
        observed.insert(role.clone(), (hash.finalize().into(), entries, bytes));
    }
    let mut records = BTreeMap::new();
    for (role, path) in inputs {
        let index = *covers.iter().find(|index| path.starts_with(&inputs[**index].1)).context("native input has no inventory root")?;
        let (parent, root) = &inputs[index]; let (digest, entries, bytes) = observed[parent];
        records.insert(role.clone(), NativeInputRecord { content_sha256: digest, relative: path.strip_prefix(root)?.to_owned(), root_kind: parent.clone(), entries, bytes });
    }
    Ok(records)
}

#[cfg(test)]
#[path = "tests/toolchain_aliases.rs"]
mod alias_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::language::repository::tests::operation_context;
    use std::time::Duration;

    #[test]
    fn p6_native_program_versions_normalize_punctuated_absolute_paths() {
        let raw = "LLD 22.1.6 (/checkout/src/llvm-project/llvm 08c84e69a84d95936296dfcab0e38b34100725d5) (compatible with GNU linkers)";
        let mut record = NativeToolchainRecord::test_record();
        record.programs[0].version = raw.into();
        assert!(record.validate().is_err(), "the validator must still refuse the actual raw LLD build path");
        let normalized = normalized_program_version(raw);
        assert_eq!(normalized, "LLD 22.1.6 @path 08c84e69a84d95936296dfcab0e38b34100725d5) (compatible with GNU linkers)");
        record.programs[0].version = normalized;
        record.validate().unwrap();
        for word in ["/sdk/bin", "(-I/sdk/include)", "'C:\\sdk\\bin'"] {
            assert_eq!(normalized_program_version(&format!("tool {word} 1.0")), "tool @path 1.0");
        }
        for version in ["cargo 1.97.0 (c980f4866 2026-06-30)", "LLD 22.1.6 (https://github.com/llvm/llvm-project.git)"] {
            assert_eq!(normalized_program_version(version), version);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn p6_apple_linker_runtime_inputs_are_exact_files_and_content_bound() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let context = operation_context(&root, false, Duration::from_secs(30));
        let bin = root.join("usr/bin"); let library = root.join("usr/lib");
        std::fs::create_dir_all(&bin).unwrap(); std::fs::create_dir(&library).unwrap();
        let linker = bin.join("ld"); std::fs::write(&linker, b"selected linker").unwrap();
        for name in ["libtapi.dylib", "libcodedirectory.dylib", "libLTO.dylib", "libswiftDemangle.dylib"] {
            std::fs::write(library.join(name), name.as_bytes()).unwrap();
        }
        let unrelated = library.join("unrelated.dylib"); std::fs::write(&unrelated, b"unrelated").unwrap();
        let selected = ["libtapi.dylib", "libcodedirectory.dylib", "libLTO.dylib", "libswiftDemangle.dylib"].into_iter()
            .map(|name| (format!("linker.runtime-{name}"), library.join(name).canonicalize().unwrap())).collect::<Vec<_>>();
        assert_eq!(selected.len(), 4);
        assert!(selected.iter().all(|(_, path)| path.is_file() && *path != unrelated));
        let first = hash_inputs(&context, &selected).unwrap();
        std::fs::write(&unrelated, b"changed unrelated file").unwrap();
        let same = hash_inputs(&context, &selected).unwrap();
        for (role, record) in &first { assert_eq!(record.content_sha256, same[role].content_sha256); }
        std::fs::write(library.join("libtapi.dylib"), b"changed real dependency").unwrap();
        let changed = hash_inputs(&context, &selected).unwrap();
        assert_ne!(first["linker.runtime-libtapi.dylib"].content_sha256, changed["linker.runtime-libtapi.dylib"].content_sha256);
    }

    #[test]
    fn p6_native_sdk_fingerprint_survives_relocation_and_tracks_actual_content() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let context = operation_context(&root, false, Duration::from_secs(30));
        let a = root.join("first-sdk"); let b = root.join("relocated-sdk");
        for sdk in [&a, &b] {
            std::fs::create_dir(sdk).unwrap();
            std::fs::write(sdk.join("input.h"), b"#define VALUE 41\n").unwrap();
            std::os::unix::fs::symlink(sdk.join("input.h"), sdk.join("absolute-link.h")).unwrap();
        }
        let first = hash_inputs(&context, &[("sdk".into(), a)]).unwrap();
        let moved = hash_inputs(&context, &[("sdk".into(), b.clone())]).unwrap();
        assert_eq!(first["sdk"].content_sha256, moved["sdk"].content_sha256);
        std::fs::write(b.join("input.h"), b"#define VALUE 42\n").unwrap();
        let changed = hash_inputs(&context, &[("sdk".into(), b)]).unwrap();
        assert_ne!(first["sdk"].content_sha256, changed["sdk"].content_sha256);
    }

    #[test]
    fn p6_native_library_inventory_covers_link_inputs_and_nested_headers() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let context = operation_context(&root, false, Duration::from_secs(30));
        let library = root.join("lib"); let include = library.join("clang/include");
        std::fs::create_dir_all(&include).unwrap();
        std::fs::write(library.join("libfixture.so"), b"fixed library").unwrap();
        std::fs::write(library.join("unrelated.py"), b"unrelated data").unwrap();
        std::fs::write(include.join("stddef.h"), b"fixed header").unwrap();
        let inputs = [("cc.library-0".into(), library.clone()), ("cc.include-0".into(), include.clone())];
        let first = hash_inputs(&context, &inputs).unwrap();
        assert_eq!(first["cc.library-0"].content_sha256, first["cc.include-0"].content_sha256);
        std::fs::write(library.join("unrelated.py"), b"changed unrelated data").unwrap();
        let same = hash_inputs(&context, &inputs).unwrap();
        assert_eq!(first["cc.library-0"].content_sha256, same["cc.library-0"].content_sha256);
        std::fs::write(include.join("stddef.h"), b"changed header").unwrap();
        let changed = hash_inputs(&context, &inputs).unwrap();
        assert_ne!(first["cc.library-0"].content_sha256, changed["cc.library-0"].content_sha256);
    }

    #[test]
    fn p6_native_gcc_lto_plugin_is_frozen_as_an_exact_file_input() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let context = operation_context(&root, false, Duration::from_secs(30));
        let library = root.join("lib"); let helpers = root.join("libexec");
        std::fs::create_dir(&library).unwrap(); std::fs::create_dir(&helpers).unwrap();
        let plugin = helpers.join("liblto_plugin.so");
        std::fs::write(&plugin, b"fixed compiler plugin").unwrap();
        std::fs::write(helpers.join("unrelated-tool"), b"not a frozen input").unwrap();
        std::os::unix::fs::symlink(&plugin, library.join("liblto_plugin.so")).unwrap();
        let error = hash_inputs(&context, &[("cc.library-0".into(), library.clone())]).unwrap_err();
        assert!(error.chain().any(|cause| cause.to_string() == "native SDK symlink leaves the frozen input roots"));
        let inputs = [("cc.library-0".into(), library), ("cc.lto-plugin".into(), plugin.clone())];
        let first = hash_inputs(&context, &inputs).unwrap();
        std::fs::write(helpers.join("unrelated-tool"), b"changed unrelated tool").unwrap();
        let same = hash_inputs(&context, &inputs).unwrap();
        assert_eq!(first["cc.lto-plugin"].content_sha256, same["cc.lto-plugin"].content_sha256);
        std::fs::write(&plugin, b"changed compiler plugin").unwrap();
        let changed = hash_inputs(&context, &inputs).unwrap();
        assert_ne!(first["cc.lto-plugin"].content_sha256, changed["cc.lto-plugin"].content_sha256);
    }
}
