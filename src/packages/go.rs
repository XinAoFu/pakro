//! Go module downloads through the mise store: GOMODCACHE is pointed at
//! `<store>/go/mod` (go's own content-addressed layout), `go mod download`
//! populates it, and the project is registered as a referrer. go.mod and
//! go.sum remain the package declaration and lockfile — go's design is
//! already sound; mise takes over where those modules physically live.

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
