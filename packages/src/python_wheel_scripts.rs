//! Entry-point and archive-script preparation from explicit inputs.
//!
//! This module neither imports targets nor chooses a runtime or writes a prefix.

use std::fmt;
use std::io::{Read, Write};
use std::path::Path;

use eyre::{Result, ensure};

mod entry_points;
mod launcher;
mod stream;

#[derive(Debug, Clone, Copy)]
pub(crate) struct WheelScriptLimits {
    pub max_entrypoints_bytes: usize,
    pub max_entrypoints: usize,
    pub max_entrypoint_value_bytes: usize,
    pub max_wrapper_bytes: usize,
    pub max_first_line_bytes: usize,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
}

impl Default for WheelScriptLimits {
    fn default() -> Self {
        Self {
            max_entrypoints_bytes: 16 * 1024 * 1024,
            max_entrypoints: 16_384,
            max_entrypoint_value_bytes: 64 * 1024,
            max_wrapper_bytes: 64 * 1024,
            max_first_line_bytes: 64 * 1024,
            max_input_bytes: 8 * 1024 * 1024 * 1024,
            max_output_bytes: 8 * 1024 * 1024 * 1024,
        }
    }
}

pub(crate) struct PythonScriptTarget<'a> {
    pub final_prefix: &'a Path,
    pub interpreter: &'a Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WheelEntryPointKind {
    Console,
    Gui,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WheelEntryPoint {
    name: String,
    module: String,
    attributes: Vec<String>,
    extras: Vec<String>,
    kind: WheelEntryPointKind,
}

impl WheelEntryPoint {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn module(&self) -> &str {
        &self.module
    }
    pub(crate) fn attributes(&self) -> &[String] {
        &self.attributes
    }
    pub(crate) fn extras(&self) -> &[String] {
        &self.extras
    }
    pub(crate) fn kind(&self) -> WheelEntryPointKind {
        self.kind
    }
}

impl fmt::Debug for WheelEntryPoint {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("WheelEntryPoint")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("attribute_count", &self.attributes.len())
            .field("extra_count", &self.extras.len())
            .finish_non_exhaustive()
    }
}

pub(crate) struct PreparedWheelScript {
    name: String,
    bytes: Vec<u8>,
    sha256: [u8; 32],
    mode: u32,
}
impl PreparedWheelScript {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub(crate) fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
    pub(crate) fn mode(&self) -> u32 {
        self.mode
    }
}
impl fmt::Debug for PreparedWheelScript {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PreparedWheelScript")
            .field("name", &self.name)
            .field("bytes", &self.bytes.len())
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WheelScriptOperation {
    Copy,
    RewritePythonShebang,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WheelScriptTransform {
    operation: WheelScriptOperation,
    raw_sha256: [u8; 32],
    raw_size: u64,
    sha256: [u8; 32],
    size: u64,
    mode: u32,
}
impl WheelScriptTransform {
    pub(crate) fn operation(&self) -> WheelScriptOperation {
        self.operation
    }
    pub(crate) fn raw_sha256(&self) -> &[u8; 32] {
        &self.raw_sha256
    }
    pub(crate) fn raw_size(&self) -> u64 {
        self.raw_size
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
}

pub(super) fn validate_limits(limits: WheelScriptLimits) -> Result<()> {
    ensure!(
        [
            limits.max_entrypoints_bytes,
            limits.max_entrypoints,
            limits.max_entrypoint_value_bytes,
            limits.max_wrapper_bytes,
            limits.max_first_line_bytes,
        ]
        .into_iter()
        .all(|value| value != 0)
            && limits.max_input_bytes != 0
            && limits.max_output_bytes != 0,
        "script limits must all be nonzero"
    );
    Ok(())
}

pub(crate) fn parse_entry_points(
    bytes: &[u8],
    limits: WheelScriptLimits,
) -> Result<Vec<WheelEntryPoint>> {
    entry_points::parse(bytes, limits)
}

pub(crate) fn generate_entry_point(
    entry: &WheelEntryPoint,
    target: &PythonScriptTarget<'_>,
    limits: WheelScriptLimits,
) -> Result<PreparedWheelScript> {
    launcher::generate(entry, target, limits)
}

pub(crate) fn rewrite_python_script<R: Read, W: Write>(
    input: R,
    output: &mut W,
    target: &PythonScriptTarget<'_>,
    source_mode: u32,
    limits: WheelScriptLimits,
) -> Result<WheelScriptTransform> {
    stream::rewrite(input, output, target, source_mode, limits)
}

#[cfg(all(test, unix))]
mod tests;
