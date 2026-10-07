//! Go and Maven: "storage-layer absorption" — go.mod and pom.xml stay the
//! package declarations; mise takes over where artifacts physically live
//! (GOMODCACHE / maven.repo.local) and orchestrates the download.

use std::path::Path;

use eyre::{Context, ContextCompat, bail};

use crate::packages::install::Locations;

pub(crate) fn install_go_modules(project_root: &Path, locations: &Locations) -> eyre::Result<bool> {
    let go_mod = project_root.join("go.mod");
    if !go_mod.exists() {
        return Ok(false);
    }
    let go_bin = find_go_binary()?;
    let modcache = locations.store_root.join("go/mod");
    std::fs::create_dir_all(&modcache)?;

    // go.sum must exist before download can verify; tidy generates it
    let tidy = std::process::Command::new(&go_bin)
        .arg("mod")
        .arg("tidy")
        .current_dir(project_root)
        .env("GOMODCACHE", &modcache)
        .env("GOFLAGS", "-mod=mod")
        .status()
        .with_context(|| format!("running {} mod tidy", go_bin.display()))?;
    if !tidy.success() {
        bail!("go mod tidy failed with {tidy}");
    }
    let status = std::process::Command::new(&go_bin)
        .arg("mod")
        .arg("download")
        .arg("all")
        .current_dir(project_root)
        .env("GOMODCACHE", &modcache)
        .env("GOFLAGS", "-mod=mod")
        .status()
        .with_context(|| format!("running {} mod download", go_bin.display()))?;
    if !status.success() {
        bail!("go mod download failed with {status}");
    }
    // go manages its own cache layout; record the project's usage so
    // `mise store ls` can report it
    let marker = locations.store_root.join("go/refs");
    std::fs::create_dir_all(&marker)?;
    let marker = marker.join(project_root.to_string_lossy().replace('/', "_"));
    std::fs::write(&marker, b"")?;
    Ok(true)
}

fn find_go_binary() -> eyre::Result<std::path::PathBuf> {
    // the go tool mise installed for this project's toolset
    let installed = crate::dirs::INSTALLS
        .join("go")
        .read_dir()
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("bin/go").exists())
                .max_by_key(|e| e.file_name().to_string_lossy().to_string())
                .map(|e| e.path().join("bin/go"))
        });
    if let Some(go) = installed {
        return Ok(go);
    }
    // fall back to whatever go is on PATH
    let go = which_go()?;
    Ok(go)
}

fn which_go() -> eyre::Result<std::path::PathBuf> {
    let path = std::env::var_os("PATH").context("PATH not set")?;
    for dir in path.to_string_lossy().split(':') {
        let candidate = std::path::Path::new(dir).join("go");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    eyre::bail!("no go binary found in PATH or mise installs")
}

pub(crate) fn install_maven_dependencies(
    project_root: &Path,
    locations: &Locations,
) -> eyre::Result<bool> {
    let pom = project_root.join("pom.xml");
    if !pom.exists() {
        return Ok(false);
    }
    let mvn = find_maven()?;
    let repo = locations.store_root.join("maven");
    std::fs::create_dir_all(&repo)?;

    let status = std::process::Command::new(&mvn)
        .arg("dependency:go-offline")
        .arg(format!("-Dmaven.repo.local={}", repo.display()))
        .arg("-B")
        .arg("-q")
        .current_dir(project_root)
        .status()
        .with_context(|| format!("running {} dependency:go-offline", mvn.display()))?;
    if !status.success() {
        bail!("mvn dependency:go-offline failed with {status}");
    }
    let marker = locations.store_root.join("maven/refs");
    std::fs::create_dir_all(&marker)?;
    let marker = marker.join(project_root.to_string_lossy().replace('/', "_"));
    std::fs::write(&marker, b"")?;
    Ok(true)
}

fn find_maven() -> eyre::Result<std::path::PathBuf> {
    let path = std::env::var_os("PATH").context("PATH not set")?;
    for dir in path.to_string_lossy().split(':') {
        let candidate = std::path::Path::new(dir).join("mvn");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    // mise-installed maven via java backend sits in installs/java/*/bin
    let installed = crate::dirs::INSTALLS
        .join("java")
        .read_dir()
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("bin/mvn").exists())
                .max_by_key(|e| e.file_name().to_string_lossy().to_string())
                .map(|e| e.path().join("bin/mvn"))
        });
    installed.ok_or_else(|| eyre::eyre!("no mvn binary found in PATH or mise installs"))
}
