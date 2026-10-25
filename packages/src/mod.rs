//! pakro-native package management: request parsing, config tables, and the
//! glue between resolver/index/store and the rest of pakro.

pub(crate) mod binding;
pub(crate) mod python_declarations;
pub(crate) mod python_environment;
pub(crate) mod python_project;
pub(crate) mod python_resolution;
pub(crate) mod python_source_build;
pub(crate) mod python_target;
pub(crate) mod python_wheel;
pub(crate) mod python_wheel_install;
pub(crate) mod python_wheel_plan;
pub(crate) mod python_wheel_scripts;
pub(crate) mod python_wheel_source;
pub mod request;

// the surface the rest of pakro consumes: CLI arg rewriting (cli/mod.rs),
// bare-name binding (cli/install.rs), and config tables (config/).
pub use request::{
    CLI_PACKAGE_ARGS, PackageInstallRequest, extract_bound_package_args, requests_from_config,
    split_bare_packages,
};
