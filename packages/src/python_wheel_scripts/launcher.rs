//! POSIX launch prefixes and Python-3.0-compatible entry point bodies.

use std::borrow::Cow;
use std::path::Component;
use std::sync::LazyLock;

use eyre::{ContextCompat, Result, ensure};
use regex::bytes::Regex;
use sha2::{Digest as _, Sha256};

use super::{PreparedWheelScript, PythonScriptTarget, WheelEntryPoint, WheelScriptLimits};

static CODING_COOKIE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u)^[ \t\x0c]*#.*?coding[:=][ \t]*([-_.a-zA-Z0-9]+)")
        .expect("constant PEP 263 byte pattern")
});

pub(super) fn executable<'a>(target: &'a PythonScriptTarget<'_>) -> Result<&'a str> {
    for path in [target.final_prefix, target.interpreter] {
        let value = path
            .to_str()
            .context("script target path must be UTF-8 ASCII")?;
        ensure!(
            path.is_absolute()
                && value.is_ascii()
                && !value.bytes().any(|byte| byte.is_ascii_control())
                && !path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir)),
            "script target must be an absolute normalized ASCII path without controls"
        );
    }
    ensure!(
        target.interpreter.starts_with(target.final_prefix)
            && target.interpreter != target.final_prefix
            && target.interpreter.file_name().is_some(),
        "script target interpreter must be inside the final prefix"
    );
    target
        .interpreter
        .to_str()
        .context("script target interpreter must be UTF-8 ASCII")
}

pub(super) fn complex(executable: &str) -> bool {
    executable.contains([' ', '#'])
        || executable
            .len()
            .checked_add(3)
            .is_none_or(|size| size > 127)
}

pub(super) fn coding_cookie(line: &[u8]) -> Option<&str> {
    let matched = CODING_COOKIE.captures(line)?.get(1)?;
    // The capture is a nonempty ASCII-only byte pattern.
    std::str::from_utf8(matched.as_bytes()).ok()
}

pub(super) fn prefix(
    executable: &str,
    cookie: Option<&str>,
    limits: WheelScriptLimits,
) -> Result<Vec<u8>> {
    ensure!(
        executable.len() <= limits.max_wrapper_bytes
            && cookie.is_none_or(|value| value.len() <= limits.max_wrapper_bytes),
        "script launch prefix byte limit exceeded"
    );
    let mut bytes = Vec::new();
    let is_complex = complex(executable);
    if is_complex {
        bytes.extend_from_slice(b"#!/bin/sh\n");
    } else {
        bytes.extend_from_slice(b"#!");
        bytes.extend_from_slice(executable.as_bytes());
        bytes.push(b'\n');
    }
    if let Some(cookie) = cookie {
        bytes.extend_from_slice(b"# coding: ");
        bytes.extend_from_slice(cookie.as_bytes());
        bytes.push(b'\n');
    }
    if is_complex {
        let quoted = shell_escape::unix::escape(Cow::Borrowed(executable));
        bytes.extend_from_slice(b"'''exec' ");
        bytes.extend_from_slice(quoted.as_bytes());
        bytes.extend_from_slice(b" \"$0\" \"$@\"\n' '''\n");
    }
    ensure!(
        bytes.len() <= limits.max_wrapper_bytes,
        "script launch prefix byte limit exceeded"
    );
    Ok(bytes)
}

pub(super) fn generate(
    entry: &WheelEntryPoint,
    target: &PythonScriptTarget<'_>,
    limits: WheelScriptLimits,
) -> Result<PreparedWheelScript> {
    super::validate_limits(limits)?;
    let executable = executable(target)?;
    super::entry_points::validate_entry(entry)?;
    let mut value_size = entry
        .name()
        .len()
        .checked_add(entry.module().len())
        .context("entry point value byte limit overflow")?;
    for value in entry.attributes().iter().chain(entry.extras()) {
        value_size = value_size
            .checked_add(value.len())
            .context("entry point value byte limit overflow")?;
    }
    ensure!(
        value_size <= limits.max_entrypoint_value_bytes,
        "entry point value byte limit exceeded"
    );
    let module = serde_json::to_string(entry.module())?;
    let attributes = serde_json::to_string(entry.attributes())?;
    // JSON string/list syntax is also Python 3 string/list syntax. None of the
    // untrusted identifiers are interpolated into executable Python syntax.
    // __import__ + sys.modules works even on Python 3.0 without importlib APIs.
    let body = format!(
        "import sys\n\ndef _pakro_entry():\n    __import__({module})\n    target = sys.modules[{module}]\n    for attribute in {attributes}:\n        target = getattr(target, attribute)\n    return target()\n\nif __name__ == '__main__':\n    sys.exit(_pakro_entry())\n"
    );
    let mut bytes = prefix(executable, None, limits)?;
    ensure!(
        body.len() <= limits.max_wrapper_bytes.saturating_sub(bytes.len()),
        "entry point wrapper byte limit exceeded"
    );
    bytes.extend_from_slice(body.as_bytes());
    ensure!(
        bytes.len() as u64 <= limits.max_output_bytes,
        "entry point output byte limit exceeded"
    );
    Ok(PreparedWheelScript {
        name: entry.name().to_owned(),
        sha256: Sha256::digest(&bytes).into(),
        bytes,
        mode: 0o755,
    })
}
