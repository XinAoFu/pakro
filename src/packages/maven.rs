//! Maven dependency downloads through the mise store: the local repository
//! is pointed at `<store>/maven` via `-Dmaven.repo.local`, `mvn
//! dependency:go-offline` populates it, and the project is registered as a
//! referrer. pom.xml remains the package declaration; the store holds the
//! artifacts so projects share them.

use std::path::Path;

use eyre::{Context, ContextCompat, bail};

use crate::packages::install::Locations;

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
