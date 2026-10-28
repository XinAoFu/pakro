use super::*;
use crate::config::FrozenConfigDiscovery;
use crate::libraries::project::snapshot::{ReadPolicy, observe_project};
use crate::packages::python_resolution::lock_model::PythonLockModel;
use std::time::{Duration, Instant};

fn observe(root: &Path) -> ObservedProjectInputs {
    let mut discovery = FrozenConfigDiscovery::new(vec!["pakro.toml".into()]);
    discovery.ceiling_paths = vec![root.parent().unwrap().to_path_buf()];
    observe_project(
        root,
        ReadPolicy::new(discovery, Instant::now() + Duration::from_secs(30)),
    )
    .unwrap()
}

fn archive_with_patchable_sysconfig() -> Vec<u8> {
    use flate2::{Compression, read::GzDecoder, write::GzEncoder};
    let original = crate::libraries::language::repository::tests::archive("3.15.0");
    let mut input = tar::Archive::new(GzDecoder::new(original.as_slice()));
    let mut output = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for entry in input.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().into_owned();
        let mut header = entry.header().clone();
        output.append_data(&mut header, path, &mut entry).unwrap();
    }
    let sysconfig = concat!(
        "# system configuration generated and used by the sysconfig module\n",
        "build_time_vars = {\"BINDIR\": \"/install/bin\", \"CFLAGS\": \"-I/install/include\"}\n",
    )
    .as_bytes();
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(sysconfig.len() as u64);
    header.set_cksum();
    output
        .append_data(
            &mut header,
            "bundle/lib/python3.15/_sysconfigdata__darwin_darwin.py",
            sysconfig,
        )
        .unwrap();
    output.into_inner().unwrap().finish().unwrap()
}

#[test]
fn p2_query_validates_declaration_commitment_without_creating_a_guard() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    std::fs::write(root.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let observed = observe(&root);
    let fingerprint =
        crate::programs::install::lock_document::declaration_fingerprint(&observed).unwrap();
    let model = PythonLockModel::from_parts(&[], &[], Default::default())
        .unwrap()
        .with_declaration_fingerprint(fingerprint);
    let document = format!(
        "lockfile_version=4\n{}",
        model.encode_projection(Default::default()).unwrap()
    );
    std::fs::write(root.join("pakro.lock"), &document).unwrap();
    let observed = observe(&root);
    let current = read_project_lock_model(&observed).unwrap().unwrap();
    crate::programs::install::lock_document::validate_declarations(&observed, &current).unwrap();
    assert!(!root.join(".pakro.lock.guard").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
        document
    );
    std::fs::write(
        root.join("pakro.toml"),
        "[tools]\npython='3.15.0'\n# changed declaration bytes\n",
    )
    .unwrap();
    let changed = observe(&root);
    let current = read_project_lock_model(&changed).unwrap().unwrap();
    assert!(
        crate::programs::install::lock_document::validate_declarations(&changed, &current).is_err()
    );
    assert!(!root.join(".pakro.lock.guard").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("pakro.lock")).unwrap(),
        document
    );
}

#[tokio::test]
async fn p3_query_lock_applies_to_the_selected_instance_across_version_spellings() {
    use crate::libraries::language::repository::tests::{
        archive, install_spec, install_spec_for, operation_context, prepare,
    };
    use crate::packages::python_environment::model::PackageGraphRecord;
    use crate::packages::python_resolution::lock_session::BoundPythonLock;
    use crate::plugins::core::python::native::{RuntimeArchiveTarget, RuntimePlatform};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let stage = root.join("stage");
    let project = root.join("project");
    std::fs::create_dir(&stage).unwrap();
    std::fs::create_dir(&project).unwrap();
    let context = operation_context(&stage, true, Duration::from_secs(60));
    let bytes = archive_with_patchable_sysconfig();
    let spec = install_spec(&bytes);
    let language = prepare(&spec, &context, &bytes).await.unwrap();
    std::fs::write(project.join("pakro.toml"), "[tools]\npython='3.15.0'\n").unwrap();
    let observed = observe(&project);
    let seed = BoundPythonLock::acquire(
        &stage.join("seed.lock"),
        Default::default(),
        context.policy().deadline,
    )
    .unwrap();
    let graph = PackageGraphRecord {
        roots: vec![],
        nodes: vec![],
        edges: vec![],
    };
    let model =
        crate::programs::install::lock::project_lock_model(&seed, &language, &graph, &[], &[])
            .unwrap();
    let document = crate::programs::install::lock_document::prepare_document(
        &seed, &observed, &language, model,
    )
    .unwrap();
    drop(seed);
    drop(observed);
    std::fs::write(project.join("pakro.lock"), &document).unwrap();
    let observed = observe(&project);
    for target in [None, Some("3.15.0"), Some("3.15.0+20261013"), Some("3.15")] {
        assert!(
            project_lock_for_selected_query(&context, &observed, target, &language)
                .unwrap()
                .is_some()
        );
    }
    let patched_spec =
        crate::libraries::language::repository::LanguageInstallSpec::from_plan(spec.plan(), true)
            .unwrap();
    let patched = prepare(&patched_spec, &context, &bytes).await.unwrap();
    assert_ne!(patched.key(), language.key());
    assert_eq!(
        patched.source_pin().unwrap(),
        language.source_pin().unwrap()
    );
    assert_eq!(
        patched.target().fingerprint(),
        language.target().fingerprint()
    );
    let sysconfig = "lib/python3.15/_sysconfigdata__darwin_darwin.py";
    assert!(
        std::fs::read_to_string(language.root().join(sysconfig))
            .unwrap()
            .contains("/install/bin")
    );
    assert!(
        std::fs::read_to_string(patched.root().join(sysconfig))
            .unwrap()
            .contains(&format!("{}/bin", patched.root().display()))
    );
    assert!(
        project_lock_for_selected_query(&context, &observed, Some("3.15.0+20261013"), &patched)
            .unwrap()
            .is_none(),
        "a different legal patch recipe must keep its independent instance scope"
    );
    let other_build_spec = install_spec_for(
        "3.15.0",
        &bytes,
        &RuntimeArchiveTarget::new("aarch64", RuntimePlatform::Macos).unwrap(),
        "20261014",
    );
    let other_build = prepare(&other_build_spec, &context, &bytes).await.unwrap();
    assert!(
        project_lock_for_selected_query(&context, &observed, Some("3.15.0+20261014"), &other_build)
            .unwrap()
            .is_none()
    );
    let other_bytes = archive("3.14.0");
    let other_spec = install_spec_for(
        "3.14.0",
        &other_bytes,
        &RuntimeArchiveTarget::new("aarch64", RuntimePlatform::Macos).unwrap(),
        "20261013",
    );
    let other = prepare(&other_spec, &context, &other_bytes).await.unwrap();
    assert!(
        project_lock_for_selected_query(&context, &observed, Some("3.14.0+20261013"), &other)
            .unwrap()
            .is_none()
    );
    drop(observed);
    let original = std::str::from_utf8(&document).unwrap();
    let invalid_key = original.replace(
        &format!(
            "content_tree = \"{}\"",
            hex::encode(language.key().fingerprint())
        ),
        &format!("content_tree = \"{}\"", "fe".repeat(32)),
    );
    let invalid_target = original.replace(
        &format!(
            "target = \"{}\"",
            hex::encode(language.target().fingerprint())
        ),
        &format!("target = \"{}\"", "fd".repeat(32)),
    );
    for invalid in [invalid_key, invalid_target] {
        assert_ne!(invalid, original);
        std::fs::write(project.join("pakro.lock"), &invalid).unwrap();
        let malformed = observe(&project);
        let locked = project_lock_for_selected_query(
            &context,
            &malformed,
            Some("3.15.0+20261013"),
            &language,
        )
        .unwrap()
        .expect("an invalid runtime key/target must not escape as an independent query");
        assert!(
            crate::programs::install::lock::validate_project_lock_model_roots(
                &locked,
                &language,
                &[]
            )
            .is_err(),
            "same-source damaged runtime facts must remain a hard query failure"
        );
        assert!(!project.join(".pakro.lock.guard").exists());
    }
    std::fs::write(project.join("pakro.lock"), &document).unwrap();
    std::fs::write(
        project.join("pakro.toml"),
        "[tools]\npython='3.15.0'\n# external edit\n",
    )
    .unwrap();
    let changed = observe(&project);
    assert!(
        project_lock_for_selected_query(&context, &changed, Some("3.15.0+20261013"), &language)
            .is_err()
    );
    assert!(!project.join(".pakro.lock.guard").exists());
    assert_eq!(std::fs::read(project.join("pakro.lock")).unwrap(), document);
}
