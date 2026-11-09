//! Closed runtime install inputs. Discovery and locked replay create the same
//! plan; installation verifies the observed interpreter against those inputs.

use eyre::{ContextCompat, Result, bail, ensure};
use pakro_index::artifact::SourceArtifact;
use pakro_store::Digest;

use super::{
    RuntimeArchiveTarget, RuntimeDistribution, RuntimeFlavor, RuntimePlatform, RuntimeRelease,
};
use crate::lockfile::PlatformInfo;
use crate::python_runtime::{
    PythonImplementation, PythonLibcObservation, PythonPlatformObservation, PythonRuntimeDescriptor,
};

fn locked_filename(locator: &str) -> Result<String> {
    let url = url::Url::parse(locator)
        .map_err(|_| eyre::eyre!("invalid locked Python runtime locator"))?;
    let encoded = url
        .path_segments()
        .and_then(|mut parts| parts.next_back())
        .context("locked Python runtime filename is missing")?;
    Ok(urlencoding::decode(encoded)
        .map_err(|_| eyre::eyre!("invalid locked Python runtime filename encoding"))?
        .into_owned())
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeInstallPlan {
    source: SourceArtifact,
    distribution: RuntimeDistribution,
    target: RuntimeArchiveTarget,
    flavor: RuntimeFlavor,
    expected_python_version: Option<String>,
    language_major: u32,
    language_minor: u32,
    implementation: PythonImplementation,
    implementation_version: String,
    source_build: Option<crate::libraries::language::source::SourcePythonBuildInput>,
    conda: Option<rattler_conda_types::RepoDataRecord>,
    conda_channel: Option<[u8; 32]>,
}

impl RuntimeInstallPlan {
    /// Describe exact installation inputs; this grants no filesystem authority.
    pub(crate) fn language_key(
        &self,
        patch_sysconfig: bool,
    ) -> Result<crate::libraries::language::identity::LanguageKey> {
        use crate::libraries::language::identity::{
            IdentityLimits, LanguageIdentityInput, LanguageInput, LanguageKey,
        };
        use crate::libraries::package::identity::ArtifactId;
        let (language, implementation, distribution, target) = self.shared_identity_inputs()?;
        if let Some(input) = &self.source_build { return input.language_key(); }
        let limits = IdentityLimits::default();
        // Recipe 1 performs contained strip-one extraction, removes group/other
        // write bits from archive modes, and binds the contained entrypoint;
        // recipe 2 additionally applies the existing PBS sysconfig rewrite.
        let recipe = if patch_sysconfig && self.distribution == RuntimeDistribution::Pbs {
            2
        } else {
            1
        };
        Ok(LanguageKey::new(
            LanguageIdentityInput {
                implementation,
                distribution: &distribution,
                exact_version: language,
                target: &target,
                input: match self.conda_channel {
                    Some(channel) => LanguageInput::CondaArtifact {
                        artifact: ArtifactId::new(self.source.digest()),
                        channel,
                    },
                    None => LanguageInput::Artifact(ArtifactId::new(self.source.digest())),
                },
                recipe,
            },
            limits,
        )?)
    }

    /// Same fixed target inputs as `language_key`, without rebuilding a
    /// second compatibility identity for shared package consumers.
    pub(crate) fn shared_target_identity(
        &self,
    ) -> Result<crate::libraries::language::identity::TargetIdentity> {
        Ok(self.shared_identity_inputs()?.3)
    }
    fn shared_identity_inputs(
        &self,
    ) -> Result<(
        &str,
        &'static str,
        String,
        crate::libraries::language::identity::TargetIdentity,
    )> {
        use crate::libraries::language::identity::{
            IdentityHash, IdentityLimits, TargetIdentity, TargetIdentityInput,
        };

        let language = self
            .expected_python_version
            .as_deref()
            .context("shared Python identity requires a complete language patch version")?;
        if let Some(input) = &self.source_build {
            return Ok((language, "cpython", "cpython-source".into(), input.target_identity));
        }
        let implementation = match self.implementation {
            PythonImplementation::CPython => "cpython",
            PythonImplementation::PyPy => "pypy",
        };
        let distribution = match self.distribution {
            RuntimeDistribution::Pbs => {
                let (_, build, _, _) = super::pbs_filename(self.source.filename())?
                    .context("shared PBS identity requires a validated distribution build")?;
                format!("pbs-{build}")
            }
            RuntimeDistribution::PyPy => format!("pypy-{}", self.implementation_version),
            RuntimeDistribution::CpythonSource => bail!("source CPython plan is missing its build inputs"),
            RuntimeDistribution::Conda => format!("conda-{}", self.conda.as_ref().context("Conda Python package record is missing")?.package_record.build),
        };
        let (os, libc) = match self.target.platform() {
            RuntimePlatform::Macos => ("macos", "none"),
            RuntimePlatform::LinuxGnu => ("linux", "glibc"),
            RuntimePlatform::LinuxMusl => ("linux", "musl"),
        };
        let flavor = match self.flavor {
            RuntimeFlavor::InstallOnly => "install-only",
            RuntimeFlavor::InstallOnlyStripped => "install-only-stripped",
            RuntimeFlavor::FreeThreadedInstallOnly => "freethreaded-install-only",
            RuntimeFlavor::FreeThreadedInstallOnlyStripped => "freethreaded-install-only-stripped",
            RuntimeFlavor::PyPy => "pypy",
        };
        let limits = IdentityLimits::default();
        // These are the fixed archive compatibility inputs, not host probe facts.
        // The artifact digest commits to the distribution's exact build baseline.
        let mut compatibility = IdentityHash::new(b"pakro.python.archive-target", limits)?;
        for (field, value) in [
            ("implementation", implementation),
            (
                "implementation_version",
                self.implementation_version.as_str(),
            ),
            ("language_version", language),
            ("distribution", distribution.as_str()),
            ("flavor", flavor),
        ] {
            compatibility.atom(field, value)?;
        }
        let abi = format!("{implementation}-{}-{flavor}", self.implementation_version);
        let target = TargetIdentity::new(
            TargetIdentityInput {
                os,
                arch: self.target.arch(),
                libc,
                abi: &abi,
                compatibility: compatibility.finish(),
            },
            limits,
        )?;
        Ok((language, implementation, distribution, target))
    }

    pub(in crate::plugins::core::python) fn from_release(release: &RuntimeRelease) -> Self {
        let key = release.key();
        // RuntimeRelease fields are private and are produced by the validated
        // native parsers. This projection fixes the declared filename/digest
        // into a fresh H0 source without another lookup or source selection.
        let source = SourceArtifact::new(
            &key.filename,
            release.source().url().as_str(),
            release.digest(),
        )
        .expect("validated RuntimeRelease source");
        let (major, minor, _) = super::version_parts(&key.python_version)
            .expect("validated RuntimeRelease language version");
        Self {
            source,
            distribution: key.distribution,
            target: key.target.clone(),
            flavor: key.flavor,
            expected_python_version: Some(key.python_version.clone()),
            language_major: major,
            language_minor: minor,
            implementation: match key.distribution {
                RuntimeDistribution::Pbs => PythonImplementation::CPython,
                RuntimeDistribution::PyPy => PythonImplementation::PyPy,
                RuntimeDistribution::CpythonSource => PythonImplementation::CPython,
                RuntimeDistribution::Conda => PythonImplementation::CPython,
            },
            implementation_version: key.implementation_version.clone(),
            source_build: None,
            conda: None,
            conda_channel: None,
        }
    }

    /// Validate one PBS lock entry against its own complete version/build.
    /// This is independent of host/settings; the operation checks those again
    /// when consuming the pin. It grants no source or filesystem authority.
    pub(crate) fn validate_locked_pbs_entry(request: &str, info: &PlatformInfo) -> Result<()> {
        let locator = info
            .url
            .as_deref()
            .context("locked Python runtime has no exact artifact URL")?;
        let filename = locked_filename(locator)?;
        let (_, _, target, flavor) = super::pbs_filename(&filename)?
            .context("locked Python artifact is not a supported PBS install archive")?;
        Self::from_locked(request, &target, flavor, info)?;
        Ok(())
    }

    pub(crate) fn from_locked(
        request: &str,
        target: &RuntimeArchiveTarget,
        flavor: RuntimeFlavor,
        info: &PlatformInfo,
    ) -> Result<Self> {
        let checksum = info
            .checksum
            .as_deref()
            .context("locked Python runtime has no expected checksum")?;
        ensure!(
            !checksum.starts_with("blake3:"),
            "legacy blake3 Python runtime lock requires explicit migration to SHA256/SHA512"
        );
        let (algorithm, hex) = checksum
            .split_once(':')
            .context("invalid locked Python runtime checksum")?;
        ensure!(
            matches!(algorithm, "sha256" | "sha512"),
            "locked Python runtime requires SHA256 or SHA512"
        );
        let digest = format!("{algorithm}-{hex}")
            .parse::<Digest>()
            .map_err(|_| eyre::eyre!("invalid locked Python runtime checksum"))?;
        let locator = info
            .url
            .as_deref()
            .context("locked Python runtime has no exact artifact URL")?;
        let filename = locked_filename(locator)?;
        let (
            distribution,
            expected_python_version,
            major,
            minor,
            implementation,
            implementation_version,
        ) = if let Some(request) = request.strip_prefix("pypy") {
            ensure!(
                flavor == RuntimeFlavor::PyPy,
                "locked PyPy artifact has a conflicting flavor"
            );
            let (language, implementation) = request
                .split_once('-')
                .context("PyPy runtime request must include an exact implementation version")?;
            let parts: Vec<_> = language.split('.').collect();
            ensure!(parts.len() == 2, "invalid exact PyPy language request");
            let (major, minor, _) = super::version_parts(&format!("{language}.0"))?;
            ensure!(major == 3, "only Python 3 PyPy runtimes are in scope");
            super::version_parts(implementation)?;
            let (platform, arch) = match (target.platform(), target.arch()) {
                (RuntimePlatform::Macos, "aarch64") => ("darwin", "arm64"),
                (RuntimePlatform::Macos, "x86_64") => ("darwin", "x64"),
                (RuntimePlatform::LinuxGnu, "x86_64") => ("linux", "x64"),
                (RuntimePlatform::LinuxGnu, "aarch64") => ("linux", "aarch64"),
                (RuntimePlatform::LinuxGnu, "i686") => ("linux", "i686"),
                (RuntimePlatform::LinuxGnu, "s390x") => ("linux", "s390x"),
                _ => bail!("official PyPy archive target is unavailable"),
            };
            let file = super::PypyFile {
                filename: filename.clone(),
                arch: arch.into(),
                platform: platform.into(),
                download_url: locator.into(),
            };
            let (parsed, suffixes) = super::pypy_file_target(&file)?
                .context("official PyPy archive target is unavailable")?;
            ensure!(
                &parsed == target,
                "locked PyPy archive target differs from requested target"
            );
            let prefix = format!("pypy{language}-v{implementation}-");
            ensure!(
                filename
                    .strip_prefix(&prefix)
                    .is_some_and(|suffix| suffixes.contains(&suffix)),
                "locked PyPy filename differs from requested language, implementation or target"
            );
            super::validate_locator(&filename, locator, RuntimeDistribution::PyPy, "")?;
            (
                RuntimeDistribution::PyPy,
                None,
                major,
                minor,
                PythonImplementation::PyPy,
                implementation.to_string(),
            )
        } else {
            let (version, build, parsed, parsed_flavor) = super::pbs_filename(&filename)?
                .context("locked Python artifact is not a supported PBS install archive")?;
            // A `+YYYYMMDD` request pin is the distribution build of the same
            // complete version; validate against the bare version plus the
            // pinned build instead of the raw request string.
            let (bare_request, build_pin) = match request.split_once('+') {
                Some((bare, pin))
                    if pin.len() == 8 && pin.bytes().all(|byte| byte.is_ascii_digit()) =>
                {
                    (bare, Some(pin))
                }
                _ => (request, None),
            };
            ensure!(
                version == bare_request,
                "locked PBS language version differs from requested version"
            );
            if let Some(pin) = build_pin {
                ensure!(
                    build == pin,
                    "locked PBS archive build {build} differs from the requested pin {pin}"
                );
            }
            ensure!(
                &parsed == target,
                "locked PBS archive target differs from requested target"
            );
            ensure!(
                parsed_flavor == flavor,
                "locked PBS archive flavor differs from requested flavor"
            );
            super::validate_locator(&filename, locator, RuntimeDistribution::Pbs, &build)?;
            let (major, minor, _) = super::version_parts(&version)?;
            (
                RuntimeDistribution::Pbs,
                Some(version.clone()),
                major,
                minor,
                PythonImplementation::CPython,
                version,
            )
        };
        let source = SourceArtifact::new(filename, locator, digest)?;
        Ok(Self {
            source,
            distribution,
            target: target.clone(),
            flavor,
            expected_python_version,
            language_major: major,
            language_minor: minor,
            implementation,
            implementation_version,
            source_build: None,
            conda: None,
            conda_channel: None,
        })
    }

    pub(crate) fn from_source(input: &crate::libraries::language::source::SourcePythonBuildInput) -> Result<Self> {
        let (major, minor, _) = super::version_parts(&input.version)?;
        Ok(Self { source: input.source.clone(), distribution: RuntimeDistribution::CpythonSource,
            target: input.target.clone(), flavor: RuntimeFlavor::InstallOnly,
            expected_python_version: Some(input.version.clone()), language_major: major, language_minor: minor,
            implementation: PythonImplementation::CPython, implementation_version: input.version.clone(),
            source_build: Some(input.clone()), conda: None, conda_channel: None })
    }

    /// A Conda Python package is an independent language input; its package
    /// digest fixes the actual Python payload across projects.
    pub(crate) fn from_conda_record(record: &rattler_conda_types::RepoDataRecord) -> Result<Self> {
        use sha2::{Digest as _, Sha256};

        ensure!(
            record.package_record.name.as_normalized() == "python",
            "Conda language record is not Python"
        );
        let config = rattler_conda_types::ChannelConfig::default_with_root_dir(
            std::path::PathBuf::from("/"),
        );
        let channel = rattler_conda_types::Channel::from_str(
            record.channel.as_deref().context("Conda Python channel is missing")?,
            &config,
        )?;
        ensure!(
            channel.base_url.url().scheme() == "https" && channel.platforms.is_none(),
            "unsupported Conda Python channel"
        );
        let conda_channel = Sha256::digest(channel.base_url.url().as_str().as_bytes()).into();
        let mut record = record.clone();
        record.channel = Some(channel.base_url.url().to_string());
        let package = &record.package_record;
        let version = package.version.to_string();
        let (major, minor, _) = super::version_parts(&version)?;
        let (arch, platform) = match package.subdir.as_str() {
            "osx-arm64" => ("aarch64", RuntimePlatform::Macos),
            "osx-64" => ("x86_64", RuntimePlatform::Macos),
            "linux-64" => ("x86_64", RuntimePlatform::LinuxGnu),
            "linux-aarch64" => ("aarch64", RuntimePlatform::LinuxGnu),
            _ => bail!("unsupported Conda Python platform"),
        };
        let hash = package
            .sha256
            .as_ref()
            .context("Conda Python SHA256 is missing")?;
        let digest = format!("sha256-{}", hex::encode(hash)).parse::<Digest>()?;
        let source =
            SourceArtifact::new(record.identifier.to_string(), record.url.as_str(), digest)?;
        Ok(Self {
            source,
            distribution: RuntimeDistribution::Conda,
            target: RuntimeArchiveTarget::new(arch, platform)?,
            flavor: RuntimeFlavor::InstallOnly,
            expected_python_version: Some(version.clone()),
            language_major: major,
            language_minor: minor,
            implementation: PythonImplementation::CPython,
            implementation_version: version,
            source_build: None,
            conda: Some(record),
            conda_channel: Some(conda_channel),
        })
    }

    pub(crate) fn conda_record(&self) -> Option<&rattler_conda_types::RepoDataRecord> { self.conda.as_ref() }

    pub(crate) fn source_build(&self) -> Option<&crate::libraries::language::source::SourcePythonBuildInput> { self.source_build.as_ref() }

    pub(crate) fn source(&self) -> &SourceArtifact {
        &self.source
    }
    /// Check the same logical official source, accepting equivalent path
    /// encodings without admitting a different publisher, build or filename.
    /// Transport mirror rewriting does not change this canonical identity.
    pub(crate) fn validate_source_locator(&self, locator: &str) -> Result<()> {
        let build = match self.distribution {
            RuntimeDistribution::Pbs => {
                super::pbs_filename(self.source.filename())?
                    .context("shared PBS source requires a validated distribution build")?
                    .1
            }
            RuntimeDistribution::PyPy => String::new(),
            RuntimeDistribution::CpythonSource => self.expected_python_version.clone().context("source CPython version is missing")?,
            RuntimeDistribution::Conda => {
                ensure!(url::Url::parse(locator)? == *self.source.url(), "Conda Python source differs from its fixed package record");
                return Ok(());
            }
        };
        super::validate_locator(self.source.filename(), locator, self.distribution, &build)
    }
    pub(crate) fn distribution(&self) -> RuntimeDistribution {
        self.distribution
    }
    pub(crate) fn target(&self) -> &RuntimeArchiveTarget {
        &self.target
    }
    pub(crate) fn flavor(&self) -> RuntimeFlavor {
        self.flavor
    }

    pub(crate) fn validate_observed(&self, descriptor: &PythonRuntimeDescriptor) -> Result<()> {
        descriptor.validate_compatibility()?;
        ensure!(
            descriptor.implementation == self.implementation,
            "observed runtime implementation differs from selected distribution"
        );
        let language = &descriptor.language_version;
        ensure!(
            (language.major, language.minor) == (self.language_major, self.language_minor),
            "observed runtime language differs from selected language series"
        );
        if let Some(expected) = &self.expected_python_version {
            ensure!(
                language.full_version() == *expected,
                "observed runtime full language version differs from selected release"
            );
        }
        ensure!(
            descriptor.implementation_version.full_version() == self.implementation_version,
            "observed runtime implementation version differs from selected release"
        );
        let observed = descriptor
            .compatibility
            .as_ref()
            .context("runtime execution compatibility observation is missing")?;
        match &observed.platform {
            PythonPlatformObservation::Macos { cpu_type, .. } => {
                ensure!(
                    self.target.platform() == RuntimePlatform::Macos,
                    "observed runtime OS differs from selected archive target"
                );
                let arch =
                    match cpu_type.context("runtime loaded Mach-O CPU observation is missing")? {
                        16777223 => "x86_64",
                        16777228 => "aarch64",
                        7 => "i386",
                        18 => "ppc",
                        16777234 => "ppc64",
                        _ => bail!("observed runtime Mach-O CPU is unsupported"),
                    };
                ensure!(
                    arch == self.target.arch(),
                    "observed runtime CPU differs from selected archive target"
                );
            }
            PythonPlatformObservation::Linux { elf, libc, .. } => {
                ensure!(
                    self.target.platform() != RuntimePlatform::Macos,
                    "observed runtime OS differs from selected archive target"
                );
                let elf = elf
                    .as_ref()
                    .context("runtime executed ELF observation is missing")?;
                let arch = match (elf.class, elf.little_endian, elf.machine) {
                    (2, true, 62) => "x86_64",
                    (2, true, 183) => "aarch64",
                    (1, true, 3) => "i686",
                    (2, true, 21) => "ppc64le",
                    (2, false, 21) => "ppc64",
                    (2, false, 22) => "s390x",
                    (2, true, 243) => "riscv64",
                    (2, true, 258) => "loongarch64",
                    _ => bail!("observed runtime ELF execution architecture is unsupported"),
                };
                ensure!(
                    arch == self.target.arch(),
                    "observed runtime CPU differs from selected archive target"
                );
                ensure!(
                    matches!(
                        (self.target.platform(), libc),
                        (
                            RuntimePlatform::LinuxGnu,
                            PythonLibcObservation::Glibc { .. }
                        ) | (
                            RuntimePlatform::LinuxMusl,
                            PythonLibcObservation::Musl { .. }
                        )
                    ),
                    "observed runtime libc differs from selected archive target or is unknown"
                );
            }
        }
        if self.implementation == PythonImplementation::CPython {
            ensure!(
                descriptor.gil_disabled == Some(free_threaded(self.flavor)),
                "observed CPython GIL mode differs from selected archive flavor or is unknown"
            );
        }
        Ok(())
    }
}

fn free_threaded(flavor: RuntimeFlavor) -> bool {
    matches!(
        flavor,
        RuntimeFlavor::FreeThreadedInstallOnly | RuntimeFlavor::FreeThreadedInstallOnlyStripped
    )
}

fn archive_priority(release: &RuntimeRelease) -> Result<u8> {
    Ok(match release.key().flavor {
        RuntimeFlavor::InstallOnlyStripped | RuntimeFlavor::FreeThreadedInstallOnlyStripped => 0,
        RuntimeFlavor::InstallOnly | RuntimeFlavor::FreeThreadedInstallOnly => 1,
        RuntimeFlavor::PyPy if release.source().filename().ends_with(".tar.gz") => 0,
        RuntimeFlavor::PyPy if release.source().filename().ends_with(".tar.bz2") => 1,
        _ => bail!("runtime archive compression is unsupported"),
    })
}

/// This chooses only a complete, explicit request. Default latest policy uses
/// upstream stability through VersionInfo; explicit unstable releases survive.
pub(in crate::plugins::core::python) fn choose_release<'a>(
    releases: &'a [RuntimeRelease],
    request: &str,
) -> Result<&'a RuntimeRelease> {
    let matching: Vec<_> = releases
        .iter()
        .filter(|release| release.request_version() == request)
        .collect();
    let first = *matching
        .first()
        .context("exact Python runtime request has no release")?;
    for release in &matching {
        ensure!(
            release.key().distribution == first.key().distribution
                && release.key().target == first.key().target
                && release.key().python_version == first.key().python_version
                && release.key().implementation_version == first.key().implementation_version
                && free_threaded(release.key().flavor) == free_threaded(first.key().flavor),
            "exact runtime request has conflicting distribution, target or implementation metadata"
        );
    }
    let build = matching
        .iter()
        .map(|release| release.key().distribution_version.as_str())
        .max()
        .context("exact Python runtime request has no release")?;
    let ranked: Vec<_> = matching
        .iter()
        .filter(|release| release.key().distribution_version == build)
        .map(|release| Ok((archive_priority(release)?, *release)))
        .collect::<Result<_>>()?;
    let priority = ranked
        .iter()
        .map(|(priority, _)| *priority)
        .min()
        .context("exact runtime request has no archive")?;
    let winning: Vec<_> = ranked
        .iter()
        .filter(|(rank, _)| *rank == priority)
        .map(|(_, release)| *release)
        .collect();
    let selected = *winning
        .first()
        .context("exact runtime request has no archive")?;
    ensure!(
        winning
            .iter()
            .all(|release| release.key() == selected.key()),
        "exact runtime request has ambiguous equally preferred artifacts"
    );
    super::select_exact_release(releases, selected.key())
}

/// F2 exact-pin selection for the shared library install path: several
/// distribution builds of one complete request are an ambiguity reported
/// with every candidate, never a silent "latest build" pick. An explicit
/// `+YYYYMMDD` build pin filters to that build; within one build the
/// established archive priority resolves the flavor deterministically and
/// equal-priority conflicts stay errors.
pub(in crate::plugins::core::python) fn choose_unique_release<'a>(
    releases: &'a [RuntimeRelease],
    request: &str,
) -> Result<&'a RuntimeRelease> {
    let (version, build_pin) = match request.split_once('+') {
        Some((version, build)) => (version, Some(build)),
        None => (request, None),
    };
    let mut matching: Vec<&RuntimeRelease> = releases
        .iter()
        .filter(|release| release.request_version() == version)
        .collect();
    if let Some(pin) = build_pin {
        matching.retain(|release| release.key().distribution_version == pin);
        ensure!(
            !matching.is_empty(),
            "exact Python runtime request {request:?} pins build {pin:?}, \
             which has no release for this target"
        );
    }
    let selected = *matching
        .first()
        .context("exact Python runtime request has no release")?;
    for release in &matching {
        ensure!(
            release.key().distribution == selected.key().distribution
                && release.key().target == selected.key().target
                && release.key().python_version == selected.key().python_version
                && release.key().implementation_version == selected.key().implementation_version
                && free_threaded(release.key().flavor) == free_threaded(selected.key().flavor),
            "exact runtime request has conflicting distribution, target or implementation metadata"
        );
    }
    let builds: std::collections::BTreeSet<&str> = matching
        .iter()
        .map(|release| release.key().distribution_version.as_str())
        .collect();
    if builds.len() > 1 {
        let candidates: Vec<String> = matching
            .iter()
            .map(|release| {
                format!(
                    "{} ({} {}/{})",
                    release.key().distribution_version,
                    release.release_date().unwrap_or("unknown date"),
                    release.key().python_version,
                    release.source().filename()
                )
            })
            .collect();
        eyre::bail!(
            "exact Python runtime request {version:?} matches {} distribution builds; \
             pin one explicitly, e.g. {version:?}+{}:\n  {}",
            builds.len(),
            builds.iter().last().unwrap_or(&""),
            candidates.join("\n  ")
        );
    }
    // One build remains: flavors resolve deterministically by the established
    // archive priority, and equally preferred conflicts stay errors.
    let ranked: Vec<_> = matching
        .iter()
        .map(|release| Ok((archive_priority(release)?, *release)))
        .collect::<Result<_>>()?;
    let priority = ranked
        .iter()
        .map(|(priority, _)| *priority)
        .min()
        .context("exact runtime request has no archive")?;
    let winning: Vec<_> = ranked
        .iter()
        .filter(|(rank, _)| *rank == priority)
        .map(|(_, release)| *release)
        .collect();
    let winner = *winning
        .first()
        .context("exact runtime request has no archive")?;
    ensure!(
        winning.iter().all(|release| release.key() == winner.key()),
        "exact runtime request has ambiguous equally preferred artifacts"
    );
    super::select_exact_release(releases, winner.key())
}

#[cfg(test)]
mod tests {
    use super::super::{RuntimePlatform, parse_pbs_releases, parse_pypy_releases};
    use super::*;
    use pakro_store::Digest;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    include!("fixtures.rs");

    fn target() -> RuntimeArchiveTarget {
        RuntimeArchiveTarget::new("aarch64", RuntimePlatform::Macos).unwrap()
    }

    fn pbs() -> Vec<RuntimeRelease> {
        parse_pbs_releases(
            &serde_json::from_str::<Value>(PBS_FIXTURE).unwrap(),
            &target(),
            RuntimeFlavor::InstallOnly,
            &BTreeMap::new(),
        )
        .unwrap()
    }

    fn pypy() -> Vec<RuntimeRelease> {
        parse_pypy_releases(
            &serde_json::from_str::<Value>(PYPY_FIXTURE).unwrap(),
            PYPY_CHECKSUMS,
            &target(),
        )
        .unwrap()
    }

    fn locked(source: &SourceArtifact) -> PlatformInfo {
        PlatformInfo {
            url: Some(source.url().to_string()),
            checksum: Some(source.digest().to_string().replacen('-', ":", 1)),
            ..Default::default()
        }
    }

    fn tuple(version: &str) -> Value {
        let parts: Vec<u32> = version
            .split('.')
            .map(|part| part.parse().unwrap())
            .collect();
        json!({"major":parts[0],"minor":parts[1],"micro":parts[2],"release_level":"final","serial":0})
    }

    fn observed(pypy: bool, language: &str, implementation: &str) -> PythonRuntimeDescriptor {
        let short = language.split('.').take(2).collect::<Vec<_>>().join(".");
        serde_json::from_value(json!({
            "schema":1,"observation_mode":"base_runtime_site_disabled",
            "probe_flags":["-E","-s","-S","-B","-c"],"executable":"/fixture/python",
            "implementation":if pypy {"pypy"} else {"cpython"},
            "language_version":tuple(language),"implementation_version":tuple(implementation),
            "implementation_version_source":if pypy {"pypy_version_info"} else {"sys_implementation_version"},
            "markers":{
                "implementation_name":if pypy {"pypy"} else {"cpython"},
                "implementation_version":implementation,"os_name":"posix","platform_machine":"arm64",
                "platform_python_implementation":if pypy {"PyPy"} else {"CPython"},
                "platform_release":"fixture kernel","platform_system":"Darwin","platform_version":"fixture kernel",
                "python_full_version":language,"python_version":short,"sys_platform":"darwin"
            },
            "soabi":null,"ext_suffix":null,"abiflags":if pypy {Value::Null} else {json!("")},
            "extension_suffixes":[],"gil_disabled":if pypy {Value::Null} else {json!(false)},
            "compatibility":{"pointer_bits":64,"debug":false,"pymalloc":true,"unicode_size":null,
                "platform":{"os":"macos","cpu_type":16777228,"product_version":{"major":14,"minor":5,"micro":0}}},
            "sysconfig_paths":{"stdlib":"/fixture/lib/python"}
        })).unwrap()
    }

    #[test]
    fn discovered_plan_retains_complete_language_implementation_and_source() {
        for release in [&pbs()[0], &pypy()[0]] {
            let plan = RuntimeInstallPlan::from_release(release);
            assert_eq!(
                plan.expected_python_version.as_deref(),
                Some(release.key().python_version.as_str())
            );
            assert_eq!(
                plan.implementation_version,
                release.key().implementation_version
            );
            assert_eq!(plan.source().filename(), release.source().filename());
            assert_eq!(plan.source().digest(), release.digest());
            assert_eq!(plan.distribution(), release.key().distribution);
            assert_eq!(plan.target(), &release.key().target);
            assert_eq!(plan.flavor(), release.key().flavor);
        }
    }

    #[test]
    fn r06_language_key_reuses_complete_release_and_separates_patch_recipe() {
        for release in [&pbs()[0], &pypy()[0]] {
            let first = RuntimeInstallPlan::from_release(release);
            let second = RuntimeInstallPlan::from_release(release);
            assert_eq!(
                first.language_key(false).unwrap(),
                second.language_key(false).unwrap()
            );
            if first.distribution() == RuntimeDistribution::Pbs {
                assert_ne!(
                    first.language_key(false).unwrap(),
                    first.language_key(true).unwrap()
                );
            } else {
                assert_eq!(
                    first.language_key(false).unwrap(),
                    first.language_key(true).unwrap()
                );
            }
        }
    }

    #[test]
    fn r06_language_key_preserves_runtime_target_and_implementation_inputs() {
        let plan = RuntimeInstallPlan::from_release(&pypy()[0]);
        let expected = plan.language_key(false).unwrap();
        let mut changed = plan.clone();
        changed.implementation_version = "8.0.2".into();
        assert_ne!(expected, changed.language_key(false).unwrap());
        let mut changed = plan.clone();
        changed.target = RuntimeArchiveTarget::new("x86_64", RuntimePlatform::Macos).unwrap();
        assert_ne!(expected, changed.language_key(false).unwrap());
        let mut changed = plan;
        changed.expected_python_version = Some("3.12.16".into());
        assert_ne!(expected, changed.language_key(false).unwrap());
    }

    #[test]
    fn r06_language_key_does_not_invent_a_missing_pypy_language_patch() {
        let releases = pypy();
        let plan = RuntimeInstallPlan::from_locked(
            "pypy3.12-8.0.1",
            &target(),
            RuntimeFlavor::PyPy,
            &locked(releases[0].source()),
        )
        .unwrap();
        assert!(plan.language_key(false).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn r06_pypy_spec_normalizes_the_ineffective_patch_setting() {
        use crate::libraries::language::repository::LanguageInstallSpec;
        let plan = RuntimeInstallPlan::from_release(&pypy()[0]);
        let unpatched = LanguageInstallSpec::from_plan(&plan, false).unwrap();
        let requested = LanguageInstallSpec::from_plan(&plan, true).unwrap();
        assert_eq!(unpatched.key(), requested.key());
        assert!(!unpatched.patch_sysconfig());
        assert!(
            !requested.patch_sysconfig(),
            "an ineffective PyPy setting cannot create a conflicting manifest for the same key"
        );
    }

    #[test]
    fn r06_native_provenance_preflight_rejects_disabled_locked_verification() {
        use crate::plugins::core::python::PythonPlugin;
        let backend = PythonPlugin::new();
        let mut tv = crate::backend::static_helpers::test_tool_version(&backend, "3.15.0", &[]);
        let plan = RuntimeInstallPlan::from_release(&pbs()[0]);
        let mut info = locked(plan.source());
        info.provenance = Some(crate::lockfile::ProvenanceType::GithubAttestations);
        tv.lock_platforms.insert("fixture".into(), info);
        let mut settings = crate::config::Settings::default();
        settings.python.github_attestations = Some(false);
        assert!(
            PythonPlugin::native_provenance_preflight(&plan, &tv, "fixture", &settings).is_err(),
            "an existing payload must not bypass a locked provenance setting refusal"
        );
    }

    #[test]
    fn r06_native_provenance_preflight_rejects_unsupported_pypy_proof() {
        use crate::plugins::core::python::PythonPlugin;
        let backend = PythonPlugin::new();
        let mut tv =
            crate::backend::static_helpers::test_tool_version(&backend, "pypy3.12-8.0.1", &[]);
        let plan = RuntimeInstallPlan::from_release(&pypy()[0]);
        let mut info = locked(plan.source());
        info.provenance = Some(crate::lockfile::ProvenanceType::GithubAttestations);
        tv.lock_platforms.insert("fixture".into(), info);
        assert!(
            PythonPlugin::native_provenance_preflight(
                &plan,
                &tv,
                "fixture",
                &crate::config::Settings::default()
            )
            .is_err(),
            "PyPy cannot acquire an unsupported proof through Ready reuse"
        );
    }

    #[test]
    fn locked_pbs_replays_sha256_and_sha512_without_catalog() {
        let releases = pbs();
        for digest in [releases[0].digest(), Digest::from_sha512_bytes(b"fixture")] {
            let mut info = locked(releases[0].source());
            info.checksum = Some(digest.to_string().replacen('-', ":", 1));
            let plan = RuntimeInstallPlan::from_locked(
                "3.15.0",
                &target(),
                RuntimeFlavor::InstallOnly,
                &info,
            )
            .unwrap();
            assert_eq!(plan.expected_python_version.as_deref(), Some("3.15.0"));
            assert_eq!((plan.language_major, plan.language_minor), (3, 15));
            assert_eq!(plan.implementation, PythonImplementation::CPython);
            assert_eq!(plan.implementation_version, "3.15.0");
            assert_eq!(plan.source().digest(), digest);
        }
    }

    #[test]
    fn legacy_pypy_lock_keeps_unknown_patch_instead_of_inventing_one() {
        let releases = pypy();
        let plan = RuntimeInstallPlan::from_locked(
            "pypy3.12-8.0.1",
            &target(),
            RuntimeFlavor::PyPy,
            &locked(releases[0].source()),
        )
        .unwrap();
        assert_eq!(plan.expected_python_version, None);
        assert_eq!((plan.language_major, plan.language_minor), (3, 12));
        assert_eq!(plan.implementation, PythonImplementation::PyPy);
        assert_eq!(plan.implementation_version, "8.0.1");
        plan.validate_observed(&observed(true, "3.12.15", "8.0.1"))
            .unwrap();
        plan.validate_observed(&observed(true, "3.12.99", "8.0.1"))
            .unwrap();
        assert!(
            plan.validate_observed(&observed(true, "3.11.15", "8.0.1"))
                .is_err()
        );
    }

    #[test]
    fn locked_missing_legacy_or_bad_hash_never_becomes_a_new_download_expectation() {
        let releases = pbs();
        for checksum in [
            None,
            Some("blake3:old-lock".into()),
            Some("sha256:wrong".into()),
            Some("md5:bad".into()),
        ] {
            let mut info = locked(releases[0].source());
            info.checksum = checksum;
            let error = RuntimeInstallPlan::from_locked(
                "3.15.0",
                &target(),
                RuntimeFlavor::InstallOnly,
                &info,
            )
            .unwrap_err();
            if info
                .checksum
                .as_deref()
                .is_some_and(|value| value.starts_with("blake3:"))
            {
                assert!(error.to_string().contains("migration"), "{error}");
            }
        }
    }

    #[test]
    fn locked_source_request_target_and_flavor_must_all_agree() {
        let releases = pbs();
        let info = locked(releases[0].source());
        assert!(
            RuntimeInstallPlan::from_locked("3.14.0", &target(), RuntimeFlavor::InstallOnly, &info)
                .is_err()
        );
        assert!(
            RuntimeInstallPlan::from_locked(
                "3.15.0",
                &target(),
                RuntimeFlavor::InstallOnlyStripped,
                &info
            )
            .is_err()
        );
        let other = RuntimeArchiveTarget::new("x86_64", RuntimePlatform::Macos).unwrap();
        assert!(
            RuntimeInstallPlan::from_locked("3.15.0", &other, RuntimeFlavor::InstallOnly, &info)
                .is_err()
        );
        let mut missing = info.clone();
        missing.url = None;
        assert!(
            RuntimeInstallPlan::from_locked(
                "3.15.0",
                &target(),
                RuntimeFlavor::InstallOnly,
                &missing
            )
            .is_err()
        );
    }

    #[test]
    fn locked_diagnostics_and_plan_debug_do_not_disclose_locator_credentials() {
        let releases = pbs();
        for url in [
            "https://secret:password@github.com/not-an-artifact",
            "https://github.com/not-an-artifact?token=hidden",
        ] {
            let mut info = locked(releases[0].source());
            info.url = Some(url.into());
            let error = RuntimeInstallPlan::from_locked(
                "3.15.0",
                &target(),
                RuntimeFlavor::InstallOnly,
                &info,
            )
            .unwrap_err();
            for secret in ["secret", "password", "hidden", "https://"] {
                assert!(!error.to_string().contains(secret));
            }
        }
        let plan = RuntimeInstallPlan::from_release(&releases[0]);
        assert!(!format!("{plan:?}").contains("https://"));
    }

    fn with_pbs_date(date: &str, flavor: RuntimeFlavor) -> Vec<RuntimeRelease> {
        let mut raw: Value = serde_json::from_str(PBS_FIXTURE).unwrap();
        raw[0]["tag_name"] = json!(date);
        for asset in raw[0]["assets"].as_array_mut().unwrap() {
            for field in ["name", "browser_download_url"] {
                asset[field] = json!(asset[field].as_str().unwrap().replace("20261013", date));
            }
        }
        parse_pbs_releases(&raw, &target(), flavor, &BTreeMap::new()).unwrap()
    }

    #[test]
    fn f2_multiple_builds_are_not_latest_selection() {
        let mut releases = with_pbs_date("20261012", RuntimeFlavor::InstallOnlyStripped);
        releases.extend(with_pbs_date("20261013", RuntimeFlavor::InstallOnly));
        // The legacy chooser auto-picks the newer build; the F2 exact-pin
        // path must report the ambiguity with every candidate listed.
        assert_eq!(
            choose_release(&releases, "3.15.0")
                .unwrap()
                .key()
                .distribution_version,
            "20261013"
        );
        let ambiguity = choose_unique_release(&releases, "3.15.0")
            .unwrap_err()
            .to_string();
        assert!(
            ambiguity.contains("pin one explicitly"),
            "the ambiguity must teach the pin spelling: {ambiguity}"
        );
        for build in ["20261012", "20261013"] {
            assert!(
                ambiguity.contains(build),
                "every candidate build is listed: {ambiguity}"
            );
        }
        // An explicit pin selects that build uniquely.
        let pinned = choose_unique_release(&releases, "3.15.0+20261012").unwrap();
        assert_eq!(pinned.key().distribution_version, "20261012");
        // A pin without a matching build is an explicit error.
        assert!(
            choose_unique_release(&releases, "3.15.0+20261099").is_err(),
            "a pin with no release is refused, not silently widened"
        );
        let single = with_pbs_date("20261014", RuntimeFlavor::InstallOnly);
        let selected = choose_unique_release(&single, "3.15.0").unwrap();
        assert_eq!(selected.key().distribution_version, "20261014");
        for request in ["latest", "3.15", "3.14.0", ""] {
            assert!(choose_unique_release(&single, request).is_err());
        }
    }

    #[test]
    fn exact_request_chooses_latest_pbs_build_then_stripped_at_same_date() {
        let mut releases = with_pbs_date("20261012", RuntimeFlavor::InstallOnlyStripped);
        releases.extend(with_pbs_date("20261013", RuntimeFlavor::InstallOnly));
        let selected = choose_release(&releases, "3.15.0").unwrap();
        assert_eq!(selected.key().distribution_version, "20261013");
        releases.extend(with_pbs_date(
            "20261013",
            RuntimeFlavor::InstallOnlyStripped,
        ));
        assert_eq!(
            choose_release(&releases, "3.15.0").unwrap().key().flavor,
            RuntimeFlavor::InstallOnlyStripped
        );
        for request in ["latest", "3.15", "3.14.0", ""] {
            assert!(choose_release(&releases, request).is_err());
        }
    }

    #[test]
    fn equal_priority_artifact_conflicts_are_errors_in_both_orders() {
        let mut one = pbs();
        let mut raw: Value = serde_json::from_str(PBS_FIXTURE).unwrap();
        for asset in raw[0]["assets"].as_array_mut().unwrap() {
            asset["digest"] = json!(format!("sha256:{}", "a".repeat(64)));
        }
        one.extend(
            parse_pbs_releases(
                &raw,
                &target(),
                RuntimeFlavor::InstallOnly,
                &BTreeMap::new(),
            )
            .unwrap(),
        );
        for _ in 0..2 {
            assert!(choose_release(&one, "3.15.0").is_err());
            one.reverse();
        }
    }

    #[test]
    fn explicit_unstable_pypy_survives_and_compression_preference_is_deterministic() {
        let mut raw: Value = serde_json::from_str(PYPY_FIXTURE).unwrap();
        raw[0]["stable"] = json!(false);
        let mut file = raw[0]["files"][0].clone();
        for field in ["filename", "download_url"] {
            file[field] = json!(file[field].as_str().unwrap().replace(".tar.gz", ".tar.bz2"));
        }
        raw[0]["files"].as_array_mut().unwrap().insert(0, file);
        let html = format!(
            "{PYPY_CHECKSUMS}<pre>{}  pypy3.12-v8.0.1-macos_arm64.tar.bz2</pre>",
            "a".repeat(64)
        );
        let mut releases = parse_pypy_releases(&raw, &html, &target()).unwrap();
        for _ in 0..2 {
            let selected = choose_release(&releases, "pypy3.12-8.0.1").unwrap();
            assert!(selected.source().filename().ends_with(".tar.gz"));
            assert_eq!(selected.upstream_stable(), Some(false));
            releases.reverse();
        }
    }

    #[test]
    fn actual_language_and_implementation_are_checked_before_accepting_runtime() {
        let cpython = pbs();
        let plan = RuntimeInstallPlan::from_release(&cpython[0]);
        plan.validate_observed(&observed(false, "3.15.0", "3.15.0"))
            .unwrap();
        assert!(
            plan.validate_observed(&observed(false, "3.15.1", "3.15.1"))
                .is_err()
        );
        assert!(
            plan.validate_observed(&observed(true, "3.15.0", "8.0.1"))
                .is_err()
        );
        let pypy = pypy();
        let plan = RuntimeInstallPlan::from_release(&pypy[0]);
        assert!(
            plan.validate_observed(&observed(true, "3.12.14", "8.0.1"))
                .is_err()
        );
        assert!(
            plan.validate_observed(&observed(true, "3.12.15", "8.0.0"))
                .is_err()
        );
    }

    #[test]
    fn loaded_cpu_evidence_beats_uname_and_unknown_compatibility_is_rejected() {
        let cpython = pbs();
        let plan = RuntimeInstallPlan::from_release(&cpython[0]);
        let mut descriptor = observed(false, "3.15.0", "3.15.0");
        descriptor.markers.platform_machine = "x86_64".into();
        plan.validate_observed(&descriptor).unwrap();
        let mut value = serde_json::to_value(&descriptor).unwrap();
        value["compatibility"]["platform"]["cpu_type"] = json!(16777223);
        assert!(
            plan.validate_observed(&serde_json::from_value(value).unwrap())
                .is_err()
        );
        descriptor.compatibility = None;
        assert!(plan.validate_observed(&descriptor).is_err());
    }

    #[test]
    fn free_threaded_and_ordinary_cpython_are_not_interchangeable() {
        let release = with_pbs_date("20261013", RuntimeFlavor::FreeThreadedInstallOnly);
        let plan = RuntimeInstallPlan::from_release(&release[0]);
        let mut descriptor = observed(false, "3.15.0", "3.15.0");
        assert!(plan.validate_observed(&descriptor).is_err());
        descriptor.gil_disabled = Some(true);
        descriptor.abiflags = Some("t".into());
        plan.validate_observed(&descriptor).unwrap();
        let ordinary = pbs();
        assert!(
            RuntimeInstallPlan::from_release(&ordinary[0])
                .validate_observed(&descriptor)
                .is_err()
        );
    }

    fn linux_plan(platform: RuntimePlatform) -> RuntimeInstallPlan {
        let suffix = if platform == RuntimePlatform::LinuxGnu {
            "unknown-linux-gnu"
        } else {
            "unknown-linux-musl"
        };
        let mut raw: Value = serde_json::from_str(PBS_FIXTURE).unwrap();
        for asset in raw[0]["assets"].as_array_mut().unwrap() {
            for field in ["name", "browser_download_url"] {
                asset[field] = json!(
                    asset[field]
                        .as_str()
                        .unwrap()
                        .replace("apple-darwin", suffix)
                );
            }
        }
        let target = RuntimeArchiveTarget::new("aarch64", platform).unwrap();
        let releases =
            parse_pbs_releases(&raw, &target, RuntimeFlavor::InstallOnly, &BTreeMap::new())
                .unwrap();
        RuntimeInstallPlan::from_release(&releases[0])
    }

    fn linux_observed(musl: bool) -> PythonRuntimeDescriptor {
        let mut raw = serde_json::to_value(observed(false, "3.15.0", "3.15.0")).unwrap();
        raw["markers"]["sys_platform"] = json!("linux");
        raw["markers"]["platform_system"] = json!("Linux");
        raw["markers"]["platform_machine"] = json!("aarch64");
        let loader = if musl {
            "/lib/ld-musl-aarch64.so.1"
        } else {
            "/lib/ld-linux-aarch64.so.1"
        };
        let libc = if musl {
            json!({"family":"musl","version":{"major":1,"minor":2},"loader":loader})
        } else {
            json!({"family":"glibc","major":2,"minor":31})
        };
        raw["compatibility"]["platform"] = json!({"os":"linux",
            "elf":{"class":2,"little_endian":true,"machine":183,"flags":0,"interpreter":loader},
            "libc":libc,"manylinux":{"state":if musl {"unknown"} else {"default"}}});
        serde_json::from_value(raw).unwrap()
    }

    #[test]
    fn linux_architecture_and_libc_require_observed_evidence() {
        let gnu = linux_plan(RuntimePlatform::LinuxGnu);
        let musl = linux_plan(RuntimePlatform::LinuxMusl);
        gnu.validate_observed(&linux_observed(false)).unwrap();
        musl.validate_observed(&linux_observed(true)).unwrap();
        assert!(gnu.validate_observed(&linux_observed(true)).is_err());
        assert!(musl.validate_observed(&linux_observed(false)).is_err());
        let mut raw = serde_json::to_value(linux_observed(false)).unwrap();
        raw["compatibility"]["platform"]["libc"] = json!({"family":"unknown"});
        assert!(
            gnu.validate_observed(&serde_json::from_value(raw).unwrap())
                .is_err()
        );
        let mut raw = serde_json::to_value(linux_observed(false)).unwrap();
        raw["compatibility"]["platform"]["elf"]["machine"] = json!(62);
        assert!(
            gnu.validate_observed(&serde_json::from_value(raw).unwrap())
                .is_err()
        );
    }
}
