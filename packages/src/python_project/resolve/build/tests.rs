use super::*;
use crate::packages::python_environment::resolved_build::{ActualRuntime, actual_clt_inputs};
use crate::packages::python_resolution::source::registry_depot::{
    ProjectRegistryFixture, RegistryRootScope,
};
use pakro_index::python_markers::PythonMarkerScope;
use pakro_index::python_source::{PythonRegistryId, PythonSourcePolicy};
use std::cell::Cell;
use std::fs;
use std::time::Duration;

const RAW: &str = "[project]\nname='app'\nversion='1.0'\nrequires-python='>=3.9'\ndependencies=['demo-pkg==1.0']\n[project.optional-dependencies]\nui=[\"dep==1.0; extra == 'ui'\"]\n[dependency-groups]\ndev=[\"dep==1.0; python_version >= '3'\"]\n";
const CHANGED: &str = "[project]\nname='changed-externally'\nversion='2'\n";

fn deadline() -> TokioInstant {
    TokioInstant::now() + Duration::from_secs(240)
}

async fn closed<'file, 'root>(
    owner: &'file BoundPythonProjectFile,
    fixture: &ProjectRegistryFixture,
    root: &'root BoundPythonLock,
    runtime: &ObservedRegistryRuntimeTarget,
    deadline: TokioInstant,
) -> ClosedPythonProjectRegistry<'file, 'root> {
    let endpoints = [fixture.endpoint()];
    let source_policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let mut context = fixture.context(&endpoints, &source_policy);
    context.deadline = deadline.into_std();
    context.environment = runtime.target().environment();
    context.supported = runtime.target().supported();
    context.semantics = runtime.target().semantics();
    context.root_scope = RegistryRootScope::Lock {
        extras: vec!["ui".into()],
        groups: vec!["dev".into()],
    };
    let selected = owner
        .select(
            PythonProjectSelection {
                include_project_dependencies: true,
                extras: &["ui"],
                groups: &["dev"],
            },
            Default::default(),
        )
        .unwrap();
    let result = prepare_project_registry(selected, owner, root, context, Default::default())
        .await
        .expect("real file/registry/T/O2/C/sidecar/unique mixed-scope graph closure prelude");
    let ProjectRegistryPreparation::Closed(graph) = result else {
        panic!("got {result:?}")
    };
    graph.recheck_for(owner, root).unwrap();
    assert_eq!(graph.graph().checked().roots.len(), 3);
    assert_eq!(graph.graph().plan().selected.len(), 2);
    assert_eq!(fixture.requests().len(), 4);
    graph
}

#[tokio::test]
async fn genuine_project_selection_and_closed_sources_reach_the_actual_complete_local_build() {
    let deadline = deadline();
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = actual_clt_inputs(
        PythonMarkerScope::Lock {
            selected_extras: &["ui"],
            dependency_groups: &["dev"],
        },
        deadline,
    )
    .await;
    let fixture = ProjectRegistryFixture::wheels();
    fs::write(fixture.project_path(), RAW).unwrap();
    let owner = BoundPythonProjectFile::open(
        &fixture.project_path(),
        Default::default(),
        deadline.into_std(),
    )
    .unwrap();
    let root = fixture.lock_at_deadline(deadline.into_std());
    let graph = closed(&owner, &fixture, &root, &runtime, deadline).await;
    let result = graph
        .prepare_environment(runtime, destination, startup, Default::default(), deadline)
        .await
        .unwrap();
    assert!(result.final_prefix().join("bin/python").exists());
    assert!(result.executable().is_file());
    assert_eq!(result.plan().selected.len(), 2);
    assert_eq!(
        result
            .selection()
            .declarations()
            .known()
            .requirements()
            .len(),
        3
    );
    result.recheck().unwrap();
    fixture.no_publish();
    drop(result);
    drop(_owner);
}

#[tokio::test]
async fn a_post_build_project_edit_is_rejected_and_only_the_unpublished_prefix_is_cleaned() {
    let deadline = deadline();
    let ActualRuntime {
        _owner,
        runtime,
        destination,
        startup,
    } = actual_clt_inputs(
        PythonMarkerScope::Lock {
            selected_extras: &["ui"],
            dependency_groups: &["dev"],
        },
        deadline,
    )
    .await;
    let final_prefix = destination.final_path().to_owned();
    let fixture = ProjectRegistryFixture::wheels();
    fs::write(fixture.project_path(), RAW).unwrap();
    let owner = BoundPythonProjectFile::open(
        &fixture.project_path(),
        Default::default(),
        deadline.into_std(),
    )
    .unwrap();
    let root = fixture.lock_at_deadline(deadline.into_std());
    let graph = closed(&owner, &fixture, &root, &runtime, deadline).await;
    let finished_real_build = Cell::new(false);
    let result = graph
        .prepare_with_finish(
            runtime,
            destination,
            startup,
            Default::default(),
            deadline,
            |built| {
                assert_eq!(built, final_prefix);
                assert!(built.join("bin/python").exists());
                assert!(built.join("pyvenv.cfg").is_file());
                finished_real_build.set(true);
                fs::write(fixture.project_path(), CHANGED).unwrap();
            },
        )
        .await;
    assert!(
        finished_real_build.get(),
        "late assertion requires the actual full build to finish first"
    );
    assert!(matches!(
        result,
        Err(ProjectEnvironmentError::Project(
            ProjectRegistryError::File(PythonProjectFileError::Observation(
                ReadOnlyFileError::Changed
            ))
        ))
    ));
    assert!(
        !final_prefix.exists(),
        "only the unpublished owned build may be removed on error"
    );
    assert_eq!(fs::read_to_string(fixture.project_path()).unwrap(), CHANGED);
    fixture.no_publish();
    drop(_owner);
}
