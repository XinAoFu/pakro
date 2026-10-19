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
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ExactVersionError {
    #[error("declaration is not a complete exact version: {0}")]
    Incomplete(String),
}

/// Accepts exactly the complete Python release syntax of the ecosystem by
/// reusing the native parser: CPython full versions (with optional pre-release
/// serial) and exact PyPy language/implementation pairs. Nothing else passes,
/// and no candidate lookup or repair happens here.
pub(crate) fn exact_python_request(raw: &str) -> Result<ExactPythonRequest, ExactVersionError> {
    if crate::plugins::core::python::native::is_exact_request(raw) {
        Ok(ExactPythonRequest {
            raw: raw.to_string(),
        })
    } else {
        Err(ExactVersionError::Incomplete(raw.to_string()))
    }
}

#[cfg(test)]
mod tests;
