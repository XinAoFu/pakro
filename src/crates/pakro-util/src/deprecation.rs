//! Deprecation warnings gated on the running pakro version, which pakro registers
//! at startup. The same rules as pakro's own `deprecated_at!`: warn from
//! `warn_at`, and fail debug builds once `remove_at` is reached.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex, OnceLock};

use versions::Versioning;

static VERSION: OnceLock<Versioning> = OnceLock::new();

#[doc(hidden)]
pub static DEPRECATED: LazyLock<Mutex<HashSet<&'static str>>> = LazyLock::new(Default::default);

/// Register the running pakro version, e.g. `env!("CARGO_PKG_VERSION")` of pakro.
pub fn set_version(version: &str) {
    let _ = VERSION.set(Versioning::new(version).expect("invalid pakro version"));
}

/// The registered pakro version.
pub fn version() -> &'static Versioning {
    VERSION
        .get()
        .expect("pakro_util::deprecation::set_version must be called first")
}
