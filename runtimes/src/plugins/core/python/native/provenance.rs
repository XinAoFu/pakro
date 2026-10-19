//! Provenance receipts for the frozen native installation context.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;

use eyre::{Result, ensure};
use nix::errno::Errno;
use nix::fcntl::{OFlag, openat};
use nix::sys::stat::{Mode, fchmod, mkdirat};
use nix::unistd::{UnlinkatFlags, unlinkat};
use pakro_index::artifact::SourceArtifact;
use pakro_store::control::{
    FileIdentity, ensure_entry_identity, open_directory, open_existing_readonly_regular,
    open_trusted_store_with_deadline, rename_noclobber, validate_owned_directory,
    validate_owned_regular,
};

use super::super::{native_provenance_name, native_provenance_record};
use crate::programs::OperationContext;

const MAX_RECORD_BYTES: usize = 64 * 1024;
const COMPONENTS: [&str; 3] = ["python", "native-v1", "provenance"];

fn record_bytes(source: &SourceArtifact) -> Result<Vec<u8>> {
    // JSON can expand a source byte to six bytes. Reserve space for the fixed
    // fields and digest before asking the existing encoder to allocate.
    let bound = source
        .filename()
        .len()
        .checked_add(source.url().as_str().len())
        .and_then(|size| size.checked_mul(6))
        .and_then(|size| size.checked_add(1024));
    ensure!(
        bound.is_some_and(|size| size <= MAX_RECORD_BYTES),
        "native provenance record exceeds its byte budget"
    );
    let bytes = native_provenance_record(source)?;
    ensure!(
        bytes.len() <= MAX_RECORD_BYTES,
        "native provenance record is too large"
    );
    Ok(bytes)
}

fn missing(error: &eyre::Report) -> bool {
    error.downcast_ref::<Errno>() == Some(&Errno::ENOENT)
}

struct ProofDirectory<'context> {
    context: &'context OperationContext,
    root: File,
    children: Vec<(&'static str, File)>,
    uid: u32,
}

impl<'context> ProofDirectory<'context> {
    fn open(context: &'context OperationContext, create: bool) -> Result<Option<Self>> {
        context.checkpoint()?;
        if create {
            ensure!(
                context.policy().writes.libraries,
                "native provenance write is not authorized"
            );
        }
        let uid = nix::unistd::geteuid().as_raw();
        let root = match open_trusted_store_with_deadline(
            context.requested_cache_root(),
            uid,
            create,
            context.policy().deadline,
        ) {
            Ok(root) => root,
            Err(error) if !create && missing(&error) => {
                context.checkpoint()?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let mut directory = Self {
            context,
            root,
            children: Vec::new(),
            uid,
        };
        for name in COMPONENTS {
            directory.check_binding()?;
            let parent = directory.file();
            let child = match open_directory(parent, name) {
                Ok(child) => child,
                Err(error) if missing(&error) && create => {
                    context.checkpoint()?;
                    let created = match mkdirat(parent, name, Mode::from_bits_truncate(0o700)) {
                        Ok(()) => true,
                        Err(Errno::EEXIST) => false,
                        Err(error) => return Err(error.into()),
                    };
                    let child = open_directory(parent, name)?;
                    if created {
                        fchmod(&child, Mode::from_bits_truncate(0o700))?;
                        context.checkpoint()?;
                        child.sync_all()?;
                        parent.sync_all()?;
                    }
                    child
                }
                Err(error) if missing(&error) => {
                    directory.check_binding()?;
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            // Existing legacy receipt directories may be 0755. Validate them
            // without changing their permissions or following aliases.
            validate_owned_directory(&child, uid)?;
            ensure_entry_identity(parent, name, FileIdentity::of(&child)?)?;
            directory.children.push((name, child));
            directory.check_binding()?;
        }
        Ok(Some(directory))
    }

    fn file(&self) -> &File {
        self.children.last().map_or(&self.root, |(_, file)| file)
    }

    fn check_binding(&self) -> Result<()> {
        self.context.checkpoint()?;
        let named = open_trusted_store_with_deadline(
            self.context.requested_cache_root(),
            self.uid,
            false,
            self.context.policy().deadline,
        )?;
        ensure!(
            FileIdentity::of(&named)? == FileIdentity::of(&self.root)?,
            "native provenance cache root changed"
        );
        validate_owned_directory(&self.root, self.uid)?;
        let mut parent = &self.root;
        for (name, child) in &self.children {
            ensure_entry_identity(parent, name, FileIdentity::of(child)?)?;
            validate_owned_directory(child, self.uid)?;
            parent = child;
        }
        self.context.checkpoint()?;
        Ok(())
    }

    fn validate_file(&self, name: &str, file: &File) -> Result<()> {
        self.check_binding()?;
        validate_owned_regular(file, self.uid)?;
        ensure!(
            file.metadata()?.mode() & 0o7777 == 0o600,
            "native provenance record is not private metadata"
        );
        ensure_entry_identity(self.file(), name, FileIdentity::of(file)?)?;
        self.context.checkpoint()?;
        Ok(())
    }

    fn contains(&self, name: &str, expected: &[u8]) -> Result<bool> {
        self.check_binding()?;
        let mut file = match open_existing_readonly_regular(self.file(), name, self.uid) {
            Ok(file) => file,
            Err(error) if missing(&error) => {
                self.check_binding()?;
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        self.validate_file(name, &file)?;
        let before = file.metadata()?;
        ensure!(
            before.len() == expected.len() as u64,
            "native provenance record has changed size"
        );
        let mut actual = Vec::with_capacity(expected.len());
        let mut buffer = [0u8; 4096];
        loop {
            self.validate_file(name, &file)?;
            let limit = buffer.len().min(expected.len() + 1 - actual.len());
            let count = file.read(&mut buffer[..limit])?;
            if count == 0 {
                break;
            }
            ensure!(
                actual.len() + count <= expected.len(),
                "native provenance record grew while reading"
            );
            actual.extend_from_slice(&buffer[..count]);
        }
        self.validate_file(name, &file)?;
        let after = file.metadata()?;
        ensure!(
            (
                before.len(),
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec()
            ) == (
                after.len(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            ),
            "native provenance record changed while reading"
        );
        ensure!(
            actual == expected,
            "native provenance record is corrupt or belongs to another source"
        );
        self.check_binding()?;
        Ok(true)
    }

    fn publish(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.check_binding()?;
        let temporary = format!(".tmp-{}", uuid::Uuid::now_v7().simple());
        let mut file = File::from(openat(
            self.file(),
            temporary.as_str(),
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_EXCL
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::from_bits_truncate(0o600),
        )?);
        let identity = FileIdentity::of(&file)?;
        let mut published = false;
        let result = (|| {
            fchmod(&file, Mode::from_bits_truncate(0o600))?;
            self.validate_file(&temporary, &file)?;
            file.write_all(bytes)?;
            file.flush()?;
            self.validate_file(&temporary, &file)?;
            file.sync_all()?;
            self.validate_file(&temporary, &file)?;
            match rename_noclobber(
                self.file(),
                OsStr::new(&temporary),
                self.file(),
                OsStr::new(name),
            ) {
                Ok(()) => published = true,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists) => {
                }
                Err(error) => return Err(error),
            }
            self.check_binding()?;
            self.file().sync_all()?;
            ensure!(
                self.contains(name, bytes)?,
                "published native provenance record disappeared"
            );
            Ok(())
        })();
        // Never remove the final name, including after an uncertain durability
        // result. A cancelled or changed directory retains its temporary evidence.
        if !published
            && self.validate_file(&temporary, &file).is_ok()
            && ensure_entry_identity(self.file(), &temporary, identity).is_ok()
        {
            let _ = unlinkat(self.file(), temporary.as_str(), UnlinkatFlags::NoRemoveDir);
        }
        result
    }
}

pub(in crate::plugins::core::python) fn cached(
    context: &OperationContext,
    source: &SourceArtifact,
) -> Result<bool> {
    context.checkpoint()?;
    let bytes = record_bytes(source)?;
    let name = native_provenance_name(&bytes);
    let Some(directory) = ProofDirectory::open(context, false)? else {
        return Ok(false);
    };
    directory.contains(&name, &bytes)
}

/// Only the native install consumer calls this, after verification or an exact
/// trusted lock, and after validating the actual held archive's digest.
pub(in crate::plugins::core::python) fn record(
    context: &OperationContext,
    source: &SourceArtifact,
) -> Result<()> {
    context.checkpoint()?;
    ensure!(
        context.policy().writes.libraries,
        "native provenance write is not authorized"
    );
    let bytes = record_bytes(source)?;
    let name = native_provenance_name(&bytes);
    let directory = ProofDirectory::open(context, true)?
        .ok_or_else(|| eyre::eyre!("native provenance directory is missing"))?;
    if directory.contains(&name, &bytes)? {
        return Ok(());
    }
    directory.publish(&name, &bytes)
}

#[cfg(all(test, feature = "self_update"))]
mod tests;
