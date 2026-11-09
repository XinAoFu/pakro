//! Strongly connected groups of the already fixed Conda records.
use std::collections::{BTreeMap, BTreeSet};

use eyre::{ContextCompat, Result, ensure};
use rattler_conda_types::RepoDataRecord;

/// Members are normalized package names; every dependency group comes before
/// the groups that refer to it. A singleton can also have a self dependency.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct DependencyGroup {
    pub(super) members: Vec<String>,
    pub(super) cyclic: bool,
}

/// This is a descriptive ordering only. It neither publishes objects nor
/// changes the installation path's current rejection of dependency cycles.
#[allow(dead_code)]
pub(super) fn dependency_groups(records: &[RepoDataRecord]) -> Result<Vec<DependencyGroup>> {
    let mut by_name = BTreeMap::new();
    for record in records {
        ensure!(
            by_name
                .insert(
                    record.package_record.name.as_normalized().to_owned(),
                    record
                )
                .is_none(),
            "duplicate fixed Conda package"
        );
    }
    let names = by_name.keys().cloned().collect::<Vec<_>>();
    let indexes = names
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect::<BTreeMap<_, _>>();
    let mut edges = vec![Vec::new(); names.len()];
    let mut reverse = vec![Vec::new(); names.len()];
    for (index, name) in names.iter().enumerate() {
        let mut dependencies = BTreeSet::new();
        for dependency in super::dependencies(by_name[name])? {
            if dependency.starts_with("__") {
                continue;
            }
            let other = *indexes
                .get(dependency.as_str())
                .context("fixed Conda graph has a missing dependency")?;
            dependencies.insert(other);
        }
        for other in dependencies {
            edges[index].push(other);
            reverse[other].push(index);
        }
    }
    let mut seen = vec![false; names.len()];
    let mut finished = Vec::with_capacity(names.len());
    for start in 0..names.len() {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![(start, 0usize)];
        while let Some((node, next)) = stack.last_mut() {
            if *next < edges[*node].len() {
                let other = edges[*node][*next];
                *next += 1;
                if !seen[other] {
                    seen[other] = true;
                    stack.push((other, 0));
                }
            } else {
                finished.push(stack.pop().expect("nonempty stack").0);
            }
        }
    }
    let mut assigned = vec![false; names.len()];
    let mut groups = Vec::new();
    for start in finished.into_iter().rev() {
        if assigned[start] {
            continue;
        }
        assigned[start] = true;
        let mut members = Vec::new();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            members.push(node);
            for &other in &reverse[node] {
                if !assigned[other] {
                    assigned[other] = true;
                    stack.push(other);
                }
            }
        }
        members.sort_unstable();
        let cyclic = members.len() > 1 || edges[members[0]].contains(&members[0]);
        groups.push(DependencyGroup {
            members: members
                .into_iter()
                .map(|index| names[index].clone())
                .collect(),
            cyclic,
        });
    }
    // The reverse pass produces referrers first; publication needs the reverse.
    groups.reverse();
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rattler_conda_types::package::{
        ArchiveIdentifier, CondaArchiveType, DistArchiveIdentifier, DistArchiveType,
    };
    use rattler_conda_types::{PackageName, PackageRecord, Version};
    use std::str::FromStr;

    fn record(name: &str, dependencies: &[&str]) -> RepoDataRecord {
        let mut package = PackageRecord::new(
            PackageName::from_str(name).unwrap(),
            Version::from_str("1.0").unwrap(),
            "build_0".into(),
        );
        package.depends = dependencies.iter().map(|value| (*value).into()).collect();
        RepoDataRecord {
            package_record: package,
            identifier: DistArchiveIdentifier::new(
                ArchiveIdentifier {
                    name: name.into(),
                    version: "1.0".into(),
                    build_string: "build_0".into(),
                },
                DistArchiveType::Conda(CondaArchiveType::Conda),
            ),
            url: format!("https://example.invalid/{name}-1.0-build_0.conda")
                .parse()
                .unwrap(),
            channel: Some("https://example.invalid/".into()),
        }
    }

    fn groups(records: &[RepoDataRecord]) -> Vec<DependencyGroup> {
        dependency_groups(records).unwrap()
    }

    #[test]
    fn acyclic_dependencies_precede_users_and_ignore_virtuals() {
        let records = vec![
            record("app", &["middle >=1", "__osx >=11"]),
            record("middle", &["base >=1"]),
            record("base", &[]),
        ];
        assert_eq!(
            groups(&records),
            vec![
                DependencyGroup {
                    members: vec!["base".into()],
                    cyclic: false
                },
                DependencyGroup {
                    members: vec!["middle".into()],
                    cyclic: false
                },
                DependencyGroup {
                    members: vec!["app".into()],
                    cyclic: false
                },
            ]
        );
        let mut reversed = records.clone();
        reversed.reverse();
        assert_eq!(groups(&records), groups(&reversed));
    }

    #[test]
    fn ordinary_cycle_is_one_group_after_external_dependencies() {
        let records = vec![
            record("left", &["right >=1", "base >=1"]),
            record("right", &["left >=1"]),
            record("base", &[]),
            record("app", &["left >=1"]),
        ];
        assert_eq!(
            groups(&records),
            vec![
                DependencyGroup {
                    members: vec!["base".into()],
                    cyclic: false
                },
                DependencyGroup {
                    members: vec!["left".into(), "right".into()],
                    cyclic: true
                },
                DependencyGroup {
                    members: vec!["app".into()],
                    cyclic: false
                },
            ]
        );
    }

    #[test]
    fn python_cycle_and_singleton_self_cycle_are_identified() {
        let records = vec![
            record("python", &["runtime >=1"]),
            record("runtime", &["python >=1", "base >=1"]),
            record("base", &["base >=1"]),
        ];
        assert_eq!(
            groups(&records),
            vec![
                DependencyGroup {
                    members: vec!["base".into()],
                    cyclic: true
                },
                DependencyGroup {
                    members: vec!["python".into(), "runtime".into()],
                    cyclic: true
                },
            ]
        );
    }

    #[test]
    fn missing_or_duplicate_records_are_rejected() {
        assert!(dependency_groups(&[record("a", &["missing >=1"])]).is_err());
        assert!(dependency_groups(&[record("a", &[]), record("a", &[])]).is_err());
    }

    /// Input preparation only: retain the production solver's actual graph;
    /// no archive is installed or fetched.
    async fn prepare_real_records(
        directory_variable: &str,
        roots: &[(&str, &str, &str)],
        require_owa_cycle: bool,
    ) {
        use crate::libraries::language::source::{PythonRuntimeRequest, PythonRuntimeSource};
        use crate::packages::python_resolution::lock_model::{LockModelLimits, PythonLockModel};
        use crate::packages::request::{PackageInstallRequest, PackageSource};
        use crate::programs::{
            AllowedWrites, OperationContextInputs, OperationPolicy, RequestBudget,
            capture_operation_context,
        };
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let stage = std::path::PathBuf::from(
            std::env::var_os(directory_variable)
                .expect("new absolute Conda input directory required"),
        );
        assert!(stage.is_absolute());
        std::fs::create_dir(&stage).expect("Conda input directory must not already exist");
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
        let stage = stage.canonicalize().unwrap();
        let mut settings = crate::config::Settings::default();
        settings.http_timeout = "30s".into();
        settings.http_download_timeout = "30m".into();
        settings.python.conda.channels = vec!["conda-forge".into()];
        let context = capture_operation_context(OperationContextInputs {
            settings: &settings,
            policy: OperationPolicy {
                locked: false,
                offline: false,
                deadline: Instant::now() + Duration::from_secs(580),
                budget: RequestBudget {
                    max_arguments: 8,
                    max_total_argument_bytes: 4096,
                    max_declarations: 16,
                    max_runtime_candidates: 8,
                },
                writes: AllowedWrites {
                    libraries: false,
                    project_selection: false,
                    lockfile: false,
                    declarations: false,
                },
            },
            cancellation: tokio_util::sync::CancellationToken::new(),
            data_root: stage.join("data"),
            cache_root: stage.join("cache"),
            python_sources: None,
            python_target_description: None,
        })
        .unwrap();
        let runtime = PythonRuntimeRequest {
            version: "3.13.13".into(),
            source: PythonRuntimeSource::Conda,
            channel: Some("conda-forge".into()),
            build: Some("h20e6be0_100_cp313".into()),
        };
        let requests = roots.iter().map(|(name, version, build)| PackageInstallRequest {
            runtime_name: "python".into(),
            runtime_version: runtime.version.clone(),
            name: (*name).into(),
            version: (*version).into(),
            source: PackageSource::Conda,
            channel: Some("conda-forge".into()),
            build: Some((*build).into()),
            extras: vec![],
            marker: None,
        }).collect::<Vec<_>>();
        let host = crate::programs::install::conda::native_target().unwrap();
        let limits = LockModelLimits::default();
        let model = crate::programs::install::conda::resolve_requests_into_lock(
            &context,
            &host,
            Some(&runtime),
            &requests,
            vec![],
            vec![],
            PythonLockModel::from_parts(&[], &[], limits).unwrap(),
            String::new(),
            "default".into(),
            [0; 32],
            limits,
        )
        .await
        .unwrap();
        let graph = &model.conda()[0];
        let groups = dependency_groups(&graph.records).unwrap();
        if require_owa_cycle {
            assert!(groups.iter().any(|group| group.cyclic
                && group.members.iter().any(|name| name == "owa-core")
                && group.members.iter().any(|name| name == "owa-msgs")),
                "resolved closure did not retain the observed package cycle");
        }
        assert!(!stage.join("data").exists(), "metadata solve wrote a data root");
        std::fs::write(
            stage.join("records.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 1,
                "artifacts_prepared": false,
                "graph": graph,
                "groups": groups.iter().map(|group| serde_json::json!({
                    "members": &group.members,
                    "cyclic": group.cyclic,
                })).collect::<Vec<_>>(),
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires public Conda metadata and a new owned input directory"]
    async fn p8_prepare_real_conda_cycle_records() {
        prepare_real_records(
            "PAKRO_TEST_CONDA_CYCLE_RECORDS_DIR",
            &[("owa-core", "0.4.1", "pyhd8ed1ab_0"),
              ("owa-msgs", "0.4.1", "pyhd8ed1ab_0")],
            true,
        ).await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires public Conda metadata and a new owned input directory"]
    async fn p8_prepare_real_conda_gdal_records() {
        prepare_real_records(
            "PAKRO_TEST_CONDA_GDAL_RECORDS_DIR",
            &[("gdal", "3.13.3", "np2py313hc201c6a_5"),
              ("libgdal-core", "3.13.3", "h8348644_5"),
              ("proj", "9.9.0", "h51dee2d_0")],
            false,
        ).await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires public Conda metadata and a new owned input directory"]
    async fn p8_prepare_real_conda_proj_records() {
        prepare_real_records(
            "PAKRO_TEST_CONDA_PROJ_RECORDS_DIR",
            &[("proj", "9.9.0", "h51dee2d_0")],
            false,
        ).await;
    }
}
