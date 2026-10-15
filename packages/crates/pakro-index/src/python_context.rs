//! Explicit interpreter observations used to evaluate Python package metadata.
//! This context does not discover interpreters or establish wheel compatibility.

use pep508_rs::MarkerEnvironment;

use crate::{IndexError, Result};

/// Eleven explicitly supplied PEP 508 fields; this builder infers no host defaults.
pub use pep508_rs::MarkerEnvironmentBuilder as PythonMarkerContextBuilder;

/// Internally coherent caller-supplied Python marker fields.
///
/// Validation does not prove that the fields came from a real interpreter and
/// does not establish an ABI, a platform floor or wheel compatibility.
#[derive(Clone, Debug)]
pub struct PythonMarkerContext {
    environment: MarkerEnvironment,
}

impl PythonMarkerContext {
    /// Validate all eleven explicit fields for version and identity consistency.
    /// No host values are inferred; the caller establishes observation provenance.
    ///
    /// Returns an error for unparsable/incomplete language versions, mismatched
    /// short or implementation versions, unsupported identities or empty platform identities.
    pub fn new(builder: PythonMarkerContextBuilder<'_>) -> Result<Self> {
        let environment = MarkerEnvironment::try_from(builder)
            .map_err(|error| IndexError::InvalidPythonContext(error.to_string()))?;
        let full = &environment.python_full_version().version;
        let release = full.release();
        if release.len() != 3 || release[0] != 3 {
            return Err(IndexError::InvalidPythonContext(
                "expected a complete Python 3.x language version".to_string(),
            ));
        }
        if environment.python_version().string != format!("{}.{}", release[0], release[1]) {
            return Err(IndexError::InvalidPythonContext(
                "python_version disagrees with python_full_version".to_string(),
            ));
        }
        match (
            environment.implementation_name(),
            environment.platform_python_implementation(),
        ) {
            ("cpython", "CPython") => {
                if environment.implementation_version().version != *full {
                    return Err(IndexError::InvalidPythonContext(
                        "CPython implementation version differs from language version".to_string(),
                    ));
                }
            }
            ("pypy", "PyPy") => {}
            _ => {
                return Err(IndexError::InvalidPythonContext(
                    "unsupported or inconsistent Python implementation identity".to_string(),
                ));
            }
        }
        if [
            environment.os_name(),
            environment.platform_machine(),
            environment.platform_system(),
            environment.sys_platform(),
        ]
        .iter()
        .any(|value| value.is_empty())
        {
            return Err(IndexError::InvalidPythonContext(
                "required Python platform identity is empty".to_string(),
            ));
        }
        Ok(Self { environment })
    }

    /// Original complete language-version marker, distinct from PyPy's implementation version.
    pub fn python_full_version(&self) -> &str {
        &self.environment.python_full_version().string
    }

    /// Explicit implementation identity; it is never inferred from the host.
    pub fn implementation_name(&self) -> &str {
        self.environment.implementation_name()
    }

    pub(crate) fn environment(&self) -> &MarkerEnvironment {
        &self.environment
    }
}
