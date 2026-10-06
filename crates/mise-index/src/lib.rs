//! Package registry metadata for mise. Fetching and parsing ecosystem
//! registries (PyPI first), plus pure-logic wheel filename and platform tag
//! matching. Network access lives here only; solving stays in mise-resolver
//! and disk storage in mise-store.

pub mod cache;
pub mod error;
pub mod npm;
pub mod pypi;
pub mod types;
pub mod wheel;

pub use error::{IndexError, Result};
pub use pypi::{Index, parse_pypi_response};
pub use types::{
    ArtifactDescriptor, ArtifactKind, DependencyConstraint, PackageMetadata, normalize_py_name,
};
pub use wheel::{RuntimeInfo, WheelFile, WheelTags, parse_wheel_filename, wheel_matches};
