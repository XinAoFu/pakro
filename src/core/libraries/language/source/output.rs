//! Fixed source-Python layout adaptation. Raw ownership facts stay unchanged.
use super::{LOGICAL_PREFIX, SourcePythonLayout};
use crate::packages::python_environment::prefix::verify::{TreeInventory, TreeInventoryKind};
use eyre::{ContextCompat, Result, ensure};
use sha2::{Digest as _, Sha256};
use std::path::{Component, Path, PathBuf};

const METADATA_BYTES: u64 = 16 * 1024 * 1024;

/// One admission of the platform inspector's bounded output. This recipe
/// publishes a self-contained CPython, with only the OS runtime left external.
pub(super) fn verify_system_runtime_dependencies(output: &str) -> Result<std::collections::BTreeSet<String>> {
    let mut libraries = std::collections::BTreeSet::new();
    #[cfg(target_os = "macos")]
    for line in output.lines().skip(1).map(str::trim).filter(|line| !line.is_empty()) {
        let (library, _) = line.split_once(" (compatibility version ")
            .context("unsupported source CPython Mach-O dependency report")?;
        path_text(Path::new(library))?;
        ensure!(Path::new(library).starts_with("/usr/lib") || Path::new(library).starts_with("/System/Library"),
            "source CPython has an unsupported non-system dynamic dependency: {library}; provide static development libraries");
        libraries.insert(library.to_owned());
    }
    #[cfg(target_os = "linux")]
    for line in output.lines() {
        ensure!(!line.contains("(RPATH)") && !line.contains("(RUNPATH)"),
            "source CPython dynamic search paths are unsupported; provide static development libraries");
        if line.contains("(NEEDED)") {
            let library = line.split_once('[').and_then(|(_, tail)| tail.strip_suffix(']'))
                .context("unsupported source CPython ELF dependency report")?;
            ensure!(!library.is_empty() && !library.contains('/') && library.len() <= 256,
                "source CPython has an unsupported non-system dynamic dependency: {library}; provide static development libraries");
            libraries.insert(library.to_owned());
        }
        if let Some((_, tail)) = line.split_once("[Requesting program interpreter: ") {
            let loader = Path::new(tail.strip_suffix(']').context("unsupported source CPython ELF interpreter report")?);
            ensure!(["/lib", "/lib64", "/usr/lib", "/usr/lib64"].iter().any(|root| loader.starts_with(root)),
                "source CPython depends on a non-system ELF loader");
        }
    }
    Ok(libraries)
}

pub(crate) fn configuration_file(path: &Path) -> bool {
    let Some(parts) = path.components().map(|part| match part {
        Component::Normal(name) => name.to_str(), _ => None,
    }).collect::<Option<Vec<_>>>() else { return false; };
    let Some(name) = parts.last() else { return false; };
    (parts.first() == Some(&"lib") && name.starts_with("_sysconfigdata_") && name.ends_with(".py"))
        || (parts.len() == 4 && parts[0] == "lib" && parts[1].starts_with("python")
            && parts[2].starts_with("config-") && matches!(*name, "Makefile" | "python-config.py"))
        || (parts.len() == 2 && parts[0] == "bin" && name.ends_with("-config"))
        || (parts.len() == 2 && parts[0] == "bin" && ["pydoc", "idle", "2to3-"]
            .iter().any(|prefix| name.strip_prefix(prefix).is_some_and(|version|
                !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.'))))
        || (parts.len() == 3 && parts[0] == "lib" && parts[1] == "pkgconfig" && name.ends_with(".pc"))
}

fn path_text(path: &Path) -> Result<&str> {
    let text = path.to_str().context("source Python layout path is not UTF-8")?;
    ensure!(path.is_absolute() && text.len() <= 16384 && !text.chars().any(char::is_control)
        && path.components().all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "source Python layout path is not a fixed absolute path");
    Ok(text)
}

fn substitutions(layout: &SourcePythonLayout, prefix: &Path) -> Result<Vec<(String, String)>> {
    ensure!(layout.schema == 1 && layout.bindings.len() <= 128, "unsupported source CPython local layout");
    let mut replacements = vec![(path_text(prefix)?.to_owned(), LOGICAL_PREFIX.to_owned())];
    for (name, path) in &layout.bindings {
        ensure!(!name.is_empty() && name.len() <= 128
            && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            && path != Path::new("/"), "invalid source Python local binding");
        replacements.push((path_text(path)?.to_owned(), format!("@{name}")));
    }
    replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.1.cmp(&b.1)));
    Ok(replacements)
}

fn replace_metadata(bytes: &[u8], replacements: &[(String, String)]) -> Result<Vec<u8>> {
    ensure!(bytes.len() as u64 <= METADATA_BYTES, "source Python metadata byte limit exceeded");
    let mut text = std::str::from_utf8(bytes)?.to_owned();
    ensure!(!text.contains('\0'), "source Python metadata contains NUL");
    for (from, to) in replacements {
        text = text.replace(from, to);
        ensure!(text.len() as u64 <= METADATA_BYTES, "adapted source Python metadata byte limit exceeded");
    }
    Ok(text.into_bytes())
}

fn hash_entry(hash: &mut Sha256, relative: &Path, mode: u32, kind: &TreeInventoryKind) {
    hash.update(relative.as_os_str().as_encoded_bytes());
    hash.update([0]);
    hash.update(mode.to_le_bytes());
    match kind {
        TreeInventoryKind::Directory => hash.update(b"directory\0"),
        TreeInventoryKind::Symlink { target } => {
            hash.update(b"symlink\0"); hash.update(target.as_os_str().as_encoded_bytes()); hash.update([0]);
        }
        TreeInventoryKind::File { size, sha256 } => {
            hash.update(b"file\0"); hash.update(size.to_le_bytes()); hash.update(sha256);
        }
    }
}

/// The caller has already verified this complete raw inventory under its real
/// owner/lease. Only the fixed metadata list needs another, different view.
pub(crate) fn portable_output_digest(inventory: &TreeInventory, prefix: &Path,
    layout: &SourcePythonLayout, mut read: impl FnMut(&Path) -> Result<Vec<u8>>) -> Result<[u8; 32]> {
    let replacements = substitutions(layout, prefix)?;
    let mut entries = inventory.entries().iter().collect::<Vec<_>>();
    entries.sort_by(|a, b| a.relative.cmp(&b.relative));
    let mut hash = Sha256::new_with_prefix(b"pakro.source-python.output/v1\0");
    for entry in entries {
        if let TreeInventoryKind::File { size, .. } = &entry.kind && configuration_file(&entry.relative) {
            ensure!(*size <= METADATA_BYTES, "source Python metadata byte limit exceeded");
            let bytes = read(&entry.relative)?;
            ensure!(bytes.len() as u64 == *size, "source Python metadata differs from its verified inventory size");
            let canonical = replace_metadata(&bytes, &replacements)?;
            hash_entry(&mut hash, &entry.relative, entry.mode, &TreeInventoryKind::File {
                size: canonical.len() as u64, sha256: Sha256::digest(&canonical).into(),
            });
        } else {
            hash_entry(&mut hash, &entry.relative, entry.mode, &entry.kind);
        }
    }
    Ok(hash.finalize().into())
}

#[cfg(unix)]
pub(crate) use staged::{import_staged, stage_digest};

#[cfg(unix)]
mod staged {
    use super::*;
    use crate::libraries::language::repository::LanguageBuildOwner;
    use crate::packages::python_environment::prefix::verify::PrefixInventoryLimits;
    use crate::programs::OperationContext;
    use std::{collections::{BTreeMap, BTreeSet}, fs::{self, OpenOptions}, io::{Read, Write}, os::unix::fs::{MetadataExt, OpenOptionsExt}};

    enum Kind { Directory, File, Link(PathBuf) }
    struct Entry { relative: PathBuf, path: PathBuf, mode: u32, kind: Kind }

    fn entries(context: &OperationContext, stage: &Path) -> Result<Vec<Entry>> {
        context.checkpoint()?;
        ensure!(fs::symlink_metadata(stage)?.is_dir(), "source Python staging payload is not a directory");
        let limits = PrefixInventoryLimits::default();
        let mut values = Vec::new();
        let mut path_bytes = 0usize;
        for entry in walkdir::WalkDir::new(stage).follow_links(false) {
            context.checkpoint()?;
            let entry = entry?;
            let relative = entry.path().strip_prefix(stage)?.to_owned();
            if relative.as_os_str().is_empty() { continue; }
            ensure!(relative.components().all(|part| matches!(part, Component::Normal(_)))
                && relative.components().count() <= limits.max_components
                && relative.as_os_str().as_encoded_bytes().len() <= limits.max_path_bytes,
                "unsafe source Python staged member path");
            path_bytes = path_bytes.checked_add(relative.as_os_str().as_encoded_bytes().len())
                .filter(|count| *count <= limits.max_total_path_bytes).context("source Python staged path budget exceeded")?;
            ensure!(values.len() < limits.max_files + limits.max_directories, "source Python staged entry budget exceeded");
            let metadata = fs::symlink_metadata(entry.path())?;
            let mode = metadata.mode() & 0o7777;
            let kind = if metadata.is_dir() {
                ensure!(mode & !0o777 == 0 && mode & 0o022 == 0 && mode & 0o500 == 0o500, "unsafe source Python staged directory mode");
                Kind::Directory
            } else if metadata.is_file() {
                ensure!(mode & !0o777 == 0 && mode & 0o022 == 0 && mode & 0o400 != 0
                    && metadata.len() <= limits.max_file_bytes, "unsafe source Python staged file");
                Kind::File
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(entry.path())?;
                let target = if target.is_absolute() {
                    let inside = target.strip_prefix(LOGICAL_PREFIX).or_else(|_| target.strip_prefix(stage))
                        .map_err(|_| eyre::eyre!("source Python staged link points outside its installation"))?;
                    let mut contained = PathBuf::new();
                    for _ in relative.parent().unwrap().components() { contained.push(".."); }
                    contained.push(inside); contained
                } else { target };
                let mut depth = relative.parent().unwrap().components().count();
                for part in target.components() {
                    match part {
                        Component::Normal(_) => depth += 1,
                        Component::CurDir => {},
                        Component::ParentDir => { ensure!(depth > 0, "source Python staged link escapes its payload"); depth -= 1; },
                        _ => eyre::bail!("source Python staged link is not relative"),
                    }
                }
                Kind::Link(target)
            } else { eyre::bail!("source Python staged member is not a file, directory or link"); };
            values.push(Entry { relative, path: entry.path().to_owned(), mode, kind });
        }
        // Only bytecode derived from the adapted sysconfig source is discarded.
        let modules = values.iter().filter(|entry| matches!(entry.kind, Kind::File)
            && configuration_file(&entry.relative) && entry.relative.file_name().is_some_and(|name| name.to_string_lossy().starts_with("_sysconfigdata_")))
            .map(|entry| (entry.relative.parent().unwrap().to_owned(), entry.relative.file_stem().unwrap().to_string_lossy().into_owned())).collect::<BTreeSet<_>>();
        values.retain(|entry| {
            let name = entry.relative.file_name().unwrap().to_string_lossy();
            if matches!(entry.kind, Kind::Directory) || !name.ends_with(".pyc") { return true; }
            let parent = entry.relative.parent().unwrap();
            !modules.iter().any(|(directory, module)| {
                (parent == directory && name == format!("{module}.pyc"))
                    || (parent.file_name().is_some_and(|name| name == "__pycache__") && parent.parent() == Some(directory.as_path())
                        && name.starts_with(&format!("{module}.")))
            })
        });
        let main = values.iter().filter(|entry| entry.relative.parent() == Some(Path::new("bin"))
            && matches!(entry.kind, Kind::File) && entry.mode & 0o111 != 0
            && entry.relative.file_name().and_then(|name| name.to_str()).is_some_and(|name|
                name.strip_prefix("python3.").is_some_and(|minor| !minor.is_empty() && minor.bytes().all(|byte| byte.is_ascii_digit()))))
            .map(|entry| entry.relative.file_name().unwrap().to_owned()).collect::<Vec<_>>();
        ensure!(main.len() == 1, "source Python staging needs exactly one regular ordinary python3.N executable");
        for name in ["python", "python3"] {
            let relative = Path::new("bin").join(name);
            if !values.iter().any(|entry| entry.relative == relative) {
                values.push(Entry { relative, path: PathBuf::new(), mode: 0o777, kind: Kind::Link(PathBuf::from(main[0].clone())) });
            }
        }
        values.sort_by(|a, b| a.relative.cmp(&b.relative));
        Ok(values)
    }

    fn adaptation(source: &Path, build: &Path, tools: &Path, prefix: &Path) -> Result<Vec<(String, String)>> {
        let mut values = vec![(path_text(source)?.into(), path_text(prefix)?.into()),
            (path_text(build)?.into(), path_text(prefix)?.into()), (LOGICAL_PREFIX.into(), path_text(prefix)?.into()),
            (format!("{}/", path_text(tools)?), String::new())];
        values.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.1.cmp(&b.1)));
        Ok(values)
    }

    pub(crate) fn import_staged(context: &OperationContext, stage: &Path, owner: &mut LanguageBuildOwner,
        source_root: &Path, build_root: &Path, tool_bin: &Path, bindings: BTreeMap<String, PathBuf>)
        -> Result<(TreeInventory, SourcePythonLayout, [u8; 32])> {
        context.checkpoint()?; owner.checkpoint()?;
        ensure!(owner.spec().plan().source_build().is_some(), "source Python import needs its reserved source-build owner");
        let layout = SourcePythonLayout { schema: 1, bindings, system_runtime: None };
        substitutions(&layout, owner.final_path())?;
        let adapted = adaptation(source_root, build_root, tool_bin, owner.final_path())?;
        for entry in entries(context, stage)? {
            context.checkpoint()?;
            match entry.kind {
                Kind::Directory => owner.create_dir(&entry.relative, entry.mode)?,
                Kind::Link(target) => owner.create_symlink(&entry.relative, &target)?,
                Kind::File => {
                    let mut input = OpenOptions::new().read(true).custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC).open(&entry.path)?;
                    let mut output = owner.create_file_exclusive(&entry.relative, entry.mode)?;
                    if configuration_file(&entry.relative) {
                        let mut bytes = Vec::new();
                        Read::by_ref(&mut input).take(METADATA_BYTES + 1).read_to_end(&mut bytes)?;
                        output.write_all(&replace_metadata(&bytes, &adapted)?)?;
                    } else {
                        let mut buffer = [0u8; 32768];
                        loop { context.checkpoint()?; let count = input.read(&mut buffer)?; if count == 0 { break; } output.write_all(&buffer[..count])?; }
                    }
                    output.finish()?;
                }
            }
        }
        let inventory = owner.capture_inventory()?;
        let digest = portable_output_digest(&inventory, owner.final_path(), &layout, |path| owner.read_file(path, METADATA_BYTES))?;
        Ok((inventory, layout, digest))
    }

    /// Candidate comparison for an Existing reservation. This makes no owner
    /// or publication receipt and does not mutate the staged installation.
    pub(crate) fn stage_digest(context: &OperationContext, stage: &Path, source_root: &Path,
        build_root: &Path, tool_bin: &Path, bindings: &BTreeMap<String, PathBuf>) -> Result<[u8; 32]> {
        let layout = SourcePythonLayout { schema: 1, bindings: bindings.clone(), system_runtime: None };
        let adapted = adaptation(source_root, build_root, tool_bin, Path::new(LOGICAL_PREFIX))?;
        let canonical = substitutions(&layout, Path::new(LOGICAL_PREFIX))?;
        let mut hash = Sha256::new_with_prefix(b"pakro.source-python.output/v1\0");
        let limits = PrefixInventoryLimits::default();
        let mut total = 0u64;
        for entry in entries(context, stage)? {
            context.checkpoint()?;
            let kind = match entry.kind {
                Kind::Directory => TreeInventoryKind::Directory,
                Kind::Link(target) => TreeInventoryKind::Symlink { target },
                Kind::File => {
                    let mut input = OpenOptions::new().read(true).custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC).open(&entry.path)?;
                    let mut digest = Sha256::new();
                    let mut size = 0u64;
                    if configuration_file(&entry.relative) {
                        let mut bytes = Vec::new();
                        Read::by_ref(&mut input).take(METADATA_BYTES + 1).read_to_end(&mut bytes)?;
                        let bytes = replace_metadata(&replace_metadata(&bytes, &adapted)?, &canonical)?;
                        size = bytes.len() as u64; digest.update(bytes);
                    } else {
                        let mut buffer = [0u8; 32768];
                        loop { context.checkpoint()?; let count = input.read(&mut buffer)?; if count == 0 { break; }
                            size = size.checked_add(count as u64).filter(|n| *n <= limits.max_file_bytes).context("source Python staged file budget exceeded")?;
                            digest.update(&buffer[..count]); }
                    }
                    total = total.checked_add(size).filter(|n| *n <= limits.max_total_bytes).context("source Python staged byte budget exceeded")?;
                    TreeInventoryKind::File { size, sha256: digest.finalize().into() }
                }
            };
            hash_entry(&mut hash, &entry.relative, entry.mode, &kind);
        }
        Ok(hash.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p6_source_python_config_script_uses_the_same_adaptation_and_portable_view() {
        let relative=Path::new("lib/python3.13/config-3.13-darwin/python-config.py");
        assert!(configuration_file(relative));
        let original=format!("#!{LOGICAL_PREFIX}/bin/python3.13\nprint('config')\n");
        for prefix in ["/owned/first/payload","/owned/second/payload"] {
            let adapted=replace_metadata(original.as_bytes(),&[(LOGICAL_PREFIX.into(),prefix.into())]).unwrap();
            assert!(std::str::from_utf8(&adapted).unwrap().starts_with(&format!("#!{prefix}/bin/python3.13\n")));
            let layout=SourcePythonLayout {schema:1,bindings:Default::default(),system_runtime:None};
            assert_eq!(replace_metadata(&adapted,&substitutions(&layout,Path::new(prefix)).unwrap()).unwrap(),original.as_bytes());
        }
    }

    #[test]
    fn p6_source_runtime_rejects_external_development_library_dependencies() {
        #[cfg(target_os = "macos")]
        {
            let system = "/payload/bin/python3.13:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1351.0.0)\n";
            verify_system_runtime_dependencies(system).unwrap();
            for external in ["/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib", "@rpath/libssl.3.dylib", "@loader_path/../../sdk/libssl.3.dylib"] {
                let report = format!("{system}\t{external} (compatibility version 3.0.0, current version 3.0.0)\n");
                assert!(verify_system_runtime_dependencies(&report).is_err(), "accepted {external}");
            }
        }
        #[cfg(target_os = "linux")]
        {
            let system = "      [Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]\n 0x0000000000000001 (NEEDED) Shared library: [libc.so.6]\n";
            verify_system_runtime_dependencies(system).unwrap();
            verify_system_runtime_dependencies(&format!("{system} 0x1 (NEEDED) Shared library: [libssl.so.3]\n")).unwrap();
            for external in ["/sdk/lib/libz.so.1", "/usr/local/lib/libssl.so.3"] {
                assert!(verify_system_runtime_dependencies(&format!("{system} 0x1 (NEEDED) Shared library: [{external}]\n")).is_err());
            }
            assert!(verify_system_runtime_dependencies(&format!("{system} 0x1 (RUNPATH) Library runpath: [/sdk/lib]\n")).is_err());
        }
    }
}
