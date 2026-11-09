//! Read-only version listings from the configured public package metadata.
use std::{collections::BTreeSet, future::IntoFuture, str::FromStr, time::Instant};

use crate::config::{Settings, SettingsExt};
use eyre::{Context, ContextCompat, Result, ensure};

/// Query metadata without selecting or downloading artifacts or creating a project.
pub async fn list_versions(name: &str, source: &str, channel: Option<&str>) -> Result<Vec<String>> {
    let settings = Settings::get();
    ensure!(
        !settings.offline(),
        "remote package metadata is unavailable in offline mode"
    );
    let deadline = Instant::now()
        .checked_add(settings.http_download_timeout())
        .context("remote package query deadline overflow")?;
    // Metadata clients may cache their responses; keep those writes in a private
    // temporary directory rather than altering the user's cache or project.
    let scratch = tempfile::tempdir()?;
    match source {
        "pypi" => {
            ensure!(channel.is_none(), "--channel is only supported for Conda");
            let project = pakro_index::python_source::NormalizedPythonName::new(name)?;
            let sources = super::python::public_sources()?;
            let registry = &sources.registries[0];
            let client = pakro_index::simple::SimpleIndexClient::new(
                scratch.path(),
                super::python::shared_registry_http_config(&settings),
            )?;
            let scope = pakro_index::python_source::AuthScopeKey::from_local_revision([0; 32]);
            let credentials = super::python::PublicCredentials;
            let grants = pakro_index::simple::AuthOriginGrants::default();
            let route = super::python::FrozenRouter {
                settings: &settings,
            };
            let result = client
                .project_at_deadline(
                    pakro_index::simple::PythonSourceSession {
                        registry,
                        auth_scope: &scope,
                        credentials: &credentials,
                        auth_origins: &grants,
                        transport: &route,
                    },
                    &project,
                    pakro_index::simple::SimpleFetchPolicy {
                        network: pakro_index::artifact::NetworkPolicy::Allowed,
                        refresh: false,
                        fresh_for: std::time::Duration::from_secs(3600),
                        now: std::time::SystemTime::now(),
                    },
                    Default::default(),
                    deadline,
                )
                .await?;
            let pakro_index::simple::ProjectLookup::Present(project) = result else {
                eyre::bail!("PyPI package {name} was not found");
            };
            Ok(pypi_versions(
                project
                    .versions()
                    .iter()
                    .map(String::as_str)
                    .chain(project.files().iter().filter_map(|file| file.version())),
            ))
        }
        "conda" => {
            let package = rattler_conda_types::PackageName::from_str(name)?;
            let names = &settings.python.conda.channels;
            let selected = if let Some(channel) = channel {
                ensure!(
                    names.iter().any(|configured| configured == channel),
                    "Conda channel is not in the configured channel list"
                );
                vec![channel.to_owned()]
            } else {
                names.clone()
            };
            let channels =
                super::install::conda::configured_channels(&selected, std::path::Path::new("/"))?;
            let platform =
                rattler_conda_types::Platform::current().context("unsupported Conda platform")?;
            ensure!(
                matches!(
                    platform,
                    rattler_conda_types::Platform::Osx64
                        | rattler_conda_types::Platform::OsxArm64
                        | rattler_conda_types::Platform::Linux64
                        | rattler_conda_types::Platform::LinuxAarch64
                ) && !cfg!(target_env = "musl"),
                "Conda does not support this platform or libc"
            );
            let http =
                crate::http::Client::new(settings.http_timeout(), crate::http::ClientKind::Http)?;
            let gateway = rattler_repodata_gateway::Gateway::builder()
                .with_client(http.reqwest()?.clone())
                .with_cache_dir(scratch.path().join("conda"))
                .finish();
            let query = gateway
                .query(
                    channels,
                    [platform, rattler_conda_types::Platform::NoArch],
                    vec![rattler_conda_types::MatchSpec::from(package)],
                )
                .channel_relations(rattler_repodata_gateway::ChannelRelationsMode::Disabled)
                .channel_notices(false)
                .recursive(false);
            let rows = tokio::time::timeout_at(deadline.into(), query.into_future())
                .await
                .context("Conda repodata request exceeded its deadline")??;
            let versions = rows
                .iter()
                .flat_map(|row| row.iter())
                .map(|record| record.package_record.version.to_string());
            Ok(conda_versions(versions))
        }
        _ => eyre::bail!("--source must be pypi or conda"),
    }
}

fn pypi_versions<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    use pakro_resolver::solve::{Pep440, VersionAdapter};
    let versions = values
        .filter_map(|raw| Pep440::parse_version(raw).map(|version| (version, raw.to_owned())))
        .collect::<BTreeSet<_>>();
    versions.into_iter().map(|(_, raw)| raw).collect()
}

fn conda_versions(values: impl Iterator<Item = String>) -> Vec<String> {
    let versions = values
        .filter_map(|raw| {
            rattler_conda_types::Version::from_str(&raw)
                .ok()
                .map(|version| (version, raw))
        })
        .collect::<BTreeSet<_>>();
    versions.into_iter().map(|(_, raw)| raw).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_versions_are_unique_and_sorted_by_source_version_rules() {
        assert_eq!(
            pypi_versions(["2.0", "1.10", "1.9", "1.9", "invalid"].into_iter()),
            ["1.9", "1.10", "2.0"]
        );
        assert_eq!(
            conda_versions(["2.0", "1.10", "1.9", "1.9"].into_iter().map(str::to_owned)),
            ["1.9", "1.10", "2.0"]
        );
    }
}
