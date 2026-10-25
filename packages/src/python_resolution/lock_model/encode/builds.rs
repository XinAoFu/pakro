use super::*;
use crate::packages::python_environment::model::SourceBuildRecord;

pub(super) fn write(
    output: &mut Output,
    builds: &[(PinId, SourceBuildRecord)],
) -> LockModelResult<()> {
    for (pin, record) in builds {
        output.raw("\n[[python.builds]]\n")?;
        output.bytes("source_pin", pin.as_bytes())?;
        output.field("source_digest", &record.source_digest)?;
        output.bytes("request_id", &record.request_id)?;
        output.number("recipe", record.recipe.into())?;
        output.field("backend", &record.backend)?;
        output.field("output_filename", &record.output_filename)?;
        output.field("output_digest", &record.output_digest)?;
        output.number("output_size", record.output_size)?;
        output.bytes("metadata_sha256", &record.metadata_sha256)?;
        if record.inputs.is_empty() {
            output.raw("inputs = []\n")?;
        }
        for input in &record.inputs {
            output.raw("[[python.builds.inputs]]\n")?;
            input_fields(output, input)?;
        }
        if let Some(observation) = &record.observation {
            output.raw("[python.builds.observation]\n")?;
            requirements(
                output,
                "initial_requirements",
                &observation.initial_requirements,
            )?;
            requirements(
                output,
                "dynamic_requirements",
                &observation.dynamic_requirements,
            )?;
            if observation.initial_inputs.is_empty() {
                output.raw("initial_inputs = []\n")?;
            }
            for input in &observation.initial_inputs {
                output.raw("[[python.builds.observation.initial_inputs]]\n")?;
                input_fields(output, input)?;
            }
        }
    }
    Ok(())
}

fn input_fields(
    output: &mut Output,
    input: &crate::packages::python_environment::model::SourceBuildInput,
) -> LockModelResult<()> {
    output.field("name", &input.name)?;
    output.field("version", &input.version)?;
    output.field("source_pin", &input.source_pin)?;
    output.field("registry", &input.registry)?;
    output.bytes("canonical_base_fingerprint", &input.base)?;
    output.field("source_filename", &input.source_filename)?;
    output.field("source_digest", &input.source_digest)?;
    output.number("source_size", input.source_size)?;
    output.raw(if input.source_is_sdist {
        "source_is_sdist = true\n"
    } else {
        "source_is_sdist = false\n"
    })?;
    output.raw(if input.advertised {
        "advertised = true\n"
    } else {
        "advertised = false\n"
    })?;
    output.field("output_filename", &input.output_filename)?;
    output.field("output_digest", &input.output_digest)?;
    output.number("output_size", input.output_size)?;
    Ok(())
}
fn requirements(output: &mut Output, key: &str, values: &[String]) -> LockModelResult<()> {
    output.raw(key)?;
    output.raw(" = [")?;
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.raw(", ")?;
        }
        output.quoted(value)?;
    }
    output.raw("]\n")
}
