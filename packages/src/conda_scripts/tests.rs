use super::*;
use crate::libraries::language::repository::tests::operation_context;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the newly built Pakro bootstrap and native sandbox"]
async fn staged_script_modifies_only_the_supplied_private_prefix() {
    let root = tempfile::tempdir().unwrap();
    let context = operation_context(root.path(), true, Duration::from_secs(30));
    let outside = root.path().join("outside");
    let script = format!(
        "test -z \"${{PAKRO_TEST_SECRET-}}\" || exit 20\nread -r value < \"$PREFIX/input\"\nprintf '%s changed\\n' \"$value\" > \"$PREFIX/output\"\nif (printf bad > '{}') 2>/dev/null; then exit 21; fi\nprintf done\n",
        outside.display()
    );
    let staged = run_staged(&context, script.as_bytes(), |prefix| {
        fs::write(prefix.join("input"), b"real staging input\n")?;
        fs::create_dir(prefix.join("lib"))?;
        std::os::unix::fs::symlink("../output", prefix.join("lib/linked"))?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        fs::read(staged.prefix().join("output")).unwrap(),
        b"real staging input changed\n"
    );
    assert_eq!(staged.output().stdout, b"done");
    assert!(!staged.programs().is_empty());
    assert!(!outside.exists());
    let store = crate::packages::python_environment::prefix::EnvironmentStore::open(
        &root.path().canonicalize().unwrap().join("layout-store"),
        nix::unistd::geteuid().as_raw(),
        crate::packages::python_environment::prefix::PrefixLimits::default(),
    )
    .unwrap();
    let mut layout = store.allocate().unwrap();
    let inventory = staged.import_layout(&context, &mut layout).unwrap();
    assert_eq!(
        fs::read(layout.final_path().join("output")).unwrap(),
        b"real staging input changed\n"
    );
    assert_eq!(
        fs::read_link(layout.final_path().join("lib/linked")).unwrap(),
        Path::new("../output")
    );
    assert_eq!(inventory.entries().len(), 4);
    let path = staged.workspace.path.clone();
    drop(staged);
    assert!(!path.exists());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the newly built Pakro bootstrap and native sandbox"]
async fn staged_prefix_embedded_in_output_cannot_be_imported() {
    let root = tempfile::tempdir().unwrap();
    let context = operation_context(root.path(), true, Duration::from_secs(30));
    let staged = run_staged(
        &context,
        b"printf '%s' \"$PREFIX\" > \"$PREFIX/path\"\n",
        |_| Ok(()),
    )
    .await
    .unwrap();
    let store = crate::packages::python_environment::prefix::EnvironmentStore::open(
        &root.path().canonicalize().unwrap().join("layout-store"),
        nix::unistd::geteuid().as_raw(),
        crate::packages::python_environment::prefix::PrefixLimits::default(),
    )
    .unwrap();
    let mut layout = store.allocate().unwrap();
    assert!(staged.import_layout(&context, &mut layout).is_err());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the newly built Pakro bootstrap and native sandbox"]
async fn post_link_runs_at_its_original_staged_path_with_package_identity() {
    use rattler_conda_types::{
        PackageName, PackageRecord, RepoDataRecord, Version,
        package::{ArchiveIdentifier, CondaArchiveType, DistArchiveIdentifier, DistArchiveType},
    };
    use std::str::FromStr;
    let record = RepoDataRecord {
        package_record: PackageRecord::new(
            PackageName::from_str("demo").unwrap(),
            Version::from_str("1.0").unwrap(),
            "build_0".into(),
        ),
        identifier: DistArchiveIdentifier::new(
            ArchiveIdentifier {
                name: "demo".into(),
                version: "1.0".into(),
                build_string: "build_0".into(),
            },
            DistArchiveType::Conda(CondaArchiveType::Conda),
        ),
        url: "https://example.invalid/demo.conda".parse().unwrap(),
        channel: None,
    };
    let root = tempfile::tempdir().unwrap();
    let context = operation_context(root.path(), true, Duration::from_secs(30));
    let staged = run_post_link(&context, &record, |prefix| {
        fs::create_dir(prefix.join("bin"))?;
        fs::write(prefix.join("bin/.demo-post-link.sh"), b"#!/bin/sh\ntest \"$0\" = \"$PREFIX/bin/.demo-post-link.sh\" || exit 2\ntest \"$PKG_NAME\" = demo || exit 3\ntest \"$PKG_VERSION\" = 1.0 || exit 4\nprintf done > \"$PREFIX/result\"\n")?;
        Ok(())
    }).await.unwrap();
    assert_eq!(fs::read(staged.prefix().join("result")).unwrap(), b"done");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the newly built Pakro bootstrap and native sandbox"]
async fn cancellation_stops_an_actually_running_staged_script() {
    let root = tempfile::tempdir().unwrap();
    let context = operation_context(root.path(), true, Duration::from_secs(30));
    let started_path = Arc::new(Mutex::new(None));
    let witness = started_path.clone();
    let cancellation = context.cancellation_token();
    let run = run_staged(
        &context,
        b": > \"$PREFIX/started\"\nwhile :; do :; done\n",
        move |prefix| {
            *witness.lock().unwrap() = Some(prefix.join("started"));
            Ok(())
        },
    );
    tokio::pin!(run);
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        tokio::select! {
            _result = &mut run => panic!("script ended before cancellation"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        if started_path
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|path| path.exists())
        {
            break;
        }
        assert!(Instant::now() < until, "script never started");
    }
    cancellation.cancel();
    assert!(run.await.is_err());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the newly built Pakro bootstrap and native sandbox"]
async fn deadline_stops_a_staged_script() {
    let root = tempfile::tempdir().unwrap();
    let context = operation_context(root.path(), true, Duration::from_secs(3));
    assert!(
        run_staged(&context, b"while :; do :; done\n", |_| Ok(()))
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the newly built Pakro bootstrap and native sandbox"]
async fn failed_script_does_not_return_importable_output() {
    let root = tempfile::tempdir().unwrap();
    let context = operation_context(root.path(), true, Duration::from_secs(30));
    let mut workspace = None;
    let result = run_staged(
        &context,
        b"printf partial > \"$PREFIX/output\"\nprintf 'explicit failure' >&2\nexit 7\n",
        |prefix| {
            workspace = Some(prefix.parent().unwrap().parent().unwrap().to_owned());
            Ok(())
        },
    ).await;
    let error = match result {
        Ok(_) => panic!("a failed script must not return a result that can be imported"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("Conda staging script failed") && error.contains("explicit failure"), "{error}");
    assert!(!workspace.unwrap().exists(), "failed script must finish cleanup before returning");
    assert!(root.path().is_dir());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires original Proj shell scripts, newly built bootstrap and native sandbox"]
async fn original_proj_activation_captures_license_condition_and_restorable_host_value() {
    let activate = fs::read(
        std::env::var_os("PAKRO_TEST_PROJ_ACTIVATE")
            .expect("original Proj activation script required"),
    )
    .unwrap();
    let deactivate = fs::read(
        std::env::var_os("PAKRO_TEST_PROJ_DEACTIVATE")
            .expect("original Proj deactivation script required"),
    )
    .unwrap();
    for license in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let context = operation_context(root.path(), true, Duration::from_secs(30));
        let active = Path::new("etc/conda/activate.d/proj4-activate.sh");
        let inactive = Path::new("etc/conda/deactivate.d/proj4-deactivate.sh");
        let staged = run_activation(&context, "proj", active, inactive, |prefix| {
            fs::create_dir_all(prefix.join("etc/conda/activate.d"))?;
            fs::create_dir_all(prefix.join("etc/conda/deactivate.d"))?;
            fs::create_dir_all(prefix.join("share/proj"))?;
            fs::write(prefix.join(active), &activate)?;
            fs::write(prefix.join(inactive), &deactivate)?;
            if license {
                fs::write(
                    prefix.join("share/proj/copyright_and_licenses.csv"),
                    b"license",
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let plan = staged.activation().unwrap();
        let original = BTreeMap::from([("PROJ_DATA".to_owned(), "/original/proj".to_owned())]);
        let applied = plan.bind(Path::new("/final/prefix"), &original).unwrap();
        assert_eq!(
            applied["PROJ_DATA"].as_deref(),
            Some("/final/prefix/share/proj")
        );
        assert_eq!(
            applied["PROJ_NETWORK"].as_deref(),
            Some(if license { "OFF" } else { "ON" })
        );
        assert_eq!(
            applied["_CONDA_SET_PROJ_DATA"].as_deref(),
            Some("/original/proj")
        );
        assert!(
            !plan
                .bind(Path::new("/final/prefix"), &BTreeMap::new())
                .unwrap()
                .contains_key("_CONDA_SET_PROJ_DATA")
        );
    }
}
