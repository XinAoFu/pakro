//! Dependency resolution for pakro packages. Pure logic: constraint solving over
//! injected metadata, deterministic graph output, and install ordering.
//!
//! Ecosystem identifiers are strings ("python", "node") so this crate carries no
//! business dependencies; typed ecosystem enums live in pakro-store and are
//! converted at the glue layer.

pub mod error;
pub mod graph;
pub mod npmver;
pub mod pyver;
pub mod solve;

pub use error::{ResolveFailure, ResolverError};
pub use graph::{DependencyRequest, ResolvedGraph, ResolvedNode};
pub use solve::{MetadataSource, solve};
