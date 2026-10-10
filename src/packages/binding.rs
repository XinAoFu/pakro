//! Application-owned package inputs and a Config-aware application snapshot.
//! Pure preparation/guards use explicit snapshots and never invent locations.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eyre::{Context, ContextCompat, Result, bail};
use indexmap::IndexMap;

use crate::backend::backend_type::BackendType;
use crate::toolset::{ToolSource, ToolVersion};

use super::PackageInstallRequest;

/// Invocation policy captured at a CLI/application boundary.
#[derive(Debug, Clone, Copy)]
pub struct PackageInvocationPolicy {
    pub locked: bool,
    pub offline: bool,
}

/// An actual resolved tool with policy captured in the same effective Config.
/// The ToolVersion retains options, install path and the canonical lockfile owner.
#[derive(Debug, Clone)]
pub struct PackageRuntimeBinding {
    pub(crate) tool: ToolVersion,
    pub(crate) locked: bool,
}

/// Capture runtime policy before leaving an effective/monorepo install Config.
/// No runtime is resolved or reconstructed here. This application facade uses
/// existing Config helpers, which can read Settings.locked_scopes indirectly.
/// Pure preparation, guards and the install kernel consume the snapshot only.
pub fn snapshot_runtime_bindings(
    config: &crate::config::Config,
    runtimes: &[ToolVersion],
    invocation_locked: bool,
) -> Vec<PackageRuntimeBinding> {
    runtimes
        .iter()
        .map(|tool| {
            let mut tool = tool.clone();
            tool.install_path = Some(tool.install_path());
            PackageRuntimeBinding {
                locked: config.invocation_locked_for(tool.request.source(), invocation_locked)
                    || tool.request.tool_config_locked(config, true),
                tool,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) struct OwnedPackageRequest {
    pub request: PackageInstallRequest,
    pub source: ToolSource,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GroupPolicy {
    pub locked: bool,
    pub offline: bool,
}

pub(crate) struct PackageInstallInputs {
    pub requests: Vec<PackageInstallRequest>,
    pub python_runtimes: BTreeMap<String, ToolVersion>,
    pub policies: BTreeMap<String, GroupPolicy>,
}

pub(crate) fn merge_owned_requests(
    sources: impl IntoIterator<Item = (ToolSource, Vec<PackageInstallRequest>)>,
    extra: &[PackageInstallRequest],
    cli: &[PackageInstallRequest],
) -> Vec<OwnedPackageRequest> {
    let mut merged = IndexMap::new();
    for (source, requests) in sources.into_iter().chain([
        (ToolSource::Argument, extra.to_vec()),
        (ToolSource::Argument, cli.to_vec()),
    ]) {
        for request in requests {
            merged.insert(
                (request.group_key(), request.name.clone()),
                OwnedPackageRequest {
                    request,
                    source: source.clone(),
                },
            );
        }
    }
    merged.into_values().collect()
}

pub(crate) fn select_python_tool(
    runtimes: &[PackageRuntimeBinding],
    selector: &str,
) -> Result<PackageRuntimeBinding> {
    let candidates = runtimes
        .iter()
        .filter(|binding| {
            let tv = &binding.tool;
            tv.ba().backend_type() == BackendType::Core
                && tv.ba().tool_name == "python"
                && (tv.version == selector || tv.request.version() == selector)
        })
        .collect::<Vec<_>>();
    let has_argument = candidates
        .iter()
        .any(|binding| matches!(binding.tool.request.source(), ToolSource::Argument));
    let mut identities: BTreeMap<PathBuf, PackageRuntimeBinding> = BTreeMap::new();
    for binding in candidates {
        let tv = &binding.tool;
        if has_argument && !matches!(tv.request.source(), ToolSource::Argument) {
            continue;
        }
        let path = tv
            .install_path()
            .canonicalize()
            .context("bound Python install root")?;
        if let Some(existing) = identities.get_mut(&path) {
            if existing.tool.version != tv.version
                || existing.tool.request.options() != tv.request.options()
                || existing.tool.request.lockfile_source() != tv.request.lockfile_source()
            {
                bail!("ambiguous Python runtime binding for {selector}");
            }
            existing.locked |= binding.locked;
        } else {
            identities.insert(path, binding.clone());
        }
    }
    if identities.len() != 1 {
        bail!(
            "expected one actual installed Python binding for {selector}, found {}",
            identities.len()
        );
    }
    identities
        .into_values()
        .next()
        .ok_or_else(|| eyre::eyre!("Python binding disappeared"))
}

pub(crate) fn prepare_inputs(
    owned: &[OwnedPackageRequest],
    runtimes: &[PackageRuntimeBinding],
    offline: bool,
    source_locked: impl Fn(&ToolSource) -> bool,
) -> Result<PackageInstallInputs> {
    let mut python_runtimes = BTreeMap::new();
    let mut policies: BTreeMap<String, GroupPolicy> = BTreeMap::new();
    for owned in owned {
        if owned.request.runtime_name != "python" {
            continue;
        }
        let key = owned.request.group_key();
        if let std::collections::btree_map::Entry::Vacant(entry) =
            python_runtimes.entry(key.clone())
        {
            let binding = select_python_tool(runtimes, &owned.request.runtime_version)?;
            policies.insert(
                key.clone(),
                GroupPolicy {
                    locked: binding.locked,
                    offline,
                },
            );
            entry.insert(binding.tool);
        }
        let policy = policies
            .get_mut(&key)
            .context("Python group policy missing")?;
        policy.locked |= source_locked(&owned.source);
    }
    Ok(PackageInstallInputs {
        requests: owned.iter().map(|owned| owned.request.clone()).collect(),
        python_runtimes,
        policies,
    })
}

pub(crate) fn guard_python_sidecar_migration(inputs: &PackageInstallInputs) -> Result<()> {
    for key in inputs.python_runtimes.keys() {
        let policy = inputs
            .policies
            .get(key)
            .context("Python group policy missing")?;
        if policy.locked || policy.offline {
            let mode = match (policy.locked, policy.offline) {
                (true, true) => "locked and offline",
                (true, false) => "locked",
                _ => "offline",
            };
            bail!(
                "native Python package lock migration required for {key}: {mode} cannot replay the legacy or missing package lock; a verified unified lock is required"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolset::{ToolRequest, tool_request::LockfileScope};
    use std::sync::Arc;

    fn request(version: &str) -> PackageInstallRequest {
        PackageInstallRequest {
            runtime_name: "python".into(),
            runtime_version: "3.10.14".into(),
            name: "demo".into(),
            version: version.into(),
        }
    }

    fn runtime(root: &std::path::Path, source: ToolSource) -> ToolVersion {
        let custom = root.join("custom-runtime");
        std::fs::create_dir_all(custom.join("bin")).unwrap();
        std::fs::write(custom.join("bin/python"), b"fixture").unwrap();
        let backend = Arc::new(crate::plugins::core::new_backend_arg("python"));
        let request = ToolRequest::new(backend, "3.10.14", source).unwrap();
        let mut tv = ToolVersion::new(request, "3.10.14".into());
        tv.install_path = Some(custom);
        tv.install_satisfied = Some(true);
        tv
    }

    fn bound(tool: ToolVersion, locked: bool) -> PackageRuntimeBinding {
        PackageRuntimeBinding { tool, locked }
    }

    #[tokio::test]
    async fn application_snapshot_freezes_an_unset_actual_install_path() {
        let root = tempfile::tempdir().unwrap();
        let mut tv = runtime(root.path(), ToolSource::Argument);
        let expected = tv.install_path.take().unwrap();
        tv.request = ToolRequest::new(
            Arc::new(crate::plugins::core::new_backend_arg("python")),
            &format!("path:{}", expected.display()),
            ToolSource::Argument,
        )
        .unwrap();
        let config = crate::config::Config::get().await.unwrap();
        let bindings = snapshot_runtime_bindings(&config, &[tv], false);
        assert_eq!(
            bindings[0].tool.install_path.as_deref(),
            Some(expected.as_path())
        );
    }

    #[test]
    fn winning_package_declaration_keeps_its_owner() {
        let global = ToolSource::MiseToml("/global/config.toml".into());
        let project = ToolSource::MiseToml("/project/mise.toml".into());
        let owned = merge_owned_requests(
            [
                (global, vec![request("1.0")]),
                (project.clone(), vec![request("2.0")]),
            ],
            &[],
            &[],
        );
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].request.version, "2.0");
        assert_eq!(owned[0].source, project);
        let extra = merge_owned_requests([(project, vec![request("2.0")])], &[request("3.0")], &[]);
        assert_eq!(extra[0].request.version, "3.0");
        assert_eq!(extra[0].source, ToolSource::Argument);
    }

    #[test]
    fn already_installed_cli_only_runtime_keeps_actual_layout_and_owner() {
        let root = tempfile::tempdir().unwrap();
        let mut tv = runtime(root.path(), ToolSource::Argument);
        let owner = ToolSource::MiseToml("/owner/mise.toml".into());
        tv.request
            .set_lockfile_scope(LockfileScope::Source(owner.clone()));
        let inputs = prepare_inputs(
            &merge_owned_requests([], &[request("1.0")], &[]),
            &[bound(tv.clone(), false)],
            false,
            |_| false,
        )
        .unwrap();
        let actual = &inputs.python_runtimes["python@3.10.14"];
        assert_eq!(actual.install_path(), tv.install_path());
        assert_eq!(actual.install_satisfied, Some(true));
        assert_eq!(actual.request.source(), &ToolSource::Argument);
        assert_eq!(actual.request.lockfile_source(), Some(&owner));
        assert_eq!(actual.request.options(), tv.request.options());
    }

    #[test]
    fn package_owner_and_runtime_lockfile_owner_are_both_enforced() {
        let root = tempfile::tempdir().unwrap();
        let package_owner = ToolSource::MiseToml("/package/mise.toml".into());
        let runtime_owner = ToolSource::MiseToml("/runtime/mise.toml".into());
        let mut tv = runtime(root.path(), ToolSource::Argument);
        tv.request
            .set_lockfile_scope(LockfileScope::Source(runtime_owner.clone()));
        let owned = merge_owned_requests([(package_owner.clone(), vec![request("1.0")])], &[], &[]);
        for (package_locked, runtime_locked) in [(true, false), (false, true)] {
            let inputs = prepare_inputs(
                &owned,
                &[bound(
                    tv.clone(),
                    runtime_locked && tv.request.lockfile_source() == Some(&runtime_owner),
                )],
                false,
                |source| package_locked && source == &package_owner,
            )
            .unwrap();
            let error = guard_python_sidecar_migration(&inputs).unwrap_err();
            assert!(error.to_string().contains("locked"), "{error}");
        }
        tv.request.set_lockfile_scope(LockfileScope::NoOwner);
        let locked = tv.request.lockfile_source() == Some(&runtime_owner);
        let inputs = prepare_inputs(&owned, &[bound(tv, locked)], false, |_| false).unwrap();
        assert!(guard_python_sidecar_migration(&inputs).is_ok());
    }

    #[test]
    fn invocation_scope_decision_is_supplied_by_the_application_boundary() {
        let root = tempfile::tempdir().unwrap();
        let package_owner = ToolSource::MiseToml("/project/mise.toml".into());
        let tv = runtime(
            root.path(),
            ToolSource::MiseToml("/project/runtime.toml".into()),
        );
        let owned = merge_owned_requests([(package_owner, vec![request("1.0")])], &[], &[]);
        let excluded =
            prepare_inputs(&owned, &[bound(tv.clone(), false)], false, |_| false).unwrap();
        assert!(guard_python_sidecar_migration(&excluded).is_ok());
        let included = prepare_inputs(&owned, &[bound(tv, true)], false, |_| true).unwrap();
        assert!(guard_python_sidecar_migration(&included).is_err());
    }

    #[tokio::test]
    async fn actual_application_scope_helper_keeps_arguments_strict() {
        let config = crate::config::Config::get().await.unwrap();
        // An argument has no config scope; locked_scopes cannot exclude it.
        assert!(config.invocation_locked_for(&ToolSource::Argument, true));
        assert!(!config.invocation_locked_for(&ToolSource::Argument, false));
    }

    #[test]
    fn offline_python_requires_verified_lock_but_does_not_change_node_policy() {
        let root = tempfile::tempdir().unwrap();
        let tv = runtime(root.path(), ToolSource::Argument);
        let owned = merge_owned_requests([], &[request("1.0")], &[]);
        let inputs = prepare_inputs(&owned, &[bound(tv, false)], true, |_| false).unwrap();
        assert!(
            guard_python_sidecar_migration(&inputs)
                .unwrap_err()
                .to_string()
                .contains("offline")
        );
        let mut node = request("1.0");
        node.runtime_name = "node".into();
        node.runtime_version = "22.0.0".into();
        let inputs =
            prepare_inputs(&merge_owned_requests([], &[node], &[]), &[], true, |_| true).unwrap();
        assert!(guard_python_sidecar_migration(&inputs).is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_policy_makes_no_metadata_request_store_or_lock_write() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/demo/1.0/json")
            .with_status(503)
            .expect(0)
            .create_async()
            .await;
        for (locked, offline) in [(true, false), (false, true), (true, true)] {
            let root = tempfile::tempdir().unwrap();
            let tv = runtime(root.path(), ToolSource::Argument);
            let owned = merge_owned_requests([], &[request("1.0")], &[]);
            let inputs = prepare_inputs(&owned, &[bound(tv, locked)], offline, |_| locked).unwrap();
            let lock = root.path().join(super::super::lockfile::LOCKFILE_NAME);
            let before =
                b"[groups]\n'python@3.10.14' = [{ name = 'demo', version = '1.0', digest = '' }]\n";
            std::fs::write(&lock, before).unwrap();
            let locations = super::super::install::Locations {
                config_root: root.path().into(),
                project_root: root.path().into(),
                store_root: root.path().join("store"),
                index_cache: root.path().join("index"),
            };
            let index = Arc::new(
                mise_index::Index::new(&locations.index_cache).with_base_url(server.url()),
            );
            let error = super::super::install::install_packages(&inputs, &locations, index)
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("native Python package lock migration required"),
                "{error}"
            );
            assert!(!locations.store_root.exists());
            assert!(!locations.index_cache.exists());
            assert_eq!(std::fs::read(&lock).unwrap(), before);
        }
        mock.assert_async().await;
    }
}
