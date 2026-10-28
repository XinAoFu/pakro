//! Bounded no-link extraction from one held, digest-verified source artifact.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, Write};

use eyre::{Context, ContextCompat, bail, ensure};

/// The directory is pinned before backend execution. Output names are labels,
/// never an authority to reopen a path through backend-controlled ancestors.
pub(super) struct WorkspaceReader {
    root: fs::File,
    path: PathBuf,
    canonical_path: PathBuf,
}

impl WorkspaceReader {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(
                nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
            );
        }
        let root = options
            .open(path)
            .context("pinning private source workspace")?;
        #[cfg(unix)]
        pakro_store::control::validate_owned_directory(&root, current_uid())?;
        Ok(Self {
            root,
            path: path.to_owned(),
            canonical_path: path.canonicalize()?,
        })
    }

    pub(super) fn root(&self) -> &fs::File {
        &self.root
    }

    fn components<'a>(&self, path: &'a Path) -> Result<Vec<&'a std::ffi::OsStr>> {
        path.strip_prefix(&self.path)
            .or_else(|_| path.strip_prefix(&self.canonical_path))
            .context("source output path is outside its held workspace")?
            .components()
            .map(|component| match component {
                std::path::Component::Normal(name) => Ok(name),
                _ => bail!("source output path has a non-normal component"),
            })
            .collect()
    }

    pub(super) fn directory(&self, path: &Path) -> Result<fs::File> {
        self.walk(&self.components(path)?)
    }

    pub(super) fn relative_bytes(&self, path: &Path) -> Result<usize> {
        let components = self.components(path)?;
        components
            .iter()
            .try_fold(components.len().saturating_sub(1), |total, name| {
                total
                    .checked_add(name.as_encoded_bytes().len())
                    .context("source workspace path byte count overflow")
            })
    }

    #[cfg(unix)]
    pub(super) fn restore_directory_permissions(
        &self,
        maximum: usize,
        mut before_directory: impl FnMut(&Path),
    ) -> Result<()> {
        use nix::fcntl::{AtFlags, OFlag, openat};
        use nix::sys::stat::{Mode, fchmod, fstatat};
        use std::ffi::{OsStr, OsString};
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        enum Pending {
            Root(fs::File),
            Child { parent: fs::File, name: OsString },
        }
        let mut pending = vec![(Pending::Root(self.root.try_clone()?), self.path.clone())];
        let mut visited = 0usize;
        while let Some((entry, path)) = pending.pop() {
            visited = visited
                .checked_add(1)
                .context("source cleanup count overflow")?;
            ensure!(visited <= maximum, "source cleanup entry budget exceeded");
            before_directory(&path);
            let directory = match entry {
                Pending::Root(file) => file,
                Pending::Child { parent, name } => {
                    let flags =
                        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
                    let fd = match openat(&parent, name.as_os_str(), flags, Mode::empty()) {
                        Ok(fd) => fd,
                        #[cfg(target_os = "macos")]
                        Err(nix::errno::Errno::EACCES) => {
                            use nix::sys::stat::{FchmodatFlags, fchmodat};
                            let Ok(before) =
                                fstatat(&parent, name.as_os_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
                            else {
                                continue;
                            };
                            if before.st_mode & nix::libc::S_IFMT != nix::libc::S_IFDIR
                                || before.st_uid != current_uid()
                            {
                                continue;
                            }
                            // This operation never follows a swapped symlink.
                            // Its success does not authenticate the directory;
                            // only the subsequent no-follow open can do that.
                            if fchmodat(
                                &parent,
                                name.as_os_str(),
                                Mode::from_bits_truncate(0o700),
                                FchmodatFlags::NoFollowSymlink,
                            )
                            .is_err()
                            {
                                continue;
                            }
                            let Ok(fd) = openat(&parent, name.as_os_str(), flags, Mode::empty())
                            else {
                                continue;
                            };
                            let file = fs::File::from(fd);
                            let after = file.metadata()?;
                            let identity = pakro_store::control::FileIdentity::of(&file)?;
                            if !after.is_dir()
                                || after.uid() != current_uid()
                                || identity.device != before.st_dev
                                || identity.inode != before.st_ino
                            {
                                continue;
                            }
                            file.into()
                        }
                        Err(_) => continue,
                    };
                    fs::File::from(fd)
                }
            };
            let metadata = directory.metadata()?;
            if !metadata.is_dir() || metadata.uid() != current_uid() {
                continue;
            }
            fchmod(&directory, Mode::from_bits_truncate(0o700))?;
            let Ok(mut listing) = nix::dir::Dir::openat(
                &directory,
                ".",
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            ) else {
                continue;
            };
            for entry in listing.iter() {
                let Ok(entry) = entry else {
                    continue;
                };
                let raw = entry.file_name().to_bytes();
                if matches!(raw, b"." | b"..") {
                    continue;
                }
                let name = OsStr::from_bytes(raw);
                let Ok(metadata) = fstatat(&directory, name, AtFlags::AT_SYMLINK_NOFOLLOW) else {
                    continue;
                };
                if metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFDIR {
                    pending.push((
                        Pending::Child {
                            parent: directory.try_clone()?,
                            name: name.to_owned(),
                        },
                        path.join(name),
                    ));
                }
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn walk(&self, components: &[&std::ffi::OsStr]) -> Result<fs::File> {
        let mut directory = self.root.try_clone()?;
        for name in components {
            directory = pakro_store::control::open_directory(&directory, *name)
                .context("source output parent is not a no-follow directory")?;
            pakro_store::control::validate_owned_directory(&directory, current_uid())?;
        }
        Ok(directory)
    }

    #[cfg(not(unix))]
    fn walk(&self, _components: &[&std::ffi::OsStr]) -> Result<fs::File> {
        bail!("source output reads require a supported Unix platform")
    }

    #[cfg(unix)]
    pub(super) fn regular(&self, path: &Path, maximum: u64) -> Result<fs::File> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::Mode;
        use std::os::unix::fs::MetadataExt;
        let components = self.components(path)?;
        let (name, parents) = components
            .split_last()
            .context("source output has no file basename")?;
        let directory = self.walk(parents)?;
        let file = fs::File::from(openat(
            &directory,
            *name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= maximum,
            "source output is not a bounded regular file"
        );
        ensure!(
            metadata.uid() == current_uid() && metadata.nlink() == 1,
            "source output file must be private and not hard-linked"
        );
        Ok(file)
    }

    #[cfg(not(unix))]
    pub(super) fn regular(&self, _path: &Path, _maximum: u64) -> Result<fs::File> {
        bail!("source output reads require a supported Unix platform")
    }
}

struct Bounded<R> {
    input: R,
    deadline: Instant,
    used: u64,
    maximum: u64,
}
impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "source extraction deadline expired",
            ));
        }
        let length = buffer.len().min(
            self.maximum
                .saturating_sub(self.used)
                .saturating_add(1)
                .min(usize::MAX as u64) as usize,
        );
        let count = self.input.read(&mut buffer[..length])?;
        self.used = self
            .used
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("source decoded size overflow"))?;
        if self.used > self.maximum {
            return Err(io::Error::other("source decoded byte budget exceeded"));
        }
        Ok(count)
    }
}

struct Tree<'a> {
    destination: &'a Path,
    bundle: Option<String>,
    seen: BTreeSet<PathBuf>,
    created: BTreeMap<PathBuf, bool>,
    count: usize,
    path_bytes: usize,
    decoded: u64,
    deadline: Instant,
    limits: SourceBuildLimits,
}
impl Tree<'_> {
    fn append(
        &mut self,
        raw: &[u8],
        directory: bool,
        mode: u32,
        size: u64,
        reader: &mut dyn Read,
    ) -> Result<()> {
        checkpoint(self.deadline)?;
        self.count = self
            .count
            .checked_add(1)
            .filter(|value| *value <= self.limits.max_entries)
            .context("source archive entry budget exceeded")?;
        ensure!(
            raw.len() <= self.limits.max_path_bytes,
            "source archive path byte budget exceeded"
        );
        let text = std::str::from_utf8(raw).context("source archive member name is not UTF-8")?;
        ensure!(
            !text.is_empty()
                && !text.starts_with('/')
                && !text.contains('\\')
                && !text.chars().any(char::is_control),
            "source archive member path is unsafe"
        );
        let text = text.trim_end_matches('/');
        let parts: Vec<_> = text.split('/').collect();
        ensure!(
            !parts.is_empty()
                && parts.iter().all(|part| !matches!(*part, "" | "." | ".."))
                && !parts[0].contains(':'),
            "source archive member path traversal is forbidden"
        );
        let bundle = self.bundle.get_or_insert_with(|| parts[0].into());
        ensure!(
            bundle.as_str() == parts[0],
            "source archive contains multiple bundle roots"
        );
        ensure!(
            parts.len() > 1 || directory,
            "source archive has no source bundle directory"
        );
        let relative = PathBuf::from(text);
        ensure!(
            self.seen.insert(relative.clone()),
            "source archive repeats a member path"
        );
        ensure!(
            mode & 0o7000 == 0,
            "source archive contains privileged permissions"
        );
        ensure!(
            size <= self.limits.max_file_bytes,
            "source archive file budget exceeded"
        );
        self.decoded = self
            .decoded
            .checked_add(size)
            .filter(|value| *value <= self.limits.max_decoded_bytes)
            .context("source archive expanded byte budget exceeded")?;
        let target = self.destination.join(&relative);
        if directory {
            ensure!(size == 0, "source archive directory carries file data");
            self.directories(&relative)?;
        } else {
            self.directories(relative.parent().context("source member parent missing")?)?;
            ensure!(
                !self.created.contains_key(&relative),
                "source archive file conflicts with an existing directory"
            );
            self.record_created(&relative, false)?;
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(if mode & 0o111 != 0 { 0o700 } else { 0o600 })
                    .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
            }
            let mut output = options
                .open(&target)
                .context("creating exclusive private source file")?;
            let mut written = 0u64;
            let mut buffer = [0; 64 * 1024];
            loop {
                checkpoint(self.deadline)?;
                let count = reader
                    .read(&mut buffer)
                    .context("source archive member read or CRC failed")?;
                if count == 0 {
                    break;
                }
                written = written
                    .checked_add(count as u64)
                    .filter(|value| *value <= size)
                    .context("source archive member exceeds declared size")?;
                output
                    .write_all(&buffer[..count])
                    .context("writing private source file")?;
            }
            ensure!(written == size, "source archive member is truncated");
        }
        Ok(())
    }

    fn record_created(&mut self, relative: &Path, directory: bool) -> Result<()> {
        ensure!(
            self.created.len() < self.limits.max_entries,
            "source archive actual entry budget exceeded"
        );
        self.path_bytes = self
            .path_bytes
            .checked_add(relative.as_os_str().as_encoded_bytes().len())
            .filter(|value| *value <= self.limits.max_total_path_bytes)
            .context("source archive actual path byte budget exceeded")?;
        self.created.insert(relative.to_owned(), directory);
        Ok(())
    }

    fn directories(&mut self, relative: &Path) -> Result<()> {
        let mut partial = PathBuf::new();
        for component in relative.components() {
            checkpoint(self.deadline)?;
            ensure!(
                matches!(component, std::path::Component::Normal(_)),
                "source archive parent is unsafe"
            );
            partial.push(component);
            if let Some(directory) = self.created.get(&partial) {
                ensure!(*directory, "source archive parent is a file");
                continue;
            }
            self.record_created(&partial, true)?;
            fs::create_dir(self.destination.join(&partial))
                .context("creating an exclusively owned source directory")?;
        }
        Ok(())
    }
}

fn checkpoint(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "source extraction deadline expired"
    );
    Ok(())
}

pub(super) fn extract(
    source: &mut VerifiedArtifact,
    scratch: &Path,
    deadline: Instant,
    limits: SourceBuildLimits,
) -> Result<ExtractedSource> {
    ensure!(
        source.filename().ends_with(".tar.gz") || source.filename().ends_with(".zip"),
        "Python source build accepts only registry tar.gz or zip distributions"
    );
    let tree = extract_raw(source, scratch, deadline, limits)?;
    parse_python_tree(tree, deadline, limits)
}

/// Raw input ownership is independent of Python package declaration parsing.
/// A tar.xz input therefore cannot fall through to a registry package backend.
pub(super) fn extract_raw(
    source: &mut VerifiedArtifact,
    scratch: &Path,
    deadline: Instant,
    limits: SourceBuildLimits,
) -> Result<RawSourceTree> {
    limits.validate()?;
    checkpoint(deadline)?;
    ensure!(
        source.size() <= limits.max_archive_bytes,
        "source archive byte budget exceeded"
    );
    let format = if source.filename().ends_with(".tar.gz") || source.filename().ends_with(".crate") {
        SourceArchiveFormat::TarGz
    } else if source.filename().ends_with(".zip") {
        SourceArchiveFormat::Zip
    } else if source.filename().ends_with(".tar.xz") {
        SourceArchiveFormat::TarXz
    } else {
        bail!("unsupported source archive format")
    };
    fs::create_dir_all(scratch).context("creating source build scratch directory")?;
    let workspace = tempfile::Builder::new()
        .prefix("source-build-")
        .tempdir_in(scratch)
        .context("allocating private source workspace")?;
    let destination = workspace.path().join("source");
    fs::create_dir(&destination)?;
    let reader = WorkspaceReader::open(workspace.path())?;
    let mut tree = Tree {
        destination: &destination,
        bundle: None,
        seen: BTreeSet::new(),
        created: BTreeMap::new(),
        count: 0,
        path_bytes: 0,
        decoded: 0,
        deadline,
        limits,
    };
    source.rewind()?;
    let compressed_size = source.size();
    let decoded_bytes = match format {
        SourceArchiveFormat::TarGz => unpack_tar(
            flate2::read::MultiGzDecoder::new(DeadlineInput {
                input: source,
                deadline,
            }),
            &mut tree,
        )?,
        SourceArchiveFormat::TarXz => {
            // XzDecoder::new/new_multi_decoder use an unbounded default.
            // Keep one explicit stream and reject every unconsumed trailer,
            // including concatenated streams and XZ stream padding.
            let stream =
                xz2::stream::Stream::new_stream_decoder(limits.max_decoder_memory_bytes, 0)
                    .context("creating memory-limited XZ decoder")?;
            let mut decoder = xz2::read::XzDecoder::new_stream(
                DeadlineInput {
                    input: source,
                    deadline,
                },
                stream,
            );
            let decoded = unpack_tar(&mut decoder, &mut tree)?;
            ensure!(
                decoder.total_in() == compressed_size,
                "XZ source has unconsumed trailer or concatenated streams"
            );
            decoded
        }
        SourceArchiveFormat::Zip => unpack_zip(source, &mut tree)?,
    };
    let bundle = tree
        .bundle
        .take()
        .context("source archive contains no source bundle")?;
    let extracted_entries = tree.created.len();
    let extracted_path_bytes = tree.path_bytes;
    drop(tree);
    let root = destination
        .join(bundle)
        .canonicalize()
        .context("source bundle root unavailable")?;
    checkpoint(deadline)?;
    Ok(RawSourceTree {
        workspace,
        native_cleanup: Default::default(),
        reader,
        root,
        format,
        deadline,
        max_file_bytes: limits.max_file_bytes,
        decoded_bytes,
        extracted_entries,
        extracted_path_bytes,
    })
}

fn parse_python_tree(
    tree: RawSourceTree,
    deadline: Instant,
    limits: SourceBuildLimits,
) -> Result<ExtractedSource> {
    let root = &tree.root;
    let reader = &tree.reader;
    let project = root.join("pyproject.toml");
    let raw = if project.exists() {
        let raw = read_bounded(reader, &project, limits.max_metadata_bytes)?;
        String::from_utf8(raw).context("source pyproject.toml is not UTF-8")?
    } else {
        ensure!(
            root.join("setup.py").is_file() || root.join("setup.cfg").is_file(),
            "source bundle has no Python build declaration"
        );
        String::new()
    };
    let parsed = crate::packages::python_declarations::parse_pyproject(&raw, Default::default())?;
    let build = parsed.build_system();
    let initial_requirements: Vec<_> = build
        .requires()
        .map(|value| value.raw_for_semantic_resolution().to_owned())
        .collect();
    ensure!(
        initial_requirements.len() <= limits.max_requirements,
        "source build requirement budget exceeded"
    );
    let backend = match build.backend().module() {
        Some(module) => match build.backend().object() {
            Some(object) => format!("{module}:{object}"),
            None => module.to_owned(),
        },
        None => "setuptools.build_meta:__legacy__".into(),
    };
    let mut backend_paths = Vec::new();
    for token in build.backend_paths() {
        let raw = Path::new(token.raw_for_source_binding());
        ensure!(
            !raw.is_absolute()
                && raw.components().all(|value| matches!(
                    value,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )),
            "source backend-path escapes source root"
        );
        let bound = root
            .join(raw)
            .canonicalize()
            .context("source backend-path is unavailable")?;
        ensure!(
            bound.starts_with(root) && bound.is_dir(),
            "source backend-path escapes source root"
        );
        backend_paths.push(bound);
    }
    checkpoint(deadline)?;
    Ok(ExtractedSource {
        raw: tree,
        initial_requirements,
        backend,
        backend_paths,
    })
}

struct DeadlineInput<R> {
    input: R,
    deadline: Instant,
}
impl<R: Read> Read for DeadlineInput<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "source extraction deadline expired",
            ));
        }
        self.input.read(buffer)
    }
}
impl<R: Seek> Seek for DeadlineInput<R> {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "source extraction deadline expired",
            ));
        }
        self.input.seek(position)
    }
}

fn unpack_tar<R: Read>(input: R, tree: &mut Tree<'_>) -> Result<u64> {
    let mut decoded = Bounded {
        input,
        deadline: tree.deadline,
        used: 0,
        maximum: tree.limits.max_decoded_bytes,
    };
    {
        let mut archive = jdx_tar::Archive::new(&mut decoded);
        for entry in archive.entries()? {
            let mut entry = entry.context("source tar member is invalid")?;
            let directory = match entry.entry_type() {
                jdx_tar::EntryType::Directory => true,
                jdx_tar::EntryType::File => false,
                _ => bail!("source tar links and special files are forbidden"),
            };
            let path = entry.path()?.as_os_str().as_encoded_bytes().to_vec();
            let mode = entry.header().mode();
            let size = entry.size();
            tree.append(&path, directory, mode, size, &mut entry)?;
        }
    }
    // The tar end marker does not finish verification of its compressed input.
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = decoded
            .read(&mut buffer)
            .context("source compression trailer is invalid or truncated")?;
        if count == 0 {
            break;
        }
        ensure!(
            buffer[..count].iter().all(|value| *value == 0),
            "source archive has data after its tar end marker"
        );
    }
    Ok(decoded.used)
}

fn unpack_zip(source: &mut VerifiedArtifact, tree: &mut Tree<'_>) -> Result<u64> {
    use crate::packages::python_wheel::{WheelLimits, zip_directory};
    let limits = WheelLimits {
        max_archive_bytes: tree.limits.max_archive_bytes,
        max_directory_bytes: tree.limits.max_total_path_bytes.min(64 * 1024 * 1024) as u64,
        max_members: tree.limits.max_entries,
        max_member_bytes: tree.limits.max_file_bytes,
        max_total_uncompressed: tree.limits.max_decoded_bytes,
        ..Default::default()
    };
    let mut input = DeadlineInput {
        input: source,
        deadline: tree.deadline,
    };
    let directory = zip_directory::preflight(&mut input, limits)?;
    let view = zip_directory::ValidatedZipView::new(input, directory.eocd_end)?;
    let mut archive = zip::ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        view,
    )
    .context("source ZIP metadata cannot be decoded")?;
    ensure!(
        archive.len() == directory.entries.len(),
        "source ZIP member count changed after preflight"
    );
    for (index, expected) in directory.entries.iter().enumerate() {
        checkpoint(tree.deadline)?;
        let mut member = archive
            .by_index(index)
            .context("source ZIP member cannot be decoded")?;
        ensure!(
            member.name_raw() == expected.archive_path.as_bytes()
                && member.size() == expected.size
                && member.compressed_size() == expected.compressed_size
                && member.header_start() == expected.header_start
                && member.central_header_start() == expected.central_start
                && member.data_start() == Some(expected.data_start)
                && member.is_dir() == expected.is_dir,
            "source ZIP decoded member differs from bounded central directory"
        );
        let mode = member
            .unix_mode()
            .unwrap_or(if expected.is_dir { 0o040755 } else { 0o100644 });
        let kind = mode & 0o170000;
        ensure!(
            !member.is_symlink()
                && (kind == 0
                    || if expected.is_dir {
                        kind == 0o040000
                    } else {
                        kind == 0o100000
                    }),
            "source ZIP links and special files are forbidden"
        );
        // Always retain the raw spelling: the generic ZIP preflight may
        // normalize a/../b, whereas source trees forbid every such component.
        tree.append(
            expected.archive_path.as_bytes(),
            expected.is_dir,
            mode,
            expected.size,
            &mut member,
        )?;
        if expected.is_dir {
            let mut tail = [0; 1];
            ensure!(
                member.read(&mut tail)? == 0,
                "source ZIP directory contains data"
            );
        }
    }
    checkpoint(tree.deadline)?;
    Ok(tree.decoded)
}

pub(super) fn read_bounded(
    workspace: &WorkspaceReader,
    path: &Path,
    maximum: u64,
) -> Result<Vec<u8>> {
    let file = workspace.regular(path, maximum)?;
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "source build output byte budget exceeded"
    );
    Ok(bytes)
}
