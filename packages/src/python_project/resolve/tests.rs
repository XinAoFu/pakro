use super::*;
use crate::packages::python_declarations::{PythonDeclarationOrigin, PythonMetadataWorkerReason};
use crate::packages::python_resolution::source::registry_depot::{
    ProjectRegistryFixture, RegistryEndpoint, RegistryRootScope,
};
use pakro_index::python_source::{
    PythonRegistryId, PythonSourcePolicy, TransportRulesFingerprint, transport_rules_fingerprint,
};
use pakro_index::simple::{SimpleClientError, SourceRequestRouter};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const BASIC: &str = "[project]\nname='app'\nversion='1.0'\n";
fn file(raw: &str) -> (tempfile::TempDir, PathBuf, BoundPythonProjectFile) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pyproject.toml");
    fs::write(&path, raw).unwrap();
    let owner = BoundPythonProjectFile::open(
        &path,
        Default::default(),
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    owner.recheck().unwrap();
    (directory, path, owner)
}
fn choose<'a>(
    owner: &'a BoundPythonProjectFile,
    extras: &[&str],
    groups: &[&str],
) -> BoundPythonDeclarationSelection<'a> {
    owner
        .select(
            PythonProjectSelection {
                include_project_dependencies: true,
                extras,
                groups,
            },
            Default::default(),
        )
        .unwrap()
}
fn policy() -> PythonSourcePolicy {
    PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap())
}
fn context<'a>(
    fixture: &'a ProjectRegistryFixture,
    endpoints: &'a [RegistryEndpoint<'a>],
    policy: &'a PythonSourcePolicy,
    extras: &[&str],
    groups: &[&str],
) -> RegistryDepotContext<'a> {
    let mut context = fixture.context(endpoints, policy);
    context.root_scope = RegistryRootScope::Lock {
        extras: extras.iter().map(|value| (*value).to_owned()).collect(),
        groups: groups.iter().map(|value| (*value).to_owned()).collect(),
    };
    context
}

#[tokio::test]
async fn mixed_origins_use_actual_project_extra_and_environment_scopes_in_real_closure() {
    let fixture = ProjectRegistryFixture::wheels();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let (_directory, _path, owner) = file(&format!(
        "{BASIC}dependencies=['demo-pkg==1.0']\n[project.optional-dependencies]\nui=[\"dep==1.0; extra == 'ui'\"]\n[dependency-groups]\ndev=[\"dep==1.0; python_version >= '3'\"]\n"
    ));
    let selected = choose(&owner, &["ui"], &["dev"]);
    let result = prepare_project_registry(
        selected,
        &owner,
        &root,
        context(&fixture, &endpoints, &policy, &["ui"], &["dev"]),
        Default::default(),
    )
    .await
    .unwrap();
    let ProjectRegistryPreparation::Closed(closed) = &result else {
        panic!("got {result:?}")
    };
    closed.recheck_for(&owner, &root).unwrap();
    assert_eq!(closed.graph().plan().selected.len(), 2);
    assert_eq!(closed.graph().checked().roots.len(), 3);
    let origins = result.selection().declarations().known().requirements();
    assert!(matches!(
        origins[0].origin(),
        PythonDeclarationOrigin::Project { index: 0 }
    ));
    assert!(
        matches!(origins[1].origin(), PythonDeclarationOrigin::Extra { name, index: 0 } if name == "ui")
    );
    assert!(
        matches!(origins[2].origin(), PythonDeclarationOrigin::Group { name, index: 0 } if name == "dev")
    );
    assert_eq!(fixture.requests().len(), 4);
    fixture.no_publish();

    let (_directory2, _path2, group_owner) = file(&format!(
        "{BASIC}[project.optional-dependencies]\nui=[]\n[dependency-groups]\ndev=[\"dep; extra == 'ui'\"]\n"
    ));
    let rejected = prepare_project_registry(
        choose(&group_owner, &["ui"], &["dev"]),
        &group_owner,
        &root,
        context(&fixture, &endpoints, &policy, &["ui"], &["dev"]),
        Default::default(),
    )
    .await;
    assert!(matches!(
        rejected,
        Err(ProjectRegistryError::Registry(
            RegistryDepotError::RootMarker
        ))
    ));
    assert_eq!(
        fixture.requests().len(),
        4,
        "group extra must fail in Environment before any new project lookup"
    );
    fixture.no_publish();
}

#[tokio::test]
async fn another_file_owner_wrong_selection_context_and_changed_source_are_rejected_before_http() {
    let fixture = ProjectRegistryFixture::wheels();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let (_directory, path, owner) = file(&format!("{BASIC}dependencies=['demo-pkg==1.0']\n"));
    let another = BoundPythonProjectFile::open(
        &path,
        Default::default(),
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    assert!(matches!(
        prepare_project_registry(
            choose(&owner, &[], &[]),
            &another,
            &root,
            context(&fixture, &endpoints, &policy, &[], &[]),
            Default::default()
        )
        .await,
        Err(ProjectRegistryError::File(
            PythonProjectFileError::WrongOwner
        ))
    ));
    let mut wrong = context(&fixture, &endpoints, &policy, &[], &[]);
    wrong.root_scope = RegistryRootScope::Environment;
    assert!(matches!(
        prepare_project_registry(
            choose(&owner, &[], &[]),
            &owner,
            &root,
            wrong,
            Default::default()
        )
        .await,
        Err(ProjectRegistryError::SelectionContext)
    ));
    let selected = choose(&owner, &[], &[]);
    fs::write(&path, format!("{BASIC}dependencies=['dep==1.0']\n")).unwrap();
    assert!(matches!(
        prepare_project_registry(
            selected,
            &owner,
            &root,
            context(&fixture, &endpoints, &policy, &[], &[]),
            Default::default()
        )
        .await,
        Err(ProjectRegistryError::File(
            PythonProjectFileError::Observation(ReadOnlyFileError::Changed)
        ))
    ));
    assert!(fixture.requests().is_empty());
    fixture.no_publish();
}

#[tokio::test]
async fn dynamic_metadata_preserves_prefix_and_context_but_static_project_python_is_checked() {
    let fixture = ProjectRegistryFixture::wheels();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let (_directory, _path, owner) = file(&format!(
        "{BASIC}dynamic=['dependencies','optional-dependencies']\ndependencies=['demo-pkg==1.0']\n"
    ));
    let result = prepare_project_registry(
        choose(&owner, &["future"], &[]),
        &owner,
        &root,
        context(&fixture, &endpoints, &policy, &["future"], &[]),
        Default::default(),
    )
    .await
    .unwrap();
    let ProjectRegistryPreparation::NeedsMetadataWorker(value) = &result else {
        panic!("got {result:?}")
    };
    value.binding.recheck_for(&owner, &root).unwrap();
    assert!(
        matches!(&value.context.root_scope, RegistryRootScope::Lock { extras, groups } if extras == &["future"] && groups.is_empty())
    );
    assert_eq!(
        result
            .selection()
            .declarations()
            .metadata_need()
            .unwrap()
            .reason(),
        PythonMetadataWorkerReason::DynamicMetadata
    );
    assert_eq!(
        result.selection().declarations().known().requirements()[0]
            .syntax()
            .raw_for_semantic_resolution(),
        "demo-pkg==1.0"
    );
    let (_directory2, _path2, incompatible) = file(&format!(
        "{BASIC}requires-python='<=3.13.4'\ndependencies=['demo-pkg==1.0']\n"
    ));
    assert!(matches!(
        prepare_project_registry(
            choose(&incompatible, &[], &[]),
            &incompatible,
            &root,
            context(&fixture, &endpoints, &policy, &[], &[]),
            Default::default()
        )
        .await,
        Err(ProjectRegistryError::RequiresPython)
    ));
    assert!(fixture.requests().is_empty());
    fixture.no_publish();
}

#[tokio::test]
async fn active_self_project_reference_is_retained_and_inactive_reference_does_not_fetch_pypi() {
    let fixture = ProjectRegistryFixture::wheels();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let (_directory, _path, owner) = file(&format!(
        "{BASIC}dependencies=['app[ui]']\n[project.optional-dependencies]\nui=[]\n"
    ));
    let result = prepare_project_registry(
        choose(&owner, &["ui"], &[]),
        &owner,
        &root,
        context(&fixture, &endpoints, &policy, &["ui"], &[]),
        Default::default(),
    )
    .await
    .unwrap();
    let ProjectRegistryPreparation::NeedsLocalProject(value) = &result else {
        panic!("got {result:?}")
    };
    assert_eq!(value.declaration(), 0);
    value.binding.recheck_for(&owner, &root).unwrap();
    assert_eq!(
        result.selection().declarations().known().requirements()[0]
            .syntax()
            .raw_for_semantic_resolution(),
        "app[ui]"
    );
    assert!(fixture.requests().is_empty());
    let (_directory2, _path2, inactive) = file(&format!(
        "{BASIC}dependencies=[\"app[ui]; python_version < '2'\",'demo-pkg==1.0']\n[project.optional-dependencies]\nui=[]\n"
    ));
    let resolved = prepare_project_registry(
        choose(&inactive, &["ui"], &[]),
        &inactive,
        &root,
        context(&fixture, &endpoints, &policy, &["ui"], &[]),
        Default::default(),
    )
    .await
    .unwrap();
    assert!(matches!(resolved, ProjectRegistryPreparation::Closed(_)));
    assert!(
        !fixture
            .requests()
            .iter()
            .any(|request| request.contains("/simple/app/"))
    );
    assert_eq!(fixture.requests().len(), 4);
    fixture.no_publish();
}

#[tokio::test]
async fn an_explicit_uri_for_the_project_name_keeps_its_source_adapter_route() {
    let fixture = ProjectRegistryFixture::wheels();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let declaration = "app @ https://private.invalid/distribution?token=PRIVATE_SENTINEL";
    let (_directory, _path, owner) = file(&format!("{BASIC}dependencies=['{declaration}']\n"));
    let selected = choose(&owner, &[], &[]);
    assert!(
        selected.declarations().known().requirements()[0]
            .syntax()
            .unresolved_reference()
            .is_some(),
        "the real parser must first establish an explicit source reference"
    );
    let result = prepare_project_registry(
        selected,
        &owner,
        &root,
        context(&fixture, &endpoints, &policy, &[], &[]),
        Default::default(),
    )
    .await
    .unwrap();
    let ProjectRegistryPreparation::NeedsRegistry(value) = &result else {
        panic!("an explicit source was routed as {result:?}");
    };
    let RegistryPreparation::NeedsSourceAdapter(source) = value.registry() else {
        panic!("an explicit URI must remain the original source-adapter continuation");
    };
    assert_eq!(source.declaration_for_source_binding(), declaration);
    value.binding.recheck_for(&owner, &root).unwrap();
    assert!(fixture.requests().is_empty());
    assert!(!format!("{result:?}").contains("PRIVATE_SENTINEL"));
    fixture.no_publish();
}

struct MutateDuringFileRoute {
    path: PathBuf,
    fired: AtomicBool,
}
impl SourceRequestRouter for MutateDuringFileRoute {
    fn fingerprint(&self) -> TransportRulesFingerprint {
        transport_rules_fingerprint(None)
    }
    fn request_url(&self, url: &url::Url) -> Result<url::Url, SimpleClientError> {
        if url.path().starts_with("/files/") && !self.fired.swap(true, Ordering::SeqCst) {
            fs::write(
                &self.path,
                format!("{BASIC}dependencies=['replaced-project']\n"),
            )
            .unwrap();
        }
        Ok(url.clone())
    }
}

#[tokio::test]
async fn real_source_build_policy_continuations_and_late_file_rejection_preserve_owners() {
    let fixture = ProjectRegistryFixture::wheels();
    let root = fixture.lock();
    let endpoints = [fixture.endpoint()];
    let policy = policy();
    let (_directory, _path, direct) = file(&format!(
        "{BASIC}dependencies=['direct @ https://private.invalid/package?token=PRIVATE_SENTINEL']\n"
    ));
    let result = prepare_project_registry(
        choose(&direct, &[], &[]),
        &direct,
        &root,
        context(&fixture, &endpoints, &policy, &[], &[]),
        Default::default(),
    )
    .await
    .unwrap();
    let ProjectRegistryPreparation::NeedsRegistry(pending) = &result else {
        panic!("got {result:?}")
    };
    let RegistryPreparation::NeedsSourceAdapter(source) = pending.registry() else {
        panic!("source continuation lost")
    };
    assert_eq!(
        source.declaration_for_source_binding(),
        "direct @ https://private.invalid/package?token=PRIVATE_SENTINEL"
    );
    pending.binding.recheck_for(&direct, &root).unwrap();
    assert!(!format!("{result:?}").contains("PRIVATE_SENTINEL"));
    let (_directory2, _path2, ordinary) = file(&format!("{BASIC}dependencies=['demo-pkg==1.0']\n"));
    let mut conditional_policy = context(&fixture, &endpoints, &policy, &[], &[]);
    conditional_policy.candidate_policy.installed_version = Some("1.0");
    let deferred = prepare_project_registry(
        choose(&ordinary, &[], &[]),
        &ordinary,
        &root,
        conditional_policy,
        Default::default(),
    )
    .await
    .unwrap();
    assert!(
        matches!(&deferred, ProjectRegistryPreparation::NeedsRegistry(value) if matches!(value.registry(), RegistryPreparation::NeedsConstraintPolicy(_)))
    );
    assert!(fixture.requests().is_empty());
    fixture.no_publish();

    let sdist = ProjectRegistryFixture::sdist();
    let sdist_root = sdist.lock();
    let sdist_endpoints = [sdist.endpoint()];
    let build = prepare_project_registry(
        choose(&ordinary, &[], &[]),
        &ordinary,
        &sdist_root,
        context(&sdist, &sdist_endpoints, &policy, &[], &[]),
        Default::default(),
    )
    .await
    .unwrap();
    let ProjectRegistryPreparation::NeedsRegistry(value) = &build else {
        panic!("got {build:?}")
    };
    let RegistryPreparation::NeedsBuild(build_source) = value.registry() else {
        panic!("legal sdist continuation lost")
    };
    assert_eq!(build_source.pin().file().filename(), "demo_pkg-1.0.tar.gz");
    value.binding.recheck_for(&ordinary, &sdist_root).unwrap();
    assert_eq!(sdist.requests().len(), 2);
    sdist.no_publish();

    let (_directory3, late_path, late) = file(&format!("{BASIC}dependencies=['demo-pkg==1.0']\n"));
    let route = MutateDuringFileRoute {
        path: late_path,
        fired: AtomicBool::new(false),
    };
    let mut late_endpoint = fixture.endpoint();
    late_endpoint.file.transport = &route;
    let late_endpoints = [late_endpoint];
    let rejected = prepare_project_registry(
        choose(&late, &[], &[]),
        &late,
        &root,
        context(&fixture, &late_endpoints, &policy, &[], &[]),
        Default::default(),
    )
    .await;
    assert!(
        route.fired.load(Ordering::SeqCst),
        "real file transport must reach mutation fixture"
    );
    assert!(matches!(
        rejected,
        Err(ProjectRegistryError::File(
            PythonProjectFileError::Observation(ReadOnlyFileError::Changed)
        ))
    ));
    fixture.no_publish();
}
