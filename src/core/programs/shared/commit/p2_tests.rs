use super::*;
use crate::packages::python_environment::model::PackageGraphRecord;
use crate::packages::python_resolution::lock_session::BoundPythonLock;
use crate::programs::shared::lock_transaction;
use std::path::{Path, PathBuf};

const INITIAL_LOCK: &[u8] =
    b"# previous native snapshot\nlockfile_version=4\n[future]\nnative='preserved'\n";
const HELPER: &str = "programs::shared::commit::tests::p2_tests::lock_commit_crash_child_helper";

struct PackageFixture {
    context: OperationContext,
    project: PathBuf,
    observed: ObservedProjectInputs,
    language: LanguageKey,
    layout: [u8; 32],
    graph: PackageGraphRecord,
    document: Vec<u8>,
}

/// Real resolution, verified wheel/source output, actual package publication,
/// and the ordinary proposed-lock writer precede the high-level commit.
async fn package_fixture(root: &Path, source: bool) -> PackageFixture {
    let stage = root.join("stage");
    std::fs::create_dir(&stage).unwrap();
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    let context = context_with_lock(&stage, true);
    let language = if source {
        crate::libraries::package::layouts::tests::real_language(&context).await
    } else {
        let archive = archive("3.15.0");
        let spec = install_spec(&archive);
        prepare(&spec, &context, &archive).await.unwrap()
    };
    let version = language.descriptor().language_version.full_version();
    std::fs::write(
        project.join("pakro.toml"),
        format!("[tools]\npython='{version}'\n[packages.\"python@{version}\"]\ndemo-pkg='1.0'\n"),
    )
    .unwrap();
    std::fs::write(
        project.join("pyproject.toml"),
        "[project]\nname='owned-commit-fixture'\nversion='0.0.0'\ndependencies=[]\n",
    )
    .unwrap();
    std::fs::write(project.join("untouched.txt"), b"user content stays\n").unwrap();
    let path = project.join("pakro.lock");
    std::fs::write(&path, INITIAL_LOCK).unwrap();
    let observed = observe(&project);
    let bound =
        BoundPythonLock::acquire(&path, Default::default(), context.policy().deadline).unwrap();
    let fixture = if source {
        ProjectRegistryFixture::deterministic_source_dependency().0
    } else {
        ProjectRegistryFixture::wheels()
    };
    let endpoints = [fixture.endpoint()];
    let policy = PythonSourcePolicy::Explicit(PythonRegistryId::new("corp").unwrap());
    let mut registry = fixture.context(&endpoints, &policy);
    if source {
        registry.candidate_policy.sdists =
            crate::packages::python_resolution::candidate_policy::Sdists::Include;
    }
    registry.deadline = context.policy().deadline;
    let closed = resolve_exact_roots(
        &context,
        &language,
        &bound,
        &[("demo-pkg", "1.0")],
        registry,
    )
    .await
    .unwrap();
    let facts = closed.package_facts(&language).unwrap();
    let graph = closed
        .projection(&facts.iter().map(|fact| fact.key).collect::<Vec<_>>())
        .unwrap();
    let fingerprint = closed.fingerprint().unwrap();
    let model = crate::programs::install::lock::project_lock_model(
        &bound,
        &language,
        &graph,
        &[("demo-pkg".into(), "1.0".into())],
        closed.builds(),
    )
    .unwrap();
    let document = crate::programs::install::lock_document::prepare_document(
        &bound, &observed, &language, model,
    )
    .unwrap();
    let (archives, evidence) = closed.into_sources().unwrap();
    let sources = archives
        .into_iter()
        .map(|archive| {
            let fact = facts
                .iter()
                .find(|fact| fact.name == archive.manifest().name())
                .unwrap()
                .clone();
            (fact, archive)
        })
        .collect();
    let layout = crate::libraries::package::layouts::prepare_layout(
        &context,
        &language,
        fingerprint,
        sources,
    )
    .unwrap();
    let layout_key = layout.key();
    let language_key = language.key();
    evidence
        .recheck(tokio::time::Instant::from_std(context.policy().deadline))
        .unwrap();
    drop(layout);
    drop(evidence);
    drop(bound);
    drop(language);
    drop(fixture);
    PackageFixture {
        context,
        project,
        observed,
        language: language_key,
        layout: layout_key,
        graph,
        document,
    }
}

fn commit_fixture(fixture: &PackageFixture) -> Result<SelectionCommitReceipt> {
    commit_package_selection_with_lock(
        prepare_package_selection_commit(
            &fixture.context,
            &fixture.observed,
            fixture.language,
            fixture.layout,
            fixture.graph.clone(),
        )?,
        &fixture.project.join("pakro.lock"),
        &fixture.document,
    )
}

#[tokio::test]
async fn p2_package_lock_commit_reaches_catalog_with_canonical_wheel_source_pins() {
    use std::os::unix::fs::MetadataExt;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let fixture = package_fixture(&root, false).await;
    assert_eq!(fixture.graph.nodes.len(), 2);
    for node in &fixture.graph.nodes {
        assert!(node.pin.build.is_none());
        let reference = language_digest(
            *crate::packages::python_resolution::lock_model::PinId::parse_hex(&node.source_pin)
                .unwrap()
                .as_bytes(),
        );
        assert!(reference.starts_with("sha256-"));
    }
    let receipt = commit_fixture(&fixture).unwrap();
    assert_eq!(receipt.revision(), 1);
    // The first commit replaced this snapshot's lock. Its descriptor-backed
    // namespace receipts are no longer inputs to any later operation.
    drop(fixture.observed);
    assert_eq!(
        std::fs::read(fixture.project.join("pakro.lock")).unwrap(),
        fixture.document
    );
    let observed = observe(&fixture.project);
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    lock_transaction::ensure_project_consistent(&fixture.context, &observed).unwrap();
    let repository = ProjectRepository::open_readonly(&fixture.context).unwrap();
    let selected = repository
        .read_views()
        .package_graph(&identity)
        .unwrap()
        .unwrap();
    assert_eq!(selected.fingerprint(), fixture.graph.fingerprint().unwrap());
    assert_eq!(selected.nodes().len(), 2);
    drop(repository);
    drop(observed);

    // The lower commit boundary also refuses a valid Ready layout when an
    // externally edited precise lock describes different complete edges.
    let mut changed = std::str::from_utf8(&fixture.document)
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let group = changed["python"]["groups"]
        .as_array_of_tables_mut()
        .unwrap()
        .get_mut(0)
        .unwrap();
    group["edges"]
        .as_array_of_tables_mut()
        .unwrap()
        .get_mut(0)
        .unwrap()["declaration"] = toml_edit::value("dep>=1.0");
    let changed = changed.to_string().into_bytes();
    crate::packages::python_resolution::lock_session::decode_root(&changed, Default::default())
        .unwrap();
    std::fs::write(fixture.project.join("pakro.lock"), &changed).unwrap();
    let changed_observed = observe(&fixture.project);
    let prepared = prepare_package_selection_commit(
        &fixture.context,
        &changed_observed,
        fixture.language,
        fixture.layout,
        fixture.graph.clone(),
    )
    .unwrap();
    assert!(commit_package_selection(prepared).is_err());
    assert_eq!(
        std::fs::read(fixture.project.join("pakro.lock")).unwrap(),
        changed
    );
    assert_eq!(
        ProjectRepository::open_readonly(&fixture.context)
            .unwrap()
            .read_views()
            .registered_projects()[0]
            .revision(),
        1
    );
    drop(changed_observed);

    std::fs::write(fixture.project.join("pakro.lock"), &fixture.document).unwrap();
    let observed = observe(&fixture.project);
    let query = open_query_context(&fixture.context, &observed).unwrap();
    let SelectionReport::Selected(candidate) = select_existing(&query).unwrap() else {
        panic!("language disappeared")
    };
    assert!(
        commit_selection(
            prepare_selection_commit(&fixture.context, &observed, &candidate).unwrap()
        )
        .is_err(),
        "a language-only commit cannot discard a locked package graph"
    );
    drop(query);
    let before = std::fs::metadata(fixture.project.join("pakro.lock"))
        .unwrap()
        .ino();
    let prepared = prepare_package_selection_commit(
        &fixture.context,
        &observed,
        fixture.language,
        fixture.layout,
        fixture.graph.clone(),
    )
    .unwrap();
    assert_eq!(commit_package_selection(prepared).unwrap().revision(), 2);
    assert_eq!(
        std::fs::read(fixture.project.join("pakro.lock")).unwrap(),
        fixture.document
    );
    assert_eq!(
        std::fs::metadata(fixture.project.join("pakro.lock"))
            .unwrap()
            .ino(),
        before
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2_package_lock_commit_reaches_catalog_with_recursive_source_build_inputs() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let fixture = package_fixture(&root, true).await;
    let build = fixture.graph.nodes[0].pin.build.as_ref().unwrap();
    assert!(build.inputs.iter().any(|input| input.source_is_sdist));
    assert!(build.inputs.iter().any(|input| !input.source_is_sdist));
    assert!(
        !build
            .observation
            .as_ref()
            .unwrap()
            .initial_inputs
            .is_empty()
    );
    assert_eq!(commit_fixture(&fixture).unwrap().revision(), 1);
    let observed = observe(&fixture.project);
    lock_transaction::ensure_project_consistent(&fixture.context, &observed).unwrap();
    let identity = ProjectIdentity::from_observation(&observed).unwrap();
    let repository = ProjectRepository::open_readonly(&fixture.context).unwrap();
    let selected = repository
        .read_views()
        .package_graph(&identity)
        .unwrap()
        .unwrap();
    assert_eq!(selected.fingerprint(), fixture.graph.fingerprint().unwrap());
    assert_eq!(selected.nodes()[0].pin.build.as_ref().unwrap(), build);
    assert_eq!(
        std::fs::read(fixture.project.join("pakro.lock")).unwrap(),
        fixture.document
    );
}

#[tokio::test]
async fn p2_post_document_declaration_edits_stop_catalog_commit_and_survive_recovery() {
    for filename in ["pakro.toml"] {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let fixture = package_fixture(&root, false).await;
        let path = fixture.project.join(filename);
        let changed = format!(
            "{}# external edit during document publication\n",
            std::fs::read_to_string(&path).unwrap()
        )
        .into_bytes();
        let edit_path = path.clone();
        let edit_bytes = changed.clone();
        let result = lock_transaction::with_commit_test_hook(
            move |stage| {
                if stage == "documents-applied" {
                    std::fs::write(&edit_path, &edit_bytes).unwrap();
                }
            },
            || commit_fixture(&fixture),
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), changed);
        assert!(
            ProjectRepository::open_readonly(&fixture.context)
                .unwrap()
                .model()
                .registered_projects()
                .next()
                .is_none()
        );
        assert!(
            lock_transaction::ensure_project_consistent(
                &fixture.context,
                &observe(&fixture.project)
            )
            .is_err()
        );
        assert!(
            lock_transaction::recover_project_lock(&fixture.context, &observe(&fixture.project))
                .unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), changed);
        assert_eq!(
            std::fs::read(fixture.project.join("pakro.lock")).unwrap(),
            INITIAL_LOCK
        );
        assert_eq!(
            std::fs::read(fixture.project.join("untouched.txt")).unwrap(),
            b"user content stays\n"
        );
    }
}

#[tokio::test]
async fn p3_ignored_user_file_edit_after_lock_publication_does_not_revoke_commit() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let fixture = package_fixture(&root, false).await;
    let pyproject = fixture.project.join("pyproject.toml");
    let requirements = fixture.project.join("requirements.txt");
    let edit_pyproject = pyproject.clone();
    let edit_requirements = requirements.clone();
    let receipt = lock_transaction::with_commit_test_hook(
        move |stage| {
            if stage == "documents-applied" {
                std::fs::write(&edit_pyproject, b"invalid TOML is not a Pakro input\n").unwrap();
                std::fs::write(&edit_requirements, b"unobserved==999\n").unwrap();
            }
        },
        || commit_fixture(&fixture),
    )
    .unwrap();
    assert_eq!(receipt.revision(), 1);
    assert_eq!(
        std::fs::read(&pyproject).unwrap(),
        b"invalid TOML is not a Pakro input\n"
    );
    assert_eq!(std::fs::read(&requirements).unwrap(), b"unobserved==999\n");
    assert_eq!(
        std::fs::read(fixture.project.join("pakro.lock")).unwrap(),
        fixture.document
    );
    lock_transaction::ensure_project_consistent(&fixture.context, &observe(&fixture.project))
        .unwrap();
    assert_eq!(
        ProjectRepository::open_readonly(&fixture.context)
            .unwrap()
            .model()
            .registered_projects()
            .count(),
        1
    );
}

#[tokio::test]
#[ignore = "subprocess-only crash/recovery helper; not an acceptance case"]
async fn lock_commit_crash_child_helper() {
    let root = PathBuf::from(std::env::var_os("PAKRO_TEST_P2_LOCK_ROOT").expect("child root"));
    match std::env::var("PAKRO_TEST_P2_LOCK_CHILD").unwrap().as_str() {
        "commit" => {
            let fixture = package_fixture(&root, false).await;
            commit_fixture(&fixture).unwrap();
            panic!("requested durable cutpoint was not reached");
        }
        "recover" => {
            let context = context_with_lock(&root.join("stage"), true);
            let project = root.join("project");
            let observed = observe(&project);
            assert!(lock_transaction::recover_project_lock(&context, &observed).unwrap());
            lock_transaction::ensure_project_consistent(&context, &observe(&project)).unwrap();
            println!("PAKRO_P2_FRESH_PROCESS_RECOVERY=complete");
        }
        other => panic!("unknown child operation {other}"),
    }
}

#[test]
fn p2_real_process_termination_and_fresh_recovery_cover_durable_commit_windows() {
    use std::os::unix::fs::MetadataExt;
    for stage in ["documents-staged", "before-catalog", "after-catalog"] {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", HELPER, "--ignored", "--nocapture"])
            .env("PAKRO_TEST_P2_LOCK_ROOT", &root)
            .env("PAKRO_TEST_P2_LOCK_CHILD", "commit")
            .env("PAKRO_TEST_P2_LOCK_CUTPOINT", stage)
            .output()
            .unwrap();
        assert_eq!(
            child.status.code(),
            Some(86),
            "cutpoint {stage} was not reached: {} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        let context = context_with_lock(&root.join("stage"), true);
        let project = root.join("project");
        let observed = observe(&project);
        assert!(lock_transaction::ensure_project_consistent(&context, &observed).is_err());
        let before_bytes = std::fs::read(project.join("pakro.lock")).unwrap();
        let before_inode = std::fs::metadata(project.join("pakro.lock")).unwrap().ino();
        let repository = ProjectRepository::open_readonly(&context).unwrap();
        assert_eq!(
            repository.model().registered_projects().count(),
            usize::from(stage == "after-catalog")
        );
        drop(repository);
        let temporaries = || {
            std::fs::read_dir(&project)
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".pakro.lock.txn-")
                })
                .count()
        };
        assert_eq!(temporaries(), usize::from(stage == "documents-staged"));
        let recovery = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", HELPER, "--ignored", "--nocapture"])
            .env("PAKRO_TEST_P2_LOCK_ROOT", &root)
            .env("PAKRO_TEST_P2_LOCK_CHILD", "recover")
            .env_remove("PAKRO_TEST_P2_LOCK_CUTPOINT")
            .output()
            .unwrap();
        assert!(
            recovery.status.success(),
            "fresh recovery {stage} failed: {} {}",
            String::from_utf8_lossy(&recovery.stdout),
            String::from_utf8_lossy(&recovery.stderr)
        );
        assert!(
            String::from_utf8_lossy(&recovery.stdout)
                .contains("PAKRO_P2_FRESH_PROCESS_RECOVERY=complete")
        );
        assert_eq!(temporaries(), 0);
        let expected = if stage == "after-catalog" {
            before_bytes.as_slice()
        } else {
            INITIAL_LOCK
        };
        assert_eq!(std::fs::read(project.join("pakro.lock")).unwrap(), expected);
        if stage == "after-catalog" {
            assert_eq!(
                std::fs::metadata(project.join("pakro.lock")).unwrap().ino(),
                before_inode
            );
        }
        assert_eq!(
            ProjectRepository::open_readonly(&context)
                .unwrap()
                .model()
                .registered_projects()
                .count(),
            usize::from(stage == "after-catalog")
        );
        lock_transaction::ensure_project_consistent(&context, &observe(&project)).unwrap();
        assert_eq!(
            std::fs::read(project.join("untouched.txt")).unwrap(),
            b"user content stays\n"
        );
    }
}
