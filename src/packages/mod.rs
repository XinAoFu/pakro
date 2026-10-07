//! mise-native package management: request parsing, config tables, and the
//! glue between resolver/index/store and the rest of mise.

pub mod install;
pub(crate) mod lockfile;
pub(crate) mod materialize;
pub(crate) mod materialize_node;
pub(crate) mod request;
pub mod system;

pub use request::{
    CLI_PACKAGE_ARGS, InstallArg, PackageInstallRequest, extract_bound_package_args,
    parse_install_arg, requests_from_bound, requests_from_config, split_bare_packages,
};
