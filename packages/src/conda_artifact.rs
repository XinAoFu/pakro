//! Acquisition and private extraction of a locked Conda archive.
use crate::{
    artifact_snapshot::{VerifiedArtifact, acquire_verified_artifact},
    config::SettingsExt,
    programs::OperationContext,
};
use eyre::{Context, ContextCompat, Result, ensure};
use pakro_index::artifact::{ArtifactTransport, NetworkPolicy, SourceArtifact};
use pakro_store::Digest;
use rattler_conda_types::{
    RepoDataRecord,
    package::{IndexJson, PackageFile, PathType, PathsJson},
};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;
const MAX_DECODED: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 262_144;

/// Owns the private tree until its verified contents have been published.
pub(crate) struct CondaTree {
    workspace: tempfile::TempDir,
    paths: PathsJson,
    record: RepoDataRecord,
}
impl CondaTree {
    pub(crate) fn root(&self) -> &Path {
        self.workspace.path()
    }
    pub(crate) fn paths(&self) -> &PathsJson {
        &self.paths
    }
    pub(crate) fn record(&self) -> &RepoDataRecord {
        &self.record
    }
    pub(crate) fn post_link(&self) -> Option<&Path> {
        let path = PathBuf::from(format!(
            "bin/.{}-post-link.sh",
            self.record.package_record.name.as_normalized()
        ));
        self.paths
            .paths
            .iter()
            .find(|entry| entry.relative_path == path)
            .map(|entry| entry.relative_path.as_path())
    }
    pub(crate) fn activation_scripts(&self) -> Vec<&Path> {
        self.paths
            .paths
            .iter()
            .map(|entry| entry.relative_path.as_path())
            .filter(|path| {
                path.starts_with("etc/conda/activate.d")
                    || path.starts_with("etc/conda/deactivate.d")
            })
            .collect()
    }
}

pub(crate) async fn acquire(
    context: &OperationContext,
    record: &RepoDataRecord,
    transport: &mut Option<ArtifactTransport>,
) -> Result<VerifiedArtifact> {
    let filename = record.identifier.to_string();
    async {
        context.checkpoint()?;
        let digest: [u8; 32] = record
            .package_record
            .sha256
            .context("Conda archive lacks SHA256")?
            .into();
        let size = record
            .package_record
            .size
            .context("Conda archive lacks size")?;
        ensure!(size <= MAX_ARCHIVE, "Conda archive exceeds size limit");
        let source = SourceArtifact::new(
            filename.as_str(),
            record.url.as_str(),
            Digest::Sha256(digest),
        )?;
        let request = crate::plugins::core::python::native::transport::download_source(
            &source,
            context.settings().url_replacements.as_ref(),
        )?;
        let channel = record
            .channel
            .as_deref()
            .context("Conda archive lacks source channel")?;
        let cache = context
            .requested_cache_root()
            .join("conda-artifacts")
            .join(hex::encode(Sha256::digest(channel.as_bytes())));
        if transport.is_none() {
            *transport = Some(ArtifactTransport::new(
                reqwest::Client::builder()
                    .user_agent(pakro_util::user_agent::get())
                    .connect_timeout(context.settings().http_timeout())
                    .read_timeout(context.settings().http_timeout())
                    .timeout(context.settings().http_download_timeout())
                    .redirect(reqwest::redirect::Policy::custom(|attempt| {
                        if attempt.previous().len() >= 10 {
                            attempt.error("too many runtime artifact redirects")
                        } else if attempt
                            .previous()
                            .last()
                            .is_some_and(|url| url.scheme() == "https")
                            && attempt.url().scheme() != "https"
                        {
                            attempt.error("runtime artifact redirect downgraded HTTPS")
                        } else {
                            attempt.follow()
                        }
                    })),
            )?);
        }
        let transport = transport.as_ref().expect("Conda transport initialized");
        let policy = if context.policy().offline {
            NetworkPolicy::Offline
        } else {
            NetworkPolicy::Allowed
        };
        let cancellation = context.cancellation_token();
        let artifact = tokio::select! {
            _ = cancellation.cancelled() => { context.checkpoint()?; unreachable!() },
            result = tokio::time::timeout_at(context.policy().deadline.into(), acquire_verified_artifact(&request, &cache, transport, policy, MAX_ARCHIVE)) => result.context("Conda download deadline expired").and_then(|result| result),
        }?;
        ensure!(
            artifact.size() == size,
            "Conda archive size differs from locked record"
        );
        Ok::<_, eyre::Report>(artifact)
    }
    .await
    .wrap_err_with(|| format!("failed to acquire Conda artifact {filename}"))
}

fn relative(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "unsafe Conda member path"
    );
    Ok(())
}

fn unpack<R: Read>(
    context: &OperationContext,
    root: &Path,
    mut input: R,
    seen: &mut BTreeSet<PathBuf>,
    decoded: &mut u64,
    entries: &mut usize,
    allowed_post_link: Option<&Path>,
) -> Result<()> {
    use std::os::unix::fs::{PermissionsExt, symlink};
    {
        let mut tar = jdx_tar::Archive::new(&mut input);
        for item in tar.entries()? {
            context.checkpoint()?;
            *entries += 1;
            ensure!(
                *entries <= MAX_ENTRIES,
                "Conda archive entry limit exceeded"
            );
            let mut item = item?;
            let path = item.path()?.into_owned();
            relative(&path)?;
            ensure!(
                path.as_os_str().as_encoded_bytes().len() <= 16_384,
                "Conda member path is too long"
            );
            let kind = item.entry_type();
            ensure!(
                matches!(
                    kind,
                    jdx_tar::EntryType::File
                        | jdx_tar::EntryType::Symlink
                        | jdx_tar::EntryType::Directory
                ),
                "unsupported Conda archive member type"
            );
            if kind != jdx_tar::EntryType::Directory {
                ensure!(
                    seen.insert(path.clone()) && seen.len() <= MAX_ENTRIES,
                    "duplicate or excessive Conda archive members"
                );
            }
            let name = path.to_string_lossy();
            let permitted_post_link =
                allowed_post_link == Some(path.as_path()) && kind == jdx_tar::EntryType::File;
            let activation = name.starts_with("etc/conda/activate.d/")
                || name.starts_with("etc/conda/deactivate.d/");
            let permitted_activation = allowed_post_link.is_some()
                && activation
                && path.components().count() == 4
                && kind == jdx_tar::EntryType::File
                && path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| matches!(value, "sh" | "csh" | "fish"));
            for phase in ["pre-link", "pre-unlink", "post-unlink"] {
                ensure!(
                    !name.ends_with(&format!("-{phase}.sh")) && !name.ends_with(&format!("-{phase}.bat")),
                    "Conda script {name}: {phase} scripts are not supported"
                );
            }
            ensure!(
                !activation || permitted_activation,
                "Conda script {name}: activation requires a regular .sh, .fish or .csh file in a project package"
            );
            ensure!(
                !name.ends_with("-post-link.sh") || permitted_post_link,
                "Conda script {name}: post-link must be a regular file belonging to its project package"
            );
            ensure!(!name.ends_with("-post-link.bat"), "Conda script {name}: Windows post-link scripts are not supported");
            let mut parent = root.to_path_buf();
            for component in path.parent().unwrap_or(Path::new("")).components() {
                let Component::Normal(name) = component else {
                    eyre::bail!("unsafe Conda directory")
                };
                parent.push(name);
                match fs::symlink_metadata(&parent) {
                    Ok(metadata) => ensure!(
                        metadata.is_dir() && !metadata.file_type().is_symlink(),
                        "Conda member parent is not a directory"
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        fs::create_dir(&parent)?
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            let output = root.join(&path);
            if kind == jdx_tar::EntryType::Directory {
                match fs::symlink_metadata(&output) {
                    Ok(metadata) => ensure!(
                        metadata.is_dir() && !metadata.file_type().is_symlink(),
                        "Conda directory was replaced"
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        fs::create_dir(output)?
                    }
                    Err(error) => return Err(error.into()),
                }
            } else if kind == jdx_tar::EntryType::Symlink {
                let target = item
                    .header()
                    .link_name()
                    .context("Conda symlink lacks target")?;
                ensure!(
                    !target.is_absolute(),
                    "absolute Conda symlink is unsupported"
                );
                let mut depth = path.parent().map_or(0, |p| p.components().count());
                for part in target.components() {
                    match part {
                        Component::Normal(_) => depth += 1,
                        Component::CurDir => {}
                        Component::ParentDir if depth > 0 => depth -= 1,
                        _ => eyre::bail!("Conda symlink escapes package"),
                    }
                }
                symlink(target, output)?;
            } else {
                let size = item.size();
                *decoded = decoded
                    .checked_add(size)
                    .context("Conda decoded size overflow")?;
                ensure!(*decoded <= MAX_DECODED, "Conda decoded size limit exceeded");
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(output)?;
                ensure!(
                    std::io::copy(&mut item, &mut file)? == size,
                    "truncated Conda member"
                );
                file.set_permissions(fs::Permissions::from_mode(item.header().mode() & 0o777))?;
                file.flush()?;
            }
        }
    }
    let mut tail = [0u8; 65536];
    loop {
        context.checkpoint()?;
        let n = input.read(&mut tail)?;
        if n == 0 {
            break;
        }
        *decoded = decoded
            .checked_add(n as u64)
            .context("Conda decoded size overflow")?;
        ensure!(
            *decoded <= MAX_DECODED && tail[..n].iter().all(|b| *b == 0),
            "invalid Conda archive tail"
        );
    }
    Ok(())
}

pub(crate) fn extract(
    context: &OperationContext,
    artifact: &mut VerifiedArtifact,
    record: &RepoDataRecord,
) -> Result<CondaTree> {
    extract_inner(context, artifact, record, false)
        .wrap_err_with(|| format!("Conda package {}", record.package_record.name.as_normalized()))
}

/// Project-package extraction only; runtime and Python still reject scripts.
pub(crate) fn extract_project(
    context: &OperationContext,
    artifact: &mut VerifiedArtifact,
    record: &RepoDataRecord,
) -> Result<CondaTree> {
    extract_inner(context, artifact, record, true)
        .wrap_err_with(|| format!("Conda package {}", record.package_record.name.as_normalized()))
}

fn extract_inner(
    context: &OperationContext,
    artifact: &mut VerifiedArtifact,
    record: &RepoDataRecord,
    allow_post_link: bool,
) -> Result<CondaTree> {
    context.checkpoint()?;
    ensure!(
        artifact.filename() == record.identifier.to_string()
            && artifact.size() == record.package_record.size.context("Conda size missing")?
            && artifact.digest()
                == Digest::Sha256(
                    record
                        .package_record
                        .sha256
                        .context("Conda SHA256 missing")?
                        .into()
                ),
        "Conda artifact does not match locked record"
    );
    fs::create_dir_all(context.requested_cache_root())?;
    let workspace = tempfile::Builder::new()
        .prefix("conda-unpack-")
        .tempdir_in(context.requested_cache_root())?;
    artifact.seek(SeekFrom::Start(0))?;
    let mut seen = BTreeSet::new();
    let mut decoded = 0;
    let mut entries = 0;
    let post_link = allow_post_link.then(|| {
        PathBuf::from(format!(
            "bin/.{}-post-link.sh",
            record.package_record.name.as_normalized()
        ))
    });
    if artifact.filename().ends_with(".tar.bz2") {
        unpack(
            context,
            workspace.path(),
            bzip2::read::MultiBzDecoder::new(artifact),
            &mut seen,
            &mut decoded,
            &mut entries,
            post_link.as_deref(),
        )?;
    } else if artifact.filename().ends_with(".conda") {
        let mut zip = zip::ZipArchive::new(artifact)?;
        let mut found_info = false;
        let mut found_pkg = false;
        for i in 0..zip.len() {
            let member = zip.by_index(i)?;
            let name = member.name().to_owned();
            if name.starts_with("info-") && name.ends_with(".tar.zst") {
                ensure!(!found_info, "duplicate Conda info archive");
                found_info = true;
                unpack(
                    context,
                    workspace.path(),
                    zstd::Decoder::new(member)?,
                    &mut seen,
                    &mut decoded,
                    &mut entries,
                    post_link.as_deref(),
                )?;
            } else if name.starts_with("pkg-") && name.ends_with(".tar.zst") {
                ensure!(!found_pkg, "duplicate Conda payload archive");
                found_pkg = true;
                unpack(
                    context,
                    workspace.path(),
                    zstd::Decoder::new(member)?,
                    &mut seen,
                    &mut decoded,
                    &mut entries,
                    post_link.as_deref(),
                )?;
            } else {
                ensure!(name == "metadata.json", "unknown Conda archive member");
            }
        }
        ensure!(
            found_info && found_pkg,
            "Conda archive is missing info or payload"
        );
    } else {
        eyre::bail!("unsupported Conda archive format")
    }
    let index = IndexJson::from_package_directory(workspace.path())?;
    ensure!(
        index.name == record.package_record.name
            && index.version == record.package_record.version
            && index.build == record.package_record.build
            && index.subdir.as_deref() == Some(record.package_record.subdir.as_str()),
        "Conda archive identity differs from locked record"
    );
    let paths = PathsJson::from_package_directory_with_deprecated_fallback(workspace.path())?;
    let mut listed = BTreeSet::new();
    for entry in &paths.paths {
        relative(&entry.relative_path)?;
        ensure!(
            listed.insert(entry.relative_path.clone()),
            "duplicate Conda path metadata"
        );
        let path = workspace.path().join(&entry.relative_path);
        let metadata = fs::symlink_metadata(&path)?;
        match entry.path_type {
            PathType::HardLink => {
                ensure!(metadata.is_file(), "Conda file metadata type mismatch");
                if let Some(size) = entry.size_in_bytes {
                    ensure!(metadata.len() == size, "Conda file size mismatch");
                }
                if let Some(expected) = entry.sha256 {
                    let mut input = fs::File::open(&path)?;
                    let mut hash = Sha256::new();
                    let mut buffer = [0u8; 65536];
                    loop {
                        context.checkpoint()?;
                        let n = input.read(&mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        hash.update(&buffer[..n]);
                    }
                    ensure!(hash.finalize() == expected, "Conda file digest mismatch");
                }
            }
            PathType::SoftLink => ensure!(
                metadata.file_type().is_symlink(),
                "Conda symlink metadata type mismatch"
            ),
            PathType::Directory => {
                ensure!(metadata.is_dir(), "Conda directory metadata type mismatch")
            }
        }
    }
    ensure!(
        seen.iter()
            .filter(|path| !path.starts_with("info"))
            .all(|path| listed.contains(path)),
        "Conda payload contains unlisted files"
    );
    context.checkpoint()?;
    Ok(CondaTree {
        workspace,
        paths,
        record: record.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::language::repository::tests::operation_context;
    use std::time::Duration;

    fn tar_member(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut archive = jdx_tar::Builder::new(Vec::new());
        let mut header = jdx_tar::Header::new_gnu(jdx_tar::EntryType::File);
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        archive.append_data(&mut header, name, bytes).unwrap();
        archive.into_inner().unwrap()
    }

    #[test]
    fn extraction_rejects_scripts_and_keeps_regular_members() {
        let scratch = tempfile::tempdir().unwrap();
        let context = operation_context(scratch.path(), true, Duration::from_secs(20));
        let destination = scratch.path().join("tree");
        fs::create_dir(&destination).unwrap();
        let mut seen = BTreeSet::new();
        let mut decoded = 0;
        let mut entries = 0;
        unpack(
            &context,
            &destination,
            &tar_member("lib/file", b"hello")[..],
            &mut seen,
            &mut decoded,
            &mut entries,
            None,
        )
        .unwrap();
        assert_eq!(fs::read(destination.join("lib/file")).unwrap(), b"hello");
        assert!(
            unpack(
                &context,
                &destination,
                &tar_member("etc/conda/activate.d/setup.sh", b"echo bad")[..],
                &mut seen,
                &mut decoded,
                &mut entries,
                None,
            )
            .is_err()
        );
        assert!(
            unpack(
                &context,
                &destination,
                &tar_member("bin/.demo-post-link.sh", b"echo bad")[..],
                &mut seen,
                &mut decoded,
                &mut entries,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn project_extraction_admits_only_its_exact_post_link_path() {
        let scratch = tempfile::tempdir().unwrap();
        let context = operation_context(scratch.path(), true, Duration::from_secs(20));
        let destination = scratch.path().join("tree");
        fs::create_dir(&destination).unwrap();
        let mut seen = BTreeSet::new();
        let mut decoded = 0;
        let mut entries = 0;
        let allowed = Path::new("bin/.demo-post-link.sh");
        unpack(
            &context,
            &destination,
            &tar_member("bin/.demo-post-link.sh", b"echo ok")[..],
            &mut seen,
            &mut decoded,
            &mut entries,
            Some(allowed),
        )
        .unwrap();
        assert!(
            unpack(
                &context,
                &destination,
                &tar_member("bin/.other-post-link.sh", b"echo wrong")[..],
                &mut seen,
                &mut decoded,
                &mut entries,
                Some(allowed)
            )
            .is_err()
        );
        assert!(
            unpack(
                &context,
                &destination,
                &tar_member("bin/.demo-pre-link.sh", b"echo wrong")[..],
                &mut seen,
                &mut decoded,
                &mut entries,
                Some(allowed)
            )
            .is_err()
        );
    }
}
