use super::*;
use crate::artifact_snapshot::spool_verified_reader;
use crate::config::Settings;
use crate::lockfile::PlatformInfo;
use crate::plugins::core::python::native::{RuntimeArchiveTarget, RuntimeFlavor, RuntimePlatform};
use crate::programs::install::runtime::prepare_language;
use crate::programs::{
    AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
    capture_operation_context,
};
use flate2::{Compression, write::GzEncoder};
use pakro_store::{Digest, control::lease::LeaseMode};
use serde_json::json;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const FILENAME: &str = "cpython-3.15.0+20261013-aarch64-apple-darwin-install_only.tar.gz";
const HELPER: &str = "libraries::language::repository::tests::r06_process_helper";

struct Fixture {
    _owner: tempfile::TempDir,
    path: PathBuf,
    bytes: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let owner = tempfile::tempdir().unwrap();
        let path = owner.path().canonicalize().unwrap();
        let bytes = archive("3.15.0");
        fs::write(path.join("archive.gz"), &bytes).unwrap();
        Self {
            _owner: owner,
            path,
            bytes,
        }
    }
    fn context(&self) -> OperationContext {
        operation_context(&self.path, true, Duration::from_secs(20))
    }
    fn spec(&self) -> LanguageInstallSpec {
        install_spec(&self.bytes)
    }
}

pub(crate) fn operation_context(
    path: &Path,
    writable: bool,
    duration: Duration,
) -> OperationContext {
    capture_operation_context(OperationContextInputs {
        settings: &Settings::default(),
        policy: OperationPolicy {
            locked: false,
            offline: true,
            deadline: Instant::now() + duration,
            budget: RequestBudget {
                max_arguments: 8,
                max_total_argument_bytes: 4096,
                max_declarations: 16,
                max_runtime_candidates: 8,
            },
            writes: AllowedWrites {
                libraries: writable,
                project_selection: false,
                lockfile: false,
                declarations: false,
            },
        },
        cancellation: CancellationToken::new(),
        data_root: path.join("data"),
        cache_root: path.join("cache"),
        python_sources: None,
        python_target_description: None,
    })
    .unwrap()
}
pub(crate) fn archive_triple(target: &RuntimeArchiveTarget) -> String {
    match target.platform() {
        RuntimePlatform::Macos => format!("{}-apple-darwin", target.arch()),
        RuntimePlatform::LinuxGnu => format!("{}-unknown-linux-gnu", target.arch()),
        RuntimePlatform::LinuxMusl => format!("{}-unknown-linux-musl", target.arch()),
    }
}
pub(crate) fn install_spec_for(
    version: &str,
    bytes: &[u8],
    target: &RuntimeArchiveTarget,
    build: &str,
) -> LanguageInstallSpec {
    let filename = format!(
        "cpython-{version}+{build}-{}-install_only.tar.gz",
        archive_triple(target)
    );
    let plan = RuntimeInstallPlan::from_locked(version, target, RuntimeFlavor::InstallOnly, &PlatformInfo {
        url: Some(format!("https://github.com/astral-sh/python-build-standalone/releases/download/{build}/{filename}")),
        checksum: Some(Digest::from_sha256_bytes(bytes).to_string().replacen('-', ":", 1)),
        ..Default::default()
    }).unwrap();
    LanguageInstallSpec::from_plan(&plan, false).unwrap()
}
pub(crate) fn install_spec(bytes: &[u8]) -> LanguageInstallSpec {
    install_spec_for(
        "3.15.0",
        bytes,
        &RuntimeArchiveTarget::new("aarch64", RuntimePlatform::Macos).unwrap(),
        "20261013",
    )
}
pub(crate) fn archive(version: &str) -> Vec<u8> {
    archive_for(
        version,
        &RuntimeArchiveTarget::new("aarch64", RuntimePlatform::Macos).unwrap(),
        "",
    )
}

fn linux_protocol_fixture(target: &RuntimeArchiveTarget) -> (Vec<u8>, serde_json::Value) {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = PathBuf::from(
        std::env::var_os("PAKRO_TEST_LINUX_PROTOCOL_FIXTURE")
            .expect("native Linux protocol fixture is required"),
    );
    assert!(path.is_absolute());
    let expected = std::env::var("PAKRO_TEST_LINUX_PROTOCOL_SHA256")
        .expect("explicit Linux protocol fixture SHA256 is required");
    assert!(
        expected.len() == 64
            && expected
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
        .open(path)
        .unwrap();
    let before = file.metadata().unwrap();
    assert!(
        before.is_file()
            && before.uid() == nix::unistd::geteuid().as_raw()
            && before.nlink() == 1
            && before.mode() & 0o022 == 0
            && before.mode() & 0o111 != 0
            && before.len() >= 64
            && before.len() <= 1024 * 1024
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    let after = file.metadata().unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec()
        ),
        (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec()
        )
    );
    assert_eq!(bytes.len() as u64, before.len());
    assert_eq!(
        Digest::from_sha256_bytes(&bytes).to_string(),
        format!("sha256-{expected}")
    );
    assert_eq!(&bytes[..6], b"\x7fELF\x02\x01");
    let u16_at = |at: usize| u16::from_le_bytes(bytes.get(at..at + 2).unwrap().try_into().unwrap());
    let u32_at = |at: usize| u32::from_le_bytes(bytes.get(at..at + 4).unwrap().try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(bytes.get(at..at + 8).unwrap().try_into().unwrap());
    assert!(matches!(u16_at(16), 2 | 3));
    let machine = match target.arch() {
        "aarch64" => 183,
        "x86_64" => 62,
        other => panic!("unsupported protocol fixture ABI {other}"),
    };
    assert_eq!(u16_at(18), machine);
    let offset = usize::try_from(u64_at(32)).unwrap();
    assert_eq!(u16_at(54), 56);
    let count = usize::from(u16_at(56));
    assert!(count > 0 && count <= 128);
    let mut interpreter = None;
    for index in 0..count {
        let at = offset.checked_add(index * 56).unwrap();
        assert!(at.checked_add(56).unwrap() <= bytes.len());
        if u32_at(at) != 3 {
            continue;
        }
        assert!(interpreter.is_none(), "multiple PT_INTERP entries");
        let start = usize::try_from(u64_at(at + 8)).unwrap();
        let size = usize::try_from(u64_at(at + 32)).unwrap();
        assert!(size > 1 && size <= 4096);
        let raw = bytes.get(start..start.checked_add(size).unwrap()).unwrap();
        assert_eq!(raw.last(), Some(&0));
        assert!(!raw[..size - 1].contains(&0));
        let loader = std::str::from_utf8(&raw[..size - 1]).unwrap();
        assert!(Path::new(loader).is_absolute());
        interpreter = Some(loader.to_owned());
    }
    let loader = interpreter.expect("native protocol fixture must have a real PT_INTERP");
    let libc = match target.platform() {
        RuntimePlatform::LinuxMusl => {
            assert_eq!(
                Path::new(&loader).file_name().unwrap(),
                format!("ld-musl-{}.so.1", target.arch()).as_str()
            );
            // The unchanged production probe fills the actual version by
            // independently reading this ELF and executing its real loader.
            json!({"family":"musl","version":null,"loader":loader})
        }
        RuntimePlatform::LinuxGnu => {
            #[cfg(all(target_os = "linux", target_env = "gnu"))]
            {
                assert!(loader.contains("ld-linux"));
                let mut raw = [0u8; 128];
                let used = unsafe {
                    nix::libc::confstr(
                        nix::libc::_CS_GNU_LIBC_VERSION,
                        raw.as_mut_ptr().cast(),
                        raw.len(),
                    )
                };
                assert!(used > 0 && used <= raw.len());
                let value = std::ffi::CStr::from_bytes_with_nul(&raw[..used])
                    .unwrap()
                    .to_str()
                    .unwrap();
                let (major, minor) = value
                    .strip_prefix("glibc ")
                    .unwrap()
                    .split_once('.')
                    .unwrap();
                json!({"family":"glibc","major":major.parse::<u32>().unwrap(),"minor":minor.parse::<u32>().unwrap()})
            }
            #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
            {
                panic!("a GNU protocol fixture requires a native GNU test host")
            }
        }
        RuntimePlatform::Macos => unreachable!(),
    };
    let platform = json!({"os":"linux","elf":{"class":2,"little_endian":true,
        "machine":machine,"flags":u32_at(48),"interpreter":loader},"libc":libc,
        "manylinux":{"state":if target.platform() == RuntimePlatform::LinuxMusl {"unknown"} else {"default"}}});
    println!("P6_SYNTHETIC_PROTOCOL_SHA256={expected} platform={platform}");
    (bytes, platform)
}
/// Offline fixture archive for one exact version and target. `variant` adds a
/// distinct member so two instances of one version can hold different digests;
/// the descriptor fields track the target the way a real probe would report it.
pub(crate) fn archive_for(version: &str, target: &RuntimeArchiveTarget, variant: &str) -> Vec<u8> {
    let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap()).collect();
    let tuple = json!({"major":parts[0],"minor":parts[1],"micro":parts[2],"release_level":"final","serial":0});
    let short = format!("{}.{}", parts[0], parts[1]);
    let (system, sys_platform, abi) = match target.platform() {
        RuntimePlatform::Macos => ("Darwin", "darwin", "darwin"),
        RuntimePlatform::LinuxGnu | RuntimePlatform::LinuxMusl => ("Linux", "linux", "linux"),
    };
    let machine = match (target.arch(), target.platform()) {
        ("aarch64", RuntimePlatform::Macos) => "arm64",
        (arch, _) => arch,
    };
    let soabi = format!("cpython-{}{}-{abi}", parts[0], parts[1]);
    let ext_suffix = format!(".{soabi}.so");
    let compatibility_os = match target.platform() {
        RuntimePlatform::Macos => "macos",
        RuntimePlatform::LinuxGnu | RuntimePlatform::LinuxMusl => "linux",
    };
    let mut descriptor = json!({
        "schema":1,"observation_mode":"base_runtime_site_disabled","probe_flags":["-E","-s","-S","-B","-c"],
        "executable":"@ROOT@/bin/python","implementation":"cpython","language_version":tuple,"implementation_version":tuple,
        "implementation_version_source":"sys_implementation_version",
        "markers":{"implementation_name":"cpython","implementation_version":version,"os_name":"posix","platform_machine":machine,"platform_python_implementation":"CPython","platform_release":"fixture kernel","platform_system":system,"platform_version":"fixture kernel","python_full_version":version,"python_version":short,"sys_platform":sys_platform},
        "soabi":soabi,"ext_suffix":ext_suffix,"abiflags":"","extension_suffixes":[ext_suffix,".abi3.so",".so"],"gil_disabled":false,
        "compatibility":{"pointer_bits":64,"debug":false,"pymalloc":true,"unicode_size":null,"platform":{"os":compatibility_os,"cpu_type":16777228,"product_version":{"major":14,"minor":5,"micro":0}}},
        "sysconfig_paths":{"stdlib":format!("@ROOT@/lib/python{short}")}
    });
    // Keep the original macOS archive bytes unchanged. Linux must be a real
    // native ELF so production musl PT_INTERP/loader checks remain in force.
    let (payload, protocol) = if target.platform() == RuntimePlatform::Macos {
        let script = format!(
            "#!/bin/sh\n[ \"$#\" = 6 ] && [ \"$1\" = -E ] && [ \"$2\" = -s ] && [ \"$3\" = -S ] && [ \"$4\" = -B ] && [ \"$5\" = -c ] || exit 93\nroot=$(CDPATH= cd -P -- \"$(/usr/bin/dirname -- \"$0\")/..\" && /bin/pwd)\nprintf '%s\\n' '{}' | /usr/bin/sed \"s|@ROOT@|$root|g\"\n",
            descriptor.to_string().replace('\'', "'\"'\"'")
        );
        (script.into_bytes(), None)
    } else {
        let (bytes, platform) = linux_protocol_fixture(target);
        descriptor["compatibility"]["platform"] = platform;
        let libc = if target.platform() == RuntimePlatform::LinuxMusl {
            "musl"
        } else {
            "gnu"
        };
        let soabi = format!(
            "cpython-{}{}-{}-linux-{libc}",
            parts[0],
            parts[1],
            target.arch()
        );
        let suffix = format!(".{soabi}.so");
        descriptor["soabi"] = json!(soabi);
        descriptor["ext_suffix"] = json!(suffix);
        descriptor["extension_suffixes"] = json!([suffix, ".abi3.so", ".so"]);
        (bytes, Some(serde_json::to_vec(&descriptor).unwrap()))
    };
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let stdlib_member = format!("bundle/lib/python{short}/stdlib-marker");
    let mut members: Vec<(String, u32, std::borrow::Cow<'_, [u8]>)> = vec![
        ("bundle/bin/python".to_string(), 0o755, payload.into()),
        (
            stdlib_member,
            0o644,
            b"fixture standard library marker".as_slice().into(),
        ),
    ];
    if let Some(protocol) = protocol {
        members.push(("bundle/.pakro-probe.json".into(), 0o644, protocol.into()));
    }
    if !variant.is_empty() {
        members.push((
            // Deterministic per-variant bytes distinguish same-version archives.
            format!("bundle/lib/python{short}/variant-marker"),
            0o644,
            format!("fixture variant {variant}").into_bytes().into(),
        ));
    }
    for (name, mode, bytes) in members {
        let mut header = tar::Header::new_gnu();
        header.set_mode(mode);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes.as_ref()).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}
pub(crate) async fn prepare(
    spec: &LanguageInstallSpec,
    context: &OperationContext,
    bytes: &[u8],
) -> Result<LanguageLease> {
    prepare_language(spec, context, || async {
        spool_verified_reader(
            bytes,
            spec.plan().source().filename(),
            spec.plan().source().digest(),
            1024 * 1024,
            context.requested_cache_root(),
        )
    })
    .await
}
fn payload_facts(lease: &LanguageLease) -> serde_json::Value {
    let root = fs::metadata(lease.root()).unwrap();
    let stdlib = fs::metadata(lease.root().join("lib/python3.15/stdlib-marker")).unwrap();
    json!({"root":lease.root(),"root_identity":[root.dev(),root.ino()],"stdlib_identity":[stdlib.dev(),stdlib.ino()]})
}
fn child(path: &Path, mode: &str, result: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", HELPER, "--nocapture"])
        .env("PAKRO_R06_FIXTURE", path)
        .env("PAKRO_R06_MODE", mode)
        .env("PAKRO_R06_RESULT", result)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}
#[derive(Default)]
struct TestChildren(Vec<std::process::Child>);
impl TestChildren {
    fn finish(mut self) {
        // Reap every process before inspecting any failure. An early assertion
        // must never abandon a peer still using this fixture's store.
        let outputs: Vec<_> = std::mem::take(&mut self.0)
            .into_iter()
            .map(|child| child.wait_with_output())
            .collect();
        let failures: Vec<_> = outputs
            .into_iter()
            .filter_map(|output| match output {
                Ok(output) if output.status.success() => None,
                Ok(output) => Some(format!(
                    "{}: {} {}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )),
                Err(error) => Some(error.to_string()),
            })
            .collect();
        assert!(
            failures.is_empty(),
            "child failures: {}",
            failures.join("\n")
        );
    }
    fn wait_marker(&mut self, path: &Path, deadline: Instant) {
        while !path.is_file() {
            assert!(
                Instant::now() < deadline,
                "process handshake exceeded its original deadline: {}",
                path.display()
            );
            for child in &mut self.0 {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "child exited before the process handshake: {}",
                    path.display()
                );
            }
            std::thread::sleep(
                Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}
impl Drop for TestChildren {
    fn drop(&mut self) {
        for child in &mut self.0 {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}
fn finish(child: std::process::Child) {
    TestChildren(vec![child]).finish();
}

#[test]
fn r06_process_helper() {
    let Some(path) = std::env::var_os("PAKRO_R06_FIXTURE") else {
        return;
    };
    let path = PathBuf::from(path);
    let bytes = fs::read(path.join("archive.gz")).unwrap();
    let spec = install_spec(&bytes);
    let context = operation_context(&path, true, Duration::from_secs(15));
    let lease = match std::env::var("PAKRO_R06_MODE").unwrap().as_str() {
        "lookup" => LanguageRepository::open_readonly(&context)
            .unwrap()
            .acquire_existing(&spec)
            .unwrap()
            .unwrap(),
        "prepare" => tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(prepare_language(&spec, &context, || async {
                fs::write(
                    path.join(format!("acquired-{}", std::process::id())),
                    b"one publisher",
                )
                .unwrap();
                spool_verified_reader(
                    bytes.as_slice(),
                    FILENAME,
                    spec.plan().source().digest(),
                    1024 * 1024,
                    context.requested_cache_root(),
                )
            }))
            .unwrap(),
        other => panic!("unknown child mode {other}"),
    };
    fs::write(
        path.join(std::env::var("PAKRO_R06_RESULT").unwrap()),
        serde_json::to_vec(&payload_facts(&lease)).unwrap(),
    )
    .unwrap();
}

#[test]
fn r06_readonly_missing_library_creates_nothing() {
    let f = Fixture::new();
    let context = f.context();
    let repository = LanguageRepository::open_readonly(&context).unwrap();
    assert!(matches!(
        repository.lookup(&f.spec()).unwrap(),
        LanguageState::Missing
    ));
    assert!(!context.requested_data_root().exists());
    assert!(!context.requested_cache_root().exists());
}

#[tokio::test]
async fn r06_protocol_two_projects_and_restart_reuse_one_payload() {
    let f = Fixture::new();
    let spec = f.spec();
    let a = prepare(&spec, &f.context(), &f.bytes).await.unwrap();
    let b = prepare_language(&spec, &f.context(), || async {
        Err(eyre::eyre!("project B unexpectedly acquired archive bytes"))
    })
    .await
    .unwrap();
    assert_eq!(payload_facts(&a), payload_facts(&b));
    finish(child(&f.path, "lookup", "restart.json"));
    assert_eq!(
        payload_facts(&a),
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(f.path.join("restart.json")).unwrap()
        )
        .unwrap()
    );
    let instances = fs::read_dir(f.path.join("data/languages"))
        .unwrap()
        .filter_map(|entry| {
            let e = entry.unwrap();
            e.file_name()
                .to_string_lossy()
                .starts_with("language-")
                .then_some(e)
        })
        .count();
    assert_eq!(instances, 1);
}

#[test]
fn r06_protocol_concurrent_first_install_publishes_one_instance() {
    let f = Fixture::new();
    let mut children = TestChildren::default();
    children.0.push(child(&f.path, "prepare", "a.json"));
    children.0.push(child(&f.path, "prepare", "b.json"));
    children.finish();
    assert_eq!(
        fs::read(f.path.join("a.json")).unwrap(),
        fs::read(f.path.join("b.json")).unwrap()
    );
    assert_eq!(
        fs::read_dir(&f.path)
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("acquired-"))
            .count(),
        1
    );
}

#[tokio::test]
async fn r06_bad_archive_never_becomes_ready() {
    let f = Fixture::new();
    let context = f.context();
    let spec = f.spec();
    let error = prepare(&spec, &context, b"wrong bytes")
        .await
        .err()
        .expect("wrong digest must fail");
    assert!(
        format!("{error:#}").contains("digest"),
        "expected actual digest verification: {error:#}"
    );
    assert!(
        LanguageRepository::open_readonly(&context)
            .unwrap()
            .acquire_existing(&spec)
            .unwrap()
            .is_none()
    );
    let second = Fixture::new();
    let bytes = archive("3.15.1");
    let spec = install_spec(&bytes);
    let error = prepare(&spec, &second.context(), &bytes)
        .await
        .err()
        .expect("wrong observed version must fail");
    assert!(
        format!("{error:#}").contains("observed runtime"),
        "expected actual probe identity rejection: {error:#}"
    );
    assert!(
        LanguageRepository::open_readonly(&second.context())
            .unwrap()
            .acquire_existing(&spec)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn r06_changed_payload_is_not_ready() {
    let f = Fixture::new();
    let spec = f.spec();
    let context = f.context();
    let lease = prepare(&spec, &context, &f.bytes).await.unwrap();
    fs::write(
        lease.root().join("lib/python3.15/stdlib-marker"),
        b"changed",
    )
    .unwrap();
    assert!(
        LanguageRepository::open_readonly(&context)
            .unwrap()
            .acquire_existing(&spec)
            .is_err()
    );
    assert!(lease.check_binding().is_err());
}

#[tokio::test]
async fn r06_missing_registered_lock_is_never_recreated() {
    let f = Fixture::new();
    let spec = f.spec();
    let context = f.context();
    let lease = prepare(&spec, &context, &f.bytes).await.unwrap();
    let lock = f
        .path
        .join("data/languages/.leases/build")
        .join(format!("{}.lock", spec.key().filename()));
    fs::remove_file(&lock).unwrap();
    assert!(
        LanguageRepository::open_writer(&context)
            .unwrap()
            .reserve_install(&spec)
            .is_err()
    );
    assert!(!lock.exists());
    assert!(lease.check_binding().is_err());
}

#[tokio::test]
async fn r06_handle_clone_retains_the_actual_shared_lock() {
    let f = Fixture::new();
    let spec = f.spec();
    let context = f.context();
    let lease = prepare(&spec, &context, &f.bytes).await.unwrap();
    let handle = lease.handle().unwrap();
    let clone = handle.clone();
    let parent = pakro_store::control::open_trusted_store_with_deadline(
        &f.path.join("data/languages/.leases/build"),
        nix::unistd::geteuid().as_raw(),
        false,
        context.policy().deadline,
    )
    .unwrap();
    let name = format!("{}.lock", spec.key().filename());
    let file = pakro_store::control::open_existing_readonly_regular(
        &parent,
        name.as_str(),
        nix::unistd::geteuid().as_raw(),
    )
    .unwrap();
    let lock = StableLock::open_existing(
        &parent,
        &name,
        nix::unistd::geteuid().as_raw(),
        FileIdentity::of(&file).unwrap(),
    )
    .unwrap();
    drop(lease);
    drop(handle);
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, context.policy().deadline)
            .unwrap()
            .is_none()
    );
    drop(clone);
    assert!(
        lock.try_acquire(LeaseMode::Exclusive, context.policy().deadline)
            .unwrap()
            .is_some()
    );
}

#[test]
fn r06_owner_enforces_original_cancellation_and_deadline() {
    let f = Fixture::new();
    let context = f.context();
    let repo = LanguageRepository::open_writer(&context).unwrap();
    let LanguageReservation::Build(mut owner) = repo.reserve_install(&f.spec()).unwrap() else {
        panic!("new key must be owned")
    };
    context.cancellation_token().cancel();
    assert!(owner.create_dir(Path::new("after-cancel"), 0o755).is_err());
    assert!(!owner.final_path().join("after-cancel").exists());
    let second = Fixture::new();
    drop(LanguageRepository::open_writer(&second.context()).unwrap());
    let context = operation_context(&second.path, true, Duration::from_millis(200));
    let repo = LanguageRepository::open_writer(&context).unwrap();
    let LanguageReservation::Build(mut owner) = repo.reserve_install(&second.spec()).unwrap()
    else {
        panic!("new key must be owned")
    };
    let mut writer = owner
        .create_file_exclusive(Path::new("late"), 0o600)
        .unwrap();
    std::thread::sleep(
        context
            .policy()
            .deadline
            .saturating_duration_since(Instant::now())
            + Duration::from_millis(5),
    );
    assert!(writer.write_all(b"late bytes").is_err());
    drop(writer);
    assert_eq!(
        fs::metadata(owner.final_path().join("late")).unwrap().len(),
        0
    );
}

#[tokio::test]
async fn r06_readonly_policy_reuses_but_cannot_reserve_missing() {
    let f = Fixture::new();
    let spec = f.spec();
    let lease = prepare(&spec, &f.context(), &f.bytes).await.unwrap();
    let readonly = operation_context(&f.path, false, Duration::from_secs(10));
    assert!(LanguageRepository::open_writer(&readonly).is_err());
    let reused = LanguageRepository::open_readonly(&readonly)
        .unwrap()
        .acquire_existing(&spec)
        .unwrap()
        .unwrap();
    assert_eq!(lease.root(), reused.root());
}

fn reserve(f: &Fixture) -> LanguageBuildOwner {
    match LanguageRepository::open_writer(&f.context())
        .unwrap()
        .reserve_install(&f.spec())
        .unwrap()
    {
        LanguageReservation::Build(owner) => owner,
        LanguageReservation::Existing(_) => panic!("fixture key is unexpectedly Ready"),
    }
}
#[test]
fn r06_owner_inventory_rejects_unowned_insertions() {
    let f = Fixture::new();
    let mut owner = reserve(&f);
    owner.create_dir(Path::new("lib"), 0o755).unwrap();
    fs::write(
        owner.final_path().join("lib/unowned"),
        b"not from verified archive",
    )
    .unwrap();
    assert!(
        owner.capture_inventory().is_err(),
        "a directory listing must not adopt unknown bytes as extraction evidence"
    );
}
#[test]
fn r06_owner_inventory_rejects_changed_finished_bytes() {
    let f = Fixture::new();
    let mut owner = reserve(&f);
    let mut writer = owner
        .create_file_exclusive(Path::new("owned"), 0o600)
        .unwrap();
    writer.write_all(b"expected").unwrap();
    writer.finish().unwrap();
    fs::write(owner.final_path().join("owned"), b"modified").unwrap();
    assert!(
        owner.capture_inventory().is_err(),
        "same-inode changes after finish are not verified archive bytes"
    );
}
#[test]
fn r06_owner_implicit_parents_preserve_explicit_directory_modes() {
    let f = Fixture::new();
    let mut owner = reserve(&f);
    owner.create_dir(Path::new("private"), 0o700).unwrap();
    owner
        .create_file_exclusive(Path::new("private/data"), 0o600)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(
        fs::metadata(owner.final_path().join("private"))
            .unwrap()
            .mode()
            & 0o7777,
        0o700
    );
}
#[test]
fn r06_unfinished_writer_cannot_produce_an_extraction_inventory() {
    let f = Fixture::new();
    let mut owner = reserve(&f);
    let mut writer = owner
        .create_file_exclusive(Path::new("unfinished"), 0o600)
        .unwrap();
    writer.write_all(b"partial").unwrap();
    drop(writer);
    assert!(owner.capture_inventory().is_err());
}
#[test]
fn r06_owned_sysconfig_replacement_preserves_inode_and_rejects_stale_bytes() {
    let f = Fixture::new();
    let mut owner = reserve(&f);
    let mut writer = owner
        .create_file_exclusive(Path::new("config.py"), 0o600)
        .unwrap();
    writer.write_all(b"before").unwrap();
    writer.finish().unwrap();
    let before = fs::metadata(owner.final_path().join("config.py")).unwrap();
    assert!(
        owner
            .replace_file(Path::new("config.py"), b"stale", b"oops")
            .is_err()
    );
    assert_eq!(
        owner.read_file(Path::new("config.py"), 128).unwrap(),
        b"before"
    );
    owner
        .replace_file(Path::new("config.py"), b"before", b"after")
        .unwrap();
    let after = fs::metadata(owner.final_path().join("config.py")).unwrap();
    assert_eq!(
        (before.dev(), before.ino(), before.mode()),
        (after.dev(), after.ino(), after.mode())
    );
    assert_eq!(
        owner.read_file(Path::new("config.py"), 128).unwrap(),
        b"after"
    );
    owner.capture_inventory().unwrap();
}

#[test]
fn r06_failed_unpublished_owner_cleans_only_its_entries_and_can_retry() {
    let f = Fixture::new();
    let path;
    {
        let mut owner = reserve(&f);
        path = owner.final_path().to_owned();
        let mut file = owner
            .create_file_exclusive(Path::new("temporary"), 0o600)
            .unwrap();
        file.write_all(b"owned incomplete installation").unwrap();
        file.finish().unwrap();
    }
    assert!(
        !path.exists(),
        "failed installation retained its verified, privately owned staging tree"
    );
    let _retry = reserve(&f);
}
#[test]
fn r06_failed_owner_preserves_unknown_or_changed_evidence() {
    let f = Fixture::new();
    let owner = reserve(&f);
    let path = owner.final_path().to_owned();
    fs::write(path.join("unknown"), b"not owned").unwrap();
    drop(owner);
    assert_eq!(fs::read(path.join("unknown")).unwrap(), b"not owned");
}
#[test]
fn r06_writer_rejects_a_parent_moved_outside_its_payload() {
    for finish_only in [false, true] {
        let f = Fixture::new();
        let mut owner = reserve(&f);
        let root = owner.final_path().to_owned();
        let mut writer = owner
            .create_file_exclusive(Path::new("bin/python"), 0o600)
            .unwrap();
        writer.write_all(b"initial").unwrap();
        fs::rename(root.join("bin"), f.path.join("moved-bin")).unwrap();
        if finish_only {
            assert!(
                writer.finish().is_err(),
                "finish trusted a parent moved outside the payload"
            );
        } else {
            assert!(
                writer.write_all(b"outside write").is_err(),
                "write followed a held parent outside the payload"
            );
            drop(writer);
        }
        assert_eq!(
            fs::read(f.path.join("moved-bin/python")).unwrap(),
            b"initial"
        );
    }
}
#[test]
fn r06_owner_total_path_budget_is_enforced_before_creation() {
    let f = Fixture::new();
    let mut owner = reserve(&f);
    owner.limits.max_total_path_bytes = 3;
    let result = owner.create_file_exclusive(Path::new("longer"), 0o600);
    assert!(result.is_err(), "writer ignored the total path budget");
    drop(result);
    assert!(
        !owner.final_path().join("longer").exists(),
        "over-budget path was created before rejection"
    );
}
#[test]
fn r06_owned_patch_cannot_launder_changed_file_bytes() {
    let f = Fixture::new();
    let mut owner = reserve(&f);
    let mut file = owner
        .create_file_exclusive(Path::new("config.py"), 0o600)
        .unwrap();
    file.write_all(b"original").unwrap();
    file.finish().unwrap();
    fs::write(owner.final_path().join("config.py"), b"external").unwrap();
    assert!(
        owner.read_file(Path::new("config.py"), 128).is_err(),
        "read adopted same-inode bytes outside the extraction receipt"
    );
    assert!(
        owner
            .replace_file(Path::new("config.py"), b"external", b"patched!")
            .is_err()
    );
    assert_eq!(
        fs::read(owner.final_path().join("config.py")).unwrap(),
        b"external"
    );
}
#[tokio::test]
async fn r06_ready_manifest_cannot_rebind_standard_library_outside_payload() {
    let f = Fixture::new();
    let spec = f.spec();
    let context = f.context();
    let _lease = prepare(&spec, &context, &f.bytes).await.unwrap();
    let path = context
        .requested_data_root()
        .join("languages/.control/catalog")
        .join(manifest_name(&spec));
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["descriptor"]["sysconfig_paths"]["stdlib"] = json!(f.path.join("outside-stdlib"));
    fs::create_dir(f.path.join("outside-stdlib")).unwrap();
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(
        LanguageRepository::open_readonly(&context)
            .unwrap()
            .acquire_existing(&spec)
            .is_err()
    );
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn r06_manifest_device_identity_must_not_truncate_into_an_existing_device() {
    let f = Fixture::new();
    let spec = f.spec();
    let context = f.context();
    let _lease = prepare(&spec, &context, &f.bytes).await.unwrap();
    let path = context
        .requested_data_root()
        .join("languages/.control/catalog")
        .join(manifest_name(&spec));
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["allocation"]["device"] =
        json!(manifest["allocation"]["device"].as_u64().unwrap() + (1u64 << 32));
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(
        LanguageRepository::open_readonly(&context)
            .unwrap()
            .acquire_existing(&spec)
            .is_err()
    );
}
#[tokio::test]
async fn r06_published_allocation_keeps_private_parent_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let spec = f.spec();
    let context = f.context();
    let lease = prepare(&spec, &context, &f.bytes).await.unwrap();
    fs::set_permissions(
        lease.root().parent().unwrap(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(lease.check_binding().is_err());
    assert!(
        LanguageRepository::open_readonly(&context)
            .unwrap()
            .acquire_existing(&spec)
            .is_err()
    );
}

#[tokio::test]
async fn r06_failure_after_manifest_cas_preserves_the_published_payload() {
    let f = Fixture::new();
    let spec = f.spec();
    let context = f.context();
    let mut owner = reserve(&f);
    let mut artifact = spool_verified_reader(
        f.bytes.as_slice(),
        FILENAME,
        spec.plan().source().digest(),
        1024 * 1024,
        context.requested_cache_root(),
    )
    .unwrap();
    let receipt = crate::plugins::core::python::native::extract::install_into_owner(
        spec.plan(),
        &mut artifact,
        &mut owner,
        false,
    )
    .await
    .unwrap();
    let verified = super::super::publication::verify_language(owner, receipt)
        .await
        .unwrap();
    let result = super::super::publication::publish_language_with_after_cas(verified, || {
        Err(eyre::eyre!("injected failure after Ready CAS"))
    });
    assert!(format!("{:#}", result.err().expect("hook must fail")).contains("injected failure"));
    let lease = LanguageRepository::open_readonly(&context)
        .unwrap()
        .acquire_existing(&spec)
        .unwrap()
        .expect("Ready commit must retain its payload despite a later error");
    assert_eq!(
        fs::read(lease.root().join("lib/python3.15/stdlib-marker")).unwrap(),
        b"fixture standard library marker"
    );
}

include!("real_tests.rs");

#[tokio::test]
async fn r06_a_ready_payload_without_its_manifest_is_corrupt_not_missing() {
    let f = Fixture::new();
    let spec = f.spec();
    let context = f.context();
    let lease = prepare(&spec, &context, &f.bytes).await.unwrap();
    let manifest = context
        .requested_data_root()
        .join("languages/.control/catalog")
        .join(manifest_name(&spec));
    fs::remove_file(manifest).unwrap();
    assert!(lease.root().is_dir());
    assert!(
        LanguageRepository::open_readonly(&context)
            .unwrap()
            .lookup(&spec)
            .is_err(),
        "a lost Ready record was reported as a cache miss"
    );
}
#[test]
fn r06_a_cleaned_reservation_without_payload_remains_missing() {
    let f = Fixture::new();
    let spec = f.spec();
    drop(reserve(&f));
    assert!(matches!(
        LanguageRepository::open_readonly(&f.context())
            .unwrap()
            .lookup(&spec)
            .unwrap(),
        LanguageState::Missing
    ));
}

#[test]
fn r06_link_target_bytes_are_charged_before_creating_the_link() {
    let f = Fixture::new();
    let mut owner = reserve(&f);
    owner
        .create_file_exclusive(Path::new("f"), 0o600)
        .unwrap()
        .finish()
        .unwrap();
    owner.limits.max_total_path_bytes = 8;
    assert!(
        owner
            .create_symlink(Path::new("a"), Path::new("./././f"))
            .is_err(),
        "file and link names plus the valid target jointly exceed the path budget"
    );
    assert!(
        fs::symlink_metadata(owner.final_path().join("a")).is_err(),
        "an over-budget link was created before rejection"
    );
}

#[test]
fn r06_unregistered_protection_inode_is_not_a_missing_language() {
    let f = Fixture::new();
    let context = f.context();
    let spec = f.spec();
    let repository = LanguageRepository::open_writer(&context).unwrap();
    let writer = repository.writer.as_ref().unwrap();
    let name = format!("{}.lock", spec.key().filename());
    let lock = StableLock::create_new(writer.build_handle(), &name, uid()).unwrap();
    assert!(
        LanguageRepository::open_readonly(&context)
            .unwrap()
            .lookup(&spec)
            .is_err()
    );
    lock.check_binding().unwrap();
}

#[tokio::test]
async fn r06_equivalent_official_url_spellings_reuse_the_same_language_key() {
    for encoded_first in [true, false] {
        let fixture = Fixture::new();
        let literal = fixture.spec();
        let encoded_plan = RuntimeInstallPlan::from_locked(
            "3.15.0",
            literal.plan().target(),
            literal.plan().flavor(),
            &PlatformInfo {
                url: Some(literal.plan().source().url().as_str().replace('+', "%2B")),
                checksum: Some(
                    literal
                        .plan()
                        .source()
                        .digest()
                        .to_string()
                        .replacen('-', ":", 1),
                ),
                ..Default::default()
            },
        )
        .unwrap();
        let encoded = LanguageInstallSpec::from_plan(&encoded_plan, false).unwrap();
        assert_eq!(literal.key(), encoded.key());
        assert_ne!(literal.plan().source().url(), encoded.plan().source().url());
        let (first, second) = if encoded_first {
            (&encoded, &literal)
        } else {
            (&literal, &encoded)
        };
        let installed = prepare(first, &fixture.context(), &fixture.bytes)
            .await
            .unwrap();
        let reused = prepare_language(second, &fixture.context(), || async {
            Err(eyre::eyre!(
                "equivalent canonical source unexpectedly fetched another artifact"
            ))
        })
        .await
        .unwrap();
        assert_eq!(payload_facts(&installed), payload_facts(&reused));
    }
}
