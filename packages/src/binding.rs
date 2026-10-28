//! Merge package declarations while retaining each winning declaration's owner.

use indexmap::IndexMap;
use std::collections::BTreeSet;

use crate::toolset::ToolSource;

use super::PackageInstallRequest;

#[derive(Debug, Clone)]
pub(crate) struct OwnedPackageRequest {
    pub request: PackageInstallRequest,
    pub source: ToolSource,
}

pub(crate) fn merge_owned_requests(
    sources: impl IntoIterator<Item = (ToolSource, Vec<PackageInstallRequest>)>,
    extra: &[PackageInstallRequest],
    cli: &[PackageInstallRequest],
) -> Result<Vec<OwnedPackageRequest>, String> {
    let mut merged = IndexMap::new();
    let mut names_by_source: Vec<(ToolSource, BTreeSet<(String, String)>)> = Vec::new();
    for (source, requests) in sources.into_iter().chain([
        (ToolSource::Argument, extra.to_vec()),
        (ToolSource::Argument, cli.to_vec()),
    ]) {
        for request in requests {
            request.validate()?;
            let name = pakro_index::python_source::NormalizedPythonName::new(&request.name)
                .map_err(|error| error.to_string())?;
            let key = (request.group_key(), name.as_str().to_owned());
            let source_index = match names_by_source
                .iter()
                .position(|(owner, _)| owner == &source)
            {
                Some(index) => index,
                None => {
                    names_by_source.push((source.clone(), BTreeSet::new()));
                    names_by_source.len() - 1
                }
            };
            if !names_by_source[source_index].1.insert(key.clone()) {
                return Err(format!(
                    "conflicting normalized package name {:?} in one declaration source",
                    request.name
                ));
            }
            merged.insert(
                key,
                OwnedPackageRequest {
                    request,
                    source: source.clone(),
                },
            );
        }
    }
    Ok(merged.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(version: &str) -> PackageInstallRequest {
        PackageInstallRequest::pypi("python", "3.10.14", "demo", version)
    }

    #[test]
    fn winning_package_declaration_keeps_its_owner() {
        let global = ToolSource::PakroToml("/global/config.toml".into());
        let project = ToolSource::PakroToml("/project/pakro.toml".into());
        let owned = merge_owned_requests(
            [
                (global, vec![request("1.0")]),
                (project.clone(), vec![request("2.0")]),
            ],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].request.version, "2.0");
        assert_eq!(owned[0].source, project);
        let extra = merge_owned_requests([(project, vec![request("2.0")])], &[request("3.0")], &[])
            .unwrap();
        assert_eq!(extra[0].request.version, "3.0");
        assert_eq!(extra[0].source, ToolSource::Argument);
    }

    #[test]
    fn normalized_collisions_in_one_source_are_rejected() {
        let source = ToolSource::PakroToml("/project/pakro.toml".into());
        let first = PackageInstallRequest::pypi("python", "3.13.16", "Demo_Pkg", "1.0");
        let second = PackageInstallRequest::pypi("python", "3.13.16", "demo-pkg", "1.0");
        assert!(merge_owned_requests([(source, vec![first, second])], &[], &[]).is_err());
    }

    #[test]
    fn higher_priority_declaration_keeps_its_complete_options() {
        let global = ToolSource::PakroToml("/global/pakro.toml".into());
        let local = ToolSource::PakroToml("/project/pakro.toml".into());
        let old = PackageInstallRequest::pypi("python", "3.13.16", "Demo_Pkg", "1.0");
        let mut next = PackageInstallRequest::pypi("python", "3.13.16", "demo-pkg", "2.0");
        next.extras = vec!["feature".into()];
        next.marker = Some("python_version >= '3.13'".into());
        let owned = merge_owned_requests(
            [(global, vec![old]), (local.clone(), vec![next.clone()])],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].request, next);
        assert_eq!(owned[0].source, local);
    }
}
