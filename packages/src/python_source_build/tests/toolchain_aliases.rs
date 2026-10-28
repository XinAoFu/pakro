use super::*;
use crate::libraries::language::repository::tests::operation_context;
use std::time::Duration;

#[test]
fn p6_native_gcc_search_aliases_fit_the_fixed_record_budget_and_track_nested_bytes() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = operation_context(&root, false, Duration::from_secs(30));
    let sdk = root.join("sdk/usr");
    let mut inputs = vec![("sdk".into(), sdk.clone())];
    for (role, paths) in [
        ("cc", vec![
            sdk.join("include"), sdk.join("include/x86_64-linux-gnu"),
            root.join("usr/lib/gcc/x86_64-linux-gnu/13/include"), root.join("usr/local/include"),
            root.join("usr/include/x86_64-linux-gnu"), root.join("usr/include"),
        ]),
        ("cxx", vec![
            sdk.join("include"), sdk.join("include/x86_64-linux-gnu"), sdk.join("include/c++/13"),
            sdk.join("include/x86_64-linux-gnu/c++/13"), sdk.join("include/c++/13/backward"),
            sdk.join("lib/gcc/x86_64-linux-gnu/13/include"), root.join("usr/local/include"),
            root.join("usr/include/x86_64-linux-gnu"), root.join("usr/include"),
        ]),
    ] {
        inputs.extend(paths.into_iter().enumerate().map(|(index, path)| (format!("{role}.include-{index}"), path)));
    }
    // These existing-directory indices and aliases are the actual GCC 13
    // search shape observed on the Linux validation host.
    for (role, entries) in [
        ("cc", vec![
            (0, root.join("usr/lib/gcc/x86_64-linux-gnu/13")),
            (5, root.join("usr/lib/x86_64-linux-gnu")), (6, root.join("usr/lib")),
            (8, root.join("usr/lib/x86_64-linux-gnu")), (9, root.join("usr/lib")),
            (11, root.join("usr/lib/x86_64-linux-gnu")), (12, root.join("usr/lib")),
            (14, root.join("usr/lib")), (15, root.join("usr/lib")), (16, root.join("usr/lib")),
        ]),
        ("cxx", vec![
            (0, sdk.join("lib/gcc/x86_64-linux-gnu/13")), (1, sdk.join("lib/gcc/x86_64-linux-gnu")),
            (2, sdk.join("lib/gcc")), (6, sdk.join("lib/x86_64-linux-gnu")), (8, sdk.join("lib")),
            (10, root.join("usr/lib/x86_64-linux-gnu")), (11, root.join("usr/lib")),
            (13, root.join("usr/lib/x86_64-linux-gnu")), (14, root.join("usr/lib")),
            (16, sdk.join("lib")), (17, root.join("usr/lib")), (18, root.join("usr/lib")),
        ]),
    ] {
        inputs.extend(entries.into_iter().map(|(index, path)| (format!("{role}.library-{index}"), path)));
    }
    for (role, prefix) in [("cc", root.join("usr")), ("cxx", sdk.clone())] {
        inputs.push((format!("{role}.lto-plugin"), prefix.join("libexec/gcc/x86_64-linux-gnu/13/liblto_plugin.so")));
    }
    assert_eq!(inputs.len(), 40);
    for (role, path) in &mut inputs {
        if role.ends_with(".lto-plugin") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path.as_path(), b"fixed compiler plugin").unwrap();
        } else {
            std::fs::create_dir_all(path.as_path()).unwrap();
        }
        *path = path.canonicalize().unwrap();
    }
    let resource = root.join("usr/lib/gcc/x86_64-linux-gnu/13/include");
    inputs.extend([("cc.resource".into(), resource.clone()), ("cxx.resource".into(), resource)]);
    let nested = sdk.join("include/c++/13/backward");
    std::fs::write(nested.join("fixture.h"), b"#define VALUE 41\n").unwrap();
    let mut base = BTreeMap::from([
        ("env.LANG", "C"), ("env.LC_ALL", "C"), ("env.TZ", "UTC"),
        ("env.SOURCE_DATE_EPOCH", "315532800"), ("env.PYTHONDONTWRITEBYTECODE", "1"),
        ("env.CARGO_NET_OFFLINE", "true"), ("env.CCACHE_DISABLE", "1"),
        ("env.SDKROOT", "@sdk"), ("env.PKG_CONFIG_PATH", "@sdk/lib/pkgconfig"),
        ("env.PYTHON", "@program:python"), ("env.CC", "@program:cc"), ("env.CXX", "@program:cxx"),
        ("env.AR", "@program:ar"), ("env.LD", "@program:linker"), ("env.PKG_CONFIG", "@program:pkg_config"),
        ("env.PATH", "@program:python:@program:cc"), ("compiler-target", "x86_64-linux-gnu"),
        ("compiler-arguments", "[]"), ("program.cc.invocation", "gcc"), ("program.cxx.invocation", "g++"),
        ("program.pkg_config.version-exit", "127"), ("native-prefix-map", "workspace-to-build-v1"),
    ].map(|(key, value)| (key.to_owned(), value.to_owned())));
    for name in ["native-driver-sha256", "native-gate-sha256", "compiler-wrapper-sha256", "supervision-sha256", "native-adapter-sha256"] {
        base.insert(name.into(), "0f".repeat(32));
    }
    assert_eq!(base.len(), 27);
    let record_for = |inputs: &[(String, PathBuf)]| {
        let observed = hash_inputs(&context, inputs).unwrap();
        let mut record = NativeToolchainRecord::test_record();
        record.configuration = base.clone();
        let mut roots = BTreeMap::new();
        for (role, input) in observed {
            roots.insert(input.root_kind.clone(), input.content_sha256);
            record.configuration.insert(format!("input.{role}"), format!("{}:{}", input.root_kind, input.relative.display()));
        }
        record.sdk_roots = roots.into_iter().map(|(kind, sha256)| NativeSdkRecord { kind, sha256 }).collect();
        record
    };
    let oversized = record_for(&inputs);
    assert_eq!(oversized.configuration.len(), 69);
    let error = oversized.validate().unwrap_err();
    assert!(error.chain().any(|cause| cause.to_string() == "native toolchain collection limit exceeded"));
    let original = inputs.clone();
    let mut paths = BTreeSet::new();
    inputs.retain(|(_, path)| paths.insert(path.clone()));
    assert_eq!(inputs.len(), 21);
    assert_eq!(inputs.first().unwrap().0, "sdk");
    for (role, path) in &inputs {
        assert_eq!(role, &original.iter().find(|(_, first)| first == path).unwrap().0);
    }
    assert!(inputs.iter().any(|(role, path)| role == "cxx.include-4" && path == &nested));
    assert!(inputs.iter().any(|(role, path)| role == "sdk" && path == &sdk));
    let first = record_for(&inputs);
    assert_eq!(first.configuration.len(), 48);
    first.validate().unwrap();
    let fingerprint = first.fingerprint().unwrap();
    std::fs::write(nested.join("fixture.h"), b"#define VALUE 42\n").unwrap();
    let changed = record_for(&inputs);
    changed.validate().unwrap();
    assert_ne!(fingerprint, changed.fingerprint().unwrap());
}


#[tokio::test(flavor = "current_thread")]
async fn p6_native_changed_program_rejects_before_sdk_inventory_without_skipping_matches() {
    use crate::programs::{OperationContextInputs, capture_operation_context};
    use std::os::unix::fs::PermissionsExt;
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let template = operation_context(&root, false, Duration::from_secs(30));
    let sdk = root.join("sdk");
    std::fs::create_dir(&sdk).unwrap();
    let fifo = sdk.join("unsupported-input");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
    // This configured program is outside /usr/bin, so Apple xcrun cannot
    // replace the file whose real SHA is compared. Its probes use installed CC.
    let installed_cc = Path::new("/usr/bin/cc").canonicalize().expect("an installed C compiler is required");
    let compiler = root.join("clang");
    let shim = format!("#!/bin/sh\nexec '{}' \"$@\"\n", installed_cc.to_str().unwrap().replace('\'', "'\\''"));
    std::fs::write(&compiler, &shim).unwrap();
    std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut settings = template.settings().clone();
    settings.python.build.tools.cc = Some(compiler);
    settings.python.build.sdk = Some(sdk);
    let context = capture_operation_context(OperationContextInputs {
        settings: &settings, policy: *template.policy(),
        cancellation: tokio_util::sync::CancellationToken::new(),
        data_root: root.join("data"), cache_root: root.join("cache"),
        python_sources: None, python_target_description: None,
    }).unwrap();
    let actual_sha: [u8; 32] = Sha256::digest(shim.as_bytes()).into();
    let mut expected = NativeToolchainRecord::test_record();
    expected.programs[0].sha256 = actual_sha;
    expected.programs[0].sha256[0] ^= 1;
    expected.validate().unwrap();
    let required = [NativeToolRole::Cc];
    let error = FrozenNativeToolchain::discover_inner(&context, None, &required, Some(&expected)).await.unwrap_err();
    assert!(error.chain().any(|cause| cause.to_string() == "locked native toolchain, SDK, driver or Cargo inputs changed"), "changed actual program must fail before the unsupported SDK input: {error:#}");
    expected.programs[0].sha256 = actual_sha;
    let mut missing_role = expected.clone();
    missing_role.programs[0].role = "unselected".into();
    missing_role.validate().unwrap();
    for expected in [None, Some(&expected), Some(&missing_role)] {
        let error = FrozenNativeToolchain::discover_inner(&context, None, &required, expected).await.unwrap_err();
        assert!(error.chain().any(|cause| cause.to_string() == "native input contains a special file: unsupported-input"), "None, matching SHA and missing role must all retain SDK validation: {error:#}");
    }
    context.checkpoint().unwrap();
}
