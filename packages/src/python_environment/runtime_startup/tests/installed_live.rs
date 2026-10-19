//! Explicit existing-runtime smoke tests; no network and no manager delegation.

use super::super::installed::verify_materialized_environment;
use super::*;
use crate::artifact_snapshot::spool_verified_reader;
use crate::packages::python_target::wheel_target;
use crate::packages::python_wheel::{WheelExpectation, WheelLimits};
use crate::packages::python_wheel_install::{
    WheelInstallLimits, WheelInstallTarget, materialize_wheel_graph, prepare_wheel_graph,
};
use crate::packages::python_wheel_plan::WheelPlanLimits;
use crate::packages::python_wheel_source::{InspectedWheelArchive, inspect_owned_wheel};
use pakro_index::SupportedWheelTags;
use pakro_store::Digest;
use std::io::{Cursor, Write};

async fn live_environment(
    label: &str,
) -> (tempfile::TempDir, UnpublishedPrefix, EmptyEnvironmentLayout) {
    let value = |suffix: &str| {
        std::env::var_os(format!("PAKRO_TEST_{label}_{suffix}"))
            .expect("explicit existing-runtime test input is required")
    };
    let executable = PathBuf::from(value("EXECUTABLE"));
    let source_root = PathBuf::from(value("SOURCE_ROOT")).canonicalize().unwrap();
    let handle = PythonRuntimeHandle::from_existing_executable(&executable).unwrap();
    let descriptor = crate::python_runtime::probe(&handle, Duration::from_secs(10))
        .await
        .unwrap();
    let mut inputs = vec![executable_pin(handle.executable())];
    if let Some(paths) = std::env::var_os(format!("PAKRO_TEST_{label}_LOADER_FILES")) {
        inputs.extend(
            std::env::split_paths(&paths)
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| {
                    let pin = pin(&path.canonicalize().unwrap());
                    RuntimeInputExpectation {
                        path: pin.path,
                        sha256: pin.sha256,
                        size: pin.size,
                        mode: pin.mode,
                        purpose: RuntimeInputPurpose::LoaderPayload,
                    }
                }),
        );
    }
    let owner = tempfile::tempdir().unwrap();
    let mut prefix = EnvironmentStore::open(
        &owner.path().canonicalize().unwrap().join("environments"),
        nix::unistd::geteuid().as_raw(),
        PrefixLimits::default(),
    )
    .unwrap()
    .allocate()
    .unwrap();
    let sources = capture_runtime_sources(
        &handle,
        &descriptor,
        RuntimeSourceBinding {
            trust: RuntimeSourceTrust::ExplicitExisting,
            authorized_roots: &[source_root],
            inputs: &inputs,
        },
        &prefix,
        RuntimeLayoutLimits::default(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let source = pin(&sources.layout().base_paths["stdlib"].join("site.py"));
    let loader = match value("STARTUP_LOADER").to_str().unwrap() {
        "source" => StartupLoaderKind::Source,
        "frozen" => StartupLoaderKind::Frozen,
        _ => panic!("explicit observed source/frozen loader required"),
    };
    let cache = if loader == StartupLoaderKind::Source {
        let path = PathBuf::from(value("STARTUP_CACHE_PATH"));
        Some(if path.try_exists().unwrap() {
            StartupCacheBinding::Pinned(pin(&path))
        } else {
            StartupCacheBinding::Missing { path }
        })
    } else {
        None
    };
    let runtime = bootstrap_runtime(
        prepare_runtime_bootstrap(sources, &prefix, RuntimeBootstrapLimits::default()).unwrap(),
        &mut prefix,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let evidence = establish_cfg_recognition(
        &runtime,
        &prefix,
        StartupAdapterBinding {
            source,
            cache,
            loader,
        },
        RuntimeStartupLimits::default(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let layout = verify_empty_environment_layout(
        runtime,
        &mut prefix,
        evidence,
        RuntimeStartupLimits::default(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    (owner, prefix, layout)
}

fn inspect(
    bytes: Vec<u8>,
    filename: &str,
    name: &str,
    supported: &SupportedWheelTags,
    scratch: &Path,
) -> InspectedWheelArchive {
    let digest = Digest::Sha512(<sha2::Sha512 as sha2::Digest>::digest(&bytes).into());
    let artifact = spool_verified_reader(
        Cursor::new(&bytes),
        filename,
        digest,
        bytes.len() as u64,
        scratch,
    )
    .unwrap();
    inspect_owned_wheel(
        artifact,
        digest,
        &WheelExpectation {
            filename,
            name,
            version: "1.0",
            supported,
        },
        WheelLimits::default(),
    )
    .unwrap()
}

fn pure_archive(supported: &SupportedWheelTags, scratch: &Path) -> InspectedWheelArchive {
    let info = "pakro_runtime_smoke-1.0.dist-info";
    let mut entries = vec![
        (
            "pakro_runtime_smoke.py".to_owned(),
            b"VALUE=42\ndef main():\n    return 0\n".to_vec(),
        ),
        ("资源/a,\"b\n.txt".to_owned(), b"CSV resource\n".to_vec()),
        (
            format!("{info}/WHEEL"),
            b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_vec(),
        ),
        (
            format!("{info}/METADATA"),
            b"Metadata-Version: 2.6\nName: pakro-runtime-smoke\nVersion: 1.0\n".to_vec(),
        ),
        (
            format!("{info}/entry_points.txt"),
            b"[console_scripts]\npakro-runtime-smoke = pakro_runtime_smoke:main\n".to_vec(),
        ),
    ];
    let mut record = String::new();
    for (path, bytes) in &entries {
        let hash = <sha2::Sha256 as sha2::Digest>::digest(bytes);
        let hash = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, hash);
        let escaped = if path.contains([',', '"', '\r', '\n']) {
            format!("\"{}\"", path.replace('"', "\"\""))
        } else {
            path.clone()
        };
        record.push_str(&format!("{escaped},sha256={hash},{}\n", bytes.len()));
    }
    record.push_str(&format!("{info}/RECORD,,\n"));
    entries.push((format!("{info}/RECORD"), record.into_bytes()));
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in entries {
        zip.start_file(
            path,
            zip::write::SimpleFileOptions::default().unix_permissions(0o644),
        )
        .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    inspect(
        zip.finish().unwrap().into_inner(),
        "pakro_runtime_smoke-1.0-py3-none-any.whl",
        "pakro-runtime-smoke",
        supported,
        scratch,
    )
}

async fn live_installed(label: &str, native: bool) {
    let (owner, mut prefix, layout) = live_environment(label).await;
    let scratch = owner.path().canonicalize().unwrap().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let supported = wheel_target(layout.runtime.observed_site_disabled())
        .unwrap()
        .supported;
    let archive = if native {
        let path = PathBuf::from(
            std::env::var_os(format!("PAKRO_TEST_{label}_NATIVE_WHEEL"))
                .expect("explicit golden native wheel for the selected runtime"),
        );
        assert!(path.is_absolute());
        inspect(
            fs::read(&path).unwrap(),
            path.file_name().unwrap().to_str().unwrap(),
            "pakro-native-demo",
            &supported,
            &scratch,
        )
    } else {
        pure_archive(&supported, &scratch)
    };
    let graph = prepare_wheel_graph(
        vec![archive],
        &WheelInstallTarget {
            scheme: layout.actual_scheme(),
            interpreter: layout.executable(),
            reserved: layout.reserved(),
        },
        WheelLimits::default(),
        WheelPlanLimits::default(),
        WheelInstallLimits::default(),
        &scratch,
    )
    .unwrap();
    let receipt = materialize_wheel_graph(graph, &mut prefix).unwrap();
    let verified = verify_materialized_environment(
        &layout,
        &receipt,
        &prefix,
        RuntimeStartupLimits::default(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(verified.sources(), receipt.sources());
    assert_eq!(verified.files(), receipt.files());
    assert_eq!(verified.directories(), receipt.directories());
    assert_ne!(verified.inventory_sha256(), &[0; 32]);
    let import_program = if native {
        "import pakro_native_demo;assert pakro_native_demo.answer()==42;import pakro_native_pure,pakro_native_platform;assert pakro_native_pure.VALUE==43 and pakro_native_platform.VALUE==44"
    } else {
        "import pakro_runtime_smoke;assert pakro_runtime_smoke.VALUE==42"
    };
    let program = format!(
        "import sys,csv,pathlib,hashlib,base64\n\
         sys.path[:0]=sys.argv[1:]\n\
         records=set()\n\
         for root in sys.argv[1:]:\n\
         \x20for record in pathlib.Path(root).glob('*.dist-info/RECORD'):\n\
         \x20\x20records.add(record)\n\
         \x20\x20with record.open(encoding='utf-8',newline='') as stream:\n\
         \x20\x20\x20for path,digest,size in csv.reader(stream):\n\
         \x20\x20\x20\x20if not digest:\n\
         \x20\x20\x20\x20\x20assert path.endswith('/RECORD') and not size;continue\n\
         \x20\x20\x20\x20contents=(record.parent.parent/path).read_bytes()\n\
         \x20\x20\x20\x20algorithm,value=digest.split('=',1)\n\
         \x20\x20\x20\x20assert len(contents)==int(size)\n\
         \x20\x20\x20\x20assert base64.urlsafe_b64encode(hashlib.new(algorithm,contents).digest()).rstrip(b'=').decode()==value\n\
         assert len(records)==1\n{import_program}\n"
    );
    let mut child = tokio::process::Command::new(layout.executable());
    child
        .args(["-E", "-s", "-S", "-B", "-c", &program])
        .arg(layout.actual_scheme().root(WheelScheme::Purelib).unwrap())
        .arg(layout.actual_scheme().root(WheelScheme::Platlib).unwrap())
        .env_clear()
        .env("LC_ALL", "C")
        .current_dir(&scratch)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), child.output())
        .await
        .expect("known tiny module smoke deadline")
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    if !native {
        let wrapper = layout
            .actual_scheme()
            .root(WheelScheme::Scripts)
            .unwrap()
            .join("pakro-runtime-smoke");
        assert!(
            receipt
                .files()
                .iter()
                .any(|file| { prefix.final_path().join(file.relative_path()) == wrapper })
        );
        let mut child = tokio::process::Command::new(wrapper);
        child
            .env_clear()
            .env("LC_ALL", "C")
            // This smoke still owns an unpublished inventory. Normal command
            // execution must not add bytecode outside its recorded C4 outputs.
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .current_dir(&scratch)
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(10), child.output())
            .await
            .expect("known tiny console wrapper smoke deadline")
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    if let Err(error) = prefix.check_binding() {
        let sites = [WheelScheme::Purelib, WheelScheme::Platlib]
            .into_iter()
            .map(|kind| {
                let root = layout.actual_scheme().root(kind).unwrap();
                let names = fs::read_dir(root)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<Vec<_>>();
                (kind, names)
            })
            .collect::<Vec<_>>();
        panic!("post-smoke prefix validation failed: {error:#}; site entries: {sites:?}");
    }
}

#[tokio::test]
#[ignore = "requires explicit CPYTHON39 runtime/source/loader/site inputs; no network"]
async fn live_cpython39_complete_pure_wheel_in_final_prefix() {
    live_installed("CPYTHON39", false).await;
}

#[tokio::test]
#[ignore = "requires explicit policy-compliant CPYTHON311 runtime/source/frozen-site inputs; no network"]
async fn live_cpython311_complete_pure_wheel_in_final_prefix() {
    live_installed("CPYTHON311", false).await;
}

#[tokio::test]
#[ignore = "requires explicit PYPY312 runtime/source/loader/site-cache inputs; no network"]
async fn live_pypy312_complete_pure_wheel_in_final_prefix() {
    live_installed("PYPY312", false).await;
}

#[tokio::test]
#[ignore = "requires explicit policy-compliant CPYTHON311 runtime and its golden native wheel; no network"]
async fn live_cpython311_native_extension_in_final_prefix() {
    live_installed("CPYTHON311", true).await;
}

#[tokio::test]
#[ignore = "requires explicit PYPY312 runtime and its golden native wheel; no network"]
async fn live_pypy312_native_extension_in_final_prefix() {
    live_installed("PYPY312", true).await;
}
