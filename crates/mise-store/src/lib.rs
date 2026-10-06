//! Content-addressed package store for mise: package identity, disk layout,
//! deduplication across projects, and reference-counted garbage collection.
//!
//! A package's identity is `ecosystem:name@exact-version[+runtime]/platform/digest`;
//! the store lays entries out under that identity so two projects requesting the
//! same artifact share one directory.

pub mod error;
pub mod fs;
pub mod identity;
pub mod path;
pub mod refcount;
pub mod store;

pub use error::{Result, StoreError};
pub use identity::{Arch, Digest, Eco, Libc, Os, PackageIdentity, Platform, RuntimeId};
pub use refcount::{GcReport, Refs, gc};
pub use store::Store;

pub use fs::hardlink_tree;
