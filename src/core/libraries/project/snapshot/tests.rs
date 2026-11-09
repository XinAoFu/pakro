use super::*;
use std::fs;
use std::time::Duration;

const BASE: &str = "[tools]\npython = \"3.11.9\"\n";

#[test]
fn conda_python_declaration_retains_exact_source_options() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pakro.toml");
    fs::write(&path, "[tools]\npython={version='3.13.13',source='conda',channel='conda-forge',build='h20e6be0_100_cp313'}\n").unwrap();
    let observed = observe_project(dir.path(), policy(dir.path())).unwrap();
    let request = observed.python_request().unwrap().unwrap();
    assert_eq!(request.source, PythonRuntimeSource::Conda);
    assert_eq!(request.version, "3.13.13");
    assert_eq!(request.channel.as_deref(), Some("conda-forge"));
    assert_eq!(request.build.as_deref(), Some("h20e6be0_100_cp313"));
    for declaration in [
        "python={version='3.13',source='conda'}",
        "python={version='3.13.13',source='conda',build='py*'}",
        "python={version='3.13.13',source='conda',channel=42}",
    ] {
        fs::write(&path, format!("[tools]\n{declaration}\n")).unwrap();
        let observed = observe_project(dir.path(), policy(dir.path())).unwrap();
        assert!(observed.python_request().is_err());
    }
}

fn policy(root: &Path) -> ReadPolicy {
    let mut discovery =
        FrozenConfigDiscovery::new(vec!["pakro.toml".into(), "pakro.local.toml".into()]);
    discovery.ceiling_paths = vec![root.parent().unwrap().to_path_buf()];
    ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(60))
}

#[test]
fn r03_new_higher_priority_file_revokes_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pakro.toml"), BASE).unwrap();
    let observed = observe_project(dir.path(), policy(dir.path())).unwrap();
    fs::write(
        dir.path().join("pakro.local.toml"),
        "[tools]\npython = \"3.12.8\"\n",
    )
    .unwrap();
    assert_eq!(observed.recheck(), Err(ProjectObservationError::StaleInput));
}

#[test]
fn r03_projectless_read_does_not_register() {
    let dir = tempfile::tempdir().unwrap();
    let mut read = policy(dir.path());
    read.scope = ReadScope::GlobalOnly;
    let observed = observe_project(dir.path(), read).unwrap();
    observed.recheck().unwrap();
    assert_eq!(observed.paths().count(), 0);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn r03_use_path_targets_only_selected_project() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("cwd");
    let target = dir.path().join("target");
    fs::create_dir(&cwd).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(cwd.join("pakro.toml"), "invalid TOML that must not be read").unwrap();
    fs::write(target.join("pakro.toml"), BASE).unwrap();
    let mut read = policy(&target);
    read.environment
        .insert("PWD".into(), cwd.to_string_lossy().into_owned());
    read.cli_arguments = vec!["python@3.12.8".into()];
    let observed = observe_project(&target, read).unwrap();
    assert_eq!(
        observed.paths().collect::<Vec<_>>(),
        vec![target.join("pakro.toml").as_path()]
    );
    assert_eq!(observed.root(), target);
    assert_eq!(
        observed.document(&target.join("pakro.toml")),
        Some(BASE.as_bytes())
    );
    assert!(observed.contains_edit_target(&target.join("pakro.toml")));
    assert!(!observed.contains_edit_target(&cwd.join("pakro.toml")));
    assert_eq!(
        fs::read(cwd.join("pakro.toml")).unwrap(),
        b"invalid TOML that must not be read"
    );
}

#[test]
fn r03_replaced_input_inode_revokes_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pakro.toml");
    fs::write(&path, BASE).unwrap();
    let observed = observe_project(dir.path(), policy(dir.path())).unwrap();
    let replacement = dir.path().join("replacement");
    fs::write(&replacement, BASE).unwrap();
    fs::rename(&replacement, &path).unwrap();
    assert_eq!(observed.recheck(), Err(ProjectObservationError::StaleInput));
}

#[test]
fn r03_new_lock_revokes_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pakro.toml"), BASE).unwrap();
    let observed = observe_project(dir.path(), policy(dir.path())).unwrap();
    fs::write(dir.path().join("pakro.lock"), "# newly created\n").unwrap();
    assert_eq!(observed.recheck(), Err(ProjectObservationError::StaleInput));
}

#[test]
fn r03_read_freezes_cli_environment_and_original_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let mut read = policy(dir.path());
    read.cli_arguments = vec!["python@3.11.9".into()];
    read.environment.insert("PAKRO_ENV".into(), "dev".into());
    let signal = read.cancellation.clone();
    let observed = observe_project(dir.path(), read.clone()).unwrap();
    read.cli_arguments.clear();
    read.environment.clear();
    assert_eq!(observed.cli_arguments(), ["python@3.11.9"]);
    assert_eq!(observed.environment().get("PAKRO_ENV").unwrap(), "dev");
    signal.cancel();
    assert_eq!(observed.recheck(), Err(ProjectObservationError::Cancelled));
}

#[test]
fn r03_remote_include_requires_static_input_without_fetch() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("pakro.toml"),
        "include = [\"git::https://example.invalid/repo.git//pakro.toml\"]\n",
    )
    .unwrap();
    assert_eq!(
        observe_project(dir.path(), policy(dir.path())).unwrap_err(),
        ProjectObservationError::MissingStaticInclude
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn r03_aggregate_document_budget_stops_before_success() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pakro.toml"), BASE).unwrap();
    fs::write(dir.path().join("pakro.local.toml"), BASE).unwrap();
    let mut read = policy(dir.path());
    read.max_total_bytes = BASE.len();
    assert_eq!(
        observe_project(dir.path(), read).unwrap_err(),
        ProjectObservationError::ResourceLimit
    );
}

#[test]
fn r03_empty_project_alias_replacement_revokes_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    fs::create_dir(&real).unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let observed = observe_project(&alias, policy(&alias)).unwrap();
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    assert_eq!(observed.recheck(), Err(ProjectObservationError::StaleInput));
}

#[test]
fn r03_missing_candidate_parent_alias_replacement_revokes_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    fs::create_dir(&real).unwrap();
    let alias = dir.path().join(".config");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let mut read = policy(dir.path());
    read.discovery.local_patterns = vec![".config/pakro.toml".into()];
    let observed = observe_project(dir.path(), read).unwrap();
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    assert_eq!(observed.recheck(), Err(ProjectObservationError::StaleInput));
}

#[test]
fn r03_local_overlay_lock_is_part_of_observed_inputs() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pakro.local.toml"), BASE).unwrap();
    let lock = dir.path().join("pakro.local.lock");
    fs::write(&lock, "# before\n").unwrap();
    let observed = observe_project(dir.path(), policy(dir.path())).unwrap();
    fs::write(&lock, "# external\n").unwrap();
    assert_eq!(observed.recheck(), Err(ProjectObservationError::StaleInput));
}

#[test]
fn r03_static_include_retains_physical_owner_and_is_rechecked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pakro.toml");
    let fragment = dir.path().join("fragment.toml");
    let reference = "git::https://example.invalid/base.git//pakro.toml?ref=main";
    fs::write(&path, format!("include=['{reference}']\n")).unwrap();
    fs::write(&fragment, BASE).unwrap();
    let mut read = policy(dir.path());
    read.static_includes.push(StaticIncludeBinding {
        including: path.clone(),
        reference: reference.into(),
        path: fragment.clone(),
    });
    let observed = observe_project(dir.path(), read).unwrap();
    assert_eq!(observed.document(&fragment), Some(BASE.as_bytes()));
    assert!(!observed.contains_edit_target(&fragment));
    fs::write(&fragment, "# changed\n").unwrap();
    assert_eq!(observed.recheck(), Err(ProjectObservationError::StaleInput));
}

#[test]
fn p3_user_python_files_never_enter_the_observation_or_its_budget() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pakro.toml"), BASE).unwrap();
    let mut read = policy(dir.path());
    read.max_files = 1;
    read.max_total_bytes = BASE.len();
    let observed = observe_project(dir.path(), read.clone()).unwrap();
    let paths = observed.paths().map(Path::to_path_buf).collect::<Vec<_>>();
    for name in ["pyproject.toml", "requirements.txt"] {
        let path = dir.path().join(name);
        fs::write(&path, [0xff; 4096]).unwrap();
        observed.recheck().unwrap();
        assert_eq!(observed.document(&path), None);
        assert!(!observed.observed_auxiliary(&path));
        assert!(!observed.contains_edit_target(&path));
        let fresh = observe_project(dir.path(), read.clone()).unwrap();
        assert_eq!(
            fresh.paths().map(Path::to_path_buf).collect::<Vec<_>>(),
            paths
        );
        assert_eq!(fresh.effective_packages().unwrap().len(), 0);
        let next = dir.path().join("replacement");
        fs::write(&next, "[project]\ndependencies=['never-install==99']\n").unwrap();
        fs::rename(next, &path).unwrap();
        observed.recheck().unwrap();
        fresh.recheck().unwrap();
        fs::remove_file(path).unwrap();
        observed.recheck().unwrap();
        fresh.recheck().unwrap();
    }
}

#[test]
fn r03_effective_packages_reuse_existing_precedence_kernel() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("pakro.toml"),
        "[packages.\"python@3.11.9\"]\ndemo='1.0'\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("pakro.local.toml"),
        "[packages.\"python@3.11.9\"]\ndemo='2.0'\n",
    )
    .unwrap();
    let observed = observe_project(dir.path(), policy(dir.path())).unwrap();
    assert_eq!(observed.declarations().count(), 2);
    let effective = observed.effective_packages().unwrap();
    assert_eq!(effective.len(), 1);
    assert_eq!(effective[0].request.version, "2.0");
    assert_eq!(
        effective[0].source,
        ToolSource::PakroToml(dir.path().join("pakro.local.toml"))
    );
}

#[test]
fn r03_global_configuration_lock_is_observed_without_a_local_project() {
    use crate::config::ConfigDiscoveryCandidate;
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("global");
    fs::create_dir(&global).unwrap();
    fs::write(global.join("config.toml"), BASE).unwrap();
    let lock = global.join("pakro.lock");
    fs::write(&lock, "# global baseline\n").unwrap();
    let cwd = dir.path().join("cwd");
    fs::create_dir(&cwd).unwrap();
    let mut read = policy(&cwd);
    read.scope = ReadScope::GlobalOnly;
    read.discovery
        .global_candidates
        .push(ConfigDiscoveryCandidate {
            directory: global,
            pattern: "config.toml".into(),
        });
    let observed = observe_project(&cwd, read).unwrap();
    fs::write(lock, "# external\n").unwrap();
    assert_eq!(observed.recheck(), Err(ProjectObservationError::StaleInput));
    assert_eq!(fs::read_dir(cwd).unwrap().count(), 0);
}

#[test]
fn r03_custom_brace_filename_uses_the_discovery_glob_grammar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Pakro{file}");
    fs::write(&path, BASE).unwrap();
    let mut read = policy(dir.path());
    read.discovery.local_patterns = vec!["Pakro{file}".into()];
    let observed = observe_project(dir.path(), read).unwrap();
    assert_eq!(observed.document(&path), Some(BASE.as_bytes()));
    observed.recheck().unwrap();
}

#[test]
fn r03_local_only_cannot_rediscover_global_files_through_ancestors() {
    use crate::config::ConfigDiscoveryCandidate;
    let home = tempfile::tempdir().unwrap();
    let global = home.path().join(".config/pakro");
    fs::create_dir_all(&global).unwrap();
    let global_file = global.join("config.toml");
    fs::write(&global_file, BASE).unwrap();
    let project = home.path().join("project");
    fs::create_dir(&project).unwrap();
    let local = project.join("pakro.toml");
    fs::write(&local, "# local\n").unwrap();
    std::os::unix::fs::symlink(&global_file, project.join(".pakro.toml")).unwrap();
    let mut discovery = FrozenConfigDiscovery::new(vec![
        ".config/pakro/config.toml".into(),
        "pakro.toml".into(),
        ".pakro.toml".into(),
    ]);
    discovery.ceiling_paths = vec![home.path().parent().unwrap().to_path_buf()];
    discovery.global_candidates.push(ConfigDiscoveryCandidate {
        directory: global,
        pattern: "config.toml".into(),
    });
    let mut read = ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(60));
    read.scope = ReadScope::LocalOnly;
    let observed = observe_project(&project, read).unwrap();
    assert_eq!(
        observed
            .declarations()
            .map(|(path, _)| path)
            .collect::<Vec<_>>(),
        vec![local.as_path()]
    );
}
