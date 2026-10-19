use super::*;
use serde_json::json;

include!("fixtures.rs");

const PBS_FILENAME: &str = "cpython-3.15.0+20261013-aarch64-apple-darwin-install_only.tar.gz";

// Frozen reduced official metadata read 2026-10-15: full JSON indices 53-55,
// retaining their exact modeled values and only the aarch64 portable files.
// https://downloads.python.org/pypy/versions.json
const PYPY_PORTABLE_METADATA: &str = r###"[
  {
    "pypy_version": "7.3.8",
    "python_version": "3.9.10",
    "stable": true,
    "latest_pypy": true,
    "date": "2022-02-19",
    "files": [
      {
        "filename": "pypy3.9-v7.3.8-aarch64-portable.tar.bz2",
        "arch": "aarch64",
        "platform": "linux",
        "download_url": "https://downloads.python.org/pypy/pypy3.9-v7.3.8-aarch64-portable.tar.bz2"
      }
    ]
  },
  {
    "pypy_version": "7.3.8",
    "python_version": "3.8.12",
    "stable": true,
    "latest_pypy": true,
    "date": "2022-02-19",
    "files": [
      {
        "filename": "pypy3.8-v7.3.8-aarch64-portable.tar.bz2",
        "arch": "aarch64",
        "platform": "linux",
        "download_url": "https://downloads.python.org/pypy/pypy3.8-v7.3.8-aarch64-portable.tar.bz2"
      }
    ]
  },
  {
    "pypy_version": "7.3.8",
    "python_version": "3.7.12",
    "stable": true,
    "latest_pypy": true,
    "date": "2022-02-19",
    "files": [
      {
        "filename": "pypy3.7-v7.3.8-aarch64-portable.tar.bz2",
        "arch": "aarch64",
        "platform": "linux",
        "download_url": "https://downloads.python.org/pypy/pypy3.7-v7.3.8-aarch64-portable.tar.bz2"
      }
    ]
  }
]
"###;

// Exact official SHA256 rows, read 2026-10-15.
// https://pypy.org/checksums.html
const PYPY_PORTABLE_CHECKSUMS: &str = r###"<pre class="literal-block">b7282bc4484bceae5bc4cc04e05ee4faf51cb624c8fc7a69d92e5fdf0d0c96aa  pypy3.9-v7.3.8-aarch64-portable.tar.bz2
0210536e9f1841ba283c13b04783394050837bb3e6f4091c9f1bd9c7f2b94b55  pypy3.8-v7.3.8-aarch64-portable.tar.bz2
639c76f128a856747aee23a34276fa101a7a157ea81e76394fbaf80b97dcf2f2  pypy3.7-v7.3.8-aarch64-portable.tar.bz2</pre>
"###;

fn target(arch: &str, platform: RuntimePlatform) -> RuntimeArchiveTarget {
    RuntimeArchiveTarget::new(arch, platform).unwrap()
}

fn mac_arm() -> RuntimeArchiveTarget {
    target("aarch64", RuntimePlatform::Macos)
}

fn pbs_fixture() -> Value {
    serde_json::from_str(PBS_FIXTURE).unwrap()
}

fn pypy_fixture() -> Value {
    serde_json::from_str(PYPY_FIXTURE).unwrap()
}

fn pbs_one() -> Value {
    let mut raw = pbs_fixture();
    raw[0]["assets"] = Value::Array(
        raw[0]["assets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|asset| asset["name"] == PBS_FILENAME)
            .cloned()
            .collect(),
    );
    assert_eq!(raw[0]["assets"].as_array().unwrap().len(), 1);
    raw
}

fn pbs(raw: &Value) -> Result<Vec<RuntimeRelease>> {
    parse_pbs_releases(
        raw,
        &mac_arm(),
        RuntimeFlavor::InstallOnly,
        &BTreeMap::new(),
    )
}

fn one(mut releases: Vec<RuntimeRelease>) -> RuntimeRelease {
    assert_eq!(releases.len(), 1);
    releases.pop().unwrap()
}

fn checksum_line(filename: &str, hash: &str) -> String {
    format!("{hash}  {filename}\n")
}

fn pbs_checksums(raw: &Value) -> BTreeMap<String, String> {
    let asset = &raw[0]["assets"][0];
    let digest = asset["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    BTreeMap::from([(
        raw[0]["tag_name"].as_str().unwrap().to_string(),
        checksum_line(asset["name"].as_str().unwrap(), digest),
    )])
}

#[test]
fn pbs_official_asset_keeps_language_build_date_and_encoded_url() {
    let release = one(pbs(&pbs_one()).unwrap());
    assert_eq!(release.key().distribution, RuntimeDistribution::Pbs);
    assert_eq!(release.key().python_version, "3.15.0");
    assert_eq!(release.key().implementation_version, "3.15.0");
    assert_eq!(release.key().distribution_version, "20261013");
    assert_eq!(release.request_version(), "3.15.0");
    assert_eq!(release.release_date(), Some("2026-10-14T02:06:09Z"));
    assert_eq!(release.key().target.arch(), "aarch64");
    assert_eq!(release.key().target.platform(), RuntimePlatform::Macos);
    assert_eq!(release.source().filename(), PBS_FILENAME);
    assert!(release.source().url().path().contains("%2B"));
    assert_eq!(
        release.digest().hex(),
        "f5d1d82e2f5660d4decae5d2b7470631850bcd7bd39595d9c3cca3c5ee54eae7"
    );
}

#[test]
fn pbs_targets_are_exact_without_host_or_architecture_fallback() {
    let raw = pbs_one();
    for selected in [
        target("x86_64", RuntimePlatform::Macos),
        target("aarch64", RuntimePlatform::LinuxGnu),
        target("aarch64", RuntimePlatform::LinuxMusl),
        target("x86_64_v3", RuntimePlatform::Macos),
    ] {
        assert!(
            parse_pbs_releases(
                &raw,
                &selected,
                RuntimeFlavor::InstallOnly,
                &BTreeMap::new()
            )
            .unwrap()
            .is_empty()
        );
    }
    for invalid in [
        "",
        "../x86_64",
        "arm64/other",
        "x86_64-unknown-linux-gnu",
        "x86 64",
    ] {
        assert!(
            RuntimeArchiveTarget::new(invalid, RuntimePlatform::LinuxGnu).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn pbs_flavors_remain_distinct_without_fallback() {
    let raw = pbs_fixture();
    for (flavor, suffix) in [
        (RuntimeFlavor::InstallOnly, "-install_only.tar.gz"),
        (
            RuntimeFlavor::InstallOnlyStripped,
            "-install_only_stripped.tar.gz",
        ),
        (
            RuntimeFlavor::FreeThreadedInstallOnly,
            "-freethreaded-install_only.tar.gz",
        ),
        (
            RuntimeFlavor::FreeThreadedInstallOnlyStripped,
            "-freethreaded-install_only_stripped.tar.gz",
        ),
    ] {
        let release = one(parse_pbs_releases(&raw, &mac_arm(), flavor, &BTreeMap::new()).unwrap());
        assert_eq!(release.key().flavor, flavor);
        assert!(release.source().filename().ends_with(suffix));
    }
    assert!(
        parse_pbs_releases(
            &pbs_one(),
            &mac_arm(),
            RuntimeFlavor::InstallOnlyStripped,
            &BTreeMap::new()
        )
        .unwrap()
        .is_empty()
    );
    assert!(parse_pbs_releases(&raw, &mac_arm(), RuntimeFlavor::PyPy, &BTreeMap::new()).is_err());
}

#[test]
fn pbs_prerelease_language_is_independent_of_distribution_date() {
    let mut raw = pbs_one();
    let old = raw[0]["assets"][0]["name"].as_str().unwrap().to_string();
    let filename = old.replace("3.15.0+", "3.16.0rc2+");
    raw[0]["assets"][0]["name"] = json!(filename);
    raw[0]["assets"][0]["browser_download_url"] = json!(format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/{filename}"
    ));
    let release = one(pbs(&raw).unwrap());
    assert_eq!(release.key().python_version, "3.16.0rc2");
    assert_eq!(release.key().distribution_version, "20261013");
}

#[test]
fn pbs_missing_api_hash_uses_only_matching_release_and_filename_checksum() {
    let mut raw = pbs_one();
    let checksums = pbs_checksums(&raw);
    raw[0]["assets"][0]
        .as_object_mut()
        .unwrap()
        .remove("digest");
    let release =
        one(parse_pbs_releases(&raw, &mac_arm(), RuntimeFlavor::InstallOnly, &checksums).unwrap());
    assert_eq!(
        release.digest().hex(),
        "f5d1d82e2f5660d4decae5d2b7470631850bcd7bd39595d9c3cca3c5ee54eae7"
    );
    for missing in [
        BTreeMap::new(),
        BTreeMap::from([("20261012".to_string(), checksums["20261013"].clone())]),
        BTreeMap::from([(
            "20261013".to_string(),
            checksum_line("other.tar.gz", &"a".repeat(64)),
        )]),
    ] {
        assert!(
            parse_pbs_releases(&raw, &mac_arm(), RuntimeFlavor::InstallOnly, &missing).is_err()
        );
    }
}

#[test]
fn pbs_malformed_or_conflicting_digest_never_becomes_a_checksum_fallback() {
    for invalid in [json!("sha256:bad"), json!("md5:abcd"), json!(42)] {
        let mut raw = pbs_one();
        let checksums = pbs_checksums(&raw);
        raw[0]["assets"][0]["digest"] = invalid;
        assert!(
            parse_pbs_releases(&raw, &mac_arm(), RuntimeFlavor::InstallOnly, &checksums).is_err()
        );
    }
    let raw = pbs_one();
    let conflict = BTreeMap::from([(
        "20261013".into(),
        checksum_line(PBS_FILENAME, &"a".repeat(64)),
    )]);
    assert!(parse_pbs_releases(&raw, &mac_arm(), RuntimeFlavor::InstallOnly, &conflict).is_err());
}

#[test]
fn pbs_source_url_release_filename_and_target_cannot_disagree() {
    for url in [
        format!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20261012/{PBS_FILENAME}"
        ),
        format!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/{}",
            PBS_FILENAME.replace("aarch64", "x86_64")
        ),
        format!("https://mirror.invalid/{PBS_FILENAME}"),
    ] {
        let mut raw = pbs_one();
        raw[0]["assets"][0]["browser_download_url"] = json!(url);
        assert!(pbs(&raw).is_err());
    }
    let mut raw = pbs_one();
    raw[0]["tag_name"] = json!("20261012");
    assert!(pbs(&raw).is_err());
}

#[test]
fn pypy_official_release_separates_language_implementation_and_publication() {
    let release = one(parse_pypy_releases(&pypy_fixture(), PYPY_CHECKSUMS, &mac_arm()).unwrap());
    assert_eq!(release.key().distribution, RuntimeDistribution::PyPy);
    assert_eq!(release.key().python_version, "3.12.15");
    assert_eq!(release.key().implementation_version, "8.0.1");
    assert_eq!(release.key().distribution_version, "8.0.1");
    assert_eq!(release.request_version(), "pypy3.12-8.0.1");
    assert_eq!(release.release_date(), Some("2026-10-14"));
    assert_eq!(release.key().flavor, RuntimeFlavor::PyPy);
    assert_eq!(
        release.digest().hex(),
        "c10baeae4a61063a1183c4ee4e8324765ba736a3980323654efb31e79f1514db"
    );
}

#[test]
fn pypy_historical_portable_preserves_three_complete_release_identities() {
    let target = target("aarch64", RuntimePlatform::LinuxGnu);
    let raw = serde_json::from_str(PYPY_PORTABLE_METADATA).unwrap();
    let releases = parse_pypy_releases(&raw, PYPY_PORTABLE_CHECKSUMS, &target).unwrap();
    assert_eq!(releases.len(), 3);
    for (release, (language, request, filename, digest)) in releases.iter().zip([
        (
            "3.9.10",
            "pypy3.9-7.3.8",
            "pypy3.9-v7.3.8-aarch64-portable.tar.bz2",
            "b7282bc4484bceae5bc4cc04e05ee4faf51cb624c8fc7a69d92e5fdf0d0c96aa",
        ),
        (
            "3.8.12",
            "pypy3.8-7.3.8",
            "pypy3.8-v7.3.8-aarch64-portable.tar.bz2",
            "0210536e9f1841ba283c13b04783394050837bb3e6f4091c9f1bd9c7f2b94b55",
        ),
        (
            "3.7.12",
            "pypy3.7-7.3.8",
            "pypy3.7-v7.3.8-aarch64-portable.tar.bz2",
            "639c76f128a856747aee23a34276fa101a7a157ea81e76394fbaf80b97dcf2f2",
        ),
    ]) {
        assert_eq!(release.key().distribution, RuntimeDistribution::PyPy);
        assert_eq!(release.key().python_version, language);
        assert_eq!(release.key().implementation_version, "7.3.8");
        assert_eq!(release.key().distribution_version, "7.3.8");
        assert_eq!(release.key().target, target);
        assert_eq!(release.key().flavor, RuntimeFlavor::PyPy);
        assert_eq!(release.key().filename, filename);
        assert_eq!(release.request_version(), request);
        assert_eq!(release.release_date(), Some("2022-02-19"));
        assert_eq!(release.upstream_stable(), Some(true));
        assert_eq!(release.digest().hex(), digest);
        assert_eq!(release.source().filename(), filename);
        assert_eq!(
            release.source().url().as_str(),
            format!("https://downloads.python.org/pypy/{filename}")
        );
    }
}

#[test]
fn pypy_historical_portable_does_not_poison_current_macos_catalog() {
    let expected = one(parse_pypy_releases(&pypy_fixture(), PYPY_CHECKSUMS, &mac_arm()).unwrap());
    let mut raw = pypy_fixture();
    raw.as_array_mut().unwrap().extend(
        serde_json::from_str::<Value>(PYPY_PORTABLE_METADATA)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .cloned(),
    );
    let checksums = PYPY_CHECKSUMS.replace("</body>", &format!("{PYPY_PORTABLE_CHECKSUMS}</body>"));
    let selected = one(parse_pypy_releases(&raw, &checksums, &mac_arm()).unwrap());
    assert_eq!(selected.key(), expected.key());
    assert_eq!(selected.source().filename(), expected.source().filename());
    assert_eq!(selected.source().url(), expected.source().url());
    assert_eq!(selected.source().digest(), expected.source().digest());
    assert_eq!(selected.release_date(), expected.release_date());
    assert_eq!(selected.upstream_stable(), expected.upstream_stable());
}

#[test]
fn pypy_historical_portable_still_binds_language_implementation_architecture_and_url() {
    let target = target("aarch64", RuntimePlatform::LinuxGnu);
    let valid: Value = serde_json::from_str(PYPY_PORTABLE_METADATA).unwrap();
    assert_eq!(
        parse_pypy_releases(&valid, PYPY_PORTABLE_CHECKSUMS, &target)
            .unwrap()
            .len(),
        3
    );
    for (field, invalid) in [("python_version", "3.8.12"), ("pypy_version", "7.3.7")] {
        let mut raw = valid.clone();
        raw[0][field] = json!(invalid);
        let error = parse_pypy_releases(&raw, PYPY_PORTABLE_CHECKSUMS, &target).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("PyPy filename and release architecture or version disagree"),
            "{field}: {error:#}"
        );
    }
    let mut wrong_arch = valid.clone();
    wrong_arch[0]["files"][0]["arch"] = json!("x64");
    let error = parse_pypy_releases(&wrong_arch, PYPY_PORTABLE_CHECKSUMS, &target).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("PyPy filename and release architecture or version disagree"),
        "{error:#}"
    );
    let mut wrong_url = valid;
    wrong_url[0]["files"][0]["download_url"] =
        json!("https://downloads.python.org/pypy/pypy3.8-v7.3.8-aarch64-portable.tar.bz2");
    let error = parse_pypy_releases(&wrong_url, PYPY_PORTABLE_CHECKSUMS, &target).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("runtime artifact locator differs from archive identity"),
        "{error:#}"
    );
}

#[test]
#[ignore = "requires PAKRO_TEST_PYPY_VERSIONS_JSON and PAKRO_TEST_PYPY_CHECKSUMS_HTML complete official snapshots"]
fn pypy_complete_official_snapshots_cover_all_supported_unix_targets() {
    let versions = std::env::var_os("PAKRO_TEST_PYPY_VERSIONS_JSON").expect(
        "set PAKRO_TEST_PYPY_VERSIONS_JSON to an explicitly selected complete official snapshot",
    );
    let checksums = std::env::var_os("PAKRO_TEST_PYPY_CHECKSUMS_HTML").expect(
        "set PAKRO_TEST_PYPY_CHECKSUMS_HTML to an explicitly selected complete official snapshot",
    );
    let raw: Value = serde_json::from_slice(&std::fs::read(versions).unwrap()).unwrap();
    let checksums = std::fs::read_to_string(checksums).unwrap();
    let releases: Vec<PypyRelease> = serde_json::from_value(raw.clone()).unwrap();
    let recognized = releases
        .iter()
        .filter(|release| {
            release.pypy_version != "nightly"
                && version_parts(&release.python_version).unwrap().0 == 3
        })
        .flat_map(|release| &release.files)
        .filter(|file| pypy_file_target(file).unwrap().is_some())
        .count();
    eprintln!(
        "complete PyPy snapshots: {} releases, {recognized} recognized Unix Python 3 file rows",
        releases.len()
    );
    for (arch, platform) in [
        ("aarch64", RuntimePlatform::Macos),
        ("x86_64", RuntimePlatform::Macos),
        ("x86_64", RuntimePlatform::LinuxGnu),
        ("i686", RuntimePlatform::LinuxGnu),
        ("aarch64", RuntimePlatform::LinuxGnu),
        ("s390x", RuntimePlatform::LinuxGnu),
    ] {
        let target = target(arch, platform);
        let selected = parse_pypy_releases(&raw, &checksums, &target).unwrap();
        assert!(
            !selected.is_empty(),
            "complete snapshot contains no releases for {target:?}"
        );
        assert!(
            selected
                .iter()
                .all(|release| release.key().target == target)
        );
        eprintln!("{target:?}: {} unique releases", selected.len());
    }
}

#[test]
fn pypy_shared_implementation_preserves_language_identity_and_rejects_conflicting_patch() {
    let mut raw = pypy_fixture();
    let mut second = raw[0].clone();
    second["python_version"] = json!("3.11.17");
    second["files"][0]["filename"] = json!("pypy3.11-v8.0.1-macos_arm64.tar.gz");
    second["files"][0]["download_url"] =
        json!("https://downloads.python.org/pypy/pypy3.11-v8.0.1-macos_arm64.tar.gz");
    raw.as_array_mut().unwrap().push(second);
    let checksums = format!(
        "{PYPY_CHECKSUMS}<pre>b839268a9eaf41d28e4dc0be20f2201e18121b31dc9ebc8bc19d140d9d34bc6c  pypy3.11-v8.0.1-macos_arm64.tar.gz</pre>"
    );
    let releases = parse_pypy_releases(&raw, &checksums, &mac_arm()).unwrap();
    assert_eq!(releases.len(), 2);
    assert_eq!(
        releases[0].key().implementation_version,
        releases[1].key().implementation_version
    );
    assert_ne!(
        releases[0].key().python_version,
        releases[1].key().python_version
    );
    assert_ne!(releases[0].request_version(), releases[1].request_version());
    let mut conflicting = pypy_fixture();
    let mut duplicate = conflicting[0].clone();
    duplicate["python_version"] = json!("3.12.14");
    conflicting.as_array_mut().unwrap().push(duplicate);
    assert!(parse_pypy_releases(&conflicting, PYPY_CHECKSUMS, &mac_arm()).is_err());
}

#[test]
fn pypy_filename_architecture_language_and_url_must_agree() {
    for (field, invalid) in [
        ("arch", "x64"),
        ("filename", "pypy3.12-v8.0.1-macos_x86_64.tar.gz"),
        (
            "download_url",
            "https://downloads.python.org/pypy/pypy3.12-v8.0.1-macos_x86_64.tar.gz",
        ),
    ] {
        let mut raw = pypy_fixture();
        raw[0]["files"][0][field] = json!(invalid);
        assert!(
            parse_pypy_releases(&raw, PYPY_CHECKSUMS, &mac_arm()).is_err(),
            "{field}"
        );
    }
    for (field, invalid) in [
        ("python_version", "3.11.15"),
        ("pypy_version", "8.0.0"),
        ("date", "2026-02-30"),
    ] {
        let mut raw = pypy_fixture();
        raw[0][field] = json!(invalid);
        assert!(
            parse_pypy_releases(&raw, PYPY_CHECKSUMS, &mac_arm()).is_err(),
            "{field}"
        );
    }
}

#[test]
fn pypy_missing_hash_and_conflicting_duplicate_hash_fail_closed() {
    for checksums in [
        String::new(),
        "<pre>bad  pypy3.12-v8.0.1-macos_arm64.tar.gz</pre>".into(),
        format!(
            "{PYPY_CHECKSUMS}<pre>{}</pre>",
            checksum_line("pypy3.12-v8.0.1-macos_arm64.tar.gz", &"a".repeat(64))
        ),
    ] {
        assert!(parse_pypy_releases(&pypy_fixture(), &checksums, &mac_arm()).is_err());
    }
}

fn manual_release(date: &str) -> RuntimeRelease {
    let filename = format!("cpython-3.10.14+{date}-aarch64-apple-darwin-install_only.tar.gz");
    RuntimeRelease {
        key: RuntimeReleaseKey {
            distribution: RuntimeDistribution::Pbs,
            python_version: "3.10.14".into(),
            implementation_version: "3.10.14".into(),
            distribution_version: date.into(),
            target: mac_arm(),
            flavor: RuntimeFlavor::InstallOnly,
            filename: filename.clone(),
            digest: Digest::from_sha256_bytes(b"fixture"),
        },
        request_version: "3.10.14".into(),
        release_date: None,
        upstream_stable: None,
        source: SourceArtifact::new(&filename, &format!("https://github.com/astral-sh/python-build-standalone/releases/download/{date}/{filename}"), Digest::from_sha256_bytes(b"fixture")).unwrap(),
    }
}

#[test]
fn exact_selection_replays_old_build_and_never_replaces_a_missing_pin() {
    let old = manual_release("20240909");
    let key = old.key().clone();
    let releases = [manual_release("20261013"), old];
    assert_eq!(select_exact_release(&releases, &key).unwrap().key(), &key);
    assert!(select_exact_release(&releases[..1], &key).is_err());
    let mut wrong_digest = key.clone();
    wrong_digest.digest = Digest::from_sha256_bytes(b"changed artifact");
    assert!(select_exact_release(&releases, &wrong_digest).is_err());
}

#[test]
fn release_debug_and_parser_errors_do_not_reveal_artifact_locator_secrets() {
    let mut release = manual_release("20261013");
    release.source = SourceArtifact::new(
        release.source.filename(),
        "https://private.invalid/archive?token=hidden",
        release.digest(),
    )
    .unwrap();
    let debug = format!("{release:?}");
    assert!(!debug.contains("private.invalid"));
    assert!(!debug.contains("hidden"));
    for url in [
        format!(
            "https://secret:password@github.com/astral-sh/python-build-standalone/releases/download/20261013/{PBS_FILENAME}"
        ),
        format!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/{PBS_FILENAME}?token=hidden"
        ),
    ] {
        let mut raw = pbs_one();
        raw[0]["assets"][0]["browser_download_url"] = json!(url);
        let error = pbs(&raw).unwrap_err().to_string();
        for secret in ["secret", "password", "hidden", "https://"] {
            assert!(!error.contains(secret), "{error}");
        }
    }
}

#[test]
fn nightly_python_two_and_unavailable_musl_do_not_gain_a_native_candidate() {
    for version in ["nightly", "8.0.1"] {
        let mut raw = pypy_fixture();
        if version == "nightly" {
            raw[0]["pypy_version"] = json!(version);
        } else {
            raw[0]["python_version"] = json!("2.7.18");
        }
        assert!(
            parse_pypy_releases(&raw, PYPY_CHECKSUMS, &mac_arm())
                .unwrap()
                .is_empty()
        );
    }
    let musl = target("aarch64", RuntimePlatform::LinuxMusl);
    assert!(
        parse_pypy_releases(&pypy_fixture(), PYPY_CHECKSUMS, &musl)
            .unwrap()
            .is_empty()
    );
    let unknown = target("mips64", RuntimePlatform::LinuxGnu);
    assert!(
        parse_pbs_releases(
            &pbs_one(),
            &unknown,
            RuntimeFlavor::InstallOnly,
            &BTreeMap::new()
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn duplicate_pbs_release_keys_deduplicate_identical_and_reject_conflicting_sources() {
    let mut raw = pbs_one();
    let duplicate = raw[0].clone();
    raw.as_array_mut().unwrap().push(duplicate);
    assert_eq!(pbs(&raw).unwrap().len(), 1);
    raw[1]["assets"][0]["digest"] = json!(format!("sha256:{}", "a".repeat(64)));
    assert!(pbs(&raw).is_err());
}

#[test]
fn pypy_checksums_require_preformatted_exact_rows_instead_of_page_body_hashes() {
    let raw = pypy_fixture();
    let filename = "pypy3.12-v8.0.1-macos_arm64.tar.gz";
    for html in [
        format!("<p>{}</p>", checksum_line(filename, &"a".repeat(64))),
        format!(
            "<pre>{}</pre>",
            checksum_line("neighbor.tar.gz", &"a".repeat(64))
        ),
        format!(
            "<pre>{} extra</pre>",
            checksum_line(filename, &"a".repeat(64)).trim()
        ),
    ] {
        assert!(parse_pypy_releases(&raw, &html, &mac_arm()).is_err());
    }
}

#[test]
fn required_pbs_checksums_are_unique_sorted_and_only_for_matching_missing_hashes() {
    let mut raw = pbs_fixture();
    for asset in raw[0]["assets"].as_array_mut().unwrap() {
        asset["digest"] = Value::Null;
    }
    let mut old = raw[0].clone();
    old["tag_name"] = json!("20261012");
    for asset in old["assets"].as_array_mut().unwrap() {
        for field in ["name", "browser_download_url"] {
            asset[field] = json!(
                asset[field]
                    .as_str()
                    .unwrap()
                    .replace("20261013", "20261012")
            );
        }
    }
    let duplicate = old.clone();
    raw.as_array_mut().unwrap().extend([old, duplicate]);
    assert_eq!(
        required_pbs_checksums(&raw, &mac_arm(), RuntimeFlavor::InstallOnly).unwrap(),
        vec!["20261012", "20261013"]
    );
    assert!(
        required_pbs_checksums(
            &raw,
            &target("x86_64", RuntimePlatform::Macos),
            RuntimeFlavor::InstallOnly
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        required_pbs_checksums(&pbs_one(), &mac_arm(), RuntimeFlavor::InstallOnlyStripped)
            .unwrap()
            .is_empty()
    );
    assert!(
        required_pbs_checksums(&pbs_one(), &mac_arm(), RuntimeFlavor::InstallOnly)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn required_pbs_checksums_reject_malformed_digest_and_metadata_without_fallback() {
    let mut malformed = pbs_one();
    malformed[0]["assets"][0]["digest"] = json!("sha256:bad");
    assert!(required_pbs_checksums(&malformed, &mac_arm(), RuntimeFlavor::InstallOnly).is_err());
    let mut malformed = pbs_one();
    malformed[0]["assets"][0]["digest"] = Value::Null;
    malformed[0]["assets"][0]["browser_download_url"] = json!("https://untrusted.invalid/wrong");
    assert!(required_pbs_checksums(&malformed, &mac_arm(), RuntimeFlavor::InstallOnly).is_err());
}

#[test]
fn required_pbs_checksums_reject_conflicting_duplicate_candidates() {
    let mut raw = pbs_one();
    let mut duplicate = raw[0].clone();
    duplicate["assets"][0]["digest"] = json!(format!("sha256:{}", "a".repeat(64)));
    raw.as_array_mut().unwrap().push(duplicate);
    assert!(required_pbs_checksums(&raw, &mac_arm(), RuntimeFlavor::InstallOnly).is_err());
}

#[test]
fn pypy_checksum_pre_tags_in_inert_html_do_not_supply_a_digest() {
    let row = checksum_line("pypy3.12-v8.0.1-macos_arm64.tar.gz", &"a".repeat(64));
    for html in [
        format!("<!-- <pre>{row}</pre> -->"),
        format!("<script>const value='<pre>{row}</pre>';</script>"),
        format!("<style>/* <pre>{row}</pre> */</style>"),
        format!("<textarea><pre>{row}</pre></textarea>"),
        format!("<template><pre>{row}</pre></template>"),
        format!("<div data-value=\"<pre>{row}</pre>\"></div>"),
    ] {
        assert!(parse_pypy_releases(&pypy_fixture(), &html, &mac_arm()).is_err());
    }
    let mixed = format!(
        "<!-- <pre>{row}</pre> -->{PYPY_CHECKSUMS}<script>const value='<pre>{row}</pre>';</script>"
    );
    let release = one(parse_pypy_releases(&pypy_fixture(), &mixed, &mac_arm()).unwrap());
    assert_eq!(
        release.digest().hex(),
        "c10baeae4a61063a1183c4ee4e8324765ba736a3980323654efb31e79f1514db"
    );
}

#[test]
fn pypy_upstream_stability_is_preserved_without_discarding_explicit_candidates() {
    let mut raw = pypy_fixture();
    assert_eq!(
        one(parse_pypy_releases(&raw, PYPY_CHECKSUMS, &mac_arm()).unwrap()).upstream_stable(),
        Some(true)
    );
    raw[0]["stable"] = json!(false);
    let release = one(parse_pypy_releases(&raw, PYPY_CHECKSUMS, &mac_arm()).unwrap());
    assert_eq!(release.upstream_stable(), Some(false));
    raw[0].as_object_mut().unwrap().remove("stable");
    assert_eq!(
        one(parse_pypy_releases(&raw, PYPY_CHECKSUMS, &mac_arm()).unwrap()).upstream_stable(),
        None
    );
}

#[test]
fn duplicate_runtime_publication_policy_cannot_depend_on_record_order() {
    for field in ["stable", "date"] {
        let mut raw = pypy_fixture();
        let mut duplicate = raw[0].clone();
        duplicate[field] = if field == "stable" {
            json!(false)
        } else {
            json!("2026-10-13")
        };
        raw.as_array_mut().unwrap().push(duplicate);
        for _ in 0..2 {
            assert!(
                parse_pypy_releases(&raw, PYPY_CHECKSUMS, &mac_arm()).is_err(),
                "{field}"
            );
            raw.as_array_mut().unwrap().reverse();
        }
    }
    let mut raw = pbs_one();
    let mut duplicate = raw[0].clone();
    duplicate["published_at"] = json!("2026-10-13T00:00:00Z");
    raw.as_array_mut().unwrap().push(duplicate);
    for _ in 0..2 {
        assert!(pbs(&raw).is_err());
        assert!(required_pbs_checksums(&raw, &mac_arm(), RuntimeFlavor::InstallOnly).is_err());
        raw.as_array_mut().unwrap().reverse();
    }
}
#[test]
fn exact_requests_require_complete_valid_release_labels() {
    for label in [
        "3.9.0",
        "3.15.0a8",
        "3.14.0b1",
        "3.14.0rc2",
        "pypy3.10-7.3.17",
        "pypy3.11-7.3.20rc1",
    ] {
        assert!(super::is_exact_request(label), "{label}");
    }
    for label in [
        "latest",
        "3",
        "3.10",
        "pypy3.10",
        "pypy3.10-7.3",
        "pypy3.10.16-7.3.17",
        "03.10.1",
        "3.010.1",
        "3.10.01",
        "3.14.0a01",
        "pypy03.10-7.3.17",
        "2.7.18",
        "pypy2.7-7.3.17",
        "miniconda3-latest",
        "3.10.1-dev",
        "3.10.1\n",
    ] {
        assert!(!super::is_exact_request(label), "{label:?}");
    }
}
