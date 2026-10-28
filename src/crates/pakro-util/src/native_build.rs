//! Per-build supervision for the supported foreground native backends.
//! Recipe 1 deliberately continues to use its original no-fork worker.
use crate::sandbox::SandboxConfig;
use eyre::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{ExitCode, Output},
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

/// Versioned implementation bytes used by a portable native-build request.
pub fn supervision_fingerprint() -> [u8; 32] {
    use digest::Digest;
    let mut hash = sha2::Sha256::new();
    hash.update(b"pakro.native-supervision/v1\0");
    for source in [
        include_bytes!("native_build.rs").as_slice(),
        include_bytes!("native_build/supervisor.rs").as_slice(),
        include_bytes!("sandbox/mod.rs").as_slice(),
        include_bytes!("sandbox/macos.rs").as_slice(),
        include_bytes!("sandbox/seccomp.rs").as_slice(),
        include_bytes!("sandbox/landlock.rs").as_slice(),
    ] {
        hash.update(source);
    }
    hash.finalize().into()
}

#[cfg(unix)]
mod supervisor;
#[cfg(all(test, unix))]
mod tests;

/// Inputs captured once for one private source-build lifetime.
#[derive(Debug)]
pub struct NativeShellInputs {
    pub programs: Vec<PathBuf>,
    pub read_inputs: Vec<PathBuf>,
    pub selector_sha256: Option<[u8; 32]>,
}

/// Describe the actual system sh chain at admission; this grants no execution.
pub fn native_shell_inputs() -> Result<NativeShellInputs> {
    let shell = Path::new("/bin/sh").canonicalize().context("system sh is unavailable")?;
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut inputs = NativeShellInputs { programs: vec![shell.clone()], read_inputs: vec![shell], selector_sha256: None };
    #[cfg(target_os = "macos")]
    {
        use digest::Digest;
        let selector = PathBuf::from("/private/var/select/sh");
        match std::fs::symlink_metadata(&selector) {
            Ok(metadata) => {
                ensure!(metadata.file_type().is_symlink(), "unsupported system sh selector");
                let target = selector.canonicalize().context("system sh selected shell is unavailable")?;
                ensure!(target.is_file(), "system sh selected shell is not a program");
                inputs.selector_sha256 = Some(sha2::Sha256::digest(std::fs::read_link(&selector)?.as_os_str().as_encoded_bytes()).into());
                inputs.programs.push(target.clone());
                inputs.read_inputs.extend([selector, target]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    Ok(inputs)
}

/// Inputs captured once for one private source-build lifetime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeBuildConfig {
    pub bootstrap: PathBuf,
    pub fixed_programs: Vec<PathBuf>,
    pub sandbox: SandboxConfig,
    pub workspace: PathBuf,
    pub exec_pool: PathBuf,
    pub generated_roots: Vec<PathBuf>,
    pub requests: PathBuf,
    pub responses: PathBuf,
    /// The fixed util-linux executable on Linux; unused on macOS.
    pub unshare: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Run {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    deadline_ms: u64,
    limit: usize,
}

#[derive(Serialize, Deserialize)]
enum Request {
    Run(Run),
    Register(Vec<NativeProgramIdentity>),
    Finish,
}

/// Actual tool bytes captured at session admission, for the native recipe ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeProgramIdentity {
    pub path: PathBuf,
    pub sha256: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Reply {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    error: Option<String>,
}

/// Coordinates destructor cleanup with the independent watcher. The caller
/// keeps its owned allocation until Finish has returned it, or EOF cleanup ends.
#[derive(Clone, Default)]
pub struct NativeWorkspaceOwnership(Arc<AtomicBool>);

impl NativeWorkspaceOwnership {
    pub fn is_delegated(&self) -> bool { self.0.load(Ordering::Acquire) }
}

/// Owns the liveness writer; neither the watcher nor workers inherit a copy.
/// Dropping the session closes it, so the watcher also handles Pakro SIGKILL.
pub struct NativeBuildSession {
    config: NativeBuildConfig,
    input: Option<tokio::process::ChildStdin>,
    output: BufReader<tokio::process::ChildStdout>,
    _watcher: tokio::process::Child,
    identities: Vec<NativeProgramIdentity>,
    workspace_owner: Option<NativeWorkspaceOwnership>,
}

impl NativeBuildSession {
    pub async fn start(config: NativeBuildConfig) -> Result<Self> {
        Self::start_with_owner(config, None).await
    }

    pub async fn start_owned(config: NativeBuildConfig, owner: NativeWorkspaceOwnership) -> Result<Self> {
        Self::start_with_owner(config, Some(owner)).await
    }

    async fn start_with_owner(mut config: NativeBuildConfig, workspace_owner: Option<NativeWorkspaceOwnership>) -> Result<Self> {
        #[cfg(not(unix))]
        eyre::bail!("native builds are unavailable on this platform");
        #[cfg(unix)]
        {
            let identities = supervisor::prepare(&mut config)?;
            let control = config.responses.join("supervisor.json");
            supervisor::write_private(&control, &serde_json::to_vec(&config)?)?;
            let mut child = tokio::process::Command::new(&config.bootstrap)
                .args(["__pakro_native_build_v1", "watcher"])
                .arg(&control)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(false)
                .spawn()
                .context("could not start native build watcher")?;
            // No await may separate spawn from handing destructor cleanup over.
            if let Some(owner) = &workspace_owner { owner.0.store(true, Ordering::Release); }
            let input = child.stdin.take();
            let mut output = BufReader::new(child.stdout.take().expect("watcher stdout"));
            let mut ready = String::new();
            let started =
                tokio::time::timeout(Duration::from_secs(5), output.read_line(&mut ready)).await;
            if !matches!(started, Ok(Ok(_))) || ready != "ready\n" {
                drop(input);
                eyre::bail!("native build watcher did not become ready");
            }
            Ok(Self {
                config,
                input,
                output,
                _watcher: child,
                identities,
                workspace_owner,
            })
        }
    }

    pub fn requests(&self) -> &Path {
        &self.config.requests
    }
    pub fn responses(&self) -> &Path {
        &self.config.responses
    }
    pub fn exec_pool(&self) -> &Path {
        &self.config.exec_pool
    }
    pub fn program_identities(&self) -> &[NativeProgramIdentity] {
        &self.identities
    }

    /// Owner-only admission of already verified, immutable build-environment bins.
    pub async fn register_fixed_inputs(
        &mut self,
        identities: &[NativeProgramIdentity],
    ) -> Result<()> {
        let mut bytes = serde_json::to_vec(&Request::Register(identities.to_vec()))?;
        ensure!(
            bytes.len() <= 1_048_576,
            "native fixed-input request exceeds its budget"
        );
        bytes.push(b'\n');
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| eyre::eyre!("native build session has ended"))?;
        input.write_all(&bytes).await?;
        input.flush().await?;
        let mut reply = String::new();
        let result =
            tokio::time::timeout(Duration::from_secs(2), self.output.read_line(&mut reply)).await;
        if !matches!(result, Ok(Ok(_))) || reply != "registered\n" {
            self.input.take();
            eyre::bail!("native fixed-input registration failed");
        }
        for identity in identities {
            if !self
                .identities
                .iter()
                .any(|known| known.path == identity.path)
            {
                self.identities.push(identity.clone());
            }
        }
        Ok(())
    }

    /// Normal handoff: the caller's source/output owner retains the workspace.
    pub async fn close(&mut self) -> Result<()> {
        if let Some(mut input) = self.input.take() {
            input.write_all(b"\"Finish\"\n").await?;
            input.flush().await?;
            let status =
                tokio::time::timeout(Duration::from_secs(2), self._watcher.wait()).await??;
            ensure!(status.success(), "native watcher failed to finish");
            if let Some(owner) = &self.workspace_owner { owner.0.store(false, Ordering::Release); }
        }
        Ok(())
    }

    pub async fn run(
        &mut self,
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        deadline: Instant,
        limit: usize,
        cancellation: &CancellationToken,
    ) -> Result<Output> {
        #[cfg(not(unix))]
        eyre::bail!("native builds are unavailable on this platform");
        #[cfg(unix)]
        {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| eyre::eyre!("native build deadline expired"))?;
            ensure!(!cancellation.is_cancelled(), "native build cancelled");
            ensure!(limit > 0, "native output budget is empty");
            let run = Run {
                program: program.into(),
                args: args.into(),
                cwd: cwd.into(),
                env: env.clone(),
                deadline_ms: supervisor::monotonic_ms()?
                    .saturating_add(remaining.as_millis().try_into()?),
                limit,
            };
            let mut request = serde_json::to_vec(&Request::Run(run))?;
            ensure!(
                request.len() <= 1_048_576,
                "native build request exceeds its budget"
            );
            request.push(b'\n');
            struct RunOwner<'a> {
                input: &'a mut Option<tokio::process::ChildStdin>,
                complete: bool,
            }
            impl Drop for RunOwner<'_> {
                fn drop(&mut self) {
                    if !self.complete {
                        self.input.take();
                    }
                }
            }
            let mut owner = RunOwner {
                input: &mut self.input,
                complete: false,
            };
            let input = owner
                .input
                .as_mut()
                .ok_or_else(|| eyre::eyre!("native build session has ended"))?;
            input.write_all(&request).await?;
            input.flush().await?;
            let mut line = String::new();
            let result = tokio::select! {
                result = self.output.read_line(&mut line) => Some(result),
                _ = cancellation.cancelled() => None,
                _ = tokio::time::sleep_until(deadline.into()) => None,
            };
            if result.is_none() {
                // Closing the owner pipe also works if the caller future is dropped.
                owner.input.take();
                let _ = tokio::time::timeout(Duration::from_secs(2), self._watcher.wait()).await;
                eyre::bail!("native build cancelled or timed out");
            }
            ensure!(
                result.unwrap()? > 0,
                "native build watcher exited without a result"
            );
            let reply: Reply =
                serde_json::from_str(&line).context("invalid native build watcher reply")?;
            if let Some(error) = reply.error {
                owner.input.take();
                eyre::bail!("{error}");
            }
            owner.complete = true;
            use std::os::unix::process::ExitStatusExt;
            Ok(Output {
                status: std::process::ExitStatus::from_raw(reply.status),
                stdout: reply.stdout,
                stderr: reply.stderr,
            })
        }
    }
}

/// Internal dispatch is intentionally before config, logging and Tokio startup.
pub fn try_handle_internal() -> Option<ExitCode> {
    let mut args = std::env::args_os();
    args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new("__pakro_native_build_v1")) {
        return None;
    }
    #[cfg(unix)]
    let result = supervisor::dispatch(args.collect());
    #[cfg(not(unix))]
    let result: Result<()> = Err(eyre::eyre!(
        "native builds are unavailable on this platform"
    ));
    match result {
        Ok(()) => Some(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("native build supervisor: {error:#}");
            Some(ExitCode::FAILURE)
        }
    }
}
