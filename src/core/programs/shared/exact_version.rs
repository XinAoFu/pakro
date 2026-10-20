//! Pure input gate for complete exact user declarations (D10).
//!
//! It only interprets the declaration string it is given; it never fetches
//! candidates, repairs a declaration, or performs IO. A request without a
//! complete release version must fail here, before any remote acquisition or
//! business write is reachable.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactPythonRequest {
    raw: String,
}

impl ExactPythonRequest {
    pub(crate) fn as_str(&self) -> &str {
        &self.raw
    }
    /// The bare complete version without the optional `+YYYYMMDD` build pin;
    /// this is what published instances and archive filenames compare on.
    pub(crate) fn version_only(&self) -> &str {
        self.raw.split('+').next().unwrap_or(&self.raw)
    }
    /// The optional explicit distribution-build pin (`+20261013` → `20261013`).
    pub(crate) fn build_pin(&self) -> Option<&str> {
        self.raw.split_once('+').map(|(_, build)| build)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ExactVersionError {
    #[error("declaration is not a complete exact version: {0}")]
    Incomplete(String),
}

/// Accepts exactly the complete Python release syntax of the ecosystem by
/// reusing the native parser: CPython full versions (with optional pre-release
/// serial) and exact PyPy language/implementation pairs, each with an
/// optional `+YYYYMMDD` distribution-build pin (the same spelling as the
/// official archive names). Nothing else passes, and no candidate lookup or
/// repair happens here.
pub(crate) fn exact_python_request(raw: &str) -> Result<ExactPythonRequest, ExactVersionError> {
    let (base, build) = match raw.split_once('+') {
        Some((base, build)) => (base, Some(build)),
        None => (raw, None),
    };
    if let Some(build) = build
        && !(build.len() == 8 && build.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(ExactVersionError::Incomplete(raw.to_string()));
    }
    if crate::plugins::core::python::native::is_exact_request(base) {
        Ok(ExactPythonRequest {
            raw: raw.to_string(),
        })
    } else {
        Err(ExactVersionError::Incomplete(raw.to_string()))
    }
}

#[cfg(test)]
mod tests;
