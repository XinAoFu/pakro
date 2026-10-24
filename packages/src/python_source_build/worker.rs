//! A fixed driver, finite protocol, and the original bounded process owner.
use super::*;
use eyre::{Context, ContextCompat, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReply {
    ok: bool,
    #[serde(default)]
    missing: bool,
    #[serde(default)]
    value: serde_json::Value,
    #[serde(default)]
    error: Option<String>,
}

pub(super) struct HookReply {
    pub missing: bool,
    pub value: serde_json::Value,
    pub directory: PathBuf,
}

pub(super) struct WorkspaceUsage {
    pub bytes: u64,
    pub entries: usize,
    pub path_bytes: usize,
}

pub(super) fn workspace_usage(
    source: &ExtractedSource,
    limits: SourceBuildLimits,
    deadline: Instant,
) -> Result<WorkspaceUsage> {
    check_workspace_pinned(
        source.reader.root(),
        source.workspace.path(),
        limits,
        deadline,
        |_| Ok(()),
    )
}

#[derive(Serialize)]
struct Request<'a> {
    protocol: u32,
    backend: &'a str,
    backend_paths: &'a [PathBuf],
    dependency_sites: [&'a Path; 1],
    hook: &'a str,
    directory: &'a Path,
    metadata_directory: Option<&'a Path>,
    max_file_bytes: u64,
}

pub(super) async fn run(
    language: &LanguageLease,
    source: &ExtractedSource,
    environment: &BuildEnvironment,
    hook: &str,
    metadata: Option<&Path>,
    deadline: Instant,
    limits: SourceBuildLimits,
) -> Result<HookReply> {
    use pakro_util::cmd::CmdLineRunner;
    use pakro_util::sandbox::SandboxConfig;
    ensure!(
        matches!(
            hook,
            "get_requires_for_build_wheel" | "prepare_metadata_for_build_wheel" | "build_wheel"
        ),
        "source build hook is unknown"
    );
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .context("source build deadline expired")?;
    let workspace = source.workspace.path();
    let workspace_root = source.reader.root();
    let driver = workspace.join("hook.py");
    if !driver.exists() {
        fs::write(&driver, include_bytes!("hook.py"))?;
    }
    let private = workspace.join(format!("hook-{}", uuid::Uuid::now_v7().simple()));
    fs::create_dir(&private)?;
    let output_directory = private.join("output");
    let temporary = private.join("tmp");
    let home = private.join("home");
    let responses = private.join("response");
    for directory in [&output_directory, &temporary, &home, &responses] {
        fs::create_dir(directory)?;
    }
    let request = private.join("request.json");
    let response = responses.join("result.json");
    fs::write(
        &request,
        serde_json::to_vec(&Request {
            protocol: 1,
            backend: &source.backend,
            backend_paths: &source.backend_paths,
            dependency_sites: [environment.site.as_path()],
            hook,
            directory: &output_directory,
            metadata_directory: metadata,
            max_file_bytes: limits.max_file_bytes.min(limits.max_generated_wheel_bytes),
        })?,
    )?;
    let mut reads = vec![workspace.to_owned(), language.root().to_owned()];
    #[cfg(target_os = "macos")]
    for path in ["/System/Library", "/usr/lib", "/dev/null", "/dev/urandom"] {
        if Path::new(path).exists() {
            reads.push(path.into());
        }
    }
    #[cfg(target_os = "linux")]
    for path in ["/lib", "/lib64", "/usr/lib", "/dev/null", "/dev/urandom"] {
        if Path::new(path).exists() {
            reads.push(path.into());
        }
    }
    let mut sandbox = SandboxConfig {
        build_worker: true,
        deny_read: true,
        deny_write: true,
        deny_net: true,
        deny_env: true,
        deny_process: true,
        deny_temp_write: true,
        allow_read: reads,
        allow_write: vec![
            source.root.clone(),
            output_directory.clone(),
            temporary.clone(),
            home.clone(),
            responses,
        ],
        allow_env: [
            "PATH",
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "SOURCE_DATE_EPOCH",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        ..Default::default()
    };
    sandbox.resolve_paths();
    let executable = language
        .entrypoint()
        .canonicalize()
        .context("fixed source build interpreter is unavailable")?;
    let mut command = CmdLineRunner::new(&executable)
        .args(["-I", "-S", "-B"])
        .arg(&driver)
        .arg(&request)
        .arg(&response)
        .current_dir(&source.root)
        .env_clear()
        .env("PATH", environment.prefix.final_path().join("bin"))
        .env("HOME", &home)
        .env("TMPDIR", &temporary)
        .env("TMP", &temporary)
        .env("TEMP", &temporary)
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("SOURCE_DATE_EPOCH", "315532800")
        .with_timeout(remaining)
        .with_sandbox(sandbox);
    command
        .apply_sandbox()
        .await
        .context("source build isolation could not be applied")?;
    check_workspace_pinned(workspace_root, workspace, limits, deadline, |_| Ok(()))?;
    let execute = command.output_isolated(limits.max_hook_output_bytes);
    tokio::pin!(execute);
    let mut interval = tokio::time::interval(Duration::from_millis(25));
    let output = loop {
        tokio::select! {
            result = &mut execute => break result.context("isolated source backend failed or exceeded its process/output budget")?,
            _ = interval.tick() => { check_workspace_pinned(workspace_root,workspace,limits,deadline,|_| Ok(()))?; },
        }
    };
    check_workspace_pinned(workspace_root, workspace, limits, deadline, |_| Ok(()))?;
    let reply: WireReply = serde_json::from_slice(
        &archive::read_bounded(
            &source.reader,
            &response,
            limits.max_hook_output_bytes as u64,
        )
        .context("source backend returned no bounded hook response")?,
    )
    .context("source backend response protocol is invalid")?;
    if !output.status.success() || !reply.ok {
        let kind = reply
            .error
            .as_deref()
            .filter(|value| {
                value.len() <= 128
                    && !value.is_empty()
                    && value
                        .bytes()
                        .all(|value| value.is_ascii_alphanumeric() || value == b'_')
            })
            .unwrap_or("BackendFailure");
        // Never replay backend output or exception text: both are arbitrary
        // peer-controlled bytes. This worker intentionally admits no compiler
        // or nested package-manager subprocesses.
        eyre::bail!(
            "source {hook} hook failed ({kind}); subprocess compilers are unavailable in this isolated worker"
        );
    }
    Ok(HookReply {
        missing: reply.missing,
        value: reply.value,
        directory: output_directory,
    })
}

#[cfg(test)]
fn open_workspace(root: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC);
    }
    let file = options
        .open(root)
        .context("source workspace root is unavailable")?;
    #[cfg(unix)]
    pakro_store::control::validate_owned_directory(&file, current_uid())?;
    Ok(file)
}

#[cfg(test)]
pub(super) fn check_workspace_with_hook(
    root: &Path,
    limits: SourceBuildLimits,
    deadline: Instant,
    before_directory: impl FnMut(&Path) -> Result<()>,
) -> Result<()> {
    let pinned = open_workspace(root)?;
    check_workspace_pinned(&pinned, root, limits, deadline, before_directory).map(|_| ())
}

#[cfg(unix)]
fn check_workspace_pinned(
    root: &fs::File,
    root_path: &Path,
    limits: SourceBuildLimits,
    deadline: Instant,
    mut before_directory: impl FnMut(&Path) -> Result<()>,
) -> Result<WorkspaceUsage> {
    use nix::fcntl::{AtFlags, OFlag};
    use nix::sys::stat::{Mode, fstatat};
    use pakro_store::control::{FileIdentity, open_directory};
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt;
    enum Pending {
        Root(fs::File),
        Child {
            parent: fs::File,
            name: OsString,
            identity: FileIdentity,
        },
    }
    let mut pending = vec![(Pending::Root(root.try_clone()?), root_path.to_owned())];
    let mut bytes = 0u64;
    let mut entries = 0usize;
    let mut path_bytes = 0usize;
    while let Some((entry, path)) = pending.pop() {
        before_directory(&path)?;
        ensure!(
            Instant::now() < deadline,
            "source backend workspace deadline expired"
        );
        let directory = match entry {
            Pending::Root(file) => file,
            Pending::Child {
                parent,
                name,
                identity,
            } => {
                let opened = match open_directory(&parent, name.as_os_str()) {
                    Ok(file) => file,
                    Err(error)
                        if error.downcast_ref::<nix::errno::Errno>()
                            == Some(&nix::errno::Errno::ENOENT) =>
                    {
                        continue;
                    }
                    Err(_) => eyre::bail!("source backend workspace directory changed"),
                };
                ensure!(
                    FileIdentity::of(&opened)? == identity,
                    "source backend workspace directory changed"
                );
                opened
            }
        };
        let mut listing = nix::dir::Dir::openat(
            &directory,
            ".",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        for entry in listing.iter() {
            ensure!(
                Instant::now() < deadline,
                "source backend workspace deadline expired"
            );
            let entry = entry?;
            let raw = entry.file_name().to_bytes();
            if matches!(raw, b"." | b"..") {
                continue;
            }
            let name = OsStr::from_bytes(raw);
            let metadata = match fstatat(&directory, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
                Ok(value) => value,
                Err(nix::errno::Errno::ENOENT) => continue,
                Err(error) => return Err(error.into()),
            };
            entries = entries
                .checked_add(1)
                .filter(|value| *value <= limits.max_workspace_entries)
                .context("source backend workspace entry budget exceeded")?;
            let child_path = path.join(name);
            let relative = child_path.strip_prefix(root_path)?;
            ensure!(
                relative.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
                "source backend workspace path budget exceeded"
            );
            path_bytes = path_bytes
                .checked_add(relative.as_os_str().as_encoded_bytes().len())
                .filter(|value| *value <= limits.max_total_path_bytes)
                .context("source backend workspace total path budget exceeded")?;
            let kind = metadata.st_mode & nix::libc::S_IFMT;
            if kind == nix::libc::S_IFDIR {
                pending.push((
                    Pending::Child {
                        parent: directory.try_clone()?,
                        name: name.to_owned(),
                        identity: FileIdentity {
                            device: metadata.st_dev,
                            inode: metadata.st_ino,
                        },
                    },
                    child_path,
                ));
            } else if kind == nix::libc::S_IFREG {
                let size = u64::try_from(metadata.st_size)
                    .context("source backend workspace size is invalid")?;
                bytes = bytes
                    .checked_add(size)
                    .filter(|value| *value <= limits.max_workspace_bytes)
                    .context("source backend workspace byte budget exceeded")?;
            }
        }
    }
    Ok(WorkspaceUsage {
        bytes,
        entries,
        path_bytes,
    })
}

#[cfg(not(unix))]
fn check_workspace_pinned(
    _root: &fs::File,
    _root_path: &Path,
    _limits: SourceBuildLimits,
    _deadline: Instant,
    _before_directory: impl FnMut(&Path) -> Result<()>,
) -> Result<WorkspaceUsage> {
    eyre::bail!("source backend workspace monitoring requires a supported Unix platform")
}
