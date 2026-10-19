//! Real D1/D2/D3 and owned A/C1/C3/C4 prerequisites for installed verification.

use super::*;

use crate::artifact_snapshot::spool_verified_reader;
use crate::packages::python_wheel::{WheelExpectation, WheelLimits};
use crate::packages::python_wheel_install::{
    PreparedWheelGraph, WheelInstallLimits, WheelInstallTarget, WheelMaterializationReceipt,
    materialize_wheel_graph, prepare_wheel_graph,
};
use crate::packages::python_wheel_plan::{WheelPlanLimits, WheelReservedKind};
use crate::packages::python_wheel_source::{InspectedWheelArchive, inspect_owned_wheel};
use pakro_index::{SupportedWheelTags, WheelTag};
use pakro_store::Digest;
use std::io::{Cursor, Write};

pub(super) struct Environment {
    pub(super) prefix: UnpublishedPrefix,
    pub(super) layout: EmptyEnvironmentLayout,
    pub(super) scratch: PathBuf,
    pub(super) probes: PathBuf,
    pub(super) active: PathBuf,
    pub(super) cas: PathBuf,
    _owner: tempfile::TempDir,
}

impl Environment {
    pub(super) async fn new(distinct: bool) -> Self {
        let mut fixture = Fixture::with_distinct_sites(distinct).await;
        let evidence = fixture
            .recognize()
            .await
            .expect("real cfg recognition prerequisite");
        let layout = verify_empty_environment_layout(
            fixture.runtime,
            &mut fixture.prefix,
            evidence,
            RuntimeStartupLimits::default(),
            Duration::from_secs(5),
        )
        .await
        .expect("real normal empty-layout prerequisite");
        fixture.prefix.check_binding().unwrap();
        let root = fixture._owner.path().canonicalize().unwrap();
        let scratch = root.join("scratch");
        fs::create_dir(&scratch).unwrap();
        let active = root.join("active-sentinel");
        let cas = root.join("cas-sentinel");
        fs::write(&active, b"old active").unwrap();
        fs::write(&cas, b"old CAS").unwrap();
        Self {
            prefix: fixture.prefix,
            layout,
            scratch,
            probes: fixture.probes,
            active,
            cas,
            _owner: fixture._owner,
        }
    }
    pub(super) fn archive(
        &self,
        name: &str,
        payloads: Vec<(&str, &[u8], u32)>,
        sha512: bool,
    ) -> InspectedWheelArchive {
        let stem = name.replace('-', "_");
        let info = format!("{stem}-1.0.dist-info");
        let mut entries = payloads
            .into_iter()
            .map(|(name, bytes, mode)| (name.to_owned(), bytes.to_vec(), mode))
            .collect::<Vec<_>>();
        entries.push((
            format!("{info}/WHEEL"),
            b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_vec(),
            0o644,
        ));
        entries.push((
            format!("{info}/METADATA"),
            format!("Metadata-Version: 2.6\nName: {name}\nVersion: 1.0\n").into_bytes(),
            0o644,
        ));
        if name == "alpha-demo" {
            entries.push((
                format!("{info}/entry_points.txt"),
                b"[console_scripts]\npakro-demo = demo_package:main\n".to_vec(),
                0o644,
            ));
        }
        let mut record = String::new();
        for (path, bytes, _) in &entries {
            let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(bytes).into();
            let encoded =
                base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, digest);
            record.push_str(&format!(
                "{},sha256={},{}\n",
                csv(path),
                encoded,
                bytes.len()
            ));
        }
        record.push_str(&format!("{info}/RECORD,,\n"));
        entries.push((format!("{info}/RECORD"), record.into_bytes(), 0o644));
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, bytes, mode) in entries {
            zip.start_file(
                path,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(mode),
            )
            .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let digest = if sha512 {
            Digest::Sha512(<sha2::Sha512 as sha2::Digest>::digest(&bytes).into())
        } else {
            Digest::from_sha256_bytes(&bytes)
        };
        let filename = format!("{stem}-1.0-py3-none-any.whl");
        let snapshot = spool_verified_reader(
            Cursor::new(&bytes),
            &filename,
            digest,
            bytes.len() as u64,
            &self.scratch,
        )
        .expect("real verified outer artifact prerequisite");
        let supported =
            SupportedWheelTags::new(vec![WheelTag::new("py3", "none", "any").unwrap()]).unwrap();
        inspect_owned_wheel(
            snapshot,
            digest,
            &WheelExpectation {
                filename: &filename,
                name,
                version: "1.0",
                supported: &supported,
            },
            WheelLimits::default(),
        )
        .expect("real A/C1 inspected archive prerequisite")
    }
    pub(super) fn prepare(
        &self,
        archives: Vec<InspectedWheelArchive>,
    ) -> Result<PreparedWheelGraph> {
        prepare_wheel_graph(
            archives,
            &WheelInstallTarget {
                scheme: self.layout.actual_scheme(),
                interpreter: self.layout.executable(),
                reserved: self.layout.reserved(),
            },
            WheelLimits::default(),
            WheelPlanLimits::default(),
            WheelInstallLimits::default(),
            &self.scratch,
        )
    }
    pub(super) fn install(
        &mut self,
        name: &str,
        sha512: bool,
        traps: bool,
    ) -> WheelMaterializationReceipt {
        let data_root = format!("{}-1.0.data", name.replace('-', "_"));
        let header = format!("{data_root}/headers/demo.h");
        let data = format!("{data_root}/data/share/{name}/data.txt");
        let script = format!("{data_root}/scripts/{name}-legacy");
        let mut entries = vec![
            ("shared_ns/value.py", b"VALUE = 1\n".as_slice(), 0o644),
            (
                "demo_package.py",
                b"VALUE = 42\ndef main():\n    return 0\n".as_slice(),
                0o644,
            ),
            (header.as_str(), b"#define DEMO 42\n".as_slice(), 0o644),
            (data.as_str(), b"runtime-bound data\n".as_slice(), 0o644),
            (script.as_str(), b"#!/bin/sh\nexit 0\n".as_slice(), 0o755),
            ("资源/a,\"b\n.txt", b"CSV resource\n".as_slice(), 0o644),
        ];
        if traps {
            entries.push((
                "trap.pth",
                b"import unknown_pakro_trap; raise RuntimeError('must not execute')\n",
                0o644,
            ));
            entries.push((
                "sitecustomize.py",
                b"raise RuntimeError('must not execute customize')\n",
                0o644,
            ));
        }
        let archive = self.archive(name, entries, sha512);
        let prepared = self
            .prepare(vec![archive])
            .expect("real complete C2/C3 plan prerequisite");
        let receipt = materialize_wheel_graph(prepared, &mut self.prefix)
            .expect("real C4 controlled writes prerequisite");
        self.prefix.check_binding().unwrap();
        receipt
    }
    pub(super) fn assert_unpublished(&self) {
        assert_eq!(fs::read(&self.active).unwrap(), b"old active");
        assert_eq!(fs::read(&self.cas).unwrap(), b"old CAS");
    }
    pub(super) fn probe_log(&self) -> Vec<u8> {
        fs::read(&self.probes).unwrap_or_default()
    }
}

fn csv(text: &str) -> String {
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.into()
    }
}

#[tokio::test]
async fn normal_reserved_projection_contains_all_created_site_and_parent_directories() {
    let environment = Environment::new(true).await;
    for relative in [
        "lib",
        "lib/python3.11",
        "lib/python3.11/site-packages",
        "unused-platlib",
    ] {
        assert!(
            environment
                .layout
                .reserved()
                .iter()
                .any(|entry| entry.kind == WheelReservedKind::Directory
                    && entry.path == environment.prefix.final_path().join(relative)),
            "normal directory missing from reserved facts: {relative}"
        );
    }
    for relative in ["bin/python", "bin/python3", "pyvenv.cfg"] {
        assert!(
            environment
                .layout
                .reserved()
                .iter()
                .any(|entry| entry.kind == WheelReservedKind::File
                    && entry.path == environment.prefix.final_path().join(relative))
        );
    }
}

#[tokio::test]
async fn unused_normal_platlib_directory_conflicts_in_planning_before_any_write() {
    let environment = Environment::new(true).await;
    let archive = environment.archive(
        "shadow",
        vec![(
            "shadow-1.0.data/data/unused-platlib",
            b"must not replace existing normal directory",
            0o644,
        )],
        false,
    );
    let before = environment.probe_log();
    assert!(
        environment.prepare(vec![archive]).is_err(),
        "planner accepted file over unused normal site directory"
    );
    assert_eq!(environment.probe_log(), before);
    environment.prefix.check_binding().unwrap();
    environment.assert_unpublished();
}
