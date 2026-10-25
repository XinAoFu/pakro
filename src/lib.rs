//! pakro's core: project configuration, shared libraries and the services the
//! command line is built on. The `pakro` binary (`src/main.rs` and `cli/src/commands/`)
//! is the only intended consumer; this is not a stable public API.
//!
//! The module tree below is the single table of contents: root keeps only the
//! entry points (`main.rs`, `lib.rs`, `frontend.rs`); implementations live in
//! thematic directories. Files whose declarations carry `#[path]` moved there
//! without changing their `crate::…` paths, and one-line re-export shims of
//! `pakro-util` are absorbed as `use` items.

#![allow(unknown_lints)]
#![deny(unreachable_pub)]

#[path = "core/artifact_snapshot.rs"]
pub(crate) mod artifact_snapshot;

use crate::config::SettingsExt;

#[cfg(test)]
#[macro_use]
#[path = "core/util/test.rs"]
mod test;

#[cfg(test)]
#[path = "build/lockfile_rollout.rs"]
mod lockfile_rollout;
#[path = "build/registry_url.rs"]
mod registry_url;

// Macro modules are declared at the crate root so `#[macro_use]` exposes
// their macros crate-wide; their files live in `core/util/`.

#[macro_use]
#[path = "core/util/output.rs"]
pub mod output;

#[macro_use]
#[path = "core/util/hint.rs"]
pub mod hint;

#[macro_use]
#[path = "core/util/timings.rs"]
pub mod timings;

#[macro_use]
#[path = "core/util/cmd.rs"]
pub mod cmd;

// Modules that are thin re-exports of `pakro-util`: importing the module keeps
// every `crate::x::…` path working without one file per alias.

pub(crate) use pakro_util::{
    deps_graph, gpg, inline_command, packslip_requirements, rand, redactions, remote_source,
    sysconfig, timeout,
};
pub use pakro_util::{
    dirs, duration, exit, forgejo, fuzzy, github, github_relay, gitlab, http, jobs, lock_file,
    packslip_pins, parallel, path, platform, sandbox, semver, tokens, wildcard, windows_console,
    windows_posix,
};

pub mod result {
    pub type Result<T> = eyre::Result<T>;
}

pub use crate::result::Result;
pub use pakro_util::exit::request as request_exit;

// Environment and shell-hook machinery (`environment/`): env var helpers, the
// hook-env protocol, env diffing, watch files and direnv.

#[path = "core/environment/direnv.rs"]
pub mod direnv;
#[path = "core/environment/env.rs"]
pub mod env;
#[path = "core/environment/env_diff.rs"]
pub mod env_diff;
#[path = "core/environment/hook_env.rs"]
pub mod hook_env;
#[path = "core/environment/hooks.rs"]
pub mod hooks;
#[path = "core/environment/path_env.rs"]
pub mod path_env;
#[path = "core/environment/watch_files.rs"]
pub mod watch_files;

// Tool installation machinery (`toolinstall/`): backend install context,
// pre-install orchestration and runtime symlinks.

#[path = "../runtimes/src/toolinstall/install_before.rs"]
pub mod install_before;
#[path = "../runtimes/src/toolinstall/install_context.rs"]
pub mod install_context;
#[path = "../runtimes/src/toolinstall/install_rollback.rs"]
pub(crate) mod install_rollback;
#[path = "../runtimes/src/toolinstall/python_runtime.rs"]
pub mod python_runtime;
#[path = "../runtimes/src/toolinstall/runtime_symlinks.rs"]
pub mod runtime_symlinks;
#[path = "../runtimes/src/toolinstall/tool_purgatory.rs"]
pub mod tool_purgatory;
#[path = "../runtimes/src/toolinstall/tool_stub.rs"]
pub mod tool_stub;

// Shims (`shims/`): the shim layer itself plus the fake-asdf compatibility
// shims (platform-swapped like the rest of pakro's Windows modules).

#[cfg_attr(not(windows), path = "../runtimes/src/shims/fake_asdf.rs")]
#[cfg_attr(windows, path = "../runtimes/src/shims/fake_asdf_windows.rs")]
mod fake_asdf;
#[path = "../runtimes/src/shims/mod.rs"]
pub mod shims;

// Tool discovery (`registry/`): the baked registry, shorthands, the searchable
// tool catalog, and the versions host.

#[path = "../runtimes/src/registry/mod.rs"]
pub mod registry;
#[path = "../runtimes/src/registry/shorthands.rs"]
mod shorthands;
#[path = "../runtimes/src/registry/tool_catalog.rs"]
pub mod tool_catalog;
#[path = "../runtimes/src/registry/versions_host.rs"]
mod versions_host;

// Signed release manifests (`packslip/`).

#[path = "../runtimes/src/packslip/mod.rs"]
pub mod packslip;
#[path = "../runtimes/src/packslip/packslip_forge.rs"]
mod packslip_forge;
#[path = "../runtimes/src/packslip/packslip_stamps.rs"]
mod packslip_stamps;

// General utilities (`util/`): small helpers shared across the crate.

#[path = "core/util/build_time.rs"]
pub mod build_time;
#[path = "core/util/cache.rs"]
pub mod cache;
#[path = "core/util/errors.rs"]
pub mod errors;
#[path = "core/util/file.rs"]
pub mod file;
#[path = "core/util/git.rs"]
pub mod git;
#[path = "core/util/hash.rs"]
pub mod hash;
#[path = "core/util/logger.rs"]
pub mod logger;
#[path = "core/util/maplit.rs"]
pub(crate) mod maplit;
#[path = "core/util/migrate.rs"]
pub mod migrate;
#[path = "core/util/minisign.rs"]
pub mod minisign;
#[path = "core/util/sops.rs"]
mod sops;
#[path = "core/util/tera.rs"]
pub mod tera;
#[doc(hidden)]
#[path = "core/util/testing.rs"]
pub mod testing;
#[path = "core/util/toml.rs"]
pub mod toml;
#[path = "core/util/upgrade_hint.rs"]
pub mod upgrade_hint;
#[path = "core/util/version.rs"]
pub mod version;
#[cfg(windows)]
#[path = "core/util/windows_job.rs"]
mod windows_job;

// Subsystem directories keep their own `mod.rs` / entry files, and
// `frontend.rs` stays at the root as the seam between core and `cli`.

#[path = "core/agecrypt/mod.rs"]
pub mod agecrypt;
#[path = "../runtimes/src/aqua/mod.rs"]
pub mod aqua;
#[path = "../cli/src/args/mod.rs"]
pub mod args;
#[path = "../runtimes/src/backend/mod.rs"]
pub mod backend;
#[path = "core/config/mod.rs"]
pub mod config;
pub mod frontend;
#[path = "../runtimes/src/install_layout/mod.rs"]
pub mod install_layout;
#[path = "core/libraries/mod.rs"]
pub(crate) mod libraries;
#[path = "core/lockfile/mod.rs"]
pub mod lockfile;
#[path = "../packages/src/mod.rs"]
pub mod packages;
#[path = "../runtimes/src/plugins/mod.rs"]
pub mod plugins;
#[path = "core/programs/mod.rs"]
pub mod programs;
#[path = "../cli/src/shell/mod.rs"]
pub mod shell;
#[path = "../runtimes/src/toolset/mod.rs"]
pub mod toolset;
#[path = "../cli/src/ui/mod.rs"]
pub mod ui;

/// Register what pakro's lower crates need from pakro itself (its settings loader,
/// build identity, version and config-layer lookups) before anything uses them.
/// Runs first in `main` and in the test harness constructor. The binary also
/// registers its [`frontend`] there.
pub fn register_util_hooks() {
    config::settings::register_loader();
    cache::register_base_cache_keys();
    let shell = env::PAKRO_SHELL.map(|s| s.to_string()).unwrap_or_default();
    pakro_util::user_agent::set(
        format!("pakro/{} {shell}", *version::VERSION)
            .trim()
            .to_string(),
    );
    pakro_util::deprecation::set_version(env!("CARGO_PKG_VERSION"));
    pakro_util::env::set_pakro_env(|| env::PAKRO_ENV.as_slice());
    pakro_util::shells::set_implicit_inline_shell(|| {
        config::Settings::get().implicit_inline_shell()
    });
}
