//! Conda source resolution. This produces metadata only and grants no install authority.

use std::{collections::BTreeSet, future::IntoFuture, path::Path, str::FromStr, time::Instant};

use eyre::{Context, ContextCompat, Result, ensure};
use rattler_conda_types::{
    Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, Matches, PackageName,
    ParseStrictness, Platform, RepoDataRecord, Version, match_spec::matcher::StringMatcher,
};
use rattler_repodata_gateway::{ChannelRelationsMode, Gateway};
use rattler_solve::{
    ChannelPriority, SolveError, SolveStrategy, SolverImpl, SolverTask,
    resolvo::Solver as ResolvoSolver,
};
use rattler_virtual_packages::{VirtualPackageOverrides, VirtualPackages};

use crate::config::SettingsExt;
use crate::libraries::language::source::{PythonRuntimeRequest, PythonRuntimeSource};
use crate::packages::python_resolution::lock_model::{
    CondaLockGraph, LockModelLimits, PythonLockModel,
};
use crate::packages::request::{PackageInstallRequest, PackageSource};
use crate::programs::OperationContext;

/// Exact direct declaration. Transitive specifications are read from repodata.
#[derive(Debug, Clone)]
pub(crate) struct CondaRoot {
    pub name: PackageName,
    pub version: Version,
    pub build: Option<String>,
    /// Index in the explicitly configured channel list, if the user chose one.
    pub channel: Option<usize>,
}
impl CondaRoot {
    fn match_spec(&self) -> Result<MatchSpec> {
        let mut spec = MatchSpec::from_str(
            &format!("{} =={}", self.name.as_normalized(), self.version),
            ParseStrictness::Strict,
        )?;
        spec.build = self.build.clone().map(StringMatcher::Exact);
        Ok(spec)
    }
}

/// Facts retained for the versioned lock; records do not authorize installation.
#[derive(Debug)]
pub(crate) struct CondaResolution {
    pub channels: Vec<Channel>,
    pub platform: Platform,
    pub virtual_packages: Vec<GenericVirtualPackage>,
    pub records: Vec<RepoDataRecord>,
}

/// Prepare the versioned lock facts from the same frozen configuration and
/// validated requests used by the installation operation. The result remains
/// descriptive until the later installation and commit stages are connected.
pub(crate) async fn resolve_requests_into_lock(
    context: &OperationContext,
    host: &(Platform, Vec<GenericVirtualPackage>),
    runtime: Option<&PythonRuntimeRequest>,
    requests: &[PackageInstallRequest],
    pinned: Vec<RepoDataRecord>,
    preferred: Vec<RepoDataRecord>,
    model: PythonLockModel,
    config_scope: String,
    group: String,
    target_fingerprint: [u8; 32],
    limits: LockModelLimits,
) -> Result<PythonLockModel> {
    context.checkpoint()?;
    let names = &context.settings().python.conda.channels;
    let channels = configured_channels(names, context.requested_data_root())?;
    let channel_index = |channel: &Option<String>| -> Result<Option<usize>> {
        channel
            .as_ref()
            .map(|name| {
                names
                    .iter()
                    .position(|configured| configured == name)
                    .context("explicit Conda channel is not in the configured channel list")
            })
            .transpose()
    };
    let mut roots = Vec::with_capacity(requests.len() + usize::from(runtime.is_some()));
    if let Some(runtime) = runtime {
        ensure!(
            runtime.source == PythonRuntimeSource::Conda,
            "expected a Conda Python runtime request"
        );
        roots.push(CondaRoot {
            name: PackageName::from_str("python")?,
            version: Version::from_str(&runtime.version)?,
            build: runtime.build.clone(),
            channel: channel_index(&runtime.channel)?,
        });
    }
    for request in requests {
        ensure!(
            request.source == PackageSource::Conda,
            "expected a Conda package request"
        );
        ensure!(
            request.marker.is_none(),
            "Conda package markers require a selected Python target and are not implemented yet"
        );
        roots.push(CondaRoot {
            name: PackageName::from_str(&request.name)?,
            version: Version::from_str(&request.version)?,
            build: request.build.clone(),
            channel: channel_index(&request.channel)?,
        });
    }
    let mut names_seen = BTreeSet::new();
    for root in &roots {
        ensure!(
            names_seen.insert(root.name.as_normalized()),
            "duplicate Conda root {}",
            root.name.as_normalized()
        );
    }
    let (platform, virtual_packages) = (host.0, host.1.clone());
    let resolution = resolve(
        context,
        channels.clone(),
        roots.clone(),
        platform,
        virtual_packages,
        pinned,
        preferred,
    )
    .await?;
    let mut locked_roots = Vec::with_capacity(roots.len());
    for root in &roots {
        let mut spec = root.match_spec()?;
        spec.channel = root
            .channel
            .map(|index| std::sync::Arc::new(channels[index].clone()));
        locked_roots.push(spec.to_canonical_string()?);
    }
    Ok(model.with_conda(
        CondaLockGraph {
            config_scope,
            group,
            target: platform.as_str().to_owned(),
            target_fingerprint,
            channels: names.clone(),
            roots: locked_roots,
            virtual_packages: resolution.virtual_packages,
            records: resolution.records,
        },
        limits,
    )?)
}

/// Interpret configured public channel names without consulting ambient settings.
pub(crate) fn configured_channels(names: &[String], root: &Path) -> Result<Vec<Channel>> {
    ensure!(!names.is_empty(), "at least one Conda channel is required");
    let config = ChannelConfig::default_with_root_dir(root.to_path_buf());
    let mut channels = Vec::with_capacity(names.len());
    for name in names {
        let channel = Channel::from_str(name, &config)?;
        ensure!(
            channel.base_url.url().scheme() == "https" && channel.platforms.is_none(),
            "Conda channels must be public HTTPS channels without a platform selector"
        );
        ensure!(
            !channels
                .iter()
                .any(|old: &Channel| old.base_url == channel.base_url),
            "duplicate Conda channel: {name}"
        );
        channels.push(channel);
    }
    Ok(channels)
}

/// Detect the actual native platform and virtual packages once for this solve.
pub(crate) fn native_target() -> Result<(Platform, Vec<GenericVirtualPackage>)> {
    let platform = Platform::current().context("unsupported Conda platform")?;
    ensure!(
        matches!(
            platform,
            Platform::Osx64 | Platform::OsxArm64 | Platform::Linux64 | Platform::LinuxAarch64
        ) && !cfg!(target_env = "musl"),
        "Conda does not support this platform or libc"
    );
    let virtual_packages: Vec<_> =
        VirtualPackages::detect_for_platform(platform, &VirtualPackageOverrides::default(), None)?
            .into_generic_virtual_packages()
            .collect();
    if matches!(platform, Platform::Linux64 | Platform::LinuxAarch64) {
        ensure!(
            virtual_packages
                .iter()
                .any(|package| package.name.as_normalized() == "__glibc"),
            "Conda Linux packages require a detected glibc runtime"
        );
    }
    Ok((platform, virtual_packages))
}

/// Check the current host against the locked packages' virtual requirements.
/// Other packages remain fixed by the already validated lock graph.
pub(crate) fn validate_host(
    graph: &CondaLockGraph,
    host: &(Platform, Vec<GenericVirtualPackage>),
) -> Result<()> {
    ensure!(
        graph.target == host.0.as_str(),
        "Conda lock target differs from the current platform"
    );
    for record in &graph.records {
        for (raw, required) in record
            .package_record
            .depends
            .iter()
            .map(|raw| (raw, true))
            .chain(
                record
                    .package_record
                    .constrains
                    .iter()
                    .map(|raw| (raw, false)),
            )
        {
            let spec = MatchSpec::from_str(raw, ParseStrictness::Lenient)?;
            let virtual_name = spec
                .name
                .as_exact()
                .is_some_and(|name| name.as_normalized().starts_with("__"))
                || graph
                    .virtual_packages
                    .iter()
                    .any(|package| spec.name.matches(&package.name))
                || host
                    .1
                    .iter()
                    .any(|package| spec.name.matches(&package.name));
            if !virtual_name {
                continue;
            }
            let actual = host
                .1
                .iter()
                .filter(|package| spec.name.matches(&package.name))
                .collect::<Vec<_>>();
            ensure!(
                if required {
                    actual.iter().any(|package| spec.matches(*package))
                } else {
                    actual.iter().all(|package| spec.matches(*package))
                },
                "current host does not satisfy Conda virtual requirement {raw}"
            );
        }
    }
    Ok(())
}

fn metadata_specs(roots: &[CondaRoot], records: &[RepoDataRecord]) -> Result<Vec<MatchSpec>> {
    let mut specs: Vec<_> = roots
        .iter()
        .map(|root| MatchSpec::from(root.name.clone()))
        .collect();
    for record in records {
        for dependency in &record.package_record.depends {
            let dependency = MatchSpec::from_str(dependency, ParseStrictness::Lenient)?;
            specs.push(MatchSpec {
                name: dependency.name,
                ..Default::default()
            });
        }
    }
    Ok(specs)
}

fn fixed_sources(
    channels: &[Channel],
    roots: &[CondaRoot],
    pinned: &[RepoDataRecord],
) -> Result<Vec<usize>> {
    let mut sources = Vec::with_capacity(pinned.len());
    let config = ChannelConfig::default_with_root_dir(Path::new("/").to_path_buf());
    for record in pinned {
        let source = Channel::from_str(
            record
                .channel
                .as_deref()
                .context("fixed Conda package source is missing")?,
            &config,
        )?;
        let index = channels
            .iter()
            .position(|channel| source.base_url == channel.base_url)
            .context("fixed Conda package source is not configured")?;
        if let Some(root) = roots
            .iter()
            .find(|root| root.name == record.package_record.name)
        {
            ensure!(
                root.channel.is_none_or(|selected| selected == index),
                "fixed Conda package {} conflicts with its explicitly selected channel",
                root.name.as_normalized()
            );
        }
        sources.push(index);
    }
    Ok(sources)
}

fn prepare_sources(
    roots: &[CondaRoot],
    pinned: &[RepoDataRecord],
    sources: &[usize],
    available: &mut [Vec<RepoDataRecord>],
) {
    for (record, &index) in pinned.iter().zip(sources) {
        if !available[index]
            .iter()
            .any(|existing| existing.url == record.url)
        {
            available[index].push(record.clone());
        }
    }
    for root in roots {
        if let Some(selected) = root.channel {
            for (index, records) in available.iter_mut().enumerate() {
                if index != selected {
                    records.retain(|record| record.package_record.name != root.name);
                }
            }
        }
    }
}

/// Old locked records are only preferences, and cannot override the current
/// configured source order, explicit root source, or an actual held pin.
fn preferred_sources(
    channels: &[Channel],
    roots: &[CondaRoot],
    pinned: &[RepoDataRecord],
    old: Vec<RepoDataRecord>,
    platform: Platform,
) -> Result<Vec<(usize, RepoDataRecord)>> {
    let config = ChannelConfig::default_with_root_dir(Path::new("/").to_path_buf());
    let mut preferences = Vec::new();
    for mut record in old {
        if record.package_record.subdir != platform.as_str()
            && record.package_record.subdir != "noarch"
        {
            continue;
        }
        if pinned
            .iter()
            .any(|pin| pin.package_record.name == record.package_record.name)
        {
            continue;
        }
        let source = Channel::from_str(
            record
                .channel
                .as_deref()
                .context("old Conda package source is missing")?,
            &config,
        )?;
        let Some(index) = channels
            .iter()
            .position(|channel| channel.base_url == source.base_url)
        else {
            continue;
        };
        if let Some(root) = roots
            .iter()
            .find(|root| root.name == record.package_record.name)
        {
            if root.channel.is_some_and(|selected| selected != index)
                || !root.match_spec()?.matches(&record.package_record)
            {
                continue;
            }
        }
        record.channel = Some(channels[index].base_url.url().to_string());
        preferences.push((index, record));
    }
    Ok(preferences)
}

fn prepare_preferences(
    available: &mut [Vec<RepoDataRecord>],
    preferences: Vec<(usize, RepoDataRecord)>,
) -> Vec<RepoDataRecord> {
    let mut preferred = Vec::new();
    for (index, record) in preferences {
        if available
            .iter()
            .position(|bucket| {
                bucket
                    .iter()
                    .any(|candidate| candidate.package_record.name == record.package_record.name)
            })
            .is_some_and(|first| first < index)
        {
            continue;
        }
        // Preserve the original record if the same artifact's advertised
        // metadata changed, instead of silently replacing its locked facts.
        available[index].retain(|candidate| candidate.identifier != record.identifier);
        available[index].push(record.clone());
        preferred.push(record);
    }
    preferred
}

/// Fetch only the explicitly listed sources and solve under the captured policy.
/// Pinned records bind an already selected Python runtime and its dependencies.
async fn resolve(
    context: &OperationContext,
    channels: Vec<Channel>,
    roots: Vec<CondaRoot>,
    platform: Platform,
    virtual_packages: Vec<GenericVirtualPackage>,
    pinned: Vec<RepoDataRecord>,
    old: Vec<RepoDataRecord>,
) -> Result<CondaResolution> {
    context.checkpoint()?;
    ensure!(
        !context.policy().locked,
        "a locked Conda operation must replay the original lock without solving"
    );
    ensure!(
        !context.policy().offline,
        "Conda repodata is unavailable without network access"
    );
    ensure!(
        !channels.is_empty() && !roots.is_empty(),
        "Conda solve needs channels and exact roots"
    );
    for root in &roots {
        ensure!(
            root.channel.is_none_or(|index| index < channels.len()),
            "Conda root channel is not configured"
        );
    }
    let pinned_sources = fixed_sources(&channels, &roots, &pinned)?;
    let preferences = preferred_sources(&channels, &roots, &pinned, old, platform)?;
    let specs = roots
        .iter()
        .map(CondaRoot::match_spec)
        .collect::<Result<Vec<_>>>()?;
    let http = crate::http::Client::new(
        context.settings().http_timeout(),
        crate::http::ClientKind::Http,
    )?;
    let gateway = Gateway::builder()
        .with_client(http.reqwest()?.clone())
        .with_cache_dir(context.requested_cache_root().join("conda"))
        .finish();
    // Fetch dependencies of already-fixed records as well: a channel may no
    // longer advertise the exact pinned record even though its bytes remain.
    let query_records = pinned
        .iter()
        .chain(preferences.iter().map(|(_, record)| record))
        .cloned()
        .collect::<Vec<_>>();
    let query_specs = metadata_specs(&roots, &query_records)?;
    let query = gateway
        .query(channels.clone(), [platform, Platform::NoArch], query_specs)
        .channel_relations(ChannelRelationsMode::Disabled)
        .channel_notices(false)
        .recursive(true);
    let cancellation = context.cancellation_token();
    let repodata = tokio::select! {
        _ = cancellation.cancelled() => { context.checkpoint()?; unreachable!() }
        result = tokio::time::timeout_at(context.policy().deadline.into(), query.into_future()) => {
            result.context("Conda repodata request exceeded its deadline")??
        }
    };
    context.checkpoint()?;
    // Gateway returns one bucket per channel/subdir, in caller order.
    ensure!(
        repodata.len() == channels.len() * 2,
        "Conda repodata channel count changed"
    );
    let mut available: Vec<Vec<RepoDataRecord>> = repodata
        .chunks(2)
        .map(|subdirs| {
            subdirs
                .iter()
                .flat_map(|source| source.iter().cloned())
                .collect()
        })
        .collect();
    prepare_sources(&roots, &pinned, &pinned_sources, &mut available);
    let preferred = prepare_preferences(&mut available, preferences);
    let token = rattler_solve::CancellationToken::new();
    let worker_token = token.clone();
    let worker_virtual = virtual_packages.clone();
    let deadline = context.policy().deadline;
    let mut worker = tokio::task::spawn_blocking(move || {
        solve_available(
            available,
            roots,
            specs,
            worker_virtual,
            pinned,
            preferred,
            deadline,
            worker_token,
        )
    });
    let result = tokio::select! {
        result = &mut worker => result?,
        _ = cancellation.cancelled() => {
            token.cancel();
            let _ = worker.await;
            context.checkpoint()?;
            unreachable!()
        }
        _ = tokio::time::sleep_until(deadline.into()) => {
            token.cancel();
            let _ = worker.await;
            context.checkpoint()?;
            unreachable!()
        }
    }?;
    context.checkpoint()?;
    Ok(CondaResolution {
        channels,
        platform,
        virtual_packages,
        records: result,
    })
}

fn solve_available(
    available: Vec<Vec<RepoDataRecord>>,
    roots: Vec<CondaRoot>,
    specs: Vec<MatchSpec>,
    virtual_packages: Vec<GenericVirtualPackage>,
    pinned: Vec<RepoDataRecord>,
    preferred: Vec<RepoDataRecord>,
    deadline: Instant,
    cancellation: rattler_solve::CancellationToken,
) -> Result<Vec<RepoDataRecord>> {
    let solve = |specs: Vec<MatchSpec>| -> Result<Vec<RepoDataRecord>, SolveError> {
        ResolvoSolver
            .solve(SolverTask {
                available_packages: available.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                locked_packages: preferred.iter().collect(),
                pinned_packages: pinned.iter().collect(),
                virtual_packages: virtual_packages.clone(),
                specs,
                constraints: vec![],
                timeout: Some(deadline.saturating_duration_since(Instant::now())),
                channel_priority: ChannelPriority::Strict,
                exclude_newer: None,
                strategy: SolveStrategy::Highest,
                dependency_overrides: vec![],
                excluded_candidates: Default::default(),
                cancellation_token: Some(cancellation.clone()),
            })
            .map(|result| result.records)
    };
    let solution =
        solve(specs.clone()).map_err(|error| eyre::eyre!("Conda solve failed: {error}"))?;
    for (index, root) in roots.iter().enumerate() {
        let selected = solution
            .iter()
            .find(|record| record.package_record.name == root.name)
            .context("Conda solve omitted a direct root")?;
        if root.build.is_some() || preferred.iter().any(|record| record == selected) {
            continue;
        }
        let source = available
            .iter()
            .find(|records| records.iter().any(|record| record.url == selected.url))
            .context("Conda selected root has no configured source")?;
        let builds: BTreeSet<_> = source
            .iter()
            .filter(|record| specs[index].matches(&record.package_record))
            .map(|record| record.package_record.build.clone())
            .collect();
        let mut compatible = vec![selected.package_record.build.clone()];
        for build in builds {
            if build == selected.package_record.build {
                continue;
            }
            let mut candidate = specs.clone();
            candidate[index].build = Some(StringMatcher::Exact(build.clone()));
            match solve(candidate) {
                Ok(_) => compatible.push(build),
                Err(SolveError::Unsolvable(_)) => {}
                Err(error) => {
                    return Err(eyre::eyre!(
                        "Conda solve failed while checking builds: {error}"
                    ));
                }
            }
        }
        compatible.sort();
        compatible.dedup();
        ensure!(
            compatible.len() == 1,
            "Conda package {} {} has multiple compatible builds: {}",
            root.name.as_normalized(),
            root.version,
            compatible.join(", ")
        );
    }
    Ok(solution)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rattler_conda_types::{
        PackageRecord,
        package::{ArchiveIdentifier, CondaArchiveType, DistArchiveIdentifier, DistArchiveType},
    };
    use std::time::Duration;

    #[cfg(all(
        feature = "native-tls",
        any(feature = "rustls", feature = "rustls-native-roots")
    ))]
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "explicit public HTTPS transport diagnostic"]
    async fn conda_metadata_tls_proxy_diagnostic() {
        let settings = crate::config::Settings::get();
        let limit = Duration::from_secs(15);
        let started = Instant::now();
        let deadline = tokio::time::Instant::from_std(started) + Duration::from_secs(60);
        let mut results = Vec::new();
        for (tls, use_proxy) in [
            ("native", true),
            ("native", false),
            ("rustls", true),
            ("rustls", false),
        ] {
            let start = Instant::now();
            let mut builder = reqwest::Client::builder()
                .user_agent(pakro_util::user_agent::get())
                .connect_timeout(settings.http_timeout().min(limit))
                .read_timeout(settings.http_timeout().min(limit))
                .timeout(settings.http_download_timeout().min(limit))
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt
                        .previous()
                        .last()
                        .is_some_and(|url| url.scheme() == "https")
                        && attempt.url().scheme() != "https"
                    {
                        attempt.error("refusing to redirect a download from HTTPS to HTTP")
                    } else {
                        reqwest::redirect::Policy::default().redirect(attempt)
                    }
                }));
            builder = if tls == "native" {
                builder.tls_backend_native()
            } else {
                builder.tls_backend_rustls()
            };
            if !use_proxy {
                builder = builder.no_proxy();
            }
            let mut result = serde_json::json!({
                "tls": tls,
                "proxy": if use_proxy { "default" } else { "disabled" },
            });
            let outcome: std::result::Result<reqwest::Response, Box<dyn std::error::Error>> =
                match builder.build() {
                    Ok(client) => {
                        let end = (tokio::time::Instant::from_std(start) + limit).min(deadline);
                        match tokio::time::timeout_at(
                            end,
                            client
                                .get("https://conda.anaconda.org/conda-forge/osx-arm64/repodata_shards.msgpack.zst")
                                .send(),
                        )
                        .await
                        {
                            Ok(response) => response.map_err(|error| {
                                Box::new(error) as Box<dyn std::error::Error>
                            }),
                            Err(error) => Err(Box::new(error)),
                        }
                    }
                    Err(error) => Err(Box::new(error)),
                };
            match outcome {
                Ok(response) => {
                    result["status"] = response.status().as_u16().into();
                    result["remote_addr"] =
                        response.remote_addr().map(|addr| addr.to_string()).into();
                    result["http_version"] = format!("{:?}", response.version()).into();
                }
                Err(error) => {
                    let mut chain = Vec::new();
                    let mut current: Option<&(dyn std::error::Error + 'static)> =
                        Some(error.as_ref());
                    while let Some(item) = current {
                        chain.push(serde_json::json!({
                            "display": item.to_string(),
                            "debug": format!("{item:?}"),
                        }));
                        current = item.source();
                    }
                    result["error_chain"] = chain.into();
                }
            }
            result["elapsed_ms"] = (start.elapsed().as_millis() as u64).into();
            results.push(result);
        }
        println!(
            "{}",
            serde_json::json!({
                "diagnostic": "conda_metadata_tls_proxy",
                "diagnostic_only": true,
                "total_elapsed_ms": started.elapsed().as_millis() as u64,
                "cases": results,
            })
        );
    }

    fn root(name: &str, version: &str, build: Option<&str>) -> CondaRoot {
        CondaRoot {
            name: PackageName::from_str(name).unwrap(),
            version: Version::from_str(version).unwrap(),
            build: build.map(str::to_owned),
            channel: None,
        }
    }

    fn record(
        name: &str,
        version: &str,
        build: &str,
        channel: &str,
        depends: &[&str],
    ) -> RepoDataRecord {
        let mut package_record = PackageRecord::new(
            PackageName::from_str(name).unwrap(),
            Version::from_str(version).unwrap(),
            build.to_owned(),
        );
        package_record.subdir = "noarch".into();
        package_record.depends = depends
            .iter()
            .map(|dependency| (*dependency).to_owned())
            .collect();
        let identifier = DistArchiveIdentifier::new(
            ArchiveIdentifier {
                name: name.into(),
                version: version.into(),
                build_string: build.into(),
            },
            DistArchiveType::Conda(CondaArchiveType::Conda),
        );
        RepoDataRecord {
            package_record,
            identifier,
            url: format!("https://example.invalid/{channel}/noarch/{name}-{version}-{build}.conda")
                .parse()
                .unwrap(),
            channel: Some(format!("https://example.invalid/{channel}/")),
        }
    }

    fn run(
        available: Vec<Vec<RepoDataRecord>>,
        roots: Vec<CondaRoot>,
        pinned: Vec<RepoDataRecord>,
    ) -> Result<Vec<RepoDataRecord>> {
        run_preferred(available, roots, pinned, vec![])
    }

    fn run_preferred(
        available: Vec<Vec<RepoDataRecord>>,
        roots: Vec<CondaRoot>,
        pinned: Vec<RepoDataRecord>,
        preferred: Vec<RepoDataRecord>,
    ) -> Result<Vec<RepoDataRecord>> {
        let specs = roots
            .iter()
            .map(|root| root.match_spec().unwrap())
            .collect();
        solve_available(
            available,
            roots,
            specs,
            vec![],
            pinned,
            preferred,
            Instant::now() + Duration::from_secs(10),
            rattler_solve::CancellationToken::new(),
        )
    }

    #[test]
    fn strictly_prioritizes_the_first_channel() {
        let high = record("demo", "1.0", "a_0", "high", &[]);
        let low = record("demo", "1.0", "b_0", "low", &[]);
        let result = run(
            vec![vec![high.clone()], vec![low]],
            vec![root("demo", "1.0", None)],
            vec![],
        )
        .unwrap();
        assert_eq!(result[0].url, high.url);
    }

    #[test]
    fn metadata_query_keeps_higher_channel_versions_before_strict_solve() {
        let channels = configured_channels(
            &[
                "https://example.invalid/high".into(),
                "https://example.invalid/low".into(),
            ],
            Path::new("/"),
        )
        .unwrap();
        let high = record("demo", "2.0", "a_0", "high", &[]);
        let low = record("demo", "1.0", "b_0", "low", &[]);
        let mut direct = root("demo", "1.0", Some("b_0"));
        let specs = metadata_specs(&[direct.clone()], &[]).unwrap();
        let pinned = record("python", "3.13.13", "build_0", "high", &["base ==1.0"]);
        let pinned_specs = metadata_specs(&[direct.clone()], &[pinned]).unwrap();
        assert!(pinned_specs.iter().any(|spec| spec.matches(&record(
            "base",
            "2.0",
            "a_0",
            "high",
            &[]
        ))));
        // Gateway retains input records matching its query specs before solving.
        let fetched = vec![vec![high], vec![low.clone()]]
            .into_iter()
            .map(|bucket| {
                bucket
                    .into_iter()
                    .filter(|record| specs.iter().any(|spec| spec.matches(record)))
                    .collect()
            })
            .collect::<Vec<Vec<_>>>();
        assert_eq!(fetched[0].len(), 1);
        let mut strict = fetched.clone();
        let sources = fixed_sources(&channels, &[direct.clone()], &[]).unwrap();
        prepare_sources(&[direct.clone()], &[], &sources, &mut strict);
        assert!(run(strict, vec![direct.clone()], vec![]).is_err());
        direct.channel = Some(1);
        let mut explicit = fetched;
        let sources = fixed_sources(&channels, &[direct.clone()], &[]).unwrap();
        prepare_sources(&[direct.clone()], &[], &sources, &mut explicit);
        assert_eq!(run(explicit, vec![direct], vec![]).unwrap()[0].url, low.url);
    }

    #[test]
    fn explicitly_selected_channel_must_match_the_pinned_record() {
        let channels = configured_channels(
            &[
                "https://example.invalid/high".into(),
                "https://example.invalid/low".into(),
            ],
            Path::new("/"),
        )
        .unwrap();
        let pinned = record("python", "3.13.13", "build_0", "high", &[]);
        let other = record("python", "3.13.13", "build_0", "low", &[]);
        let mut direct = root("python", "3.13.13", Some("build_0"));
        direct.channel = Some(1);
        let mut available = vec![vec![pinned.clone()], vec![other]];
        assert!(fixed_sources(&channels, &[direct.clone()], &[pinned.clone()]).is_err());
        direct.channel = Some(0);
        let sources = fixed_sources(&channels, &[direct.clone()], &[pinned.clone()]).unwrap();
        prepare_sources(
            &[direct.clone()],
            &[pinned.clone()],
            &sources,
            &mut available,
        );
        assert_eq!(
            run(available, vec![direct], vec![pinned.clone()]).unwrap()[0].url,
            pinned.url
        );
    }

    #[test]
    fn fixed_package_uses_recorded_channel_when_artifact_is_on_cdn() {
        let channels = configured_channels(
            &[
                "https://example.invalid/high".into(),
                "https://example.invalid/low".into(),
            ],
            Path::new("/"),
        )
        .unwrap();
        let mut pinned = record("python", "3.13.13", "build_0", "high", &[]);
        pinned.url = "https://cdn.example.invalid/osx-arm64/python-3.13.13-build_0.conda"
            .parse()
            .unwrap();
        let mut direct = root("python", "3.13.13", Some("build_0"));
        direct.channel = Some(0);
        let sources = fixed_sources(&channels, &[direct.clone()], &[pinned.clone()]).unwrap();
        let mut available = vec![vec![], vec![]];
        prepare_sources(
            &[direct.clone()],
            &[pinned.clone()],
            &sources,
            &mut available,
        );
        assert_eq!(
            run(available, vec![direct], vec![pinned.clone()]).unwrap()[0].url,
            pinned.url
        );
    }

    #[test]
    fn reports_only_builds_compatible_with_other_roots() {
        let a = record("demo", "1.0", "a_0", "main", &["base ==1.0"]);
        let b = record("demo", "1.0", "b_0", "main", &["base ==2.0"]);
        let base = record("base", "1.0", "base_0", "main", &[]);
        let second_base = record("base", "2.0", "base_0", "main", &[]);
        let roots = vec![root("demo", "1.0", None), root("base", "1.0", None)];
        let result = run(vec![vec![a.clone(), b, base, second_base]], roots, vec![]).unwrap();
        assert!(result.iter().any(|record| record.url == a.url));
    }

    #[test]
    fn rejects_ambiguous_compatible_builds_but_accepts_an_explicit_build() {
        let a = record("demo", "1.0", "a_0", "main", &[]);
        let b = record("demo", "1.0", "b_0", "main", &[]);
        let error = run(
            vec![vec![a.clone(), b.clone()]],
            vec![root("demo", "1.0", None)],
            vec![],
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("a_0, b_0"));
        let result = run(
            vec![vec![a.clone(), b]],
            vec![root("demo", "1.0", Some("a_0"))],
            vec![],
        )
        .unwrap();
        assert_eq!(result[0].url, a.url);
    }

    #[test]
    fn pinned_runtime_cannot_be_replaced_by_a_new_request() {
        let old = record("base", "1.0", "base_0", "main", &[]);
        let new = record("base", "2.0", "base_0", "main", &[]);
        let result = run(
            vec![vec![old.clone(), new]],
            vec![root("base", "2.0", None)],
            vec![old],
        );
        assert!(result.is_err());
    }

    #[test]
    fn update_prefers_compatible_locked_dependencies_and_preserves_their_artifact() {
        let channels =
            configured_channels(&["https://example.invalid/main".into()], Path::new("/")).unwrap();
        let app = record("app", "1.0", "app_0", "main", &["dep >=1"]);
        let old = record("dep", "1.0", "dep_0", "main", &[]);
        let new = record("dep", "2.0", "dep_0", "main", &[]);
        let roots = vec![root("app", "1.0", Some("app_0"))];
        let preferences = preferred_sources(
            &channels,
            &roots,
            &[],
            vec![old.clone()],
            Platform::OsxArm64,
        )
        .unwrap();
        let mut available = vec![vec![app, old.clone(), new]];
        let preferred = prepare_preferences(&mut available, preferences);
        let solution = run_preferred(available, roots, vec![], preferred).unwrap();
        assert!(solution.iter().any(|record| record.url == old.url));
    }

    #[test]
    fn update_can_change_an_old_dependency_when_new_roots_require_it() {
        let channels =
            configured_channels(&["https://example.invalid/main".into()], Path::new("/")).unwrap();
        let app = record("app", "1.0", "app_0", "main", &["dep >=2"]);
        let old = record("dep", "1.0", "dep_0", "main", &[]);
        let new = record("dep", "2.0", "dep_0", "main", &[]);
        let roots = vec![root("app", "1.0", Some("app_0"))];
        let preferences = preferred_sources(
            &channels,
            &roots,
            &[],
            vec![old.clone()],
            Platform::OsxArm64,
        )
        .unwrap();
        let mut available = vec![vec![app, old, new.clone()]];
        let preferred = prepare_preferences(&mut available, preferences);
        let solution = run_preferred(available, roots, vec![], preferred).unwrap();
        assert!(solution.iter().any(|record| record.url == new.url));
    }

    #[test]
    fn update_preference_cannot_override_strict_or_explicit_channel_or_a_held_pin() {
        let channels = configured_channels(
            &[
                "https://example.invalid/high".into(),
                "https://example.invalid/low".into(),
            ],
            Path::new("/"),
        )
        .unwrap();
        let high = record("demo", "1.0", "high_0", "high", &[]);
        let old = record("demo", "1.0", "low_0", "low", &[]);
        let mut direct = root("demo", "1.0", None);
        let preferences = preferred_sources(
            &channels,
            &[direct.clone()],
            &[],
            vec![old.clone()],
            Platform::OsxArm64,
        )
        .unwrap();
        let mut available = vec![vec![high.clone()], vec![old.clone()]];
        let preferred = prepare_preferences(&mut available, preferences);
        assert!(preferred.is_empty());
        assert_eq!(
            run_preferred(available, vec![direct.clone()], vec![], preferred).unwrap()[0].url,
            high.url
        );

        direct.channel = Some(0);
        assert!(
            preferred_sources(
                &channels,
                &[direct.clone()],
                &[],
                vec![old.clone()],
                Platform::OsxArm64
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            preferred_sources(
                &channels,
                &[direct.clone()],
                &[high.clone()],
                vec![old.clone()],
                Platform::OsxArm64
            )
            .unwrap()
            .is_empty()
        );

        // Changing the explicit source rejects the old candidate rather than
        // reintroducing it through Rattler's independent preference list.
        direct.channel = Some(1);
        let preferences = preferred_sources(
            &channels,
            &[direct.clone()],
            &[],
            vec![high.clone()],
            Platform::OsxArm64,
        )
        .unwrap();
        let mut available = vec![vec![high], vec![old.clone()]];
        prepare_sources(&[direct.clone()], &[], &[], &mut available);
        let preferred = prepare_preferences(&mut available, preferences);
        assert_eq!(
            run_preferred(available, vec![direct], vec![], preferred).unwrap()[0].url,
            old.url
        );
    }

    #[test]
    fn update_keeps_an_existing_build_but_fresh_resolution_stays_ambiguous() {
        let channels =
            configured_channels(&["https://example.invalid/main".into()], Path::new("/")).unwrap();
        let old = record("demo", "1.0", "a_0", "main", &[]);
        let other = record("demo", "1.0", "b_0", "main", &[]);
        let roots = vec![root("demo", "1.0", None)];
        assert!(
            run(
                vec![vec![old.clone(), other.clone()]],
                roots.clone(),
                vec![]
            )
            .is_err()
        );
        let preferences = preferred_sources(
            &channels,
            &roots,
            &[],
            vec![old.clone()],
            Platform::OsxArm64,
        )
        .unwrap();
        let mut available = vec![vec![old.clone(), other]];
        let preferred = prepare_preferences(&mut available, preferences);
        let solution = run_preferred(available, roots, vec![], preferred).unwrap();
        assert_eq!(solution[0].url, old.url);
    }

    #[test]
    fn update_does_not_silently_replace_an_existing_artifact_digest() {
        let channels =
            configured_channels(&["https://example.invalid/main".into()], Path::new("/")).unwrap();
        let mut old = record("demo", "1.0", "a_0", "main", &[]);
        old.package_record.sha256 = Some([1u8; 32].into());
        let mut changed = old.clone();
        changed.package_record.sha256 = Some([2u8; 32].into());
        let roots = vec![root("demo", "1.0", Some("a_0"))];
        let preferences = preferred_sources(
            &channels,
            &roots,
            &[],
            vec![old.clone()],
            Platform::OsxArm64,
        )
        .unwrap();
        let mut available = vec![vec![changed]];
        let preferred = prepare_preferences(&mut available, preferences);
        let solution = run_preferred(available, roots, vec![], preferred).unwrap();
        assert_eq!(solution[0].package_record.sha256, old.package_record.sha256);
    }

    #[test]
    fn locked_virtual_requirements_accept_a_compatible_newer_host() {
        let virtual_package = |name: &str, version: &str| GenericVirtualPackage {
            name: PackageName::from_str(name).unwrap(),
            version: Version::from_str(version).unwrap(),
            build_string: String::new(),
        };
        let mut python = record("python", "3.13.13", "cp313", "main", &["__osx >=11"]);
        python.package_record.constrains = vec!["__cuda >=12".into()];
        let graph = CondaLockGraph {
            config_scope: String::new(),
            group: "default".into(),
            target: "osx-arm64".into(),
            target_fingerprint: [1; 32],
            channels: vec!["conda-forge".into()],
            roots: vec!["python ==3.13.13".into()],
            virtual_packages: vec![virtual_package("__osx", "13")],
            records: vec![python],
        };
        let original = graph.clone();
        assert!(
            validate_host(
                &graph,
                &(Platform::OsxArm64, vec![virtual_package("__osx", "10")])
            )
            .is_err()
        );
        assert!(
            validate_host(
                &graph,
                &(Platform::OsxArm64, vec![virtual_package("__osx", "14")])
            )
            .is_ok()
        );
        assert!(
            validate_host(
                &graph,
                &(
                    Platform::OsxArm64,
                    vec![
                        virtual_package("__osx", "14"),
                        virtual_package("__cuda", "11")
                    ]
                )
            )
            .is_err()
        );
        assert!(
            validate_host(
                &graph,
                &(
                    Platform::OsxArm64,
                    vec![
                        virtual_package("__osx", "14"),
                        virtual_package("__cuda", "12")
                    ]
                )
            )
            .is_ok()
        );
        assert!(
            validate_host(
                &graph,
                &(Platform::Osx64, vec![virtual_package("__osx", "14")])
            )
            .is_err()
        );
        assert_eq!(graph, original, "host checks must not change the lock");

        let mut linux = graph.clone();
        linux.target = "linux-64".into();
        linux.records[0].package_record.depends = vec!["__glibc >=2.28".into()];
        linux.records[0].package_record.constrains.clear();
        linux.virtual_packages = vec![virtual_package("__glibc", "2.35")];
        assert!(
            validate_host(
                &linux,
                &(Platform::Linux64, vec![virtual_package("__glibc", "2.17")])
            )
            .is_err()
        );
        assert!(
            validate_host(
                &linux,
                &(Platform::Linux64, vec![virtual_package("__glibc", "2.39")])
            )
            .is_ok()
        );
    }
}
