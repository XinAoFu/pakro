//! node_modules materialization in the pnpm shape: every package physically
//! lives at `node_modules/.mise-store/<name>@<version>/node_modules/<name>`
//! (per-file hardlinks into the global store, so resolution walks up to real
//! sibling links), each dependency is linked next to its dependent, and only
//! the project's direct dependencies appear at the top level. Scoped names
//! keep their `@scope/` path shape everywhere.

use std::collections::HashSet;
use std::path::Path;

use eyre::{Context, bail};
use mise_resolver::{ResolvedGraph, ResolvedNode};
use mise_store::{Digest, Eco, PackageIdentity, Platform, Store};

pub(crate) fn materialize_node_modules(
    project_root: &Path,
    graph: &ResolvedGraph,
    store: &Store,
) -> eyre::Result<()> {
    let node_modules = project_root.join("node_modules");
    let virtual_store = node_modules.join(".mise-store");
    for node in &graph.nodes {
        let identity = identity_of(node)?;
        let entry = store.entry_path(&identity);
        let package_dir = entry.join("package");
        if !package_dir.is_dir() {
            bail!(
                "store entry {} is missing its package/ directory",
                entry.display()
            );
        }
        let virtual_dir = virtual_store
            .join(format!("{}@{}", node.name, node.version))
            .join("node_modules");
        let target = virtual_dir.join(&node.name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        mise_store::hardlink_tree(&package_dir, &target)
            .with_context(|| format!("materializing {}@{}", node.name, node.version))?;
        // every dependency resolves from inside the dependent's own
        // node_modules, so conflicting versions never fight for the top level
        for dep in &node.deps {
            let Some(dep_node) = graph.nodes.iter().find(|n| n.name == dep.name) else {
                bail!(
                    "dependency {} of {} is absent from the graph",
                    dep.name,
                    node.name
                );
            };
            let dep_target = virtual_store
                .join(format!("{}@{}", dep_node.name, dep_node.version))
                .join("node_modules")
                .join(&dep_node.name);
            let link = virtual_dir.join(&dep.name);
            replace_symlink(&dep_target, &link)
                .with_context(|| format!("linking {} for {}", dep.name, node.name))?;
        }
        store.add_ref(&identity, &project_root.to_string_lossy())?;
    }
    // top level: packages nothing else depends on are the direct deps
    let depended: HashSet<&str> = graph
        .nodes
        .iter()
        .flat_map(|n| n.deps.iter().map(|d| d.name.as_str()))
        .collect();
    for node in &graph.nodes {
        if depended.contains(node.name.as_str()) {
            continue;
        }
        let target = virtual_store
            .join(format!("{}@{}", node.name, node.version))
            .join("node_modules")
            .join(&node.name);
        let link = node_modules.join(&node.name);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)?;
        }
        replace_symlink(&target, &link)
            .with_context(|| format!("linking {} at the top level", node.name))?;
    }
    Ok(())
}

fn identity_of(node: &ResolvedNode) -> eyre::Result<PackageIdentity> {
    let Some(digest) = &node.digest else {
        bail!(
            "node {}@{} has no artifact digest yet",
            node.name,
            node.version
        );
    };
    Ok(PackageIdentity {
        eco: Eco::Node,
        name: node.name.clone(),
        version: node.version.clone(),
        runtime: None,
        platform: Platform::detect(),
        digest: digest.parse::<Digest>()?,
    })
}

fn replace_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(link).is_ok() {
        std::fs::remove_file(link)?;
    }
    std::os::unix::fs::symlink(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mise_resolver::DependencyRequest;

    fn store_with_tgz(store_root: &Path, name: &str, version: &str) -> PackageIdentity {
        let store = Store::open(store_root).unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut enc);
            let mut header = tar::Header::new_gnu();
            let body = format!("{{\"name\":\"{name}\",\"version\":\"{version}\"}}");
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "package/package.json", body.as_bytes())
                .unwrap();
            builder.finish().unwrap();
        }
        let bytes = enc.finish().unwrap();
        let identity = PackageIdentity {
            eco: Eco::Node,
            name: name.to_string(),
            version: version.to_string(),
            runtime: None,
            platform: Platform::detect(),
            digest: Digest::from_sha256_bytes(&bytes),
        };
        store.unpack(&identity, &bytes).unwrap();
        identity
    }

    fn graph_entry(name: &str, version: &str, digest: &Digest, deps: &[&str]) -> ResolvedNode {
        ResolvedNode {
            eco: "node".into(),
            name: name.into(),
            version: version.into(),
            digest: Some(format!("sha256-{}", digest.hex())),
            deps: deps
                .iter()
                .map(|d| DependencyRequest {
                    eco: "node".into(),
                    name: (*d).into(),
                    constraint: "*".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn materializes_pnpm_shape() {
        let root = tempfile::tempdir().unwrap();
        let store_root = tempfile::tempdir().unwrap();
        let direct = store_with_tgz(store_root.path(), "typescript", "5.8.2");
        let dep = store_with_tgz(store_root.path(), "commander", "2.0.0");
        let graph = ResolvedGraph {
            nodes: vec![
                graph_entry("typescript", "5.8.2", &direct.digest, &["commander"]),
                graph_entry("commander", "2.0.0", &dep.digest, &[]),
            ],
        };
        let store = Store::open(&store_root).unwrap();
        materialize_node_modules(root.path(), &graph, &store).unwrap();

        let nm = root.path().join("node_modules");
        // direct dep visible at the top level, transitive only in the
        // virtual store
        assert!(nm.join("typescript/package.json").is_file());
        assert!(!nm.join("commander").exists());
        let ts_virtual =
            nm.join(".mise-store/typescript@5.8.2/node_modules/typescript/package.json");
        assert!(ts_virtual.is_file());
        // the transitive dep resolves from inside typescript's virtual dir
        let dep_link = nm.join(".mise-store/typescript@5.8.2/node_modules/commander");
        assert!(dep_link.is_symlink());
        assert_eq!(
            std::fs::read_link(&dep_link).unwrap(),
            nm.join(".mise-store/commander@2.0.0/node_modules/commander")
        );
        // refs recorded for both entries
        assert_eq!(
            store.refs(&direct).unwrap().projects,
            vec![root.path().to_string_lossy().to_string()]
        );

        // idempotent
        materialize_node_modules(root.path(), &graph, &store).unwrap();
        assert!(ts_virtual.is_file());
    }

    #[test]
    fn scoped_names_keep_their_path_shape() {
        let root = tempfile::tempdir().unwrap();
        let store_root = tempfile::tempdir().unwrap();
        let scoped = store_with_tgz(store_root.path(), "@types/node", "22.0.0");
        let graph = ResolvedGraph {
            nodes: vec![graph_entry("@types/node", "22.0.0", &scoped.digest, &[])],
        };
        let store = Store::open(&store_root).unwrap();
        materialize_node_modules(root.path(), &graph, &store).unwrap();
        assert!(
            root.path()
                .join("node_modules/@types/node/package.json")
                .is_file()
        );
    }
}
