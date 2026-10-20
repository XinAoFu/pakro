//! Readonly runtime source commitments from one bound project observation.
use super::snapshot::ObservedProjectInputs;
use crate::lockfile::PlatformInfo;
use crate::programs::shared::exact_version::ExactPythonRequest;
use eyre::Result;

pub(crate) fn observed_runtime_pin(
    observed: &ObservedProjectInputs,
    request: &ExactPythonRequest,
) -> Result<Option<PlatformInfo>> {
    use crate::lockfile::Lockfile;
    use crate::programs::shared::exact_version::exact_python_request;
    use eyre::{ContextCompat, ensure};
    observed.recheck()?;
    let platform = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let mut selected: Option<PlatformInfo> = None;
    for path in observed
        .paths()
        .filter(|path| path.extension() == Some(std::ffi::OsStr::new("lock")))
    {
        let bytes = observed
            .document(path)
            .context("observed native lock document is missing")?;
        // Both native and Python projections must remain valid before any
        // language acquisition can become a side effect of package intent.
        crate::packages::python_resolution::lock_session::decode_root(bytes, Default::default())?;
        let lockfile = Lockfile::parse_observed(bytes)?;
        let Some(entries) = lockfile.tools().get("python") else {
            continue;
        };
        ensure!(
            entries.len() <= 4096,
            "observed Python runtime pin limit exceeded"
        );
        for entry in entries {
            let entry_request = match exact_python_request(&entry.version) {
                Ok(version) => version,
                Err(_) if entry.specifiers.contains(request.as_str()) => eyre::bail!(
                    "the project runtime lock does not name a complete exact Python version"
                ),
                Err(_) => continue,
            };
            if entry_request.version_only() != request.version_only() {
                continue;
            }
            // A same-version source commitment must never disappear because
            // the newer call requests another build. The native input kernel
            // will check its exact archive against that requested build.
            let Some(pin) = entry.platforms.get(&platform) else {
                continue;
            };
            ensure!(
                entry
                    .backend
                    .as_deref()
                    .is_none_or(|backend| matches!(backend, "core:python" | "python")),
                "the project's Python source pin uses an unsupported backend"
            );
            ensure!(
                entry.options.is_empty(),
                "the project's Python source pin uses unsupported runtime options"
            );
            ensure!(
                pin.url.is_some() && pin.checksum.is_some(),
                "the project's Python runtime source pin needs both an exact URL and checksum"
            );
            ensure!(
                pin.install.is_none()
                    && pin.conda_deps.is_none()
                    && pin.additional_artifacts.is_empty(),
                "the project's Python source pin requires unsupported source or additional artifacts"
            );
            if entry_request.build_pin().is_some()
                && !entry_request.version_only().starts_with("pypy")
            {
                crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::validate_locked_pbs_entry(
                    entry_request.as_str(),
                    pin,
                )?;
            }
            if let Some(previous) = &selected {
                ensure!(
                    previous == pin,
                    "applicable project lock owners contain conflicting Python runtime source pins"
                );
            } else {
                selected = Some(pin.clone());
            }
        }
    }
    observed.recheck()?;
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FrozenConfigDiscovery;
    use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
    use std::time::{Duration, Instant};
    fn observe(root: &std::path::Path) -> ObservedProjectInputs {
        let mut discovery =
            FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
        discovery.ceiling_paths = vec![root.parent().unwrap().to_owned()];
        observe_project(
            root,
            ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(30)),
        )
        .unwrap()
    }
    fn lock(checksum: &str) -> String {
        let platform = crate::backend::platform_target::PlatformTarget::from_current().to_key();
        format!(
            "lockfile_version=3\n[[tools.python]]\nversion='3.13.16'\nbackend='core:python'\nspecifiers=['3.13.16']\n[tools.python.platforms.{platform}]\nurl='https://github.com/astral-sh/python-build-standalone/releases/download/20261013/cpython-3.13.16+20261013-aarch64-apple-darwin-install_only_stripped.tar.gz'\nchecksum='{checksum}'\nprovenance='github-attestations'\n"
        )
    }
    #[test]
    fn f2_observed_runtime_pin_is_preserved_without_writes() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        std::fs::write(root.join("pakro.toml"), "[tools]\npython='3.13.16'\n").unwrap();
        let checksum = format!("sha256:{}", "a".repeat(64));
        let document = lock(&checksum);
        std::fs::write(root.join("pakro.lock"), &document).unwrap();
        let observed = observe(&root);
        let request =
            crate::programs::shared::exact_version::exact_python_request("3.13.16").unwrap();
        let pin = observed_runtime_pin(&observed, &request).unwrap().unwrap();
        assert_eq!(pin.checksum.as_deref(), Some(checksum.as_str()));
        assert!(pin.url.as_deref().unwrap().contains("20261013"));
        assert!(pin.provenance.is_some());
        assert_eq!(
            std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
            document
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
    }
    #[test]
    fn f2_observed_runtime_conflicts_and_partial_pin_are_not_empty() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        std::fs::write(root.join("pakro.toml"), "[tools]\npython='3.13.16'\n").unwrap();
        std::fs::write(root.join("pakro.local.toml"), "[tools]\npython='3.13.16'\n").unwrap();
        std::fs::write(
            root.join("pakro.lock"),
            lock(&format!("sha256:{}", "a".repeat(64))),
        )
        .unwrap();
        std::fs::write(
            root.join("pakro.local.lock"),
            lock(&format!("sha256:{}", "b".repeat(64))),
        )
        .unwrap();
        let observed = observe(&root);
        let request =
            crate::programs::shared::exact_version::exact_python_request("3.13.16").unwrap();
        assert!(observed_runtime_pin(&observed, &request).is_err());
        drop(observed);
        std::fs::remove_file(root.join("pakro.local.lock")).unwrap();
        let document = lock(&format!("sha256:{}", "a".repeat(64)))
            .lines()
            .filter(|line| !line.starts_with("url="))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(root.join("pakro.lock"), document).unwrap();
        let observed = observe(&root);
        assert!(observed_runtime_pin(&observed, &request).is_err());
    }

    #[test]
    fn f2_observed_runtime_entry_build_cannot_disagree_with_frozen_source() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        std::fs::write(root.join("pakro.toml"), "[tools]\npython='3.13.16'\n").unwrap();
        let document = lock(&format!("sha256:{}", "a".repeat(64)))
            .replace("version='3.13.16'", "version='3.13.16+20261009'");
        std::fs::write(root.join("pakro.lock"), &document).unwrap();
        let observed = observe(&root);
        let request =
            crate::programs::shared::exact_version::exact_python_request("3.13.16").unwrap();
        assert!(
            observed_runtime_pin(&observed, &request).is_err(),
            "the lock's explicit build cannot be discarded by a bare current request"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
            document
        );
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 2);
    }
}
