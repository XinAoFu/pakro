//! Fixed upstream bytes prepared once by tests/scripts/p6-native-inputs.py.
use super::*;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(serde::Deserialize)]
struct Assets {
    schema: u32,
    complete: bool,
    assets: Vec<Asset>,
    cases: Vec<Case>,
}
#[derive(serde::Deserialize)]
struct Asset {
    kind: String,
    name: String,
    filename: String,
    path: String,
    sha256: String,
    size: u64,
    requires_python: Option<String>,
}
#[derive(serde::Deserialize)]
struct Case {
    case: String,
    name: String,
    version: String,
    backend: String,
    #[serde(rename = "import")]
    import_code: String,
}
pub(crate) struct NativeRegistryCase {
    pub name: String,
    pub version: String,
    pub backend: String,
    pub import_code: String,
    crates: Vec<(String, Digest, Vec<u8>)>,
}
impl NativeRegistryCase {
    pub(crate) fn seed_crate_cache(&self, context: &crate::programs::OperationContext) {
        std::fs::create_dir_all(context.requested_cache_root()).unwrap();
        for (filename, digest, bytes) in &self.crates {
            context.checkpoint().unwrap();
            let mut artifact = crate::artifact_snapshot::spool_verified_reader(
                bytes.as_slice(), filename, *digest, 64 * 1024 * 1024,
                context.requested_cache_root(),
            ).unwrap();
            crate::artifact_snapshot::publish_verified_artifact(&mut artifact, context.requested_cache_root(), 64 * 1024 * 1024).unwrap();
        }
    }
}

pub(super) fn load(manifest: &Path, case: &str) -> (ProjectRegistryFixture, NativeRegistryCase) {
    assert!(manifest.is_absolute() && std::fs::metadata(manifest).unwrap().len() <= 4 * 1024 * 1024);
    let assets: Assets = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    assert!(assets.schema == 1 && assets.complete, "native input preparation must have completed");
    assert!(assets.assets.len() <= 1024 && assets.cases.len() == 6);
    let pure = case == "pure-setuptools";
    let selected = if pure {
        Case { case: case.into(), name: "p6-setuptools-pure".into(), version: "1.0.0".into(),
            backend: "setuptools.build_meta".into(),
            import_code: "import p6_pure_demo; assert p6_pure_demo.answer()==42".into() }
    } else {
        assets.cases.into_iter().find(|value| value.case == case).expect("requested real backend input is absent")
    };
    let root = manifest.parent().unwrap();
    let mut pages = BTreeMap::<String, Vec<serde_json::Value>>::new();
    let mut files = BTreeMap::new();
    let mut crates = Vec::new();
    let mut total = 0u64;
    for asset in assets.assets {
        // The pure regression reuses only these existing universal backend
        // wheels; no native fixtures, crates, manifest copies or downloads.
        if pure && (asset.kind != "wheel" || !matches!(asset.name.as_str(), "setuptools" | "wheel" | "packaging")) { continue; }
        assert!(Path::new(&asset.filename).file_name().unwrap() == asset.filename.as_str());
        assert!(Path::new(&asset.path).components().all(|c| matches!(c, std::path::Component::Normal(_))));
        assert!(asset.size <= 64 * 1024 * 1024 && asset.sha256.len() == 64);
        let path = root.join(&asset.path);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), asset.size, "prepared asset is not bounded by its manifest");
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes.len() as u64, asset.size, "prepared asset size differs: {}", asset.filename);
        assert_eq!(hex::encode(Sha256::digest(&bytes)), asset.sha256, "prepared asset SHA differs: {}", asset.filename);
        total += asset.size;
        assert!(total <= 256 * 1024 * 1024);
        if asset.kind == "crate" {
            crates.push((asset.filename, Digest::from_sha256_bytes(&bytes), bytes));
            continue;
        }
        let normalized = pakro_index::normalize_py_name(&asset.name);
        let file = serde_json::json!({"filename": asset.filename, "url":format!("/files/{}", asset.filename),
            "hashes":{"sha256":asset.sha256}, "requires-python":asset.requires_python.unwrap_or_else(|| ">=3".into())});
        pages.entry(normalized).or_default().push(file);
        assert!(files.insert(format!("/files/{}", asset.filename), bytes).is_none());
    }
    if pure {
        for filename in ["setuptools-80.9.0-py3-none-any.whl", "wheel-0.45.1-py3-none-any.whl", "packaging-25.0-py3-none-any.whl"] {
            assert!(files.contains_key(&format!("/files/{filename}")), "the existing fixed backend wheel is missing: {filename}");
        }
        let encoder = flate2::GzBuilder::new().mtime(0).write(Vec::new(), flate2::Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        for (name, content) in [
            ("pyproject.toml", "[build-system]\nrequires=['setuptools==80.9.0','wheel==0.45.1']\nbuild-backend='setuptools.build_meta'\n[project]\nname='p6-setuptools-pure'\nversion='1.0.0'\nrequires-python='>=3.9'\n[tool.setuptools]\npy-modules=['p6_pure_demo']\n"),
            ("p6_pure_demo.py", "def answer():\n    return 42\n"),
            ("PKG-INFO", "Metadata-Version: 2.4\nName: p6-setuptools-pure\nVersion: 1.0.0\nRequires-Python: >=3.9\n\n"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64); header.set_mode(0o644);
            header.set_uid(0); header.set_gid(0); header.set_mtime(0); header.set_cksum();
            archive.append_data(&mut header, format!("p6_setuptools_pure-1.0.0/{name}"), content.as_bytes()).unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let filename = "p6_setuptools_pure-1.0.0.tar.gz";
        pages.entry(selected.name.clone()).or_default().push(serde_json::json!({"filename":filename,
            "url":format!("/files/{filename}"),"hashes":{"sha256":hex::encode(Sha256::digest(&bytes))},"requires-python":">=3.9"}));
        assert!(files.insert(format!("/files/{filename}"), bytes).is_none());
    }
    let peer = Peer::new(move |target| {
        if let Some(bytes) = files.get(target) { return Reply::file(bytes); }
        if let Some(project) = target.strip_prefix("/simple/").and_then(|p| p.strip_suffix('/')) {
            if let Some(files) = pages.get(project) { return Reply::page(project, files.clone()); }
        }
        Reply::missing()
    });
    let fixture = Fixture::new(&peer);
    (ProjectRegistryFixture { fixture, peer }, NativeRegistryCase {
        name: selected.name, version: selected.version, backend: selected.backend,
        import_code: selected.import_code, crates,
    })
}
