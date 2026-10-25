//! Merge package declarations while retaining each winning declaration's owner.

use indexmap::IndexMap;

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
) -> Vec<OwnedPackageRequest> {
    let mut merged = IndexMap::new();
    for (source, requests) in sources.into_iter().chain([
        (ToolSource::Argument, extra.to_vec()),
        (ToolSource::Argument, cli.to_vec()),
    ]) {
        for request in requests {
            merged.insert(
                (request.group_key(), request.name.clone()),
                OwnedPackageRequest {
                    request,
                    source: source.clone(),
                },
            );
        }
    }
    merged.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(version: &str) -> PackageInstallRequest {
        PackageInstallRequest {
            runtime_name: "python".into(),
            runtime_version: "3.10.14".into(),
            name: "demo".into(),
            version: version.into(),
        }
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
        );
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].request.version, "2.0");
        assert_eq!(owned[0].source, project);
        let extra = merge_owned_requests([(project, vec![request("2.0")])], &[request("3.0")], &[]);
        assert_eq!(extra[0].request.version, "3.0");
        assert_eq!(extra[0].source, ToolSource::Argument);
    }
}
