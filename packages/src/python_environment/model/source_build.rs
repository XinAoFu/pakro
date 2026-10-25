//! Bounded, portable build descriptions. These values never authorize IO.
use super::*;
use crate::packages::python_resolution::lock_model::{
    ContentPin, LockModelLimits, PinEvidence, RegistryArtifactKind, RegistryFileRef,
    RegistryFileRefInput,
};
use pakro_resolver::solve::{Pep440, VersionAdapter};

pub(crate) const MAX_BUILD_INPUTS: usize = 4096;
const MAX_BUILD_TEXT: usize = 16 * 1024 * 1024;

fn canonical_digest(raw: &str) -> Result<Digest> {
    ensure!(
        raw.len() <= 135,
        "source build digest exceeds its byte budget"
    );
    let digest: Digest = raw.parse()?;
    ensure!(
        digest.to_string() == raw,
        "source build digest is not canonical"
    );
    Ok(digest)
}
fn output(filename: &str, digest: &str, size: u64) -> Result<pakro_index::WheelFile> {
    ensure!(
        filename.len() <= 16 * 1024,
        "source build output name exceeds its byte budget"
    );
    let wheel = pakro_index::parse_wheel_filename(filename)
        .ok_or_else(|| eyre::eyre!("source build output is not a wheel"))?;
    canonical_digest(digest)?;
    ensure!(
        size > 0 && size <= LockModelLimits::default().max_artifact_bytes,
        "source build output size exceeds its budget"
    );
    Ok(wheel)
}
fn same_version(a: &str, b: &str) -> bool {
    matches!((Pep440::parse_version(a), Pep440::parse_version(b)), (Some(a),Some(b)) if a==b)
}

impl SourceBuildInput {
    pub(crate) fn content_pin(&self) -> Result<ContentPin> {
        ensure!(
            self.name.len() <= 1024 && self.version.len() <= 1024 && self.registry.len() <= 1024,
            "source build input identity exceeds its byte budget"
        );
        let file = RegistryFileRef::new(
            RegistryFileRefInput {
                registry_id: &self.registry,
                canonical_base_fingerprint: self.base,
                project: &self.name,
                version: &self.version,
                filename: &self.source_filename,
                kind: if self.source_is_sdist {
                    RegistryArtifactKind::Sdist
                } else {
                    RegistryArtifactKind::Wheel
                },
            },
            Default::default(),
        )?;
        let pin = ContentPin::new(
            &file,
            canonical_digest(&self.source_digest)?,
            self.source_size,
            if self.advertised {
                PinEvidence::Advertised
            } else {
                PinEvidence::ObservedDuringFreshResolution
            },
            Default::default(),
        )?;
        ensure!(
            hex::encode(pin.id().as_bytes()) == self.source_pin,
            "source build input disagrees with its exact content pin"
        );
        let wheel = output(&self.output_filename, &self.output_digest, self.output_size)?;
        ensure!(
            pakro_index::normalize_py_name(&wheel.name) == self.name
                && same_version(&wheel.version, &self.version),
            "source build input output identity differs"
        );
        if !self.source_is_sdist {
            ensure!(
                self.source_filename == self.output_filename
                    && self.source_digest == self.output_digest
                    && self.source_size == self.output_size,
                "source build wheel input output differs from its original pin"
            );
        }
        Ok(pin)
    }
}
impl SourceBuildRecord {
    pub(crate) fn validate(&self) -> Result<()> {
        canonical_digest(&self.source_digest)?;
        ensure!(self.recipe == 1, "source build recipe is unsupported");
        ensure!(
            !self.backend.is_empty() && self.backend.len() <= 4096,
            "source build backend exceeds its byte budget"
        );
        // Reuse the declaration grammar without resolving paths or invoking a backend.
        let raw = format!(
            "[build-system]\nrequires=[]\nbuild-backend={}\n",
            serde_json::to_string(&self.backend)?
        );
        crate::packages::python_declarations::parse_pyproject(&raw, Default::default())?;
        output(&self.output_filename, &self.output_digest, self.output_size)?;
        ensure!(
            self.inputs.len() <= MAX_BUILD_INPUTS,
            "source build input limit exceeded"
        );
        let mut names = BTreeSet::new();
        let mut bytes = self.backend.len()
            + self.source_digest.len()
            + self.output_filename.len()
            + self.output_digest.len();
        for input in &self.inputs {
            input.content_pin()?;
            ensure!(
                names.insert(&input.name),
                "source build repeats an input package name"
            );
            for value in [
                &input.name,
                &input.version,
                &input.source_pin,
                &input.registry,
                &input.source_filename,
                &input.source_digest,
                &input.output_filename,
                &input.output_digest,
            ] {
                bytes = bytes
                    .checked_add(value.len())
                    .filter(|n| *n <= MAX_BUILD_TEXT)
                    .ok_or_else(|| eyre::eyre!("source build text budget exceeded"))?;
            }
        }
        if let Some(observation) = &self.observation {
            ensure!(
                observation.initial_inputs.len() <= MAX_BUILD_INPUTS,
                "initial build input limit exceeded"
            );
            let mut initial_names = BTreeSet::new();
            for initial in &observation.initial_inputs {
                initial.content_pin()?;
                ensure!(
                    initial_names.insert(&initial.name),
                    "duplicate initial build input"
                );
                let mut expected = initial.clone();
                expected.advertised = false;
                ensure!(
                    self.inputs.iter().any(|input| {
                        let mut final_input = input.clone();
                        final_input.advertised = false;
                        final_input == expected
                    }),
                    "initial build input is absent from the final environment"
                );
            }
            for requirements in [
                &observation.initial_requirements,
                &observation.dynamic_requirements,
            ] {
                ensure!(
                    requirements.len() <= MAX_BUILD_INPUTS,
                    "build requirement count exceeded"
                );
                for requirement in requirements {
                    bytes = bytes
                        .checked_add(requirement.len())
                        .filter(|n| *n <= MAX_BUILD_TEXT)
                        .ok_or_else(|| eyre::eyre!("source build text budget exceeded"))?;
                    let parsed = pakro_index::python_requirements::parse_python_requirement(
                        requirement,
                        Default::default(),
                    )?;
                    ensure!(
                        parsed.unresolved_reference().is_none(),
                        "unsupported source build requirement source"
                    );
                }
            }
        }
        Ok(())
    }
    pub(super) fn validate_for_node(
        &self,
        name: &str,
        version: &str,
        artifact: &str,
    ) -> Result<()> {
        self.validate()?;
        let wheel = output(&self.output_filename, &self.output_digest, self.output_size)?;
        ensure!(
            pakro_index::normalize_py_name(&wheel.name) == name
                && same_version(&wheel.version, version)
                && self.output_digest == artifact,
            "source build output differs from its package graph node"
        );
        Ok(())
    }
    pub(super) fn normalize(&mut self) {
        for input in &mut self.inputs {
            input.advertised = false;
        }
        self.inputs.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(observation) = &mut self.observation {
            for input in &mut observation.initial_inputs {
                input.advertised = false;
            }
            observation
                .initial_inputs
                .sort_by(|a, b| a.name.cmp(&b.name));
        }
    }
    /// Receipt identity includes actual output; it is not an input-only BuildRequestId.
    pub(crate) fn fingerprint(&self) -> Result<[u8; 32]> {
        use sha2::{Digest as _, Sha256};
        self.validate()?;
        let mut normalized = self.clone();
        normalized.normalize();
        let mut hash = Sha256::new();
        hash.update(b"pakro.source-build-record/v1\0");
        hash.update(serde_json::to_vec(&normalized)?);
        Ok(hash.finalize().into())
    }
}
