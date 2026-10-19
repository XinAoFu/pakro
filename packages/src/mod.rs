//! pakro-native package management: request parsing, config tables, and the
//! glue between resolver/index/store and the rest of pakro.

pub(crate) mod binding;
pub mod install;
pub(crate) mod lockfile;
pub(crate) mod materialize;
pub(crate) mod materialize_node;
pub(crate) mod python;
pub(crate) mod python_declarations;
pub(crate) mod python_environment;
pub(crate) mod python_project;
pub(crate) mod python_resolution;
pub(crate) mod python_target;
pub(crate) mod python_wheel;
pub(crate) mod python_wheel_install;
pub(crate) mod python_wheel_plan;
pub(crate) mod python_wheel_scripts;
pub(crate) mod python_wheel_source;
pub mod request;
pub(crate) mod system;
pub(crate) mod venv;

// the surface the rest of pakro consumes: CLI arg rewriting (cli/mod.rs),
// bare-name binding (cli/install.rs), config tables (config/), env injection
// (toolset/). The request/venv internals stay reachable through their modules.
pub use request::{
    CLI_PACKAGE_ARGS, PackageInstallRequest, extract_bound_package_args, requests_from_config,
    split_bare_packages,
};
pub use venv::venv_bin_dirs;
