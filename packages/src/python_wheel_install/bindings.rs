//! Bind the complete plan to every actual owned source and prepared output.

use std::collections::BTreeSet;

use eyre::{ContextCompat, Result, ensure};

use super::super::python_wheel_plan::{WheelOwner, WheelPlanSource, WheelWriteOperation};
use super::{OutputKey, PreparedWheelGraph};

pub(super) fn validate(prepared: &mut PreparedWheelGraph) -> Result<()> {
    let mut summaries = BTreeSet::new();
    for source in &prepared.sources {
        ensure!(
            summaries.insert(source.owner.clone()),
            "wheel source summary has a duplicate owner"
        );
        let archive = prepared
            .archives
            .get(&source.owner)
            .context("wheel source summary has no owned archive")?;
        ensure!(
            WheelOwner::from_manifest(archive.manifest()) == source.owner
                && archive.filename() == source.filename
                && archive.digest() == source.digest
                && archive.size() == source.size,
            "wheel source envelope disagrees with its owned archive"
        );
    }
    ensure!(
        summaries.len() == prepared.archives.len(),
        "wheel source summary is incomplete"
    );
    let mut archive_owners = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for file in prepared.complete.plan().files() {
        let mut owners = BTreeSet::new();
        for source in file.sources() {
            let (owner, key) = match source {
                WheelPlanSource::Archive {
                    owner,
                    archive_index,
                    archive_path,
                    raw_sha256,
                    raw_size,
                } => {
                    let archive = prepared
                        .archives
                        .get(owner)
                        .context("wheel source plan has no owned archive")?;
                    ensure!(
                        &WheelOwner::from_manifest(archive.manifest()) == owner,
                        "wheel source owner disagrees with owned archive identity"
                    );
                    let members = archive.manifest().members();
                    let position = members
                        .binary_search_by_key(archive_index, |member| member.archive_index())
                        .ok()
                        .context("wheel source index is not in its owned archive")?;
                    let member = &members[position];
                    ensure!(
                        member.archive_path() == archive_path
                            && member.observed_sha256() == raw_sha256
                            && member.size() == *raw_size,
                        "wheel source member disagrees with owned inspection"
                    );
                    archive_owners.insert(owner.clone());
                    let key = match file.operation() {
                        WheelWriteOperation::Copy => {
                            ensure!(
                                file.sha256() == raw_sha256
                                    && file.size() == *raw_size
                                    && file.mode() == (member.unix_mode().unwrap_or(0o644) & 0o777),
                                "wheel source Copy facts disagree with owned inspection"
                            );
                            None
                        }
                        WheelWriteOperation::RewritePythonShebang => Some(OutputKey::Archive {
                            owner: owner.clone(),
                            archive_index: *archive_index,
                        }),
                        _ => eyre::bail!("wheel source archive has an invalid output operation"),
                    };
                    (owner, key)
                }
                WheelPlanSource::Generated { owner, target } => {
                    ensure!(
                        prepared.archives.contains_key(owner),
                        "wheel source generated owner has no owned archive"
                    );
                    archive_owners.insert(owner.clone());
                    (
                        owner,
                        Some(OutputKey::Generated {
                            owner: owner.clone(),
                            target: target.clone(),
                        }),
                    )
                }
            };
            owners.insert(owner.clone());
            if let Some(key) = key {
                let output = prepared
                    .outputs
                    .get(&key)
                    .context("wheel output required by complete plan is missing")?;
                ensure!(
                    &output.sha256 == file.sha256()
                        && output.size == file.size()
                        && output.mode == file.mode()
                        && output.operation == file.operation(),
                    "wheel output facts disagree with complete plan"
                );
                ensure!(
                    outputs.insert(key),
                    "wheel output is referenced more than once"
                );
            }
        }
        ensure!(
            owners.into_iter().collect::<Vec<_>>() == file.owners(),
            "wheel source owners disagree with complete plan"
        );
    }
    ensure!(
        archive_owners == summaries,
        "wheel source inventory disagrees with complete plan"
    );
    ensure!(
        outputs.len() == prepared.outputs.len(),
        "wheel output is not part of the complete plan"
    );
    for output in prepared.outputs.values_mut() {
        output.check_bytes()?;
    }
    Ok(())
}
