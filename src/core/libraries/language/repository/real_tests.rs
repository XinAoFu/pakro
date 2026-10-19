// Explicit real-distribution acceptance. These tests never discover, download,
// copy, or adopt an existing interpreter. The caller supplies a PBS archive.
const R06_REAL_HELPER: &str = "libraries::language::repository::tests::r06_real_process_helper";

#[derive(serde::Serialize, serde::Deserialize)]
struct R06RealInput {
    archive: PathBuf,
    filename: String,
    request: String,
    digest: String,
    url: String,
    arch: String,
    platform: String,
}

impl R06RealInput {
    fn supplied() -> Self {
        use sha2::Digest as _;
        use std::io::Read;
        let archive = PathBuf::from(
            std::env::var_os("PAKRO_R06_ARCHIVE")
                .expect("set PAKRO_R06_ARCHIVE to the explicitly supplied complete PBS archive"),
        );
        assert!(
            archive.is_absolute(),
            "the supplied archive must have an absolute path"
        );
        let filename = archive.file_name().unwrap().to_str().unwrap().to_owned();
        let declared = filename
            .strip_prefix("cpython-")
            .expect("this acceptance input must be a complete PBS CPython archive");
        let (version, suffix) = declared
            .split_once('+')
            .expect("PBS filename must include its release build");
        let build = suffix.split_once('-').unwrap().0;
        let request = std::env::var("PAKRO_R06_REQUEST").unwrap_or_else(|_| version.into());
        let url = format!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/{build}/{filename}"
        );
        let platform = if filename.contains("-apple-darwin-") {
            "macos"
        } else if filename.contains("-unknown-linux-gnu-") {
            "linux-gnu"
        } else if filename.contains("-unknown-linux-musl-") {
            "linux-musl"
        } else {
            panic!("unsupported declared archive target")
        };
        let arch = suffix
            .strip_prefix(build)
            .unwrap()
            .strip_prefix('-')
            .unwrap()
            .split('-')
            .next()
            .unwrap()
            .to_owned();
        let mut input = fs::File::open(&archive).unwrap();
        assert!(input.metadata().unwrap().is_file());
        let mut digest = sha2::Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = input.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            bytes = bytes.checked_add(count as u64).unwrap();
            assert!(
                bytes <= 4 * 1024 * 1024 * 1024,
                "archive exceeds the acceptance input budget"
            );
            digest.update(&buffer[..count]);
        }
        let digest = format!("sha256-{}", hex::encode(digest.finalize()));
        if let Ok(expected) = std::env::var("PAKRO_R06_SHA256") {
            assert_eq!(
                digest,
                format!(
                    "sha256-{}",
                    expected
                        .trim_start_matches("sha256-")
                        .trim_start_matches("sha256:")
                ),
                "supplied known archive digest differs"
            );
        }
        Self {
            archive,
            filename,
            request,
            digest,
            url,
            arch,
            platform: platform.into(),
        }
    }

    fn spec(&self) -> LanguageInstallSpec {
        let platform = match self.platform.as_str() {
            "macos" => RuntimePlatform::Macos,
            "linux-gnu" => RuntimePlatform::LinuxGnu,
            "linux-musl" => RuntimePlatform::LinuxMusl,
            _ => panic!("unsupported declared archive target"),
        };
        let flavor = match (
            self.filename.contains("-freethreaded-"),
            self.filename.contains("-install_only_stripped."),
        ) {
            (false, false) => RuntimeFlavor::InstallOnly,
            (false, true) => RuntimeFlavor::InstallOnlyStripped,
            (true, false) => RuntimeFlavor::FreeThreadedInstallOnly,
            (true, true) => RuntimeFlavor::FreeThreadedInstallOnlyStripped,
        };
        let plan = RuntimeInstallPlan::from_locked(
            &self.request,
            &RuntimeArchiveTarget::new(&self.arch, platform).unwrap(),
            flavor,
            &PlatformInfo {
                url: Some(self.url.clone()),
                checksum: Some(self.digest.replacen('-', ":", 1)),
                ..Default::default()
            },
        )
        .unwrap();
        LanguageInstallSpec::from_plan(&plan, false).unwrap()
    }
}

fn r06_real_facts(lease: &LanguageLease) -> serde_json::Value {
    lease.check_binding().unwrap();
    let root = fs::metadata(lease.root()).unwrap();
    let stdlib = Path::new(
        lease
            .descriptor()
            .sysconfig_paths
            .get("stdlib")
            .expect("actual probe must report stdlib"),
    );
    assert!(
        stdlib.starts_with(lease.root()),
        "the selected stdlib is outside the one shared payload"
    );
    let stdlib = fs::metadata(stdlib).unwrap();
    assert!(stdlib.is_dir());
    json!({"root":lease.root(), "root_identity":[root.dev(),root.ino()], "stdlib_identity":[stdlib.dev(),stdlib.ino()], "version":lease.descriptor().language_version.full_version()})
}

fn r06_real_child(path: &Path, mode: &str, result: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", R06_REAL_HELPER, "--nocapture"])
        .env("PAKRO_R06_REAL_FIXTURE", path)
        .env("PAKRO_R06_REAL_MODE", mode)
        .env("PAKRO_R06_REAL_RESULT", result)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
#[ignore = "only invoked by real archive acceptance parents with an isolated fixture"]
fn r06_real_process_helper() {
    let path = PathBuf::from(
        std::env::var_os("PAKRO_R06_REAL_FIXTURE")
            .expect("real acceptance helper requires its parent fixture"),
    );
    let input: R06RealInput =
        serde_json::from_slice(&fs::read(path.join("real-input.json")).unwrap()).unwrap();
    let spec = input.spec();
    let context = operation_context(&path, true, Duration::from_secs(600));
    let mode = std::env::var("PAKRO_R06_REAL_MODE").unwrap();
    let lease = match mode.as_str() {
        "lookup" => LanguageRepository::open_readonly(&context)
            .unwrap()
            .acquire_existing(&spec)
            .unwrap()
            .unwrap(),
        "prepare" | "prepare-first" | "prepare-second" => {
            if mode == "prepare-second" {
                fs::write(
                    path.join("real-second-attempt"),
                    b"entering preparation while first owns EX",
                )
                .unwrap();
            }
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(prepare_language(&spec, &context, || async {
                    fs::write(
                        path.join(format!("real-acquired-{}", std::process::id())),
                        b"actual publisher",
                    )
                    .unwrap();
                    if mode == "prepare-first" {
                        // The acquisition callback runs only after reservation has
                        // obtained the actual object EX. Keep it held until both
                        // independent requests have entered the handshake.
                        fs::write(path.join("real-first-acquired"), b"holding object EX")?;
                        while !path.join("real-release-first").is_file() {
                            context.checkpoint()?;
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        context.checkpoint()?;
                    }
                    spool_verified_reader(
                        fs::File::open(&input.archive)?,
                        &input.filename,
                        input.digest.parse::<Digest>()?,
                        4 * 1024 * 1024 * 1024,
                        context.requested_cache_root(),
                    )
                }))
                .unwrap()
        }
        other => panic!("unknown real acceptance mode {other}"),
    };
    fs::write(
        path.join(std::env::var("PAKRO_R06_REAL_RESULT").unwrap()),
        serde_json::to_vec(&r06_real_facts(&lease)).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "requires the explicitly supplied complete PBS archive; never an empty success"]
async fn r06_two_projects_and_restart_reuse_one_payload() {
    let input = R06RealInput::supplied();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap();
    fs::write(
        path.join("real-input.json"),
        serde_json::to_vec(&input).unwrap(),
    )
    .unwrap();
    let spec = input.spec();
    let context = operation_context(&path, true, Duration::from_secs(600));
    let a = prepare_language(&spec, &context, || async {
        spool_verified_reader(
            fs::File::open(&input.archive)?,
            &input.filename,
            input.digest.parse::<Digest>()?,
            4 * 1024 * 1024 * 1024,
            context.requested_cache_root(),
        )
    })
    .await
    .unwrap();
    let b = prepare_language(
        &spec,
        &operation_context(&path, true, Duration::from_secs(600)),
        || async {
            Err(eyre::eyre!(
                "a second project must not obtain another archive"
            ))
        },
    )
    .await
    .unwrap();
    let expected = r06_real_facts(&a);
    assert_eq!(expected, r06_real_facts(&b));
    drop(a);
    drop(b);
    finish(r06_real_child(&path, "lookup", "real-restart.json"));
    assert_eq!(
        expected,
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(path.join("real-restart.json")).unwrap()
        )
        .unwrap()
    );
    assert_eq!(
        fs::read_dir(path.join("data/languages"))
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("language-"))
            .count(),
        1
    );
}

#[test]
#[ignore = "requires the explicitly supplied complete PBS archive; never an empty success"]
fn r06_concurrent_first_install_publishes_one_instance() {
    let input = R06RealInput::supplied();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap();
    fs::write(
        path.join("real-input.json"),
        serde_json::to_vec(&input).unwrap(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut children = TestChildren::default();
    children
        .0
        .push(r06_real_child(&path, "prepare-first", "real-a.json"));
    children.wait_marker(&path.join("real-first-acquired"), deadline);
    children
        .0
        .push(r06_real_child(&path, "prepare-second", "real-b.json"));
    children.wait_marker(&path.join("real-second-attempt"), deadline);
    fs::write(path.join("real-release-first"), b"both requests are live").unwrap();
    children.finish();
    assert_eq!(
        fs::read(path.join("real-a.json")).unwrap(),
        fs::read(path.join("real-b.json")).unwrap()
    );
    assert_eq!(
        fs::read_dir(&path)
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("real-acquired-"))
            .count(),
        1
    );
    assert_eq!(
        fs::read_dir(path.join("data/languages"))
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("language-"))
            .count(),
        1
    );
}
