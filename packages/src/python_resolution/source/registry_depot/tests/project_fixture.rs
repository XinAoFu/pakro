//! Test-only declaration-factory producer using the existing real HTTP/A assets.
use super::*;

pub(crate) struct ProjectRegistryFixture {
    fixture: Fixture,
    peer: Peer,
}
impl ProjectRegistryFixture {
    pub(crate) fn wheels() -> Self {
        let demo = wheel("demo_pkg", "1.0", &["dep==1.0"], "");
        let dep = wheel("dep", "1.0", &[], "");
        let peer = Peer::new(move |target| match target {
            "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(DEMO)]),
            "/simple/dep/" => Reply::page("dep", vec![wire_file(DEP)]),
            value if value == format!("/files/{DEMO}") => Reply::file(&demo),
            value if value == format!("/files/{DEP}") => Reply::file(&dep),
            _ => Reply::missing(),
        });
        Self {
            fixture: Fixture::new(&peer),
            peer,
        }
    }
    /// Candidate discovery order deliberately differs from normalized names.
    pub(crate) fn reversed_name_dependency() -> Self {
        const ROOT: &str = "z_root-1.0-py3-none-any.whl";
        const DEPENDENCY: &str = "a_dep-1.0-py3-none-any.whl";
        let root = wheel("z_root", "1.0", &["a-dep>=1,<2"], "");
        let dependency = wheel("a_dep", "1.0", &[], "");
        let peer = Peer::new(move |target| match target {
            "/simple/z-root/" => Reply::page("z-root", vec![wire_file(ROOT)]),
            "/simple/a-dep/" => Reply::page("a-dep", vec![wire_file(DEPENDENCY)]),
            value if value == format!("/files/{ROOT}") => Reply::file(&root),
            value if value == format!("/files/{DEPENDENCY}") => Reply::file(&dependency),
            _ => Reply::missing(),
        });
        Self {
            fixture: Fixture::new(&peer),
            peer,
        }
    }
    pub(crate) fn sdist() -> Self {
        const SDIST: &str = "demo_pkg-1.0.tar.gz";
        let bytes = include_bytes!("../../fresh/wheel/fixtures/demo_pkg-1.0.tar.gz");
        let peer = Peer::new(move |target| match target {
            "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![wire_file(SDIST)]),
            "/files/demo_pkg-1.0.tar.gz" => Reply::file(bytes),
            _ => Reply::missing(),
        });
        Self {
            fixture: Fixture::new(&peer),
            peer,
        }
    }
    /// Actual recursive PEP 517 inputs. The counter wheel deliberately changes
    /// on every real backend build, so a dynamic re-solve must reuse its first
    /// sealed output instead of quietly building a replacement.
    pub(crate) fn dynamic_source_dependency() -> (Self, Digest, Digest) {
        Self::source_dependency(false)
    }
    pub(crate) fn deterministic_source_dependency() -> (Self, Digest, Digest) {
        Self::source_dependency(true)
    }
    fn source_dependency(deterministic: bool) -> (Self, Digest, Digest) {
        fn source(name: &str, requires: &str, backend: &str) -> Vec<u8> {
            let project = format!(
                "[build-system]\nrequires={requires}\nbuild-backend='backend'\nbackend-path=['.']\n"
            );
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let mut archive = tar::Builder::new(encoder);
            for (member, bytes) in [
                ("pyproject.toml", project.as_bytes()),
                ("backend.py", backend.as_bytes()),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, format!("{name}-1.0/{member}"), bytes)
                    .unwrap();
            }
            archive.into_inner().unwrap().finish().unwrap()
        }
        const WRITER: &str = r#"
import base64, hashlib, os, pathlib, zipfile
def _wheel(directory, name, value):
    info = name + '-1.0.dist-info'
    members = {
        name + '/__init__.py': ('VALUE=' + repr(value) + '\n').encode(),
        info + '/METADATA': ('Metadata-Version: 2.4\nName: ' + name + '\nVersion: 1.0\n').encode(),
        info + '/WHEEL': b'Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n',
    }
    rows = [path + ',sha256=' + base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b'=').decode() + ',' + str(len(data)) + '\n' for path, data in members.items()]
    members[info + '/RECORD'] = (''.join(rows) + info + '/RECORD,,\n').encode()
    filename = name + '-1.0-py3-none-any.whl'
    with zipfile.ZipFile(pathlib.Path(directory) / filename, 'w') as archive:
        for path, data in members.items():
            member = zipfile.ZipInfo(path, (1980, 1, 1, 0, 0, 0))
            member.external_attr = 0o100644 << 16
            archive.writestr(member, data)
    return filename
"#;
        let value = if deterministic {
            "'fixed source input'"
        } else {
            "os.urandom(16).hex()"
        };
        // This legitimate temporary output is larger than the compressed
        // wheel. Strict replay must keep the ordinary worker file-size budget.
        let intermediate = if deterministic {
            "    pathlib.Path('intermediate.bin').write_bytes(b'x' * (64 * 1024))\n"
        } else {
            ""
        };
        let counter_backend = format!(
            "{WRITER}\ndef get_requires_for_build_wheel(config_settings=None): return []\ndef build_wheel(wheel_directory, config_settings=None, metadata_directory=None):\n{intermediate}    return _wheel(wheel_directory, 'counter_dep', {value})\n"
        );
        let demo_backend = format!(
            "{WRITER}\ndef get_requires_for_build_wheel(config_settings=None):\n    import counter_dep\n    pathlib.Path('observed-counter.txt').write_text(counter_dep.VALUE)\n    return ['helper==1.0']\ndef build_wheel(wheel_directory, config_settings=None, metadata_directory=None):\n    import counter_dep, helper\n    assert counter_dep.VALUE == pathlib.Path('observed-counter.txt').read_text(), 'dynamic solve rebuilt the observed source dependency'\n    assert helper.VALUE == 42\n    return _wheel(wheel_directory, 'demo_pkg', counter_dep.VALUE)\n"
        );
        let demo = source("demo_pkg", "['counter-dep==1.0']", &demo_backend);
        let other = source(
            "other_pkg",
            "['counter-dep==1.0']",
            &demo_backend.replace("'demo_pkg'", "'other_pkg'"),
        );
        let counter = source("counter_dep", "[]", &counter_backend);
        let helper = wheel("helper", "1.0", &[], "");
        let counter_digest = Digest::from_sha256_bytes(&counter);
        let helper_digest = Digest::from_sha256_bytes(&helper);
        let mut demo_file = wire_file("demo_pkg-1.0.tar.gz");
        let mut counter_file = wire_file("counter_dep-1.0.tar.gz");
        let mut helper_file = wire_file("helper-1.0-py3-none-any.whl");
        let mut other_file = wire_file("other_pkg-1.0.tar.gz");
        for (file, bytes) in [
            (&mut demo_file, &demo),
            (&mut counter_file, &counter),
            (&mut helper_file, &helper),
            (&mut other_file, &other),
        ] {
            file["hashes"] = serde_json::json!({"sha256":hex::encode(Sha256::digest(bytes))});
        }
        let peer = Peer::new(move |target| match target {
            "/simple/demo-pkg/" => Reply::page("demo-pkg", vec![demo_file.clone()]),
            "/simple/counter-dep/" => Reply::page("counter-dep", vec![counter_file.clone()]),
            "/simple/helper/" => Reply::page("helper", vec![helper_file.clone()]),
            "/simple/other-pkg/" if deterministic => {
                Reply::page("other-pkg", vec![other_file.clone()])
            }
            "/files/demo_pkg-1.0.tar.gz" => Reply::file(&demo),
            "/files/counter_dep-1.0.tar.gz" => Reply::file(&counter),
            "/files/helper-1.0-py3-none-any.whl" => Reply::file(&helper),
            "/files/other_pkg-1.0.tar.gz" if deterministic => Reply::file(&other),
            _ => Reply::missing(),
        });
        (
            Self {
                fixture: Fixture::new(&peer),
                peer,
            },
            counter_digest,
            helper_digest,
        )
    }
    pub(crate) fn lock(&self) -> BoundPythonLock {
        self.fixture.lock()
    }
    pub(crate) fn lock_at_deadline(&self, deadline: Instant) -> BoundPythonLock {
        BoundPythonLock::acquire(
            &self.fixture.root.path().join("pakro.lock"),
            Default::default(),
            deadline,
        )
        .unwrap()
    }
    pub(crate) fn project_path(&self) -> std::path::PathBuf {
        self.fixture.root.path().join("pyproject.toml")
    }
    pub(crate) fn endpoint(&self) -> RegistryEndpoint<'_> {
        self.fixture.endpoint()
    }
    pub(crate) fn context<'a>(
        &'a self,
        endpoints: &'a [RegistryEndpoint<'a>],
        source_policy: &'a PythonSourcePolicy,
    ) -> RegistryDepotContext<'a> {
        self.fixture.context(endpoints, source_policy)
    }
    pub(crate) fn requests(&self) -> Vec<String> {
        self.peer.requests()
    }
    pub(crate) fn no_publish(&self) {
        self.fixture.no_publish();
    }
    pub(crate) fn no_publish_with_existing_lock(&self, expected: &[u8]) {
        self.fixture.no_publish_with_existing_lock(expected);
    }
}
