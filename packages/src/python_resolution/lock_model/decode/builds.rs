//! Build arrays use the same before-allocation collection and text budgets.
use super::*;
use crate::packages::python_environment::model::{
    NativeToolchainRecord, SourceBuildInput, SourceBuildObservation, SourceBuildRecord,
};

pub(super) struct BuildWire(pub PinId, pub SourceBuildRecord);

impl Wire for NativeToolchainRecord {
    fn read<'de, D: de::Deserializer<'de>>(ctx: &mut Context, de: D) -> Result<Self, D::Error> {
        let raw = TextSeed { ctx: &mut *ctx, mode: TextMode::Native }.deserialize(de)?;
        NativeToolchainRecord::decode_json(&raw).map_err(|_| ctx.fail(LockModelError::InvalidField(LockField::Native)))
    }
}

impl Wire for bool {
    fn read<'de, D: de::Deserializer<'de>>(_ctx: &mut Context, de: D) -> Result<Self, D::Error> {
        serde::Deserialize::deserialize(de)
    }
}

record!(SourceBuildInput, f, {
    let (mut name, mut version, mut source_pin, mut registry, mut base) =
        (None, None, None, None, None);
    let (
        mut source_filename,
        mut source_digest,
        mut source_size,
        mut source_is_sdist,
        mut advertised,
    ) = (None, None, None, None, None);
    let (mut output_filename, mut output_digest, mut output_size) = (None, None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::Name => name = Some(f.text(TextMode::Name)?),
            Key::Version => version = Some(f.text(TextMode::Version)?),
            Key::Source => source_pin = Some(f.value::<PinId>()?),
            Key::Registry => registry = Some(f.text(TextMode::Plain(LockField::Registry))?),
            Key::Base => base = Some(f.value::<[u8; 32]>()?),
            Key::SourceFilename => {
                source_filename = Some(f.text(TextMode::Plain(LockField::Filename))?)
            }
            Key::SourceDigest => source_digest = Some(f.value::<Digest>()?),
            Key::SourceSize => source_size = Some(f.value::<u64>()?),
            Key::IsSdist => source_is_sdist = Some(f.value::<bool>()?),
            Key::Advertised => advertised = Some(f.value::<bool>()?),
            Key::OutputFilename => {
                output_filename = Some(f.text(TextMode::Plain(LockField::Filename))?)
            }
            Key::OutputDigest => output_digest = Some(f.value::<Digest>()?),
            Key::OutputSize => output_size = Some(f.value::<u64>()?),
            _ => return Err(f.bad()),
        }
    }
    let value = SourceBuildInput {
        name: f.required(name)?,
        version: f.required(version)?,
        source_pin: hex::encode(f.required(source_pin)?.as_bytes()),
        registry: f.required(registry)?,
        base: f.required(base)?,
        source_filename: f.required(source_filename)?,
        source_digest: f.required(source_digest)?.to_string(),
        source_size: f.required(source_size)?,
        source_is_sdist: f.required(source_is_sdist)?,
        advertised: f.required(advertised)?,
        output_filename: f.required(output_filename)?,
        output_digest: f.required(output_digest)?.to_string(),
        output_size: f.required(output_size)?,
    };
    if value.content_pin().is_err() {
        return Err(f.ctx.fail(LockModelError::IdentityMismatch));
    }
    Ok(value)
});

record!(BuildWire, f, {
    let mut observation = None;
    let mut native = None;
    let (mut source_pin, mut source_digest, mut request_id, mut recipe, mut backend) =
        (None, None, None, None, None);
    let (mut inputs, mut output_filename, mut output_digest, mut output_size, mut metadata_sha256) =
        (None, None, None, None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::Observation => observation = Some(f.value::<SourceBuildObservation>()?),
            Key::Native => native = Some(f.value::<NativeToolchainRecord>()?),
            Key::Source => source_pin = Some(f.value::<PinId>()?),
            Key::SourceDigest => source_digest = Some(f.value::<Digest>()?),
            Key::RequestId => request_id = Some(f.value::<[u8; 32]>()?),
            Key::Recipe => recipe = Some(f.value::<u64>()?),
            Key::Backend => backend = Some(f.text(TextMode::Plain(LockField::Filename))?),
            Key::Inputs => inputs = Some(f.array::<SourceBuildInput>(ArrayKind::BuildInputs)?),
            Key::OutputFilename => {
                output_filename = Some(f.text(TextMode::Plain(LockField::Filename))?)
            }
            Key::OutputDigest => output_digest = Some(f.value::<Digest>()?),
            Key::OutputSize => output_size = Some(f.value::<u64>()?),
            Key::Metadata => metadata_sha256 = Some(f.value::<[u8; 32]>()?),
            _ => return Err(f.bad()),
        }
    }
    let recipe = u32::try_from(f.required(recipe)?)
        .map_err(|_| f.ctx.fail(LockModelError::UnsupportedRuntimeRecipe))?;
    let record = SourceBuildRecord {
        source_digest: f.required(source_digest)?.to_string(),
        request_id: f.required(request_id)?,
        recipe,
        backend: f.required(backend)?,
        inputs: f.required(inputs)?,
        output_filename: f.required(output_filename)?,
        output_digest: f.required(output_digest)?.to_string(),
        output_size: f.required(output_size)?,
        metadata_sha256: f.required(metadata_sha256)?,
        observation,
        native,
    };
    if record.validate().is_err() {
        return Err(f.ctx.fail(LockModelError::IdentityMismatch));
    }
    Ok(BuildWire(f.required(source_pin)?, record))
});

record!(SourceBuildObservation, f, {
    let (mut initial_requirements, mut initial_inputs, mut dynamic_requirements) =
        (None, None, None);
    while let Some(key) = f.next()? {
        match key {
            Key::InitialRequirements => {
                initial_requirements = Some(f.array::<String>(ArrayKind::RootDeclarations)?)
            }
            Key::InitialInputs => {
                initial_inputs = Some(f.array::<SourceBuildInput>(ArrayKind::BuildInputs)?)
            }
            Key::DynamicRequirements => {
                dynamic_requirements = Some(f.array::<String>(ArrayKind::RootDeclarations)?)
            }
            _ => return Err(f.bad()),
        }
    }
    Ok(SourceBuildObservation {
        initial_requirements: f.required(initial_requirements)?,
        initial_inputs: f.required(initial_inputs)?,
        dynamic_requirements: f.required(dynamic_requirements)?,
    })
});
