//! Package registry metadata for mise. Fetching and parsing ecosystem
//! registries (PyPI first), plus pure-logic wheel filename and platform tag
//! matching. Network access lives here only; solving stays in mise-resolver
//! and disk storage in mise-store.

pub mod wheel;

pub use wheel::{WheelFile, WheelTags, parse_wheel_filename};
