//! Wheel materialization into an owned unpublished generation only.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use eyre::{ContextCompat, Result, ensure, eyre};
use sha2::{Digest as _, Sha256};

use super::super::python_environment::prefix::{PrefixFile, UnpublishedPrefix};
use super::super::python_wheel_plan::{
    WheelFilePlan, WheelOwner, WheelPlanSource, WheelWriteOperation,
};
use super::{OutputKey, PreparedBytes, PreparedOutput, PreparedWheelGraph, PreparedWheelSource};

pub(crate) struct WheelMaterializationReceipt {
    final_prefix: PathBuf,
    allocation_id: String,
    sources: Vec<PreparedWheelSource>,
    files: Vec<MaterializedWheelFile>,
    directories: Vec<MaterializedWheelDirectory>,
}
impl WheelMaterializationReceipt {
    pub(crate) fn final_prefix(&self) -> &Path {
        &self.final_prefix
    }
    pub(crate) fn allocation_id(&self) -> &str {
        &self.allocation_id
    }
    pub(crate) fn sources(&self) -> &[PreparedWheelSource] {
        &self.sources
    }
    pub(crate) fn files(&self) -> &[MaterializedWheelFile] {
        &self.files
    }
    pub(crate) fn directories(&self) -> &[MaterializedWheelDirectory] {
        &self.directories
    }
}
impl fmt::Debug for WheelMaterializationReceipt {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("WheelMaterializationReceipt")
            .field("allocation_id", &self.allocation_id)
            .field("source_count", &self.sources.len())
            .field("file_count", &self.files.len())
            .field("directory_count", &self.directories.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MaterializedWheelFile {
    relative: PathBuf,
    sha256: [u8; 32],
    size: u64,
    mode: u32,
    operation: WheelWriteOperation,
    owners: Vec<WheelOwner>,
}
impl MaterializedWheelFile {
    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn mode(&self) -> u32 {
        self.mode
    }
    pub(crate) fn operation(&self) -> WheelWriteOperation {
        self.operation
    }
    pub(crate) fn owners(&self) -> &[WheelOwner] {
        &self.owners
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MaterializedWheelDirectory {
    relative: PathBuf,
    owners: Vec<WheelOwner>,
}
impl MaterializedWheelDirectory {
    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative
    }
    pub(crate) fn owners(&self) -> &[WheelOwner] {
        &self.owners
    }
}

struct ObservedWriter<'a> {
    output: PrefixFile<'a>,
    hasher: Sha256,
    size: u64,
    maximum: u64,
}
impl Write for ObservedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.maximum.saturating_sub(self.size) {
            return Err(std::io::Error::other(
                "wheel final output exceeds planned size",
            ));
        }
        let count = self.output.write(bytes)?;
        self.hasher.update(&bytes[..count]);
        self.size = self
            .size
            .checked_add(count as u64)
            .ok_or_else(|| std::io::Error::other("wheel final output size overflow"))?;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}
impl ObservedWriter<'_> {
    fn finish(self, relative: &Path, plan: &WheelFilePlan) -> Result<MaterializedWheelFile> {
        let sha256: [u8; 32] = self.hasher.finalize().into();
        ensure!(
            self.size == plan.size() && &sha256 == plan.sha256(),
            "wheel final output statistics disagree with plan"
        );
        let receipt = self.output.finish()?;
        ensure!(
            receipt.relative_path() == relative
                && receipt.size() == plan.size()
                && receipt.mode() == plan.mode(),
            "wheel prefix writer receipt disagrees with plan"
        );
        Ok(MaterializedWheelFile {
            relative: relative.into(),
            sha256,
            size: receipt.size(),
            mode: receipt.mode(),
            operation: plan.operation(),
            owners: plan.owners().to_vec(),
        })
    }
}

fn relative_destination<'a>(prefix: &Path, file: &'a Path) -> Result<&'a Path> {
    file.strip_prefix(prefix)
        .map_err(|_| eyre!("wheel destination escapes the bound final prefix"))
}

fn copy_output(output: &mut PreparedOutput, writer: &mut ObservedWriter<'_>) -> Result<()> {
    match &mut output.bytes {
        PreparedBytes::Memory(bytes) => {
            for chunk in bytes.chunks(64 * 1024) {
                writer
                    .write_all(chunk)
                    .map_err(|_| eyre!("wheel generated output write failed"))?;
            }
        }
        PreparedBytes::Spool(file) => {
            file.rewind()
                .map_err(|_| eyre!("wheel generated output seek failed"))?;
            let mut buffer = [0; 64 * 1024];
            loop {
                let count = file
                    .read(&mut buffer)
                    .map_err(|_| eyre!("wheel generated output read failed"))?;
                if count == 0 {
                    break;
                }
                writer
                    .write_all(&buffer[..count])
                    .map_err(|_| eyre!("wheel generated output write failed"))?;
            }
        }
    }
    Ok(())
}

fn attempt(
    prepared: &mut PreparedWheelGraph,
    prefix: &mut UnpublishedPrefix,
    after_directories: impl FnOnce(&mut PreparedWheelGraph, &mut UnpublishedPrefix) -> Result<()>,
) -> Result<WheelMaterializationReceipt> {
    let root = prefix.final_path().to_path_buf();
    let mut directories = Vec::new();
    for directory in prepared.complete.plan().directories() {
        let relative = relative_destination(&root, directory.destination())?;
        prefix.create_dir_all(relative)?;
        directories.push(MaterializedWheelDirectory {
            relative: relative.into(),
            owners: directory.owners().to_vec(),
        });
    }
    after_directories(prepared, prefix)?;
    let mut written = BTreeSet::new();
    let mut files = Vec::new();
    let PreparedWheelGraph {
        archives,
        outputs,
        complete,
        ..
    } = prepared;
    let mut actions = BTreeMap::<WheelOwner, Vec<(&WheelFilePlan, &WheelPlanSource, bool)>>::new();
    for plan in complete.plan().files() {
        for (index, source) in plan.sources().iter().enumerate() {
            if let WheelPlanSource::Archive { owner, .. } = source {
                actions
                    .entry(owner.clone())
                    .or_default()
                    .push((plan, source, index == 0));
            }
        }
    }
    // Every wheel has one member session in this copying phase. A co-owned
    // destination is written only by its canonical first source; every other
    // owned source is still independently streamed through its CRC/SHA gate.
    for (owner, archive) in archives {
        archive.with_members(|session| {
            for &(plan, source, primary) in actions
                .get(owner)
                .context("wheel archive copy actions are missing")?
            {
                let WheelPlanSource::Archive {
                    owner: source_owner,
                    archive_index,
                    raw_sha256,
                    raw_size,
                    ..
                } = source
                else {
                    continue;
                };
                ensure!(
                    source_owner == owner,
                    "wheel source action belongs to another owner"
                );
                let relative = relative_destination(&root, plan.destination())?;
                if primary && plan.operation() == WheelWriteOperation::Copy {
                    ensure!(
                        written.insert(relative.to_path_buf()),
                        "wheel destination was written twice"
                    );
                    let output = prefix.create_file_exclusive(relative, plan.mode())?;
                    let mut writer = ObservedWriter {
                        output,
                        hasher: Sha256::new(),
                        size: 0,
                        maximum: plan.size(),
                    };
                    let observed = session.copy_member(*archive_index, &mut writer)?;
                    ensure!(
                        observed.archive_index() == *archive_index
                            && observed.size() == *raw_size
                            && observed.sha256() == raw_sha256,
                        "wheel archive source changed during materialization"
                    );
                    files.push(writer.finish(relative, plan)?);
                } else {
                    let observed = session.copy_member(*archive_index, &mut std::io::sink())?;
                    ensure!(
                        observed.archive_index() == *archive_index
                            && observed.size() == *raw_size
                            && observed.sha256() == raw_sha256,
                        "wheel archive source changed during materialization"
                    );
                    if primary {
                        ensure!(
                            plan.operation() == WheelWriteOperation::RewritePythonShebang,
                            "wheel archive source has an invalid output operation"
                        );
                        ensure!(
                            written.insert(relative.to_path_buf()),
                            "wheel destination was written twice"
                        );
                        let prepared_output = outputs
                            .get_mut(&OutputKey::Archive {
                                owner: owner.clone(),
                                archive_index: *archive_index,
                            })
                            .context("wheel rewritten output is missing")?;
                        let output = prefix.create_file_exclusive(relative, plan.mode())?;
                        let mut writer = ObservedWriter {
                            output,
                            hasher: Sha256::new(),
                            size: 0,
                            maximum: plan.size(),
                        };
                        copy_output(prepared_output, &mut writer)?;
                        files.push(writer.finish(relative, plan)?);
                    }
                }
            }
            Ok(())
        })?;
    }
    for plan in complete.plan().files() {
        let Some(WheelPlanSource::Generated { owner, target }) = plan.sources().first() else {
            continue;
        };
        let relative = relative_destination(&root, plan.destination())?;
        ensure!(
            written.insert(relative.to_path_buf()),
            "wheel destination was written twice"
        );
        let prepared_output = outputs
            .get_mut(&OutputKey::Generated {
                owner: owner.clone(),
                target: target.clone(),
            })
            .context("wheel generated output is missing")?;
        let output = prefix.create_file_exclusive(relative, plan.mode())?;
        let mut writer = ObservedWriter {
            output,
            hasher: Sha256::new(),
            size: 0,
            maximum: plan.size(),
        };
        copy_output(prepared_output, &mut writer)?;
        files.push(writer.finish(relative, plan)?);
    }
    ensure!(
        files.len() == complete.plan().files().len(),
        "wheel materialization omitted planned files"
    );
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    prefix.check_binding()?;
    Ok(WheelMaterializationReceipt {
        final_prefix: root,
        allocation_id: prefix.allocation_id().into(),
        sources: prepared.sources.clone(),
        files,
        directories,
    })
}

pub(crate) fn materialize_wheel_graph(
    prepared: PreparedWheelGraph,
    prefix: &mut UnpublishedPrefix,
) -> Result<WheelMaterializationReceipt> {
    materialize_with_hook(prepared, prefix, |_, _| Ok(()))
}

fn materialize_with_hook(
    mut prepared: PreparedWheelGraph,
    prefix: &mut UnpublishedPrefix,
    after_directories: impl FnOnce(&mut PreparedWheelGraph, &mut UnpublishedPrefix) -> Result<()>,
) -> Result<WheelMaterializationReceipt> {
    ensure!(
        prepared.plan().final_prefix() == prefix.final_path(),
        "wheel prefix binding disagrees with plan"
    );
    prefix.check_binding()?;
    super::validate_prepared_bindings(&mut prepared)?;
    let result = attempt(&mut prepared, prefix, after_directories);
    if result.is_err() {
        prefix.mark_failed();
    }
    result
}

#[cfg(all(test, unix))]
mod tests;
