//! mise's core: config, toolsets, backends, tasks and everything else the
//! command line is built on. The `mise` binary (`src/main.rs` and `src/cli/`)
//! is the only intended consumer; this is not a stable public API.
//!
//! The module tree below is the single table of contents: root keeps only the
//! entry points (`main.rs`, `lib.rs`, `frontend.rs`); implementations live in
//! thematic directories. Files whose declarations carry `#[path]` moved there
//! without changing their `crate::…` paths, and one-line re-export shims of
//! `mise-util` are absorbed as `use` items.

#![allow(unknown_lints)]
#![deny(unreachable_pub)]

use crate::config::SettingsExt;

#[cfg(test)]
#[macro_use]
#[path = "util/test.rs"]
mod test;

#[cfg(test)]
#[path = "../build/lockfile_rollout.rs"]
mod lockfile_rollout;
#[path = "../build/registry_url.rs"]
mod registry_url;

// Macro modules are declared at the crate root so `#[macro_use]` exposes
// their macros crate-wide; their files live in `util/`.

#[macro_use]
#[path = "util/output.rs"]
pub mod output;

#[macro_use]
#[path = "util/hint.rs"]
pub mod hint;

#[macro_use]
#[path = "util/timings.rs"]
pub mod timings;

#[macro_use]
#[path = "util/cmd.rs"]
pub mod cmd;

// Modules that are thin re-exports of `mise-util`: importing the module keeps
// every `crate::x::…` path working without one file per alias.

pub(crate) use mise_util::{
    deps_graph, gpg, inline_command, packslip_requirements, rand, redactions, remote_source,
    sysconfig, timeout,
};
pub use mise_util::{
    dirs, duration, exit, forgejo, fuzzy, github, github_relay, gitlab, http, jobs, lock_file,
    packslip_pins, parallel, path, platform, sandbox, semver, tokens, wildcard, windows_console,
    windows_posix,
};

pub mod result {
    pub type Result<T> = eyre::Result<T>;
}

pub use crate::result::Result;
pub use mise_util::exit::request as request_exit;

// Environment and shell-hook machinery (`environment/`): env var helpers, the
// hook-env protocol, env diffing, watch files, direnv, uv venv integration.

#[path = "environment/direnv.rs"]
pub mod direnv;
#[path = "environment/env.rs"]
pub mod env;
#[path = "environment/env_diff.rs"]
pub mod env_diff;
#[path = "environment/hook_env.rs"]
pub mod hook_env;
#[path = "environment/hooks.rs"]
pub mod hooks;
#[path = "environment/path_env.rs"]
pub mod path_env;
#[path = "environment/uv.rs"]
mod uv;
#[path = "environment/watch_files.rs"]
pub mod watch_files;

// Tool installation machinery (`toolinstall/`): backend install context,
// pre-install orchestration, system package fallbacks, runtime symlinks.

#[path = "toolinstall/install_before.rs"]
pub mod install_before;
#[path = "toolinstall/install_context.rs"]
pub mod install_context;
#[path = "toolinstall/runtime_symlinks.rs"]
pub mod runtime_symlinks;
#[cfg(unix)]
#[path = "toolinstall/system_install.rs"]
pub mod system_install;
#[path = "toolinstall/tool_purgatory.rs"]
pub mod tool_purgatory;
#[path = "toolinstall/tool_stub.rs"]
pub mod tool_stub;

// Shims (`shims/`): the shim layer itself plus the fake-asdf compatibility
// shims (platform-swapped like the rest of mise's Windows modules).

#[cfg_attr(not(windows), path = "shims/fake_asdf.rs")]
#[cfg_attr(windows, path = "shims/fake_asdf_windows.rs")]
mod fake_asdf;
pub mod shims;

// Tool discovery (`registry/`): the baked registry, shorthands, the searchable
// tool catalog, and the versions host.

pub mod registry;
#[path = "registry/shorthands.rs"]
mod shorthands;
#[path = "registry/tool_catalog.rs"]
pub mod tool_catalog;
#[path = "registry/versions_host.rs"]
mod versions_host;

// Signed release manifests (`packslip/`).

pub mod packslip;
#[path = "packslip/packslip_forge.rs"]
mod packslip_forge;
#[path = "packslip/packslip_stamps.rs"]
mod packslip_stamps;

// General utilities (`util/`): small helpers shared across the crate.

#[path = "util/build_time.rs"]
pub mod build_time;
#[path = "util/cache.rs"]
pub mod cache;
#[path = "util/errors.rs"]
pub mod errors;
#[path = "util/file.rs"]
pub mod file;
#[path = "util/git.rs"]
pub mod git;
#[path = "util/hash.rs"]
pub mod hash;
#[path = "util/logger.rs"]
pub mod logger;
#[path = "util/maplit.rs"]
pub(crate) mod maplit;
#[path = "util/migrate.rs"]
pub mod migrate;
#[path = "util/minisign.rs"]
pub mod minisign;
#[path = "util/sops.rs"]
mod sops;
#[path = "util/tera.rs"]
pub mod tera;
#[doc(hidden)]
#[path = "util/testing.rs"]
pub mod testing;
#[path = "util/toml.rs"]
pub mod toml;
#[path = "util/upgrade_hint.rs"]
pub mod upgrade_hint;
#[path = "util/version.rs"]
pub mod version;
#[cfg(windows)]
#[path = "util/windows_job.rs"]
mod windows_job;

// Subsystem directories keep their own `mod.rs` / entry files, and
// `frontend.rs` stays at the root as the seam between core and `cli`.

pub mod agecrypt;
pub mod aqua;
pub mod args;
pub mod backend;
pub mod config;
pub mod daemons;
pub mod deps;
pub mod frontend;
pub mod install_layout;
pub mod lockfile;
pub mod oci;
pub mod otel;
pub mod packages;
pub mod plugins;
pub mod shell;
pub mod system;
pub mod task;
pub mod toolset;
pub mod ui;

/// Register what mise's lower crates need from mise itself (its settings loader,
/// build identity, version and config-layer lookups) before anything uses them.
/// Runs first in `main` and in the test harness constructor. The binary also
/// registers its [`frontend`] there.
pub fn register_util_hooks() {
    config::settings::register_loader();
    cache::register_base_cache_keys();
    let shell = env::MISE_SHELL.map(|s| s.to_string()).unwrap_or_default();
    mise_util::user_agent::set(
        format!("mise/{} {shell}", *version::VERSION)
            .trim()
            .to_string(),
    );
    mise_util::deprecation::set_version(env!("CARGO_PKG_VERSION"));
    mise_util::env::set_mise_env(|| env::MISE_ENV.as_slice());
    mise_util::shells::set_implicit_inline_shell(|| {
        config::Settings::get().implicit_inline_shell()
    });
}
