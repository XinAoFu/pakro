//! Exact Cargo inputs from one already extracted third-party source archive.

use super::*;
use crate::programs::OperationContext;
use pakro_index::artifact::{ArtifactTransport, NetworkPolicy, SourceArtifact};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Component;

const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
const CRATES_IO_SPARSE: &str = "registry+sparse+https://index.crates.io/";

#[derive(Debug, Clone, Serialize)]
struct CrateInput {
    name: String,
    version: String,
    sha256: [u8; 32],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    version: u32,
    #[serde(default)]
    package: Vec<LockedPackage>,
    #[serde(default)]
    patch: Unused,
    #[serde(default)]
    metadata: BTreeMap<String, toml::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(root: &Path, relative: &str, value: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        put(
            root.path(),
            "Cargo.toml",
            "[package]\nname='demo'\nversion='0.1.0'\n[dependencies]\ndep='1.0'\n",
        );
        put(
            root.path(),
            "Cargo.lock",
            &format!(
                "version=4\n[[package]]\nname='demo'\nversion='0.1.0'\ndependencies=['dep']\n[[package]]\nname='dep'\nversion='1.0.0'\nsource='{CRATES_IO}'\nchecksum='{}'\n",
                "11".repeat(32)
            ),
        );
        root
    }

    #[test]
    fn p6_cargo_inputs_keep_exact_lock_and_manifest_fingerprints_without_writes() {
        let root = fixture();
        let raw = fs::read(root.path().join("Cargo.lock")).unwrap();
        let inputs = CargoBuildInputs::parse(root.path(), Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(inputs.cargo_root, root.path().canonicalize().unwrap());
        assert_eq!(inputs.manifest_relative(), Path::new("Cargo.toml"));
        assert_eq!(inputs.lock_digest, <[u8; 32]>::from(Sha256::digest(&raw)));
        assert_eq!(inputs.crates.len(), 1);
        assert_eq!(inputs.crates[0].sha256, [0x11; 32]);
        let restored = CargoBuildInputs::parse(root.path(), Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(restored.inputs_digest, inputs.inputs_digest);
        assert_eq!(fs::read(root.path().join("Cargo.lock")).unwrap(), raw);
        let changed = String::from_utf8(raw.clone())
            .unwrap()
            .replace(&"11".repeat(32), &"12".repeat(32));
        put(root.path(), "Cargo.lock", &changed);
        let changed = CargoBuildInputs::parse(root.path(), Default::default())
            .unwrap()
            .unwrap();
        assert_ne!(changed.lock_digest, inputs.lock_digest);
        assert_ne!(changed.inputs_digest, inputs.inputs_digest);
    }

    #[test]
    fn p6_cargo_inputs_support_maturin_workspace_and_contained_parent_path_members() {
        let root = tempfile::tempdir().unwrap();
        put(
            root.path(),
            "pyproject.toml",
            "[tool.maturin]\nmanifest-path='rust/Cargo.toml'\n",
        );
        put(
            root.path(),
            "Cargo.toml",
            "[workspace]\nmembers=['rust','core','excluded/*']\nexclude=['excluded/unused']\n[workspace.package]\nversion='0.1.0'\n",
        );
        put(
            root.path(),
            "rust/Cargo.toml",
            "[package]\nname='demo'\nversion.workspace=true\n[dependencies]\ncore={path='../core'}\n",
        );
        put(
            root.path(),
            "core/Cargo.toml",
            "[package]\nname='core'\nversion='0.1.0'\n",
        );
        put(
            root.path(),
            "excluded/unused/Cargo.toml",
            "[package]\nname='unused'\nversion='0.1.0'\n[dependencies]\nunsupported={git='https://example.invalid/repo'}\n",
        );
        put(
            root.path(),
            "Cargo.lock",
            "version=3\n[[package]]\nname='demo'\nversion='0.1.0'\ndependencies=['core']\n[[package]]\nname='core'\nversion='0.1.0'\n",
        );
        let inputs = CargoBuildInputs::parse(root.path(), Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(inputs.manifest_relative(), Path::new("rust/Cargo.toml"));
        assert_eq!(inputs.cargo_root, root.path().canonicalize().unwrap());
        assert!(inputs.crates.is_empty());
        assert!(!root.path().join("vendor").exists());
        put(
            root.path(),
            "pyproject.toml",
            "[[tool.setuptools-rust.ext-modules]]\ntarget='demo'\npath='rust/Cargo.toml'\n",
        );
        let setuptools = CargoBuildInputs::parse(root.path(), Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(setuptools.manifest_relative(), Path::new("rust/Cargo.toml"));
        assert_eq!(setuptools.lock_digest, inputs.lock_digest);
    }

    #[test]
    fn p6_cargo_inputs_reject_missing_lock_git_external_paths_config_and_open_graphs() {
        for case in [
            "missing",
            "git-manifest",
            "external",
            "config",
            "checksum",
            "git-lock",
            "open",
        ] {
            let root = fixture();
            match case {
                "missing" => fs::remove_file(root.path().join("Cargo.lock")).unwrap(),
                "git-manifest" => put(
                    root.path(),
                    "Cargo.toml",
                    "[package]\nname='demo'\nversion='0.1.0'\n[dependencies]\ndep={git='https://example.invalid/repo'}\n",
                ),
                "external" => put(
                    root.path(),
                    "Cargo.toml",
                    "[package]\nname='demo'\nversion='0.1.0'\n[dependencies]\ndep={path='../outside'}\n",
                ),
                "config" => put(root.path(), ".cargo/config.toml", "[net]\noffline=false\n"),
                _ => {
                    let path = root.path().join("Cargo.lock");
                    let raw = fs::read_to_string(&path).unwrap();
                    let changed = match case {
                        "checksum" => raw.replace(&format!("checksum='{}'\n", "11".repeat(32)), ""),
                        "git-lock" => {
                            raw.replace(CRATES_IO, "git+https://example.invalid/repo#deadbeef")
                        }
                        _ => raw.replace("dependencies=['dep']", "dependencies=['absent']"),
                    };
                    fs::write(path, changed).unwrap();
                }
            }
            assert!(
                CargoBuildInputs::parse(root.path(), Default::default()).is_err(),
                "{case}"
            );
        }
        let root = tempfile::tempdir().unwrap();
        assert!(
            CargoBuildInputs::parse(root.path(), Default::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn p6_cargo_crate_keeps_its_filename_and_only_uses_raw_gzip_entry() {
        let root = tempfile::tempdir().unwrap();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        let body = b"[package]\nname='dep'\nversion='1.0.0'\n";
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(body.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, "dep-1.0.0/Cargo.toml", body.as_slice())
            .unwrap();
        let bytes = tar.into_inner().unwrap().finish().unwrap();
        let mut source = spool_verified_reader(
            bytes.as_slice(),
            "dep-1.0.0.crate",
            pakro_store::Digest::from_sha256_bytes(&bytes),
            1024 * 1024,
            root.path(),
        )
        .unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        let tree =
            extract_raw_source(&mut source, root.path(), deadline, Default::default()).unwrap();
        assert_eq!(source.filename(), "dep-1.0.0.crate");
        assert_eq!(tree.format(), SourceArchiveFormat::TarGz);
        assert_eq!(
            tree.read_relative(Path::new("Cargo.toml"), 1024).unwrap(),
            body
        );
        assert!(extract_source(&mut source, root.path(), deadline, Default::default()).is_err());
        let owned = tree.root().to_owned();
        drop(tree);
        assert!(!owned.exists());
    }
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Unused {
    #[serde(default)]
    unused: Vec<LockedPackage>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    replace: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CargoBuildInputs {
    pub cargo_root: PathBuf,
    pub manifest_path: PathBuf,
    pub lock_digest: [u8; 32],
    pub inputs_digest: [u8; 32],
    source_root: PathBuf,
    crates: Vec<CrateInput>,
    limits: SourceBuildLimits,
}

fn contained(root: &Path, base: &Path, relative: &Path) -> Result<PathBuf> {
    ensure!(
        !relative.is_absolute(),
        "Cargo path must stay inside the source archive"
    );
    let mut path = base.strip_prefix(root)?.to_owned();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => path.push(value),
            Component::ParentDir => ensure!(path.pop(), "Cargo path escapes the source archive"),
            _ => eyre::bail!("Cargo path escapes the source archive"),
        }
    }
    Ok(root.join(path))
}

fn read_manifest(
    root: &Path,
    path: &Path,
    limits: SourceBuildLimits,
) -> Result<(Vec<u8>, toml::Value)> {
    let path = path.canonicalize().context("Cargo input is missing")?;
    ensure!(
        path.starts_with(root),
        "Cargo input resolves outside its source archive"
    );
    let mut file = fs::File::open(&path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= limits.max_metadata_bytes,
        "Cargo metadata exceeds its byte budget"
    );
    let mut raw = Vec::new();
    Read::by_ref(&mut file)
        .take(limits.max_metadata_bytes + 1)
        .read_to_end(&mut raw)?;
    ensure!(
        raw.len() as u64 <= limits.max_metadata_bytes,
        "Cargo metadata exceeds its byte budget"
    );
    let value = toml::from_str(std::str::from_utf8(&raw).context("Cargo metadata is not UTF-8")?)
        .context("invalid Cargo TOML input")?;
    Ok((raw, value))
}

fn dependency_paths(
    root: &Path,
    base: &Path,
    table: &toml::Value,
    pending: &mut VecDeque<PathBuf>,
) -> Result<()> {
    let Some(dependencies) = table.as_table() else {
        eyre::bail!("invalid Cargo dependency table");
    };
    for value in dependencies.values() {
        let Some(fields) = value.as_table() else {
            ensure!(value.is_str(), "invalid Cargo dependency declaration");
            continue;
        };
        ensure!(
            !fields.contains_key("git"),
            "Git Cargo dependencies are unsupported"
        );
        if let Some(registry) = fields.get("registry") {
            ensure!(
                registry.as_str() == Some("crates-io"),
                "only crates.io Cargo dependencies are supported"
            );
        }
        if let Some(path) = fields.get("path") {
            let path = path.as_str().context("Cargo dependency path is not text")?;
            pending.push_back(contained(root, base, Path::new(path))?.join("Cargo.toml"));
        }
    }
    Ok(())
}

impl CargoBuildInputs {
    pub(crate) fn constrain_workspace(&mut self, bytes: u64, entries: usize) -> Result<()> {
        ensure!(
            bytes > 0 && entries > 0,
            "Cargo remaining workspace budget is empty"
        );
        self.limits.max_workspace_bytes = self.limits.max_workspace_bytes.min(bytes);
        self.limits.max_workspace_entries = self.limits.max_workspace_entries.min(entries);
        Ok(())
    }

    pub(crate) fn source_root(&self) -> &Path {
        &self.source_root
    }
    pub(crate) fn manifest_relative(&self) -> &Path {
        self.manifest_path
            .strip_prefix(&self.source_root)
            .expect("parsed Cargo manifest is contained")
    }

    pub(crate) fn parse(source_root: &Path, limits: SourceBuildLimits) -> Result<Option<Self>> {
        limits.validate()?;
        let root = source_root.canonicalize()?;
        let pyproject = root.join("pyproject.toml");
        let mut selected = root.join("Cargo.toml");
        let mut requested_manifests = Vec::new();
        if pyproject.exists() {
            let (_, value) = read_manifest(&root, &pyproject, limits)?;
            if let Some(path) = value
                .get("tool")
                .and_then(|v| v.get("maturin"))
                .and_then(|v| v.get("manifest-path"))
            {
                selected = contained(
                    &root,
                    &root,
                    Path::new(path.as_str().context("maturin manifest-path is not text")?),
                )?;
                ensure!(selected.is_file(), "maturin Cargo manifest is missing");
                requested_manifests.push(selected.clone());
            }
            if let Some(modules) = value
                .get("tool")
                .and_then(|v| v.get("setuptools-rust"))
                .and_then(|v| v.get("ext-modules"))
            {
                for module in modules
                    .as_array()
                    .context("setuptools-rust ext-modules must be an array")?
                {
                    ensure!(
                        module.is_table(),
                        "invalid setuptools-rust extension declaration"
                    );
                    let path = module
                        .get("path")
                        .map(|p| {
                            p.as_str()
                                .context("setuptools-rust manifest path is not text")
                        })
                        .transpose()?
                        .unwrap_or("Cargo.toml");
                    let path = contained(&root, &root, Path::new(path))?;
                    ensure!(path.is_file(), "setuptools-rust Cargo manifest is missing");
                    if !requested_manifests.contains(&path) {
                        requested_manifests.push(path);
                    }
                    ensure!(
                        requested_manifests.len() <= limits.max_requirements,
                        "Cargo manifest count exceeded"
                    );
                }
            }
        }
        if let Some(path) = requested_manifests.first() {
            selected = path.clone();
        }
        if !selected.exists() {
            return Ok(None);
        }
        selected = selected.canonicalize()?;
        ensure!(
            selected.starts_with(&root),
            "Cargo manifest is outside its source archive"
        );
        let mut manifests = BTreeMap::new();
        let mut hashes = BTreeMap::new();
        if requested_manifests.is_empty() {
            requested_manifests.push(selected.clone());
        }
        let mut pending = VecDeque::from(requested_manifests.clone());
        let mut cargo_root = selected
            .parent()
            .context("Cargo manifest has no parent")?
            .to_owned();
        let mut metadata_bytes = 0usize;
        // Cargo uses the enclosing workspace's one lock, including for maturin's sub-crate.
        for directory in selected
            .parent()
            .unwrap()
            .ancestors()
            .take_while(|p| p.starts_with(&root))
        {
            let path = directory.join("Cargo.toml");
            if !path.is_file() {
                continue;
            }
            let (raw, value) = read_manifest(&root, &path, limits)?;
            let workspace = value.get("workspace").is_some();
            let mut explicit_workspace = false;
            if path == selected {
                if let Some(declared) = value.get("package").and_then(|p| p.get("workspace")) {
                    cargo_root = contained(
                        &root,
                        directory,
                        Path::new(
                            declared
                                .as_str()
                                .context("Cargo workspace path is not text")?,
                        ),
                    )?;
                    pending.push_back(cargo_root.join("Cargo.toml"));
                    explicit_workspace = true;
                }
            }
            hashes.insert(
                path.strip_prefix(&root)?.to_string_lossy().into_owned(),
                <[u8; 32]>::from(Sha256::digest(&raw)),
            );
            metadata_bytes = metadata_bytes
                .checked_add(raw.len())
                .filter(|n| *n as u64 <= limits.max_workspace_bytes)
                .context("Cargo manifest byte budget exceeded")?;
            manifests.insert(path.clone(), value);
            pending.push_back(path);
            if explicit_workspace {
                break;
            }
            if workspace {
                cargo_root = directory.to_owned();
                break;
            }
        }
        let mut processed = BTreeSet::new();
        while let Some(path) = pending.pop_front() {
            let path = path
                .canonicalize()
                .context("Cargo workspace/path member is missing")?;
            ensure!(
                path.starts_with(&root),
                "Cargo member resolves outside its source archive"
            );
            if !processed.insert(path.clone()) {
                continue;
            }
            ensure!(
                processed.len() <= limits.max_requirements,
                "Cargo manifest count exceeded"
            );
            let value = if let Some(value) = manifests.get(&path) {
                value.clone()
            } else {
                let (raw, value) = read_manifest(&root, &path, limits)?;
                metadata_bytes = metadata_bytes
                    .checked_add(raw.len())
                    .filter(|n| *n as u64 <= limits.max_workspace_bytes)
                    .context("Cargo manifest byte budget exceeded")?;
                hashes.insert(
                    path.strip_prefix(&root)?.to_string_lossy().into_owned(),
                    Sha256::digest(&raw).into(),
                );
                manifests.insert(path.clone(), value.clone());
                value
            };
            let base = path.parent().unwrap();
            for directory in base.ancestors() {
                ensure!(
                    !directory.join(".cargo/config").exists()
                        && !directory.join(".cargo/config.toml").exists(),
                    "source Cargo configuration is unsupported"
                );
            }
            if let Some(workspace) = value.get("package").and_then(|p| p.get("workspace")) {
                pending.push_back(
                    contained(
                        &root,
                        base,
                        Path::new(
                            workspace
                                .as_str()
                                .context("Cargo workspace path is not text")?,
                        ),
                    )?
                    .join("Cargo.toml"),
                );
            }
            // A path dependency can inherit metadata from its own contained workspace.
            for directory in base
                .ancestors()
                .skip(1)
                .take_while(|p| p.starts_with(&root))
            {
                if value
                    .get("package")
                    .and_then(|p| p.get("workspace"))
                    .is_some()
                {
                    break;
                }
                let parent = directory.join("Cargo.toml");
                if !parent.is_file() {
                    continue;
                }
                if !manifests.contains_key(&parent) {
                    let (raw, value) = read_manifest(&root, &parent, limits)?;
                    metadata_bytes = metadata_bytes
                        .checked_add(raw.len())
                        .filter(|n| *n as u64 <= limits.max_workspace_bytes)
                        .context("Cargo manifest byte budget exceeded")?;
                    hashes.insert(
                        parent.strip_prefix(&root)?.to_string_lossy().into_owned(),
                        Sha256::digest(&raw).into(),
                    );
                    manifests.insert(parent.clone(), value);
                    ensure!(
                        manifests.len() <= limits.max_requirements,
                        "Cargo manifest count exceeded"
                    );
                }
                if manifests[&parent].get("workspace").is_some() {
                    pending.push_back(parent);
                    break;
                }
            }
            for name in ["dependencies", "build-dependencies", "dev-dependencies"] {
                if let Some(table) = value.get(name) {
                    dependency_paths(&root, base, table, &mut pending)?;
                }
                if let Some(targets) = value.get("target").and_then(toml::Value::as_table) {
                    for target in targets.values() {
                        if let Some(table) = target.get(name) {
                            dependency_paths(&root, base, table, &mut pending)?;
                        }
                    }
                }
            }
            if let Some(workspace) = value.get("workspace") {
                if let Some(table) = workspace.get("dependencies") {
                    dependency_paths(&root, base, table, &mut pending)?;
                }
                if let Some(members) = workspace.get("members") {
                    let members = members
                        .as_array()
                        .context("Cargo workspace members must be an array")?;
                    let mut excluded = Vec::new();
                    if let Some(excludes) = workspace.get("exclude") {
                        for pattern in excludes
                            .as_array()
                            .context("Cargo workspace exclude must be an array")?
                        {
                            let pattern = contained(
                                &root,
                                base,
                                Path::new(
                                    pattern
                                        .as_str()
                                        .context("Cargo workspace exclude is not text")?,
                                ),
                            )?;
                            excluded.push(glob::Pattern::new(
                                pattern.to_str().context("Cargo exclusion is not UTF-8")?,
                            )?);
                        }
                    }
                    for member in members {
                        let pattern = contained(
                            &root,
                            base,
                            Path::new(
                                member
                                    .as_str()
                                    .context("Cargo workspace member is not text")?,
                            ),
                        )?
                        .join("Cargo.toml");
                        let mut found = false;
                        for path in
                            glob::glob(pattern.to_str().context("Cargo member path is not UTF-8")?)?
                        {
                            let path = path?;
                            found = true;
                            if excluded
                                .iter()
                                .any(|pattern| pattern.matches_path(path.parent().unwrap()))
                            {
                                continue;
                            }
                            pending.push_back(path);
                            ensure!(
                                pending.len() <= limits.max_requirements,
                                "Cargo member count exceeded"
                            );
                        }
                        ensure!(found, "Cargo workspace member is missing");
                    }
                }
            }
            if let Some(patches) = value.get("patch").and_then(toml::Value::as_table) {
                for (source, table) in patches {
                    ensure!(
                        matches!(
                            source.as_str(),
                            "crates-io" | "https://github.com/rust-lang/crates.io-index"
                        ),
                        "non-crates.io Cargo patch is unsupported"
                    );
                    dependency_paths(&root, base, table, &mut pending)?;
                }
            }
            if let Some(table) = value.get("replace") {
                dependency_paths(&root, base, table, &mut pending)?;
            }
        }
        ensure!(
            manifests.contains_key(&cargo_root.join("Cargo.toml")),
            "Cargo workspace manifest is missing"
        );
        for path in &requested_manifests {
            let path = path.canonicalize()?;
            let manifest = manifests
                .get(&path)
                .context("declared Rust manifest was not parsed")?;
            let declared = manifest
                .get("package")
                .and_then(|p| p.get("workspace"))
                .and_then(toml::Value::as_str);
            let workspace = if let Some(declared) = declared {
                contained(&root, path.parent().unwrap(), Path::new(declared))?
            } else {
                path.parent()
                    .unwrap()
                    .ancestors()
                    .take_while(|p| p.starts_with(&root))
                    .find(|p| {
                        manifests
                            .get(&p.join("Cargo.toml"))
                            .is_some_and(|value| value.get("workspace").is_some())
                    })
                    .unwrap_or(path.parent().unwrap())
                    .to_owned()
            };
            ensure!(
                workspace == cargo_root,
                "multiple Rust extension manifests require the same contained workspace and Cargo.lock"
            );
        }
        let mut locals = BTreeSet::new();
        for (path, value) in &manifests {
            let Some(package) = value.get("package") else {
                continue;
            };
            let name = package
                .get("name")
                .and_then(toml::Value::as_str)
                .context("Cargo package name is missing")?;
            let version = match package.get("version") {
                Some(value) if value.as_str().is_some() => value.as_str().unwrap(),
                Some(value)
                    if value.get("workspace").and_then(toml::Value::as_bool) == Some(true) =>
                {
                    let declared = package
                        .get("workspace")
                        .and_then(toml::Value::as_str)
                        .map(|relative| {
                            contained(&root, path.parent().unwrap(), Path::new(relative))
                        })
                        .transpose()?;
                    let workspace_manifest = if let Some(declared) = declared {
                        manifests.get(&declared.join("Cargo.toml"))
                    } else {
                        path.parent()
                            .unwrap()
                            .ancestors()
                            .take_while(|p| p.starts_with(&root))
                            .find_map(|p| {
                                manifests
                                    .get(&p.join("Cargo.toml"))
                                    .filter(|v| v.get("workspace").is_some())
                            })
                    }
                    .context("inherited Cargo workspace is missing")?;
                    workspace_manifest
                        .get("workspace")
                        .and_then(|w| w.get("package"))
                        .and_then(|p| p.get("version"))
                        .and_then(toml::Value::as_str)
                        .context("inherited Cargo package version is missing")?
                }
                None => "0.0.0",
                _ => eyre::bail!("invalid Cargo package version"),
            };
            locals.insert((name.to_owned(), version.to_owned()));
        }
        ensure!(!locals.is_empty(), "Cargo workspace contains no packages");
        let lock_path = cargo_root.join("Cargo.lock");
        let (raw_lock, lock_value) = read_manifest(&root, &lock_path, limits)
            .context("a complete Cargo.lock is required for native Rust builds")?;
        let lock: Lock = lock_value.try_into()?;
        ensure!(
            matches!(lock.version, 3 | 4) && lock.metadata.is_empty(),
            "unsupported Cargo.lock format"
        );
        ensure!(
            !lock.package.is_empty() && lock.package.len() <= limits.max_requirements,
            "Cargo.lock package count exceeded"
        );
        let mut identities = BTreeSet::new();
        let mut crates = Vec::new();
        for package in &lock.package {
            ensure!(
                !package.name.is_empty()
                    && package.name.len() <= 128
                    && package
                        .name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
                "invalid Cargo package name"
            );
            ensure!(
                package.version.len() <= 128
                    && semver::Version::parse(&package.version)?.to_string() == package.version,
                "invalid exact Cargo version"
            );
            ensure!(
                identities.insert((&package.name, &package.version, &package.source)),
                "duplicate Cargo.lock package"
            );
            match package.source.as_deref() {
                Some(CRATES_IO | CRATES_IO_SPARSE) => {
                    let checksum = package
                        .checksum
                        .as_deref()
                        .context("registry crate has no fixed checksum")?;
                    let mut sha256 = [0; 32];
                    hex::decode_to_slice(checksum, &mut sha256)
                        .context("invalid Cargo crate checksum")?;
                    ensure!(
                        hex::encode(sha256) == checksum && sha256 != [0; 32],
                        "Cargo checksum is not canonical SHA256"
                    );
                    crates.push(CrateInput {
                        name: package.name.clone(),
                        version: package.version.clone(),
                        sha256,
                    });
                }
                None => ensure!(
                    package.checksum.is_none()
                        && locals.contains(&(package.name.clone(), package.version.clone())),
                    "Cargo.lock local package has no archive-contained manifest"
                ),
                _ => eyre::bail!("only fixed standard crates.io Cargo sources are supported"),
            }
            ensure!(
                package.dependencies.len() <= limits.max_requirements,
                "Cargo dependency count exceeded"
            );
            for dependency in package.dependencies.iter().chain(package.replace.iter()) {
                let (identity, source) =
                    if let Some((identity, source)) = dependency.split_once(" (") {
                        (
                            identity,
                            Some(
                                source
                                    .strip_suffix(')')
                                    .context("invalid Cargo dependency source reference")?,
                            ),
                        )
                    } else {
                        (dependency.as_str(), None)
                    };
                let mut words = identity.split_ascii_whitespace();
                let name = words.next().context("empty Cargo dependency reference")?;
                let version = words.next();
                ensure!(
                    words.next().is_none() && dependency.len() <= 4096,
                    "invalid Cargo dependency reference"
                );
                ensure!(
                    lock.package
                        .iter()
                        .filter(|p| p.name == name
                            && version.is_none_or(|v| p.version == v)
                            && source.is_none_or(|s| p.source.as_deref() == Some(s)))
                        .count()
                        == 1,
                    "Cargo.lock dependency is missing or ambiguous"
                );
            }
        }
        ensure!(
            lock.patch.unused.len() <= limits.max_requirements,
            "unused Cargo package count exceeded"
        );
        for unused in &lock.patch.unused {
            ensure!(
                unused
                    .source
                    .as_deref()
                    .is_none_or(|s| matches!(s, CRATES_IO | CRATES_IO_SPARSE)),
                "unsupported unused Cargo source"
            );
        }
        crates.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
        ensure!(
            crates
                .windows(2)
                .all(|p| (&p[0].name, &p[0].version) != (&p[1].name, &p[1].version)),
            "Cargo source identities collide in vendor directory"
        );
        let lock_digest: [u8; 32] = Sha256::digest(&raw_lock).into();
        let mut digest = Sha256::new();
        digest.update(b"pakro.cargo-inputs/v1\0");
        digest.update(lock_digest);
        digest.update(serde_json::to_vec(&(
            selected
                .strip_prefix(&root)?
                .to_str()
                .context("Cargo manifest path is not UTF-8")?,
            cargo_root
                .strip_prefix(&root)?
                .to_str()
                .context("Cargo root is not UTF-8")?,
            hashes,
            &crates,
        ))?);
        Ok(Some(Self {
            cargo_root,
            manifest_path: selected,
            lock_digest,
            inputs_digest: digest.finalize().into(),
            source_root: root,
            crates,
            limits,
        }))
    }

    pub(crate) async fn prepare(
        &self,
        context: &OperationContext,
        transport: &ArtifactTransport,
        vendor_root: &Path,
        cargo_home: &Path,
        scratch: &Path,
    ) -> Result<()> {
        context.checkpoint()?;
        ensure!(
            vendor_root != cargo_home
                && !vendor_root.starts_with(cargo_home)
                && !cargo_home.starts_with(vendor_root),
            "Cargo vendor and home directories overlap"
        );
        for path in [vendor_root, cargo_home] {
            ensure!(
                path.is_absolute()
                    && path != self.source_root.as_path()
                    && path != self.cargo_root.as_path(),
                "Cargo preparation needs private output directories"
            );
            fs::create_dir_all(path)?;
            ensure!(
                fs::symlink_metadata(path)?.is_dir() && fs::read_dir(path)?.next().is_none(),
                "Cargo preparation directory must be empty"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            }
        }
        fs::create_dir_all(scratch)?;
        let cancellation = context.cancellation_token();
        let deadline = context.policy().deadline;
        let network = if context.policy().offline {
            NetworkPolicy::Offline
        } else {
            NetworkPolicy::Allowed
        };
        let mut entries = 0usize;
        let mut total_bytes = 0u64;
        let mut archive_bytes = 0u64;
        for input in &self.crates {
            context.checkpoint()?;
            let filename = format!("{}-{}.crate", input.name, input.version);
            let source = SourceArtifact::new(
                &filename,
                &format!("https://static.crates.io/crates/{}/{filename}", input.name),
                pakro_store::Digest::Sha256(input.sha256),
            )?;
            let mut artifact = tokio::select! {
                result = crate::artifact_snapshot::acquire_verified_artifact(
                    &source, context.requested_cache_root(), transport, network, self.limits.max_archive_bytes,
                ) => result?,
                _ = cancellation.cancelled() => eyre::bail!("Cargo input acquisition cancelled"),
                _ = tokio::time::sleep_until(deadline.into()) => eyre::bail!("Cargo input acquisition deadline expired"),
            };
            context.checkpoint()?;
            archive_bytes = archive_bytes
                .checked_add(artifact.size())
                .filter(|n| *n <= self.limits.max_workspace_bytes)
                .context("Cargo archive total byte budget exceeded")?;
            let tree = extract_raw_source(&mut artifact, scratch, deadline, self.limits)?;
            let expected_root = format!("{}-{}", input.name, input.version);
            ensure!(
                tree.root().file_name().and_then(|v| v.to_str()) == Some(expected_root.as_str()),
                "crate archive bundle differs from Cargo.lock"
            );
            entries = entries
                .checked_add(tree.extracted_entries() + 1)
                .filter(|n| *n <= self.limits.max_workspace_entries)
                .context("Cargo vendor entry budget exceeded")?;
            let manifest = tree.read_relative(
                Path::new("Cargo.toml"),
                self.limits
                    .max_metadata_bytes
                    .min(self.limits.max_file_bytes),
            )?;
            let manifest: toml::Value = toml::from_str(std::str::from_utf8(&manifest)?)?;
            ensure!(
                manifest
                    .get("package")
                    .and_then(|p| p.get("name"))
                    .and_then(toml::Value::as_str)
                    == Some(input.name.as_str())
                    && manifest
                        .get("package")
                        .and_then(|p| p.get("version"))
                        .and_then(toml::Value::as_str)
                        == Some(input.version.as_str()),
                "crate package identity differs from Cargo.lock"
            );
            let destination = vendor_root.join(&expected_root);
            fs::create_dir(&destination)?;
            let mut files = BTreeMap::new();
            for entry in walkdir::WalkDir::new(tree.root()).follow_links(false) {
                context.checkpoint()?;
                let entry = entry?;
                let relative = entry.path().strip_prefix(tree.root())?;
                if relative.as_os_str().is_empty() || relative == Path::new(".cargo-checksum.json")
                {
                    continue;
                }
                let target = destination.join(relative);
                if entry.file_type().is_dir() {
                    fs::create_dir(&target)?;
                    continue;
                }
                ensure!(
                    entry.file_type().is_file(),
                    "crate vendor member is not a regular file"
                );
                let mut file = fs::File::open(entry.path())?;
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                    options.mode(if file.metadata()?.permissions().mode() & 0o111 != 0 {
                        0o700
                    } else {
                        0o600
                    });
                }
                let mut output = options.open(&target)?;
                let mut hash = Sha256::new();
                let mut copied = 0u64;
                let mut buffer = [0u8; 32768];
                loop {
                    context.checkpoint()?;
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    copied = copied
                        .checked_add(count as u64)
                        .filter(|n| *n <= self.limits.max_file_bytes)
                        .context("crate file byte budget exceeded")?;
                    total_bytes = total_bytes
                        .checked_add(count as u64)
                        .filter(|n| *n <= self.limits.max_workspace_bytes)
                        .context("Cargo vendor byte budget exceeded")?;
                    hash.update(&buffer[..count]);
                    output.write_all(&buffer[..count])?;
                }
                files.insert(
                    relative
                        .to_str()
                        .context("crate path is not UTF-8")?
                        .replace('\\', "/"),
                    hex::encode(hash.finalize()),
                );
            }
            let checksum = serde_json::to_vec(
                &serde_json::json!({"files": files, "package": hex::encode(input.sha256)}),
            )?;
            ensure!(
                checksum.len() as u64 <= self.limits.max_metadata_bytes,
                "Cargo checksum manifest exceeds its byte budget"
            );
            total_bytes = total_bytes
                .checked_add(checksum.len() as u64)
                .filter(|n| *n <= self.limits.max_workspace_bytes)
                .context("Cargo vendor byte budget exceeded")?;
            fs::write(destination.join(".cargo-checksum.json"), checksum)?;
        }
        context.checkpoint()?;
        let vendor = vendor_root.canonicalize()?;
        let config = format!(
            "[net]\noffline = true\n[source.crates-io]\nreplace-with = \"pakro-vendored\"\n[source.pakro-vendored]\ndirectory = {}\n",
            serde_json::to_string(vendor.to_str().context("Cargo vendor path is not UTF-8")?)?
        );
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(cargo_home.join("config.toml"))?;
        output.write_all(config.as_bytes())?;
        context.checkpoint()?;
        Ok(())
    }
}
