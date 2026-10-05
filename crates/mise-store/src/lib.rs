//! Content-addressed package store for mise: package identity, disk layout,
//! deduplication across projects, and reference-counted garbage collection.
//!
//! A package's identity is `ecosystem:name@exact-version[+runtime]/platform/digest`;
//! the store lays entries out under that identity so two projects requesting the
//! same artifact share one directory.
