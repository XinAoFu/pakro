//! Two-pass whole-graph preparation from owned inspected archives.

use std::collections::BTreeMap;
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};

use eyre::{Context, ContextCompat, Result, ensure, eyre};

use super::super::python_wheel::{WheelLimits, WheelManifest, WheelMember, WheelScheme};
use super::super::python_wheel_plan::{
    WheelGeneratedIntent, WheelGeneratedTarget, WheelOwner, WheelPlanLimits, WheelTransformIntent,
    WheelWriteOperation, plan_wheel_graph,
};
use super::super::python_wheel_scripts::{
    PythonScriptTarget, WheelScriptOperation, generate_entry_point, parse_entry_points,
    rewrite_python_script,
};
use super::super::python_wheel_source::InspectedWheelArchive;
use super::outputs::GeneratedBudget;
use super::{
    INSTALLER, OutputKey, PreparedBytes, PreparedOutput, PreparedWheelGraph, PreparedWheelSource,
    WheelInstallLimits, WheelInstallTarget,
};

fn validate_target(target: &WheelInstallTarget<'_>) -> Result<()> {
    for path in [target.scheme.final_prefix(), target.interpreter] {
        let text = path
            .to_str()
            .context("wheel script target must be UTF-8 ASCII")?;
        ensure!(
            path.is_absolute()
                && text.is_ascii()
                && !text.bytes().any(|byte| byte.is_ascii_control())
                && !path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir)),
            "wheel script target must be an absolute normalized ASCII path"
        );
    }
    ensure!(
        target.interpreter.starts_with(target.scheme.final_prefix())
            && target.interpreter != target.scheme.final_prefix()
            && target.interpreter.file_name().is_some(),
        "wheel script target interpreter must be inside the final prefix"
    );
    Ok(())
}

fn root_entry_points(manifest: &WheelManifest, member: &WheelMember) -> bool {
    // Metadata is physically in the archive's top-level dist-info tree, not an
    // unrelated payload or a .data source with an entry_points.txt basename.
    let mut canonical = PathBuf::new();
    for part in Path::new(member.archive_path()).components() {
        match part {
            Component::Normal(part) => canonical.push(part),
            Component::ParentDir => {
                canonical.pop();
            }
            _ => {}
        }
    }
    canonical == Path::new(manifest.dist_info()).join("entry_points.txt")
}

fn generated_intent(
    owner: &WheelOwner,
    target: WheelGeneratedTarget,
    output: &PreparedOutput,
) -> WheelGeneratedIntent {
    WheelGeneratedIntent {
        owner: owner.clone(),
        target,
        operation: output.operation,
        sha256: output.sha256,
        size: output.size,
        mode: output.mode,
    }
}

fn add_output(
    outputs: &mut BTreeMap<OutputKey, PreparedOutput>,
    key: OutputKey,
    output: PreparedOutput,
    budget: &mut GeneratedBudget,
) -> Result<()> {
    budget.reserve(output.size)?;
    ensure!(
        outputs.insert(key, output).is_none(),
        "wheel output has duplicate source identity"
    );
    Ok(())
}

struct ScriptPreparation<'a> {
    target: PythonScriptTarget<'a>,
    wheel_limits: WheelLimits,
    limits: WheelInstallLimits,
    scratch_root: &'a Path,
    outputs: &'a mut BTreeMap<OutputKey, PreparedOutput>,
    budget: &'a mut GeneratedBudget,
    generated: &'a mut Vec<WheelGeneratedIntent>,
    transformed: &'a mut Vec<WheelTransformIntent>,
}
impl ScriptPreparation<'_> {
    fn archive(&mut self, owner: &WheelOwner, archive: &mut InspectedWheelArchive) -> Result<()> {
        let scripts: Vec<_> = archive
            .manifest()
            .members()
            .iter()
            .filter(|member| member.scheme() == WheelScheme::Scripts)
            .cloned()
            .collect();
        let entry_points = archive
            .manifest()
            .members()
            .iter()
            .find(|member| root_entry_points(archive.manifest(), member))
            .map(|member| member.archive_index());
        archive.with_members(|session| {
            if let Some(index) = entry_points {
                let maximum = self
                    .limits
                    .scripts
                    .max_entrypoints_bytes
                    .min(self.wheel_limits.max_metadata_bytes);
                let bytes = session.read_member(index, maximum as u64)?;
                for entry in parse_entry_points(&bytes, self.limits.scripts)? {
                    let wrapper = generate_entry_point(&entry, &self.target, self.limits.scripts)?;
                    let output = PreparedOutput::memory(
                        wrapper.bytes().to_vec(),
                        wrapper.mode(),
                        WheelWriteOperation::GenerateEntryPoint,
                    );
                    ensure!(
                        output.sha256 == *wrapper.sha256() && output.size == wrapper.size(),
                        "wheel output wrapper facts changed during preparation"
                    );
                    let target = WheelGeneratedTarget::Scripts(wrapper.name().into());
                    self.generated
                        .push(generated_intent(owner, target.clone(), &output));
                    add_output(
                        self.outputs,
                        OutputKey::Generated {
                            owner: owner.clone(),
                            target,
                        },
                        output,
                        self.budget,
                    )?;
                }
            }
            for member in &scripts {
                let mut raw = tempfile::tempfile_in(self.scratch_root)
                    .map_err(|_| eyre!("wheel script private input spool creation failed"))?;
                let receipt = session.copy_member(member.archive_index(), &mut raw)?;
                ensure!(
                    receipt.archive_index() == member.archive_index()
                        && receipt.size() == member.size()
                        && receipt.sha256() == member.observed_sha256(),
                    "wheel source script spool disagrees with inspection"
                );
                raw.rewind()
                    .map_err(|_| eyre!("wheel script spool seek failed"))?;
                let mut marker = [0; 8];
                let mut count = 0;
                while count < marker.len() {
                    let read = raw
                        .read(&mut marker[count..])
                        .map_err(|_| eyre!("wheel script spool read failed"))?;
                    if read == 0 {
                        break;
                    }
                    count += read;
                }
                let rewrite = count == marker.len() && marker == *b"#!python";
                raw.rewind()
                    .map_err(|_| eyre!("wheel script spool seek failed"))?;
                let mut script_limits = self.limits.scripts;
                script_limits.max_input_bytes = script_limits
                    .max_input_bytes
                    .min(self.wheel_limits.max_member_bytes);
                let mode = member.unix_mode().unwrap_or(0o644) & 0o777;
                let (transform, prepared_bytes) = if rewrite {
                    ensure!(
                        self.budget.remaining() != 0,
                        "wheel generated byte limit exceeded"
                    );
                    script_limits.max_output_bytes =
                        script_limits.max_output_bytes.min(self.budget.remaining());
                    let mut output = tempfile::tempfile_in(self.scratch_root)
                        .map_err(|_| eyre!("wheel generated script spool creation failed"))?;
                    let transform = rewrite_python_script(
                        &mut raw,
                        &mut output,
                        &self.target,
                        mode,
                        script_limits,
                    )
                    .context("wheel generated script preparation failed")?;
                    (transform, Some(PreparedBytes::Spool(output)))
                } else {
                    (
                        rewrite_python_script(
                            &mut raw,
                            &mut std::io::sink(),
                            &self.target,
                            mode,
                            script_limits,
                        )?,
                        None,
                    )
                };
                ensure!(
                    transform.raw_size() == member.size()
                        && transform.raw_sha256() == member.observed_sha256(),
                    "wheel source script statistics disagree with inspection"
                );
                let operation = match transform.operation() {
                    WheelScriptOperation::Copy => WheelWriteOperation::Copy,
                    WheelScriptOperation::RewritePythonShebang => {
                        WheelWriteOperation::RewritePythonShebang
                    }
                };
                ensure!(
                    rewrite == (operation == WheelWriteOperation::RewritePythonShebang),
                    "wheel output script operation changed during preparation"
                );
                self.transformed.push(WheelTransformIntent {
                    owner: owner.clone(),
                    archive_index: member.archive_index(),
                    operation,
                    sha256: *transform.sha256(),
                    size: transform.size(),
                    mode: transform.mode(),
                });
                if let Some(bytes) = prepared_bytes {
                    add_output(
                        self.outputs,
                        OutputKey::Archive {
                            owner: owner.clone(),
                            archive_index: member.archive_index(),
                        },
                        PreparedOutput {
                            bytes,
                            sha256: *transform.sha256(),
                            size: transform.size(),
                            mode: transform.mode(),
                            operation,
                        },
                        self.budget,
                    )?;
                }
            }
            Ok(())
        })
    }
}

pub(super) fn prepare(
    archives: Vec<InspectedWheelArchive>,
    target: &WheelInstallTarget<'_>,
    wheel_limits: WheelLimits,
    plan_limits: WheelPlanLimits,
    limits: WheelInstallLimits,
    scratch_root: &Path,
) -> Result<PreparedWheelGraph> {
    super::super::python_wheel::validate_limits(wheel_limits)?;
    super::super::python_wheel_scripts::validate_limits(limits.scripts)?;
    ensure!(
        limits.max_record_rows != 0
            && limits.max_record_bytes != 0
            && limits.max_generated_bytes != 0,
        "wheel install limits must all be nonzero"
    );
    validate_target(target)?;
    ensure!(
        plan_limits.max_wheels != 0
            && plan_limits.max_sources != 0
            && plan_limits.max_files != 0
            && plan_limits.max_directories != 0
            && plan_limits.max_total_bytes != 0
            && plan_limits.max_path_bytes != 0,
        "wheel graph limits must all be nonzero"
    );
    ensure!(
        archives.len() <= plan_limits.max_wheels,
        "wheel graph wheel limit exceeded"
    );
    let mut owned = BTreeMap::new();
    let mut source_count = target.reserved.len();
    for archive in archives {
        archive.check_limits(wheel_limits)?;
        source_count = source_count
            .checked_add(archive.manifest().members().len())
            .and_then(|count| count.checked_add(2))
            .context("wheel graph source limit overflow")?;
        ensure!(
            source_count <= plan_limits.max_sources,
            "wheel graph source limit exceeded"
        );
        let owner = WheelOwner::from_manifest(archive.manifest());
        ensure!(
            owned.insert(owner, archive).is_none(),
            "wheel source owner has multiple owned archives"
        );
    }
    let mut generated = Vec::new();
    let mut transformed = Vec::new();
    let mut outputs = BTreeMap::new();
    let mut budget = GeneratedBudget::new(limits.max_generated_bytes);
    for (owner, archive) in &mut owned {
        ScriptPreparation {
            target: PythonScriptTarget {
                final_prefix: target.scheme.final_prefix(),
                interpreter: target.interpreter,
            },
            wheel_limits,
            limits,
            scratch_root,
            outputs: &mut outputs,
            budget: &mut budget,
            generated: &mut generated,
            transformed: &mut transformed,
        }
        .archive(owner, archive)?;
    }
    let manifests: Vec<_> = owned
        .values()
        .map(|archive| archive.manifest().clone())
        .collect();
    let initial = plan_wheel_graph(
        &manifests,
        target.scheme,
        &generated,
        &transformed,
        target.reserved,
        plan_limits,
    )?;
    for owner in owned.keys() {
        let installer = PreparedOutput::memory(
            INSTALLER.to_vec(),
            0o644,
            WheelWriteOperation::GenerateInstaller,
        );
        generated.push(generated_intent(
            owner,
            WheelGeneratedTarget::Installer,
            &installer,
        ));
        add_output(
            &mut outputs,
            OutputKey::Generated {
                owner: owner.clone(),
                target: WheelGeneratedTarget::Installer,
            },
            installer,
            &mut budget,
        )?;
        let record = PreparedOutput::memory(
            super::record::generate(&initial, owner, limits)?,
            0o644,
            WheelWriteOperation::GenerateRecord,
        );
        generated.push(generated_intent(
            owner,
            WheelGeneratedTarget::InstalledRecord,
            &record,
        ));
        add_output(
            &mut outputs,
            OutputKey::Generated {
                owner: owner.clone(),
                target: WheelGeneratedTarget::InstalledRecord,
            },
            record,
            &mut budget,
        )?;
    }
    let complete = plan_wheel_graph(
        &manifests,
        target.scheme,
        &generated,
        &transformed,
        target.reserved,
        plan_limits,
    )?
    .require_complete()?;
    let unchanged: Vec<_> = complete
        .plan()
        .files()
        .iter()
        .filter(|file| {
            !matches!(
                file.operation(),
                WheelWriteOperation::GenerateRecord | WheelWriteOperation::GenerateInstaller
            )
        })
        .cloned()
        .collect();
    ensure!(
        unchanged == initial.files(),
        "wheel output nonmetadata plan changed while completing metadata"
    );
    let sources = owned
        .iter()
        .map(|(owner, archive)| PreparedWheelSource {
            owner: owner.clone(),
            filename: archive.filename().into(),
            digest: archive.digest(),
            size: archive.size(),
        })
        .collect();
    let mut prepared = PreparedWheelGraph {
        archives: owned,
        outputs,
        complete,
        sources,
    };
    super::validate_prepared_bindings(&mut prepared)?;
    Ok(prepared)
}
