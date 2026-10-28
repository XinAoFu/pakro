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
    let active_names = observed
        .declarations()
        .find_map(|(_, declarations)| {
            let names: Vec<_> = declarations
                .tools
                .iter()
                .filter(|tool| {
                    crate::programs::query::snapshot::python_tool_name(observed, &tool.name)
                })
                .map(|tool| tool.name.as_str())
                .collect();
            (!names.is_empty()).then_some(names)
        })
        .unwrap_or_default();
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
        let entries = lockfile
            .tools()
            .iter()
            .filter(|(name, _)| {
                (name.as_str() == "python" || active_names.contains(&name.as_str()))
                    && crate::programs::query::snapshot::python_tool_name(observed, name)
            })
            .flat_map(|(_, entries)| entries)
            .collect::<Vec<_>>();
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
                entry.options.iter().all(|(key,value)| key == "source" && matches!(value.as_str(), "prebuilt" | "source")),
                "the project's Python source pin uses unsupported runtime options"
            );
            ensure!(
                pin.url.is_some() && pin.checksum.is_some(),
                "the project's Python runtime source pin needs both an exact URL and checksum"
            );
            ensure!(
                (pin.install.is_none() || pin.source_python.is_some())
                    && pin.conda_deps.is_none()
                    && pin.additional_artifacts.is_empty(),
                "the project's Python source pin requires unsupported source or additional artifacts"
            );
            let source = crate::libraries::language::source::input_from_pin(&entry.version, pin)?;
            ensure!(source.is_some() == entry.options.get("source").is_some_and(|value| value == "source"),
                "the project's Python source kind is missing or conflicts with its build result");
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

/// An update may choose another release, but it does not turn existing lock
/// facts into unvalidated hints. Validate every old Python entry first, then
/// retain an applicable source commitment as a preference for the same release.
pub(crate) fn observed_runtime_pin_for_update(
    observed: &ObservedProjectInputs,
    request: &ExactPythonRequest,
) -> Result<Option<PlatformInfo>> {
    use crate::lockfile::Lockfile;
    use crate::programs::shared::exact_version::exact_python_request;
    use eyre::{Context, ContextCompat, ensure};
    observed.recheck()?;
    let platform = crate::backend::platform_target::PlatformTarget::from_current().to_key();
    let active_names = observed
        .declarations()
        .flat_map(|(_, declarations)| &declarations.tools)
        .filter(|tool| crate::programs::query::snapshot::python_tool_name(observed, &tool.name))
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut selected: Option<PlatformInfo> = None;
    let mut old_pins = std::collections::BTreeMap::new();
    let mut count = 0usize;
    for path in observed
        .paths()
        .filter(|path| path.extension() == Some(std::ffi::OsStr::new("lock")))
    {
        let bytes = observed
            .document(path)
            .context("observed native lock document is missing")?;
        crate::packages::python_resolution::lock_session::decode_root(bytes, Default::default())?;
        let lockfile = Lockfile::parse_observed(bytes)?;
        for (name, entries) in lockfile.tools() {
            for entry in entries {
                // A renamed or removed alias cannot hide a Python entry that
                // explicitly records its old backend identity.
                if name != "python"
                    && !active_names.contains(name.as_str())
                    && !entry.backend.as_deref().is_some_and(|backend| {
                        matches!(backend, "core:python" | "python" | "asdf:python")
                    })
                {
                    continue;
                }
                count += 1;
                ensure!(count <= 4096, "observed Python runtime pin limit exceeded");
                let entry_request = exact_python_request(&entry.version).context(
                    "the project runtime lock does not name a complete exact Python version",
                )?;
                ensure!(
                    entry
                        .backend
                        .as_deref()
                        .is_none_or(|backend| { matches!(backend, "core:python" | "python") }),
                    "the project's Python source pin uses an unsupported backend"
                );
                ensure!(
                    entry.options.iter().all(|(key,value)| key == "source" && matches!(value.as_str(), "prebuilt" | "source")),
                    "the project's Python source pin uses unsupported runtime options"
                );
                ensure!(
                    !entry.platforms.is_empty(),
                    "the project's Python runtime source pin has no platform artifact"
                );
                for (target, pin) in &entry.platforms {
                    ensure!(
                        pin.url.is_some() && pin.checksum.is_some(),
                        "the project's Python runtime source pin needs both an exact URL and checksum"
                    );
                    ensure!(
                        (pin.install.is_none() || pin.source_python.is_some())
                            && pin.conda_deps.is_none()
                            && pin.additional_artifacts.is_empty(),
                        "the project's Python source pin requires unsupported source or additional artifacts"
                    );
                    let source = crate::libraries::language::source::input_from_pin(&entry.version, pin)?;
                    ensure!(source.is_some() == entry.options.get("source").is_some_and(|value| value == "source"),
                        "the old Python source kind conflicts with its complete build result");
                    let old_build = if let Some(source) = source { format!("source-{}", hex::encode(source.request.fingerprint())) } else {
                        crate::plugins::core::python::native::pipeline::RuntimeInstallPlan::validate_locked_pbs_entry(entry_request.as_str(), pin)?;
                        let locator = url::Url::parse(pin.url.as_deref().expect("validated URL"))?;
                        let filename = locator.path_segments().and_then(|mut parts| parts.next_back()).context("locked Python runtime filename is missing")?;
                        crate::plugins::core::python::native::pbs_filename(&urlencoding::decode(filename)?)?
                            .context("locked Python artifact is not a supported PBS install archive")?.1
                    };
                    let old_identity = (
                        entry_request.version_only().to_owned(),
                        old_build.clone(),
                        target.clone(),
                    );
                    if let Some(previous) = old_pins.insert(old_identity, pin.clone()) {
                        ensure!(
                            previous == *pin,
                            "project lock owners contain conflicting old Python runtime source pins"
                        );
                    }
                    if target != &platform
                        || entry_request.version_only() != request.version_only()
                        || request.build_pin().is_some_and(|build| build != old_build)
                    {
                        continue;
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

    fn update_fixture(document: &str, declaration: &str) -> tempfile::TempDir {
        let owner = tempfile::tempdir().unwrap();
        std::fs::write(
            owner.path().join("pakro.toml"),
            format!("[tools]\npython='{declaration}'\n"),
        )
        .unwrap();
        std::fs::write(owner.path().join("pakro.lock"), document).unwrap();
        owner
    }

    #[test]
    fn p3_update_preserves_the_same_release_build_and_original_source_without_writes() {
        let document = lock(&format!("sha256:{}", "a".repeat(64)))
            .replace("version='3.13.16'", "version='3.13.16+20261013'");
        for declaration in ["3.13.16", "3.13.16+20261013"] {
            let owner = update_fixture(&document, declaration);
            let root = owner.path().canonicalize().unwrap();
            let observed = observe(&root);
            let request =
                crate::programs::shared::exact_version::exact_python_request(declaration).unwrap();
            let pin = observed_runtime_pin_for_update(&observed, &request)
                .unwrap()
                .unwrap();
            let strict = observed_runtime_pin(&observed, &request).unwrap().unwrap();
            assert_eq!(pin, strict);
            assert!(pin.url.as_deref().unwrap().contains("3.13.16+20261013"));
            assert!(pin.provenance.is_some());
            assert_eq!(
                std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
                document
            );
            assert_eq!(std::fs::read_dir(root).unwrap().count(), 2);
        }
    }

    #[test]
    fn p3_update_can_retarget_version_or_build_without_inheriting_the_old_pin() {
        let document = lock(&format!("sha256:{}", "a".repeat(64)))
            .replace("version='3.13.16'", "version='3.13.16+20261013'");
        for declaration in ["3.14.0", "3.13.16+20261025"] {
            let owner = update_fixture(&document, declaration);
            let root = owner.path().canonicalize().unwrap();
            let observed = observe(&root);
            let request =
                crate::programs::shared::exact_version::exact_python_request(declaration).unwrap();
            assert!(
                observed_runtime_pin_for_update(&observed, &request)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
                document
            );
        }
        // Original P2 entries can carry the build only in their exact URL.
        let bare_entry = lock(&format!("sha256:{}", "a".repeat(64)));
        let owner = update_fixture(&bare_entry, "3.13.16+20261013");
        let observed = observe(&owner.path().canonicalize().unwrap());
        let matching =
            crate::programs::shared::exact_version::exact_python_request("3.13.16+20261013")
                .unwrap();
        assert!(
            observed_runtime_pin_for_update(&observed, &matching)
                .unwrap()
                .is_some()
        );
        let different =
            crate::programs::shared::exact_version::exact_python_request("3.13.16+20261025")
                .unwrap();
        assert!(
            observed_runtime_pin_for_update(&observed, &different)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn p3_update_retarget_does_not_hide_corrupt_or_unsupported_old_lock_facts() {
        let valid = lock(&format!("sha256:{}", "a".repeat(64)))
            .replace("version='3.13.16'", "version='3.13.16+20261013'");
        let documents = [
            valid.replace("backend='core:python'", "backend='asdf:python'"),
            valid.replace("specifiers=", "options={compile='true'}\nspecifiers="),
            valid.lines().filter(|line| !line.starts_with("url=")).collect::<Vec<_>>().join("\n"),
            valid.replace(&format!("sha256:{}", "a".repeat(64)), "sha256:broken"),
            valid.replace("tools.python", "tools.oldalias")
                .replace(&format!("sha256:{}", "a".repeat(64)), "sha256:broken"),
            valid.replace("provenance='github-attestations'", "install='external'"),
            valid.replace("provenance='github-attestations'", "additional_artifacts=[{url='https://example.invalid/extra',checksum='sha256:broken'}]"),
            valid.replace("version='3.13.16+20261013'", "version='3.13'"),
            valid.replace("version='3.13.16+20261013'", "version='3.13.16+20261009'"),
            valid.replace("lockfile_version=3", "lockfile_version=9999"),
            format!("{valid}\n[python]\nschema=9999\n"),
        ];
        for document in documents {
            let owner = update_fixture(&document, "3.14.0");
            let root = owner.path().canonicalize().unwrap();
            let observed = observe(&root);
            let request =
                crate::programs::shared::exact_version::exact_python_request("3.14.0").unwrap();
            assert!(
                observed_runtime_pin_for_update(&observed, &request).is_err(),
                "accepted a damaged old runtime pin: {document}"
            );
            assert_eq!(
                std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
                document
            );
        }
    }

    #[test]
    fn p3_update_retarget_rejects_conflicting_old_lock_owners() {
        let document = lock(&format!("sha256:{}", "a".repeat(64)));
        let owner = update_fixture(&document, "3.14.0");
        let root = owner.path().canonicalize().unwrap();
        std::fs::write(root.join("pakro.local.toml"), "# overlay\n").unwrap();
        let other = lock(&format!("sha256:{}", "b".repeat(64)));
        std::fs::write(root.join("pakro.local.lock"), &other).unwrap();
        let observed = observe(&root);
        let request =
            crate::programs::shared::exact_version::exact_python_request("3.14.0").unwrap();
        assert!(observed_runtime_pin_for_update(&observed, &request).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
            document
        );
        assert_eq!(
            std::fs::read_to_string(root.join("pakro.local.lock")).unwrap(),
            other
        );
    }
}
