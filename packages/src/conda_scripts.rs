//! Supervised private staging for a single Conda shell script.
//!
//! Installation imports the verified result through its controlled layout owner
//! before publishing the layout and committing a project selection.

use crate::packages::python_environment::prefix::{
    UnpublishedPrefix,
    verify::{
        InventoryRoot, PrefixExpectedDirectory, PrefixExpectedFile, PrefixExpectedSymlink,
        PrefixInventoryLimits, TreeInventory, TreeInventoryKind, observe_tree_inventory,
        verify_layout_inventory,
    },
};
use crate::programs::OperationContext;
pub(crate) mod activation;
use eyre::{Context, ContextCompat, Result, ensure};
use pakro_util::{
    native_build::{
        NativeBuildConfig, NativeBuildSession, NativeProgramIdentity, NativeWorkspaceOwnership,
    },
    native_exec,
    sandbox::SandboxConfig,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Output,
};

const SCRIPT_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 1024 * 1024;
const TOOLS: &[&str] = &[
    "sh", "env", "awk", "basename", "cat", "chmod", "cp", "cut", "dirname", "expr", "find", "grep",
    "head", "ln", "ls", "mkdir", "mv", "pwd", "rm", "rmdir", "sed", "sort", "tail", "test",
    "touch", "tr", "uname", "wc", "xargs",
];

struct Workspace {
    path: PathBuf,
    ownership: NativeWorkspaceOwnership,
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if self.ownership.is_delegated() {
            return;
        }
        // Scripts may have made their private staging directories readonly.
        for entry in walkdir::WalkDir::new(&self.path)
            .follow_links(false)
            .into_iter()
            .flatten()
        {
            if entry.file_type().is_dir() {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700));
            }
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub(crate) struct CondaStagedScript {
    workspace: Workspace,
    output: Output,
    programs: Vec<NativeProgramIdentity>,
    activation: Option<activation::ActivationPlan>,
}
impl CondaStagedScript {
    pub(crate) fn prefix(&self) -> PathBuf {
        self.workspace.path.join("work/prefix")
    }
    pub(crate) fn output(&self) -> &Output {
        &self.output
    }
    pub(crate) fn programs(&self) -> &[NativeProgramIdentity] {
        &self.programs
    }
    pub(crate) fn activation(&self) -> Option<&activation::ActivationPlan> {
        self.activation.as_ref()
    }

    /// Import the complete staged result into an empty controlled layout.
    /// Staging paths embedded in output bytes require relocation support and
    /// are rejected until that transformation can be verified.
    pub(crate) fn import_layout(
        &self,
        context: &OperationContext,
        destination: &mut UnpublishedPrefix,
    ) -> Result<TreeInventory> {
        use nix::{
            fcntl::{OFlag, openat},
            sys::stat::Mode,
        };
        use sha2::{Digest as _, Sha256};

        context.checkpoint()?;
        let root = StagedRoot::open(self.prefix())?;
        let inventory = observe_tree_inventory(&root, PrefixInventoryLimits::default())?;
        let index: BTreeMap<_, _> = inventory
            .entries()
            .iter()
            .map(|entry| (entry.relative.as_path(), entry))
            .collect();
        let mut files = Vec::new();
        let mut directories = Vec::new();
        let mut links = Vec::new();
        let staging = self.workspace.path.as_os_str().as_encoded_bytes();
        for entry in inventory.entries() {
            context.checkpoint()?;
            match &entry.kind {
                TreeInventoryKind::Directory => {
                    destination.create_dir_all(&entry.relative)?;
                    directories.push(PrefixExpectedDirectory {
                        relative: entry.relative.clone(),
                        mode: 0o755,
                    });
                }
                TreeInventoryKind::Symlink { target } => {
                    destination.create_layout_symlink(&entry.relative, target)?;
                    links.push(PrefixExpectedSymlink {
                        relative: entry.relative.clone(),
                        target: target.clone(),
                    });
                }
                TreeInventoryKind::File { size, sha256 } => {
                    let mut parent = root.file.try_clone()?;
                    let mut relative = PathBuf::new();
                    for component in entry
                        .relative
                        .parent()
                        .unwrap_or(Path::new(""))
                        .components()
                    {
                        relative.push(component.as_os_str());
                        let expected = index
                            .get(relative.as_path())
                            .context("staged parent is absent")?;
                        ensure!(
                            matches!(expected.kind, TreeInventoryKind::Directory),
                            "staged parent is not a directory"
                        );
                        parent =
                            pakro_store::control::open_directory(&parent, component.as_os_str())?;
                        let meta = parent.metadata()?;
                        ensure!(
                            [meta.dev(), meta.ino()] == expected.identity,
                            "staged directory identity changed"
                        );
                    }
                    let name = entry
                        .relative
                        .file_name()
                        .context("staged file has no name")?;
                    let mut input = File::from(openat(
                        &parent,
                        name,
                        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                        Mode::empty(),
                    )?);
                    let metadata = input.metadata()?;
                    ensure!(
                        metadata.is_file()
                            && [metadata.dev(), metadata.ino()] == entry.identity
                            && metadata.nlink() == 1
                            && metadata.len() == *size
                            && metadata.mode() & 0o7777 == entry.mode,
                        "staged file changed before import"
                    );
                    ensure!(
                        entry.mode & !0o777 == 0
                            && entry.mode & 0o022 == 0
                            && entry.mode & 0o400 != 0,
                        "staged file has unsupported mode"
                    );
                    let mut output =
                        destination.create_file_exclusive(&entry.relative, entry.mode)?;
                    let mut hash = Sha256::new();
                    let mut copied = 0u64;
                    let mut tail = Vec::new();
                    let mut buffer = [0u8; 65536];
                    loop {
                        context.checkpoint()?;
                        let count = input.read(&mut buffer)?;
                        if count == 0 {
                            break;
                        }
                        copied = copied
                            .checked_add(count as u64)
                            .context("staged file size overflow")?;
                        ensure!(copied <= *size, "staged file grew during import");
                        tail.extend_from_slice(&buffer[..count]);
                        ensure!(
                            !tail.windows(staging.len()).any(|window| window == staging),
                            "staged file contains its temporary prefix and needs relocation"
                        );
                        let retain = staging.len().saturating_sub(1).min(tail.len());
                        tail.drain(..tail.len() - retain);
                        hash.update(&buffer[..count]);
                        output.write_all(&buffer[..count])?;
                    }
                    ensure!(
                        copied == *size && <[u8; 32]>::from(hash.finalize()) == *sha256,
                        "staged file differs from its observed inventory"
                    );
                    output.finish()?;
                    files.push(PrefixExpectedFile {
                        relative: entry.relative.clone(),
                        sha256: *sha256,
                        size: *size,
                        mode: entry.mode,
                    });
                }
            }
        }
        ensure!(
            observe_tree_inventory(&root, PrefixInventoryLimits::default())? == inventory,
            "staged tree changed during import"
        );
        Ok(verify_layout_inventory(
            destination,
            &files,
            &directories,
            &links,
            PrefixInventoryLimits::default(),
        )?
        .into_tree_inventory())
    }
}

struct StagedRoot {
    path: PathBuf,
    file: File,
    identity: [u64; 2],
    uid: u32,
}
impl StagedRoot {
    fn open(path: PathBuf) -> Result<Self> {
        let file = File::open(&path)?;
        let metadata = file.metadata()?;
        let root = Self {
            path,
            file,
            identity: [metadata.dev(), metadata.ino()],
            uid: nix::unistd::geteuid().as_raw(),
        };
        root.check_binding()?;
        Ok(root)
    }
}
impl InventoryRoot for StagedRoot {
    fn root_file(&self) -> &File {
        &self.file
    }
    fn root_path(&self) -> &Path {
        &self.path
    }
    fn allocation_id(&self) -> &str {
        "conda-staging"
    }
    fn expected_uid(&self) -> u32 {
        self.uid
    }
    fn check_binding(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        let opened = self.file.metadata()?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == self.uid
                && metadata.mode() & 0o7777 == 0o700
                && [metadata.dev(), metadata.ino()] == self.identity
                && [opened.dev(), opened.ino()] == self.identity,
            "Conda staging root changed"
        );
        Ok(())
    }
}

/// The caller populates only the private staging prefix before the script is
/// started. The returned tree remains private and is not an installation.
pub(crate) async fn run_staged(
    context: &OperationContext,
    script: &[u8],
    prepare_prefix: impl FnOnce(&Path) -> Result<()>,
) -> Result<CondaStagedScript> {
    run(context, Script::Inline(script), prepare_prefix).await
}

/// Read the post-link script from its original location in the staged prefix.
pub(crate) async fn run_post_link(
    context: &OperationContext,
    record: &rattler_conda_types::RepoDataRecord,
    prepare_prefix: impl FnOnce(&Path) -> Result<()>,
) -> Result<CondaStagedScript> {
    run(context, Script::PostLink(record), prepare_prefix).await.wrap_err_with(|| {
        let name = record.package_record.name.as_normalized();
        format!("Conda package {name}, script bin/.{name}-post-link.sh")
    })
}

pub(crate) async fn run_activation(
    context: &OperationContext,
    package: &str,
    activate: &Path,
    deactivate: &Path,
    prepare_prefix: impl FnOnce(&Path) -> Result<()>,
) -> Result<CondaStagedScript> {
    run(
        context,
        Script::Activation(activate, deactivate),
        prepare_prefix,
    )
    .await.wrap_err_with(|| format!(
        "Conda package {package}, scripts {} and {}", activate.display(), deactivate.display()
    ))
}

#[derive(Clone, Copy)]
enum Script<'a> {
    Inline(&'a [u8]),
    PostLink(&'a rattler_conda_types::RepoDataRecord),
    Activation(&'a Path, &'a Path),
}

async fn run(
    context: &OperationContext,
    script: Script<'_>,
    prepare_prefix: impl FnOnce(&Path) -> Result<()>,
) -> Result<CondaStagedScript> {
    context.checkpoint()?;
    ensure!(
        native_exec::SUPPORTED,
        "Conda script isolation is unavailable on this platform"
    );
    if let Script::Inline(bytes) = script {
        ensure!(
            !bytes.is_empty() && bytes.len() <= SCRIPT_LIMIT,
            "Conda script exceeds its input limit"
        );
    }
    fs::create_dir_all(context.requested_cache_root())?;
    let path = tempfile::Builder::new()
        .prefix("conda-script-")
        .tempdir_in(context.requested_cache_root())?
        .keep()
        .canonicalize()?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    let workspace = Workspace {
        path,
        ownership: NativeWorkspaceOwnership::default(),
    };
    let work = workspace.path.join("work");
    let prefix = work.join("prefix");
    let home = work.join("home");
    let temp = work.join("tmp");
    for directory in [&work, &prefix, &home, &temp] {
        fs::create_dir(directory)?;
    }
    fs::set_permissions(&prefix, fs::Permissions::from_mode(0o700))?;
    prepare_prefix(&prefix)?;
    context.checkpoint()?;
    let mut activation_shape = None;
    let script_path = match script {
        Script::Inline(bytes) => {
            let path = work.join("script.sh");
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(bytes)?;
            path
        }
        Script::PostLink(record) => {
            let path = prefix.join("bin").join(format!(
                ".{}-post-link.sh",
                record.package_record.name.as_normalized()
            ));
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file() && metadata.len() > 0 && metadata.len() <= SCRIPT_LIMIT as u64,
                "Conda post-link script is not a bounded regular staged file"
            );
            let bytes = fs::read(&path)?;
            if bytes.starts_with(b"#!") {
                let line = bytes
                    .split(|byte| *byte == b'\n')
                    .next()
                    .unwrap_or_default();
                let interpreter = std::str::from_utf8(&line[2..])?.trim();
                ensure!(
                    matches!(interpreter, "/bin/sh" | "/usr/bin/env sh"),
                    "Conda post-link interpreter is not supported yet"
                );
            }
            path
        }
        Script::Activation(activate, deactivate) => {
            for relative in [activate, deactivate] {
                ensure!(
                    relative
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_))),
                    "unsafe Conda activation script path"
                );
                let path = prefix.join(relative);
                let metadata = fs::symlink_metadata(&path)?;
                ensure!(
                    metadata.is_file()
                        && metadata.len() > 0
                        && metadata.len() <= SCRIPT_LIMIT as u64,
                    "Conda activation script is not a bounded regular file"
                );
            }
            let shape = activation::script_shape(&fs::read(prefix.join(activate))?)
                .wrap_err_with(|| format!("unsupported activation script {}", activate.display()))?;
            activation::script_shape(&fs::read(prefix.join(deactivate))?)
                .wrap_err_with(|| format!("unsupported deactivation script {}", deactivate.display()))?;
            let root = StagedRoot::open(prefix.clone())?;
            let inventory = observe_tree_inventory(&root, PrefixInventoryLimits::default())?;
            activation_shape = Some((shape, inventory));
            prefix.join(activate)
        }
    };
    let tools = workspace.path.join("tools");
    let bin = tools.join("bin");
    fs::create_dir(&tools)?;
    fs::create_dir(&bin)?;
    let gate = tools.join(native_exec::GATE_LIBRARY_NAME);
    fs::write(&gate, native_exec::GATE_LIBRARY_BYTES)?;
    fs::set_permissions(&gate, fs::Permissions::from_mode(0o444))?;
    let shell_inputs = pakro_util::native_build::native_shell_inputs()?;
    let shell = Path::new("/bin/sh").canonicalize()?;
    let mut programs: BTreeSet<PathBuf> = shell_inputs.programs.into_iter().collect();
    let mut reads = shell_inputs.read_inputs;
    reads.push(workspace.path.clone());
    for name in TOOLS {
        let found = ["/usr/bin", "/bin"]
            .into_iter()
            .map(|directory| Path::new(directory).join(name))
            .find(|candidate| candidate.is_file());
        let Some(found) = found else {
            ensure!(
                *name != "sh" && *name != "env",
                "required system POSIX tool {name} is unavailable"
            );
            continue;
        };
        let program = found.canonicalize()?;
        std::os::unix::fs::symlink(&program, bin.join(name))?;
        reads.push(program.clone());
        programs.insert(program);
    }
    for name in [
        "/System/Library",
        "/usr/lib",
        "/lib",
        "/lib64",
        "/dev/null",
        "/dev/urandom",
    ] {
        let path = Path::new(name);
        if path.exists() {
            reads.push(path.to_owned());
        }
    }
    #[cfg(test)]
    let bootstrap = PathBuf::from(
        std::env::var_os("PAKRO_TEST_BINARY")
            .context("PAKRO_TEST_BINARY must name the new native main")?,
    )
    .canonicalize()?;
    #[cfg(not(test))]
    let bootstrap = std::env::current_exe()?.canonicalize()?;
    let unshare = if cfg!(target_os = "linux") {
        Some(
            Path::new("/usr/bin/unshare")
                .canonicalize()
                .context("util-linux unshare is required")?,
        )
    } else {
        None
    };
    let config = NativeBuildConfig {
        bootstrap,
        fixed_programs: programs.into_iter().collect(),
        sandbox: SandboxConfig {
            allow_read: reads,
            allow_write: vec![work.clone()],
            ..Default::default()
        },
        workspace: workspace.path.clone(),
        exec_pool: workspace.path.join("exec-pool"),
        generated_roots: vec![work.clone()],
        requests: workspace.path.join("requests"),
        responses: workspace.path.join("responses"),
        unshare,
    };
    let as_text = |path: &Path| -> Result<String> {
        Ok(path
            .to_str()
            .context("Conda staging path is not UTF-8")?
            .to_owned())
    };
    let mut env = BTreeMap::from([
        ("PATH".into(), as_text(&bin)?),
        ("HOME".into(), as_text(&home)?),
        ("TMPDIR".into(), as_text(&temp)?),
        ("TMP".into(), as_text(&temp)?),
        ("TEMP".into(), as_text(&temp)?),
        ("PREFIX".into(), as_text(&prefix)?),
        ("LANG".into(), "C".into()),
        ("LC_ALL".into(), "C".into()),
        ("PAKRO_NATIVE_GATE_LIBRARY".into(), as_text(&gate)?),
    ]);
    env.insert(
        if cfg!(target_os = "macos") {
            "DYLD_INSERT_LIBRARIES"
        } else {
            "LD_PRELOAD"
        }
        .into(),
        as_text(&gate)?,
    );
    if let Script::PostLink(record) = script {
        env.insert(
            "PKG_NAME".into(),
            record.package_record.name.as_normalized().into(),
        );
        env.insert(
            "PKG_VERSION".into(),
            record.package_record.version.to_string(),
        );
        env.insert(
            "PKG_BUILDNUM".into(),
            record.package_record.build_number.to_string(),
        );
    }
    if let Some(((copies, _), _)) = &activation_shape {
        env.insert("CONDA_PREFIX".into(), as_text(&prefix)?);
        for source in copies.values() {
            env.insert(
                source.clone(),
                format!("pakro-conda-activation-probe-{source}"),
            );
        }
    }
    let arguments = match script {
        Script::Activation(_, deactivate) => vec!["-c".into(),
            "umask 022; env -0 > \"$2\" || exit; . \"$1\" || exit; env -0 > \"$3\" || exit; . \"$4\" || exit; env -0 > \"$5\" || exit".into(),
            as_text(&script_path)?, as_text(&script_path)?, as_text(&work.join("env-before"))?,
            as_text(&work.join("env-after"))?, as_text(&prefix.join(deactivate))?, as_text(&work.join("env-deactivated"))?],
        _ => vec!["-c".into(), "umask 022; exec \"$@\"".into(), "pakro-conda".into(),
            as_text(&shell)?, as_text(&script_path)?],
    };
    let mut session = NativeBuildSession::start_owned(config, workspace.ownership.clone()).await?;
    let programs = session.program_identities().to_vec();
    let deadline = context.policy().deadline.min(
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );
    let result = session
        .run(
            &shell,
            &arguments,
            &work,
            &env,
            deadline,
            OUTPUT_LIMIT,
            &context.cancellation_token(),
        )
        .await;
    let output = match result {
        Ok(output) => output,
        Err(error) => {
            let stopped = session.stop().await.is_ok();
            return Err(error.wrap_err(format!(
                "Conda script execution failed (fixed limit: 60 seconds). This installation was not committed; the previous lock and project registration are unchanged. Child processes confirmed stopped: {}. Review the script output, choose a compatible package version or dependency, then rerun the original command.",
                if stopped { "yes" } else { "no; retain the private work directory for inspection" },
            )));
        }
    };
    session.close().await?;
    ensure!(
        output.status.success(),
        "Conda staging script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let activation = if let Some(((copies, unsets), inventory)) = activation_shape {
        let root = StagedRoot::open(prefix.clone())?;
        ensure!(
            observe_tree_inventory(&root, PrefixInventoryLimits::default())? == inventory,
            "Conda activation script changed its prepared layout"
        );
        let read_snapshot = |name: &str| -> Result<BTreeMap<String, String>> {
            let path = work.join(name);
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file() && metadata.len() <= 1024 * 1024,
                "invalid Conda activation snapshot"
            );
            activation::parse_snapshot(&fs::read(path)?)
        };
        let before = read_snapshot("env-before")?;
        let after = read_snapshot("env-after")?;
        let _ = read_snapshot("env-deactivated")?;
        for (destination, source) in &copies {
            ensure!(
                after.get(destination) == env.get(source),
                "Conda conditional environment copy was not observed"
            );
        }
        Some(activation::ActivationPlan::from_observations(
            &before,
            &after,
            &unsets,
            copies,
            &prefix,
            &workspace.path,
        )?)
    } else {
        None
    };
    Ok(CondaStagedScript {
        workspace,
        output,
        programs,
        activation,
    })
}

#[cfg(test)]
#[path = "conda_scripts/tests.rs"]
mod tests;
