//! Pure official Python distribution adapters. Fetching and installation are
//! separate consumers; a release always carries an exact, validated source.

pub(super) mod artifact;
pub(super) mod catalog;
pub(super) mod entrypoint;
#[cfg(unix)]
pub(crate) mod extract;
pub(crate) mod pipeline;
#[cfg(unix)]
pub(super) mod provenance;
pub(crate) mod transport;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use eyre::{ContextCompat, Result, ensure};
use pakro_index::artifact::SourceArtifact;
use pakro_store::Digest;
use serde::Deserialize;
use serde_json::Value;
use xx::regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RuntimeDistribution {
    Pbs,
    PyPy,
    CpythonSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RuntimePlatform {
    Macos,
    LinuxGnu,
    LinuxMusl,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RuntimeArchiveTarget {
    arch: String,
    platform: RuntimePlatform,
}

impl RuntimeArchiveTarget {
    pub(crate) fn new(arch: &str, platform: RuntimePlatform) -> Result<Self> {
        ensure!(
            !arch.is_empty()
                && arch
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "invalid runtime archive architecture"
        );
        Ok(Self {
            arch: arch.to_string(),
            platform,
        })
    }

    pub(crate) fn arch(&self) -> &str {
        &self.arch
    }

    pub(crate) fn platform(&self) -> RuntimePlatform {
        self.platform
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RuntimeFlavor {
    InstallOnly,
    InstallOnlyStripped,
    FreeThreadedInstallOnly,
    FreeThreadedInstallOnlyStripped,
    PyPy,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct RuntimeReleaseKey {
    pub(super) distribution: RuntimeDistribution,
    pub(super) python_version: String,
    pub(super) implementation_version: String,
    pub(super) distribution_version: String,
    pub(super) target: RuntimeArchiveTarget,
    pub(super) flavor: RuntimeFlavor,
    pub(super) filename: String,
    pub(super) digest: Digest,
}

#[derive(Debug, Clone)]
pub(super) struct RuntimeRelease {
    key: RuntimeReleaseKey,
    request_version: String,
    release_date: Option<String>,
    upstream_stable: Option<bool>,
    source: SourceArtifact,
}

impl RuntimeRelease {
    pub(super) fn key(&self) -> &RuntimeReleaseKey {
        &self.key
    }

    pub(super) fn request_version(&self) -> &str {
        &self.request_version
    }

    pub(super) fn release_date(&self) -> Option<&str> {
        self.release_date.as_deref()
    }

    pub(super) fn source(&self) -> &SourceArtifact {
        &self.source
    }

    pub(super) fn digest(&self) -> Digest {
        self.source.digest()
    }

    pub(super) fn upstream_stable(&self) -> Option<bool> {
        self.upstream_stable
    }
}

#[derive(Deserialize)]
struct PbsRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    published_at: Option<String>,
    assets: Vec<PbsAsset>,
}

#[derive(Deserialize)]
struct PbsAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

struct PbsCandidate {
    python_version: String,
    build: String,
    release_date: Option<String>,
    target: RuntimeArchiveTarget,
    flavor: RuntimeFlavor,
    filename: String,
    url: String,
    api_digest: Option<Digest>,
}

/// Exact labels can defer authoritative archive validation to installation.
pub(crate) fn is_exact_request(value: &str) -> bool {
    if let Some(value) = value.strip_prefix("pypy") {
        let Some((language, implementation)) = value.split_once('-') else {
            return false;
        };
        return language.split('.').count() == 2
            && version_parts(&format!("{language}.0")).is_ok_and(|(major, _, _)| major == 3)
            && version_parts(implementation).is_ok();
    }
    version_parts(value).is_ok_and(|(major, _, _)| major == 3)
}

fn version_parts(value: &str) -> Result<(u32, u32, u32)> {
    let captures = regex!(r"^([0-9]+)\.([0-9]+)\.([0-9]+)(?:(a|b|rc)([0-9]+))?$")
        .captures(value)
        .context("invalid runtime release version")?;
    let number = |text: &str| -> Result<u32> {
        ensure!(
            text.len() == 1 || !text.starts_with('0'),
            "invalid runtime release version"
        );
        text.parse()
            .map_err(|_| eyre::eyre!("runtime version component exceeds bounds"))
    };
    if let Some(serial) = captures.get(5) {
        number(serial.as_str())?;
    }
    Ok((
        number(&captures[1])?,
        number(&captures[2])?,
        number(&captures[3])?,
    ))
}

fn pbs_build_date(value: &str) -> Result<()> {
    ensure!(
        value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid PBS release date"
    );
    format!("{}-{}-{}", &value[..4], &value[4..6], &value[6..])
        .parse::<jiff::civil::Date>()
        .map_err(|_| eyre::eyre!("invalid PBS release date"))?;
    Ok(())
}

/// Reconstructs the archive identity facts of an official PBS filename. The
/// language library bridge reuses this to rebuild installation specs from
/// stored manifests without contacting any remote source.
pub(crate) fn pbs_filename(
    filename: &str,
) -> Result<Option<(String, String, RuntimeArchiveTarget, RuntimeFlavor)>> {
    let flavors = [
        (
            RuntimeFlavor::FreeThreadedInstallOnlyStripped,
            "-freethreaded-install_only_stripped.tar.gz",
        ),
        (
            RuntimeFlavor::FreeThreadedInstallOnly,
            "-freethreaded-install_only.tar.gz",
        ),
        (
            RuntimeFlavor::InstallOnlyStripped,
            "-install_only_stripped.tar.gz",
        ),
        (RuntimeFlavor::InstallOnly, "-install_only.tar.gz"),
    ];
    let Some(rest) = filename.strip_prefix("cpython-") else {
        return Ok(None);
    };
    let Some((flavor, stem)) = flavors
        .iter()
        .find_map(|(flavor, suffix)| rest.strip_suffix(suffix).map(|stem| (*flavor, stem)))
    else {
        return Ok(None);
    };
    let (release, triple) = stem
        .split_once('-')
        .context("invalid PBS archive filename")?;
    let (version, build) = release
        .split_once('+')
        .context("invalid PBS archive identity")?;
    let (major, minor, _) = version_parts(version)?;
    if major != 3 {
        return Ok(None);
    }
    pbs_build_date(build)?;
    if matches!(
        flavor,
        RuntimeFlavor::FreeThreadedInstallOnly | RuntimeFlavor::FreeThreadedInstallOnlyStripped
    ) {
        ensure!(
            minor >= 13,
            "free-threaded PBS archive requires Python 3.13 or later"
        );
    }
    let platform_suffixes = [
        ("-apple-darwin", RuntimePlatform::Macos),
        ("-unknown-linux-gnu", RuntimePlatform::LinuxGnu),
        ("-unknown-linux-musl", RuntimePlatform::LinuxMusl),
    ];
    let Some((arch, platform)) = platform_suffixes
        .into_iter()
        .find_map(|(suffix, platform)| triple.strip_suffix(suffix).map(|arch| (arch, platform)))
    else {
        return Ok(None);
    };
    Ok(Some((
        version.into(),
        build.into(),
        RuntimeArchiveTarget::new(arch, platform)?,
        flavor,
    )))
}

fn validate_locator(
    filename: &str,
    locator: &str,
    distribution: RuntimeDistribution,
    build: &str,
) -> Result<()> {
    let url =
        url::Url::parse(locator).map_err(|_| eyre::eyre!("invalid runtime artifact locator"))?;
    let (host, expected) = match distribution {
        RuntimeDistribution::Pbs => (
            "github.com",
            vec![
                "astral-sh",
                "python-build-standalone",
                "releases",
                "download",
                build,
                filename,
            ],
        ),
        RuntimeDistribution::PyPy => ("downloads.python.org", vec!["pypy", filename]),
        RuntimeDistribution::CpythonSource => ("www.python.org", vec!["ftp", "python", build, filename]),
    };
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some(host)
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid official runtime artifact locator"
    );
    let actual: Vec<_> = url
        .path_segments()
        .context("invalid runtime artifact path")?
        .collect();
    ensure!(
        actual.len() == expected.len(),
        "runtime artifact locator differs from archive identity"
    );
    for (actual, expected) in actual.into_iter().zip(expected) {
        let actual = urlencoding::decode(actual)
            .map_err(|_| eyre::eyre!("invalid runtime artifact path encoding"))?;
        ensure!(
            actual == expected,
            "runtime artifact locator differs from archive identity"
        );
    }
    Ok(())
}

fn sha256_hex(value: &str) -> Result<Digest> {
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid runtime SHA256 checksum"
    );
    format!("sha256-{value}")
        .parse()
        .map_err(|_| eyre::eyre!("invalid runtime SHA256 checksum"))
}

fn checksum_rows(text: &str, hashes: &mut BTreeMap<String, Digest>) -> Result<()> {
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() == 2, "invalid runtime checksum row");
        let digest = sha256_hex(fields[0])?;
        let filename = fields[1].strip_prefix('*').unwrap_or(fields[1]);
        ensure!(
            !filename.is_empty()
                && !matches!(filename, "." | "..")
                && !filename
                    .chars()
                    .any(|ch| ch.is_control() || matches!(ch, '/' | '\\')),
            "invalid runtime checksum filename"
        );
        if let Some(previous) = hashes.insert(filename.into(), digest) {
            ensure!(
                previous == digest,
                "conflicting runtime checksums for one filename"
            );
        }
    }
    Ok(())
}

fn pbs_candidates(
    raw: &Value,
    target: &RuntimeArchiveTarget,
    flavor: RuntimeFlavor,
) -> Result<Vec<PbsCandidate>> {
    ensure!(
        flavor != RuntimeFlavor::PyPy,
        "PyPy flavor is not a PBS archive flavor"
    );
    let releases: Vec<PbsRelease> = serde_json::from_value(raw.clone())
        .map_err(|_| eyre::eyre!("invalid official PBS release metadata"))?;
    let mut candidates: Vec<PbsCandidate> = Vec::new();
    let mut seen = HashMap::<String, usize>::new();
    for release in releases {
        if release.draft {
            continue;
        }
        for asset in release.assets {
            let Some((version, build, archive_target, archive_flavor)) = pbs_filename(&asset.name)?
            else {
                continue;
            };
            if archive_target != *target || archive_flavor != flavor {
                continue;
            }
            ensure!(
                release.tag_name == build,
                "PBS filename and release date disagree"
            );
            if let Some(date) = &release.published_at {
                date.parse::<jiff::Timestamp>()
                    .map_err(|_| eyre::eyre!("invalid PBS publication date"))?;
            }
            validate_locator(
                &asset.name,
                &asset.browser_download_url,
                RuntimeDistribution::Pbs,
                &build,
            )?;
            let api_digest = asset
                .digest
                .as_deref()
                .map(|value| {
                    let hex = value
                        .strip_prefix("sha256:")
                        .context("invalid PBS API SHA256 digest")?;
                    sha256_hex(hex)
                })
                .transpose()?;
            if let Some(previous) = seen.get(&asset.name).map(|index| &candidates[*index]) {
                ensure!(
                    previous.api_digest == api_digest
                        && previous.python_version == version
                        && previous.build == build
                        && previous.target == archive_target
                        && previous.flavor == archive_flavor
                        && previous.release_date == release.published_at,
                    "conflicting PBS metadata for one archive"
                );
                continue;
            }
            seen.insert(asset.name.clone(), candidates.len());
            candidates.push(PbsCandidate {
                python_version: version,
                build,
                release_date: release.published_at.clone(),
                target: archive_target,
                flavor: archive_flavor,
                filename: asset.name,
                url: asset.browser_download_url,
                api_digest,
            });
        }
    }
    Ok(candidates)
}

pub(super) fn required_pbs_checksums(
    raw: &Value,
    target: &RuntimeArchiveTarget,
    flavor: RuntimeFlavor,
) -> Result<Vec<String>> {
    Ok(pbs_candidates(raw, target, flavor)?
        .into_iter()
        .filter(|candidate| candidate.api_digest.is_none())
        .map(|candidate| candidate.build)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

pub(super) fn parse_pbs_releases(
    raw: &Value,
    target: &RuntimeArchiveTarget,
    flavor: RuntimeFlavor,
    release_checksums: &BTreeMap<String, String>,
) -> Result<Vec<RuntimeRelease>> {
    let mut releases = Vec::new();
    let mut manifests = HashMap::new();
    for candidate in pbs_candidates(raw, target, flavor)? {
        if let Some(manifest) = release_checksums.get(&candidate.build)
            && let std::collections::hash_map::Entry::Vacant(entry) =
                manifests.entry(candidate.build.clone())
        {
            let mut hashes = BTreeMap::new();
            checksum_rows(manifest, &mut hashes)?;
            entry.insert(hashes);
        }
        let manifest_digest = manifests
            .get(&candidate.build)
            .and_then(|hashes| hashes.get(&candidate.filename))
            .copied();
        if let (Some(api), Some(manifest)) = (candidate.api_digest, manifest_digest) {
            ensure!(api == manifest, "PBS API and manifest SHA256 disagree");
        }
        let digest = candidate
            .api_digest
            .or(manifest_digest)
            .context("PBS archive has no expected SHA256 checksum")?;
        let source = SourceArtifact::new(&candidate.filename, &candidate.url, digest)?;
        releases.push(RuntimeRelease {
            key: RuntimeReleaseKey {
                distribution: RuntimeDistribution::Pbs,
                python_version: candidate.python_version.clone(),
                implementation_version: candidate.python_version.clone(),
                distribution_version: candidate.build,
                target: candidate.target,
                flavor: candidate.flavor,
                filename: candidate.filename,
                digest,
            },
            request_version: candidate.python_version,
            release_date: candidate.release_date,
            upstream_stable: None,
            source,
        });
    }
    Ok(releases)
}

#[derive(Deserialize)]
struct PypyRelease {
    pypy_version: String,
    python_version: String,
    date: Option<String>,
    stable: Option<bool>,
    files: Vec<PypyFile>,
}

#[derive(Deserialize)]
struct PypyFile {
    filename: String,
    arch: String,
    platform: String,
    download_url: String,
}

fn pypy_file_target(file: &PypyFile) -> Result<Option<(RuntimeArchiveTarget, Vec<&'static str>)>> {
    let (arch, platform, suffixes) = match (file.platform.as_str(), file.arch.as_str()) {
        ("darwin", "arm64") => (
            "aarch64",
            RuntimePlatform::Macos,
            vec!["macos_arm64.tar.gz", "macos_arm64.tar.bz2"],
        ),
        ("darwin", "x64") => (
            "x86_64",
            RuntimePlatform::Macos,
            vec![
                "macos_x86_64.tar.gz",
                "macos_x86_64.tar.bz2",
                "osx64.tar.bz2",
            ],
        ),
        ("linux", "x64") => (
            "x86_64",
            RuntimePlatform::LinuxGnu,
            vec!["linux64.tar.gz", "linux64.tar.bz2"],
        ),
        ("linux", "i686") => (
            "i686",
            RuntimePlatform::LinuxGnu,
            vec!["linux32.tar.gz", "linux32.tar.bz2"],
        ),
        ("linux", "aarch64") => (
            "aarch64",
            RuntimePlatform::LinuxGnu,
            vec![
                "aarch64.tar.gz",
                "aarch64.tar.bz2",
                "aarch64-portable.tar.bz2",
            ],
        ),
        ("linux", "s390x") => (
            "s390x",
            RuntimePlatform::LinuxGnu,
            vec!["s390x.tar.gz", "s390x.tar.bz2"],
        ),
        _ => return Ok(None),
    };
    Ok(Some((RuntimeArchiveTarget::new(arch, platform)?, suffixes)))
}

// This accepts the official page's literal checksum blocks, rather than
// interpreting HTML examples inside comments, attributes or raw-text elements.
// Nested markup/entities inside a checksum block fail closed.
fn html_tag_end(html: &str, start: usize) -> Result<usize> {
    let mut quote = None;
    for (relative, byte) in html.as_bytes()[start + 1..].iter().copied().enumerate() {
        match (quote, byte) {
            (Some(current), byte) if current == byte => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Ok(start + 1 + relative),
            _ => {}
        }
    }
    Err(eyre::eyre!(
        "unterminated HTML tag in runtime checksum page"
    ))
}

fn html_tag_name(tag: &str) -> Result<(&str, bool)> {
    let tag = tag.trim();
    let closing = tag.starts_with('/');
    let tag = tag.strip_prefix('/').unwrap_or(tag);
    let name = tag
        .split(|ch: char| ch.is_ascii_whitespace() || ch == '/')
        .next()
        .unwrap_or_default();
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "invalid HTML tag in runtime checksum page"
    );
    Ok((name, closing))
}

fn skip_raw_text(html: &str, mut cursor: usize, name: &str) -> Result<usize> {
    let needle = format!("</{name}");
    while let Some(relative) = html[cursor..].find(&needle) {
        let start = cursor + relative;
        let end = html_tag_end(html, start)?;
        let (actual, closing) = html_tag_name(&html[start + 1..end])?;
        if closing && actual == name {
            return Ok(end + 1);
        }
        cursor = end + 1;
    }
    Err(eyre::eyre!(
        "unterminated raw-text element in runtime checksum page"
    ))
}

fn visible_preformatted_blocks(html: &str) -> Result<Vec<&str>> {
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    let mut templates = 0_usize;
    let mut blocks = Vec::new();
    while let Some(relative) = lower[cursor..].find('<') {
        let start = cursor + relative;
        if lower[start..].starts_with("<!--") {
            cursor = start
                + 4
                + lower[start + 4..]
                    .find("-->")
                    .context("unterminated comment in runtime checksum page")?
                + 3;
            continue;
        }
        let end = html_tag_end(&lower, start)?;
        let tag = &lower[start + 1..end];
        cursor = end + 1;
        if tag.starts_with('!') {
            ensure!(
                tag.starts_with("!doctype ") && !tag.contains('['),
                "unsupported HTML declaration in runtime checksum page"
            );
            continue;
        }
        let (name, closing) = html_tag_name(tag)?;
        if name == "template" {
            if closing {
                templates = templates.saturating_sub(1);
            } else {
                templates += 1;
            }
            continue;
        }
        if closing {
            continue;
        }
        if matches!(
            name,
            "script" | "style" | "textarea" | "title" | "xmp" | "iframe" | "noembed" | "noframes"
        ) {
            cursor = skip_raw_text(&lower, cursor, name)?;
            continue;
        }
        if templates > 0 || name != "pre" {
            continue;
        }
        let content_end = cursor
            + lower[cursor..]
                .find('<')
                .context("unterminated checksum preformatted block")?;
        let close_end = html_tag_end(&lower, content_end)?;
        let (close_name, closing) = html_tag_name(&lower[content_end + 1..close_end])?;
        ensure!(
            closing && close_name == "pre",
            "unsupported markup in runtime checksum rows"
        );
        let text = &html[cursor..content_end];
        ensure!(
            !text.contains(['>', '&']),
            "unsupported markup in runtime checksum rows"
        );
        blocks.push(text);
        cursor = close_end + 1;
    }
    ensure!(
        templates == 0,
        "unterminated template in runtime checksum page"
    );
    Ok(blocks)
}

fn pypy_checksum_rows(html: &str) -> Result<BTreeMap<String, Digest>> {
    let blocks = visible_preformatted_blocks(html)?;
    ensure!(
        !blocks.is_empty(),
        "official PyPy checksum page has no preformatted checksum rows"
    );
    let mut hashes = BTreeMap::new();
    for block in blocks {
        checksum_rows(block, &mut hashes)?;
    }
    Ok(hashes)
}

pub(super) fn parse_pypy_releases(
    raw: &Value,
    official_checksums_html: &str,
    target: &RuntimeArchiveTarget,
) -> Result<Vec<RuntimeRelease>> {
    let raw: Vec<PypyRelease> = serde_json::from_value(raw.clone())
        .map_err(|_| eyre::eyre!("invalid official PyPy release metadata"))?;
    let mut hashes = None;
    let mut releases: Vec<RuntimeRelease> = Vec::new();
    let mut seen = HashMap::<String, usize>::new();
    for release in raw {
        if release.pypy_version == "nightly" {
            continue;
        }
        let (major, minor, _) = version_parts(&release.python_version)?;
        if major != 3 {
            continue;
        }
        version_parts(&release.pypy_version)?;
        if let Some(date) = &release.date {
            date.parse::<jiff::civil::Date>()
                .map_err(|_| eyre::eyre!("invalid PyPy publication date"))?;
        }
        for file in release.files {
            let Some((file_target, suffixes)) = pypy_file_target(&file)? else {
                continue;
            };
            let prefix = format!("pypy{major}.{minor}-v{}-", release.pypy_version);
            ensure!(
                file.filename
                    .strip_prefix(&prefix)
                    .is_some_and(|suffix| suffixes.contains(&suffix)),
                "PyPy filename and release architecture or version disagree"
            );
            validate_locator(
                &file.filename,
                &file.download_url,
                RuntimeDistribution::PyPy,
                "",
            )?;
            if file_target != *target {
                continue;
            }
            if hashes.is_none() {
                hashes = Some(pypy_checksum_rows(official_checksums_html)?);
            }
            let digest = *hashes
                .as_ref()
                .and_then(|hashes| hashes.get(&file.filename))
                .context("PyPy archive has no expected SHA256 checksum")?;
            let key = RuntimeReleaseKey {
                distribution: RuntimeDistribution::PyPy,
                python_version: release.python_version.clone(),
                implementation_version: release.pypy_version.clone(),
                distribution_version: release.pypy_version.clone(),
                target: file_target,
                flavor: RuntimeFlavor::PyPy,
                filename: file.filename.clone(),
                digest,
            };
            if let Some(previous) = seen.get(&file.filename).map(|index| &releases[*index]) {
                ensure!(
                    previous.key == key
                        && previous.upstream_stable == release.stable
                        && previous.release_date == release.date,
                    "conflicting PyPy metadata for one archive"
                );
                continue;
            }
            seen.insert(file.filename.clone(), releases.len());
            let source = SourceArtifact::new(&file.filename, &file.download_url, digest)?;
            releases.push(RuntimeRelease {
                key,
                request_version: format!("pypy{major}.{minor}-{}", release.pypy_version),
                release_date: release.date.clone(),
                upstream_stable: release.stable,
                source,
            });
        }
    }
    Ok(releases)
}

pub(super) fn select_exact_release<'a>(
    releases: &'a [RuntimeRelease],
    key: &RuntimeReleaseKey,
) -> Result<&'a RuntimeRelease> {
    releases
        .iter()
        .find(|release| release.key == *key)
        .context("exact Python runtime artifact is unavailable")
}

#[cfg(test)]
mod tests;
