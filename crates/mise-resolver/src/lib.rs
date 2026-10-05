//! Dependency resolution for mise packages. Pure logic: constraint solving over
//! injected metadata, deterministic graph output, and install ordering.
//!
//! Ecosystem identifiers are strings ("python", "node") so this crate carries no
//! business dependencies; typed ecosystem enums live in mise-store and are
//! converted at the glue layer.
