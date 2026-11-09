use super::*;
use digest::Digest;
use nix::libc;
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            process::{CommandExt, ExitStatusExt},
        },
    },
    process::{Child, Command, Stdio},
    thread,
};

const MAX_PROGRAM: u64 = 512 * 1024 * 1024;

pub(super) fn monotonic_ms() -> Result<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } == 0,
        "monotonic clock unavailable"
    );
    Ok((value.tv_sec as u64).saturating_mul(1000) + value.tv_nsec as u64 / 1_000_000)
}

fn canonical(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "native build path must be absolute");
    Ok(path
        .canonicalize()
        .with_context(|| format!("native build input unavailable: {}", path.display()))?)
}

fn program(path: &Path) -> Result<NativeProgramIdentity> {
    let path = canonical(path)?;
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file() && metadata.mode() & 0o111 != 0 && metadata.mode() & 0o6000 == 0,
        "native program must be a non-privileged executable file"
    );
    ensure!(
        metadata.len() <= MAX_PROGRAM,
        "native program exceeds its byte budget"
    );
    // Capture the actual executable bytes once at the session boundary.
    let mut file = File::open(&path)?;
    let mut hash = sha2::Sha256::new();
    let mut buffer = [0u8; 32768];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    let after = file.metadata()?;
    ensure!(
        metadata.dev() == after.dev()
            && metadata.ino() == after.ino()
            && metadata.len() == after.len()
            && metadata.mtime() == after.mtime()
            && metadata.mtime_nsec() == after.mtime_nsec(),
        "native executable changed while captured"
    );
    Ok(NativeProgramIdentity {
        path,
        sha256: hash.finalize().into(),
    })
}

pub(super) fn prepare(config: &mut NativeBuildConfig) -> Result<Vec<NativeProgramIdentity>> {
    ensure!(
        cfg!(any(target_os = "macos", target_os = "linux")),
        "native build supervision unavailable"
    );
    let bootstrap = program(&config.bootstrap)?;
    config.bootstrap = bootstrap.path.clone();
    let mut identities = vec![bootstrap];
    config.workspace = canonical(&config.workspace)?;
    let metadata = fs::symlink_metadata(&config.workspace)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "native workspace must be an owned private directory"
    );
    ensure!(
        !config.fixed_programs.is_empty(),
        "native build needs fixed programs"
    );
    for path in &mut config.fixed_programs {
        *path = canonical(path)?;
    }
    config.fixed_programs.sort();
    config.fixed_programs.dedup();
    for path in &config.fixed_programs {
        if !identities.iter().any(|identity| identity.path == *path) {
            identities.push(program(path)?);
        }
    }
    for path in &mut config.generated_roots {
        *path = canonical(path)?;
        ensure!(
            path.starts_with(&config.workspace),
            "native generated root is outside its private workspace"
        );
    }
    for path in [
        &mut config.exec_pool,
        &mut config.requests,
        &mut config.responses,
    ] {
        ensure!(
            path.is_absolute() && path.starts_with(&config.workspace),
            "native mailbox/pool is outside its private workspace"
        );
        if !path.exists() {
            fs::create_dir_all(&*path)?;
        }
        *path = canonical(path)?;
        ensure!(
            path.starts_with(&config.workspace),
            "native mailbox/pool resolved outside its workspace"
        );
        fs::set_permissions(&*path, fs::Permissions::from_mode(0o700))?;
    }
    ensure!(
        config.requests != config.responses
            && config.requests != config.exec_pool
            && config.responses != config.exec_pool,
        "native mailbox and pool directories must differ"
    );
    for (a, b) in [
        (&config.requests, &config.responses),
        (&config.requests, &config.exec_pool),
        (&config.responses, &config.exec_pool),
    ] {
        ensure!(
            !a.starts_with(b) && !b.starts_with(a),
            "native mailbox/pool directories overlap"
        );
    }
    for writable in config
        .sandbox
        .allow_write
        .iter()
        .chain(std::iter::once(&config.requests))
    {
        let writable = canonical(writable)?;
        ensure!(
            !config.exec_pool.starts_with(&writable)
                && !writable.starts_with(&config.exec_pool)
                && !config.responses.starts_with(&writable)
                && !writable.starts_with(&config.responses),
            "native executable pool/responses must not be worker-writable"
        );
        ensure!(
            !config
                .fixed_programs
                .iter()
                .any(|program| program.starts_with(&writable)),
            "native fixed program is worker-writable"
        );
    }
    ensure!(
        config
            .generated_roots
            .iter()
            .all(|p| !config.exec_pool.starts_with(p) && !config.responses.starts_with(p)),
        "native generated roots include supervisor-owned outputs"
    );
    #[cfg(target_os = "linux")]
    {
        let unshare = program(
            config
                .unshare
                .as_deref()
                .ok_or_else(|| eyre::eyre!("util-linux unshare is required"))?,
        )?;
        config.unshare = Some(unshare.path.clone());
        identities.push(unshare);
    }
    config.sandbox.build_worker = true;
    config.sandbox.native_build = true;
    config.sandbox.deny_process = false;
    config.sandbox.native_exec_pool = Some(config.exec_pool.clone());
    config.sandbox.deny_read = true;
    config.sandbox.deny_write = true;
    config.sandbox.deny_net = true;
    config.sandbox.deny_env = true;
    config.sandbox.deny_temp_write = true;
    config.sandbox.allow_net.clear();
    config.sandbox.allow_process = config.fixed_programs.clone();
    #[cfg(target_os = "linux")]
    {
        config
            .sandbox
            .capture_runtime_loaders(&config.fixed_programs)?;
        for loader in &config.sandbox.runtime_loaders {
            if !identities.iter().any(|identity| identity.path == *loader) {
                identities.push(program(loader)?);
            }
        }
    }
    config
        .sandbox
        .allow_read
        .extend([config.exec_pool.clone(), config.responses.clone()]);
    config.sandbox.allow_write.push(config.requests.clone());
    config.sandbox.resolve_paths();
    config
        .sandbox
        .validate_build_worker(config.fixed_programs.first().map(PathBuf::as_path))?;
    // The token binds EOF cleanup to the actual private directory handed over.
    write_private(
        &config.responses.join("owner.json"),
        &serde_json::to_vec(&(metadata.dev(), metadata.ino(), metadata.uid()))?,
    )?;
    Ok(identities)
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= maximum,
        "native control input exceeds its budget"
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "native control input exceeds its budget"
    );
    Ok(bytes)
}

pub(super) fn dispatch(args: Vec<OsString>) -> Result<()> {
    ensure!(
        args.len() == 2 || args.len() == 3,
        "invalid native bootstrap arguments"
    );
    let config_path = PathBuf::from(&args[1]);
    let config: NativeBuildConfig =
        serde_json::from_slice(&bounded(&config_path, 4 * 1024 * 1024)?)?;
    match args[0].to_str() {
        Some("watcher") if args.len() == 2 => watcher(config, config_path),
        Some("exec") if args.len() == 3 => exec(config, Path::new(&args[2])),
        _ => eyre::bail!("invalid native bootstrap mode"),
    }
}

fn exec(config: NativeBuildConfig, run_path: &Path) -> Result<()> {
    let run: Run = serde_json::from_slice(&bounded(run_path, 1_048_576)?)?;
    #[cfg(target_os = "linux")]
    ensure!(
        unsafe { libc::getpid() } == 1,
        "native worker did not enter its owned PID namespace"
    );
    let mut command = Command::new(&run.program);
    command
        .args(&run.args)
        .current_dir(&run.cwd)
        .env_clear()
        .envs(&run.env);
    // Install directly rather than executing the SIP-protected sandbox-exec:
    // the native gate's DYLD environment must reach the fixed interpreter.
    config.sandbox.install_native(&run.program)?;
    Err(command.exec()).context("native worker exec failed")
}

fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "native supervisor pipe setup failed"
    );
    Ok(())
}

struct OwnerInput {
    bytes: Vec<u8>,
    eof: bool,
}
impl OwnerInput {
    fn receive(&mut self) -> Result<Option<Request>> {
        let mut chunk = [0u8; 8192];
        loop {
            let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
            if n == 0 {
                self.eof = true;
                break;
            }
            if n < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            self.bytes.extend_from_slice(&chunk[..n as usize]);
            ensure!(
                self.bytes.len() <= 1_048_577,
                "native owner request exceeds its budget"
            );
        }
        if let Some(end) = self.bytes.iter().position(|b| *b == b'\n') {
            let request = serde_json::from_slice(&self.bytes[..end])?;
            self.bytes.drain(..=end);
            return Ok(Some(request));
        }
        Ok(None)
    }
}

fn watcher(mut config: NativeBuildConfig, config_path: PathBuf) -> Result<()> {
    #[cfg(target_os = "linux")]
    ensure!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == 0,
        "native watcher could not own orphaned namespace children"
    );
    #[cfg(target_os = "macos")]
    ensure!(
        unsafe { libc::setsid() } >= 0,
        "native watcher could not create its session"
    );
    nonblocking(0)?;
    println!("ready");
    std::io::stdout().flush()?;
    let mut owner = OwnerInput {
        bytes: Vec::new(),
        eof: false,
    };
    let mut registered = HashSet::new();
    let mut retain = false;
    let mut safe_to_remove = true;
    let outcome = (|| -> Result<()> {
        loop {
            let request = owner.receive()?;
            if owner.eof {
                return Ok(());
            }
            match request {
                Some(Request::Run(mut run)) => {
                    let result = run_child(
                        &config,
                        &config_path,
                        &mut run,
                        &mut owner,
                        &mut registered,
                        &mut safe_to_remove,
                    );
                    let reply = match result {
                        Ok(reply) => reply,
                        Err(error) => Reply {
                            status: 0,
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                            error: Some(format!("{error:#}")),
                        },
                    };
                    let failed = reply.error.is_some();
                    if owner.eof {
                        return Ok(());
                    }
                    serde_json::to_writer(std::io::stdout(), &reply)?;
                    println!();
                    std::io::stdout().flush()?;
                    if failed {
                        return Ok(());
                    }
                }
                Some(Request::Finish) => {
                    retain = true;
                    return Ok(());
                }
                Some(Request::Register(identities)) => {
                    #[cfg(target_os = "linux")]
                    {
                        let programs: Vec<_> =
                            identities.iter().map(|entry| entry.path.clone()).collect();
                        config.sandbox.capture_runtime_loaders(&programs)?;
                    }
                    for identity in identities {
                        ensure!(
                            identity.path.starts_with(&config.workspace)
                                && canonical(&identity.path)? == identity.path,
                            "native fixed input is outside its private build environment"
                        );
                        ensure!(
                            !config
                                .sandbox
                                .allow_write
                                .iter()
                                .any(|root| identity.path.starts_with(root)),
                            "native fixed input is worker-writable"
                        );
                        if !config.fixed_programs.contains(&identity.path) {
                            config.fixed_programs.push(identity.path);
                        }
                    }
                    config.sandbox.allow_process = config.fixed_programs.clone();
                    let temporary = config.responses.join("supervisor-update.tmp");
                    write_private(&temporary, &serde_json::to_vec(&config)?)?;
                    fs::rename(temporary, &config_path)?;
                    println!("registered");
                    std::io::stdout().flush()?;
                }
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
    })();
    // No normal run removes the source; only the session's owner EOF/abort does.
    let cleanup = if retain {
        Ok(())
    } else if safe_to_remove {
        cleanup_workspace(&config)
    } else {
        Err(eyre::eyre!(
            "native children did not stop; private workspace retained"
        ))
    };
    outcome.and(cleanup)
}

fn cleanup_workspace(config: &NativeBuildConfig) -> Result<()> {
    let (device, inode, uid): (u64, u64, u32) =
        serde_json::from_slice(&bounded(&config.responses.join("owner.json"), 4096)?)?;
    let actual = fs::symlink_metadata(&config.workspace)?;
    ensure!(
        actual.is_dir() && (actual.dev(), actual.ino(), actual.uid()) == (device, inode, uid),
        "native cleanup workspace identity changed"
    );
    for entry in walkdir::WalkDir::new(&config.workspace).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_dir() {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700))?;
        }
    }
    fs::remove_dir_all(&config.workspace).context("native private workspace cleanup failed")
}

struct Cohort {
    child: Child,
    stopped: bool,
}
impl Cohort {
    fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            let _ = self.child.kill();
        }
        #[cfg(target_os = "macos")]
        stop_session()?;
        // Killing the util-linux waiter triggers PID1's parent-death SIGKILL.
        // The subreaper also owns PID1 after the waiter exits, so reaping only
        // the waiter is insufficient: wait until the namespace init and all
        // adopted children have actually finished their kernel exit cleanup.
        let until = Instant::now() + Duration::from_secs(1);
        loop {
            if self.child.try_wait()?.is_some() {
                #[cfg(target_os = "linux")]
                {
                    let result = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
                    if result < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.raw_os_error() == Some(libc::ECHILD) {
                            self.stopped = true;
                            return Ok(());
                        }
                        if error.kind() != std::io::ErrorKind::Interrupted { return Err(error.into()); }
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    self.stopped = true;
                    return Ok(());
                }
            }
            ensure!(
                Instant::now() < until,
                "native cohort did not exit during cleanup"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Cohort {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(target_os = "macos")]
fn stop_session() -> Result<()> {
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_listpids(kind: u32, info: u32, buffer: *mut libc::c_void, size: i32) -> i32;
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            size: i32,
        ) -> i32;
    }
    let sid = unsafe { libc::getsid(0) };
    let me = unsafe { libc::getpid() };
    let until = Instant::now() + Duration::from_secs(1);
    let mut empty_passes = 0;
    loop {
        let needed = unsafe { proc_listpids(1, 0, std::ptr::null_mut(), 0) };
        ensure!(
            needed > 0 && needed <= 4 * 1024 * 1024,
            "native session process enumeration failed"
        );
        let mut pids = vec![0i32; needed as usize / 4 + 64];
        let bytes =
            unsafe { proc_listpids(1, 0, pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
        ensure!(
            bytes > 0 && (bytes as usize) < pids.len() * 4,
            "native session process enumeration was incomplete"
        );
        let mut found = false;
        for pid in pids
            .into_iter()
            .take(bytes as usize / 4)
            .filter(|p| *p > 0 && *p != me)
        {
            if unsafe { libc::getsid(pid) } == sid {
                // An exited child may remain a zombie until its parent reaps it.
                let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
                let size = std::mem::size_of_val(&info) as i32;
                let read = unsafe {
                    proc_pidinfo(
                        pid,
                        libc::PROC_PIDTBSDINFO,
                        0,
                        (&mut info as *mut libc::proc_bsdinfo).cast(),
                        size,
                    )
                };
                if read == 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    continue;
                }
                ensure!(read == size, "native session member could not be inspected");
                if info.pbi_status == libc::SZOMB {
                    continue;
                }
                let result = unsafe { libc::kill(pid, libc::SIGKILL) };
                if result != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                found = true;
            }
        }
        if !found {
            empty_passes += 1;
        } else {
            empty_passes = 0;
        }
        if empty_passes == 2 {
            return Ok(());
        }
        ensure!(
            Instant::now() < until,
            "native session still has members after cleanup"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn drain(pipe: &mut impl Read, output: &mut Vec<u8>, other: usize, limit: usize) -> Result<()> {
    let mut bytes = [0u8; 8192];
    loop {
        match pipe.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                ensure!(
                    output.len().saturating_add(other).saturating_add(n) <= limit,
                    "native build output exceeded its budget"
                );
                output.extend_from_slice(&bytes[..n]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn run_child(
    config: &NativeBuildConfig,
    config_path: &Path,
    run: &mut Run,
    owner: &mut OwnerInput,
    registered: &mut HashSet<PathBuf>,
    safe_to_remove: &mut bool,
) -> Result<Reply> {
    ensure!(
        config.fixed_programs.contains(&canonical(&run.program)?),
        "native run program is not fixed"
    );
    run.program = canonical(&run.program)?;
    run.cwd = canonical(&run.cwd)?;
    ensure!(
        run.cwd.starts_with(&config.workspace),
        "native run cwd is outside its workspace"
    );
    let now = monotonic_ms()?;
    ensure!(
        run.limit > 0 && now < run.deadline_ms,
        "native build deadline expired"
    );
    run.env.insert(
        "PAKRO_NATIVE_REQUESTS".into(),
        config.requests.to_string_lossy().into_owned(),
    );
    run.env.insert(
        "PAKRO_NATIVE_RESPONSES".into(),
        config.responses.to_string_lossy().into_owned(),
    );
    run.env.insert(
        "PAKRO_NATIVE_EXEC_POOL".into(),
        config.exec_pool.to_string_lossy().into_owned(),
    );
    run.env.insert(
        "PAKRO_NATIVE_DEADLINE_MS".into(),
        (run.deadline_ms - now).to_string(),
    );
    let run_file = config.responses.join("run.json");
    if run_file.exists() {
        fs::remove_file(&run_file)?;
    }
    write_private(&run_file, &serde_json::to_vec(run)?)?;
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new(config.unshare.as_ref().expect("captured unshare"));
        command
            .args(["--user", "--pid", "--fork", "--kill-child=KILL"])
            .arg(&config.bootstrap);
        let parent = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::other(
                        "native supervisor exited before registration",
                    ));
                }
                Ok(())
            });
        }
        command
    };
    #[cfg(not(target_os = "linux"))]
    let mut command = Command::new(&config.bootstrap);
    command
        .args(["__pakro_native_build_v1", "exec"])
        .arg(config_path)
        .arg(&run_file)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().context("native cohort bootstrap failed")?;
    *safe_to_remove = false;
    let mut cohort = Cohort {
        child,
        stopped: false,
    };
    let mut stdout = cohort.child.stdout.take().expect("native stdout");
    let mut stderr = cohort.child.stderr.take().expect("native stderr");
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = (|| -> Result<i32> {
        loop {
            if owner.receive()?.is_some() {
                eyre::bail!("native build cancelled");
            }
            ensure!(!owner.eof, "native build owner exited");
            ensure!(
                monotonic_ms()? < run.deadline_ms,
                "native build deadline expired"
            );
            service_mailbox(config, registered, run.deadline_ms)?;
            drain(&mut stdout, &mut out, err.len(), run.limit)?;
            drain(&mut stderr, &mut err, out.len(), run.limit)?;
            if let Some(status) = cohort.child.try_wait()? {
                // The direct process may have left background jobs/pipes behind.
                cohort.stop()?;
                *safe_to_remove = true;
                drain(&mut stdout, &mut out, err.len(), run.limit)?;
                drain(&mut stderr, &mut err, out.len(), run.limit)?;
                return Ok(status.into_raw());
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        cohort.stop()?;
        *safe_to_remove = true;
    }
    let error = result.as_ref().err().map(|error| format!("{error:#}"));
    Ok(Reply {
        status: result.unwrap_or(0),
        stdout: out,
        stderr: err,
        error,
    })
}

fn service_mailbox(
    config: &NativeBuildConfig,
    registered: &mut HashSet<PathBuf>,
    deadline_ms: u64,
) -> Result<()> {
    for entry in fs::read_dir(&config.requests)?.take(128) {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("req") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| eyre::eyre!("invalid native request name"))?;
        let Some((pid, counter)) = stem.split_once('-') else {
            eyre::bail!("invalid native request name");
        };
        ensure!(
            pid.parse::<u32>().is_ok() && counter.parse::<u64>().is_ok(),
            "invalid native request name"
        );
        let result = (|| -> Result<Option<PathBuf>> {
            let bytes = bounded(&path, 8192)?;
            ensure!(
                bytes.len() > 2 && bytes[1] == 0 && matches!(bytes[0], b'E' | b'R'),
                "invalid native executable request"
            );
            let input = PathBuf::from(std::str::from_utf8(&bytes[2..])?);
            register(config, &input, bytes[0] == b'R', registered, deadline_ms)
        })();
        let response = match result {
            Ok(Some(path)) => {
                let mut bytes = vec![0];
                bytes.extend_from_slice(path.as_os_str().as_encoded_bytes());
                bytes
            }
            Ok(None) => vec![2],
            Err(_) => vec![1],
        };
        let response_path = config.responses.join(format!("{stem}.resp"));
        let temporary = config.responses.join(format!("{stem}.tmp"));
        write_private(&temporary, &response)?;
        fs::rename(temporary, response_path)?;
        fs::remove_file(path)?;
    }
    Ok(())
}

fn register(
    config: &NativeBuildConfig,
    path: &Path,
    registration: bool,
    registered: &mut HashSet<PathBuf>,
    deadline_ms: u64,
) -> Result<Option<PathBuf>> {
    let actual = canonical(path)?;
    if config.fixed_programs.contains(&actual) {
        return Ok(Some(actual));
    }
    if actual.starts_with(&config.exec_pool) && registered.contains(&actual) {
        return Ok(Some(actual));
    }
    ensure!(
        config.generated_roots.iter().any(|p| actual.starts_with(p)),
        "native program is outside captured inputs"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&actual)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.len() <= MAX_PROGRAM
            && metadata.uid() == unsafe { libc::geteuid() },
        "invalid generated executable"
    );
    let mut header = [0u8; 4096];
    let n = file.read(&mut header)?;
    if !executable_image(&header[..n]) {
        ensure!(registration, "requested program is not an executable image");
        return Ok(None);
    }
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut output = tempfile::NamedTempFile::new_in(&config.exec_pool)?;
    let mut hash = sha2::Sha256::new();
    let mut copied = 0u64;
    let mut bytes = [0u8; 32768];
    loop {
        ensure!(
            monotonic_ms()? < deadline_ms,
            "native registration deadline expired"
        );
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        copied += n as u64;
        ensure!(
            copied <= MAX_PROGRAM,
            "generated executable exceeds its budget"
        );
        hash.update(&bytes[..n]);
        output.write_all(&bytes[..n])?;
    }
    let after = file.metadata()?;
    ensure!(
        copied == metadata.len()
            && metadata.len() == after.len()
            && metadata.mtime() == after.mtime()
            && metadata.mtime_nsec() == after.mtime_nsec(),
        "generated executable changed during registration"
    );
    let sealed = config.exec_pool.join(hex::encode(hash.finalize()));
    if !sealed.exists() {
        output
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o555))?;
        output.persist_noclobber(&sealed).map_err(|e| e.error)?;
    } else {
        ensure!(
            registered.contains(&sealed),
            "native pool contains an unregistered digest path"
        );
    }
    let temporary = actual.with_extension(format!("pakro-sealed-{}", unsafe { libc::getpid() }));
    std::os::unix::fs::symlink(&sealed, &temporary)?;
    fs::rename(&temporary, &actual)?;
    registered.insert(sealed.clone());
    Ok(Some(sealed))
}

fn executable_image(bytes: &[u8]) -> bool {
    // Shared objects and relocatable objects are never made executable merely
    // because the compiler wrapper reported an output file.
    if bytes.len() >= 64 && bytes.starts_with(b"\x7fELF") && bytes[4] == 2 && bytes[5] == 1 {
        let kind = u16::from_le_bytes([bytes[16], bytes[17]]);
        if !matches!(kind, 2 | 3) || u64::from_le_bytes(bytes[24..32].try_into().unwrap()) == 0 {
            return false;
        }
        let offset = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) as usize;
        let size = u16::from_le_bytes(bytes[54..56].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(bytes[56..58].try_into().unwrap()) as usize;
        let executable_load = size >= 56
            && count > 0
            && count <= 64
            && (0..count).any(|index| {
                let start = offset.saturating_add(index.saturating_mul(size));
                bytes
                    .get(start..start.saturating_add(8))
                    .is_some_and(|value| {
                        value[..4] == 1u32.to_le_bytes()
                            && u32::from_le_bytes(value[4..8].try_into().unwrap()) & 1 != 0
                    })
            });
        let interpreter = (0..count.min(64)).any(|index| {
            let start = offset.saturating_add(index.saturating_mul(size));
            bytes
                .get(start..start.saturating_add(4))
                .is_some_and(|value| value == 3u32.to_le_bytes())
        });
        return executable_load && (kind == 2 || interpreter);
    }
    if bytes.len() >= 16 && matches!(&bytes[..4], b"\xcf\xfa\xed\xfe" | b"\xce\xfa\xed\xfe") {
        return u32::from_le_bytes(bytes[12..16].try_into().unwrap()) == 2;
    }
    false
}
