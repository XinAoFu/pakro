//! The Python import path cannot replace the filesystem adjacency expected by
//! a native extension's relative dynamic-library search path.

use super::{Command, Duration, prepare_layout, real_language, wheel_tagged};
use std::path::Path;

fn without_dyld_overrides(command: &mut Command) -> &mut Command {
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"DYLD_") {
            command.env_remove(name);
        }
    }
    command
}

fn compile(command: &mut Command, description: &str) {
    let output = without_dyld_overrides(command).output().unwrap();
    assert!(
        output.status.success(),
        "{description}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn import_from(interpreter: &Path, reference: Option<&Path>) -> std::process::Output {
    let mut command = Command::new(interpreter);
    if let Some(reference) = reference {
        command.args([
            "-I",
            "-S",
            "-B",
            "-c",
            "import sys; sys.path.insert(0, sys.argv[1]); from pakro_adjacent_native import native; assert native.VALUE == 42; print(native.__file__)",
        ]);
        command.arg(reference);
    } else {
        // The managed wrapper supplies the exact layout's startup path.
        command.args([
            "-B",
            "-c",
            "from pakro_adjacent_native import native; from importlib import metadata, resources; assert native.VALUE == 42; assert metadata.version('adjacencyconsumer') == '1.0'; assert metadata.version('adjacencyprovider') == '1.0'; assert resources.files('pakro_adjacent_native').joinpath('lib/libpakro_layout_provider.dylib').is_file(); print(native.__file__)",
        ]);
    }
    without_dyld_overrides(&mut command).output().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f3_native_dependency_keeps_cross_wheel_loader_adjacency() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let context = crate::libraries::language::repository::tests::operation_context(
        &root,
        true,
        Duration::from_secs(300),
    );
    let language = real_language(&context).await;
    let fixture = root.join("native-fixture");
    let reference = root.join("reference");
    let package_dir = reference.join("pakro_adjacent_native");
    let library_dir = package_dir.join("lib");
    std::fs::create_dir_all(&fixture).unwrap();
    std::fs::create_dir_all(&library_dir).unwrap();
    let provider_source = fixture.join("provider.c");
    let extension_source = fixture.join("native.c");
    let provider = library_dir.join("libpakro_layout_provider.dylib");
    let extension = package_dir.join("native.so");
    let init = b"__path__ = __import__('pkgutil').extend_path(__path__, __name__)\n";
    std::fs::write(package_dir.join("__init__.py"), init).unwrap();
    std::fs::write(
        &provider_source,
        b"int pakro_layout_provider_value(void) { return 42; }\n",
    )
    .unwrap();
    std::fs::write(
        &extension_source,
        b"#include <Python.h>\nextern int pakro_layout_provider_value(void);\nstatic struct PyModuleDef module = {PyModuleDef_HEAD_INIT, \"native\", NULL, -1, NULL};\nPyMODINIT_FUNC PyInit_native(void) { PyObject *value = PyModule_Create(&module); if (value) PyModule_AddIntConstant(value, \"VALUE\", pakro_layout_provider_value()); return value; }\n",
    )
    .unwrap();
    compile(
        Command::new("/usr/bin/cc")
            .args([
                "-dynamiclib",
                "-Wl,-install_name,@rpath/libpakro_layout_provider.dylib",
            ])
            .arg(&provider_source)
            .arg("-o")
            .arg(&provider),
        "provider fixture compilation failed",
    );
    compile(
        Command::new("/usr/bin/cc")
            .args([
                "-bundle",
                "-undefined",
                "dynamic_lookup",
                "-Wl,-rpath,@loader_path/lib",
            ])
            .arg("-I")
            .arg(language.root().join("include/python3.13"))
            .arg(&extension_source)
            .arg(&provider)
            .arg("-o")
            .arg(&extension),
        "consumer fixture compilation failed",
    );

    // Prove the fixture's ABI and @rpath install name before testing Pakro's
    // projection. A fresh process prevents a prior load from masking failure.
    let control = import_from(language.entrypoint(), Some(&reference));
    assert!(
        control.status.success(),
        "adjacent reference tree must import before testing the split layout: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(String::from_utf8_lossy(&control.stdout).contains(extension.to_str().unwrap()));

    let tag = language
        .target()
        .supported()
        .ordered()
        .iter()
        .find(|tag| tag.interpreter() == "cp313" && tag.abi() == "cp313" && tag.platform() != "any")
        .unwrap();
    let tag = format!("{}-{}-{}", tag.interpreter(), tag.abi(), tag.platform());
    let provider_bytes = std::fs::read(&provider).unwrap();
    let extension_bytes = std::fs::read(&extension).unwrap();
    let provider_key = [81; 32];
    let consumer_key = [82; 32];
    let provider_wheel = wheel_tagged(
        &context,
        &language,
        "adjacencyprovider",
        provider_key,
        vec![],
        vec![(
            "pakro_adjacent_native/lib/libpakro_layout_provider.dylib",
            provider_bytes.as_slice(),
        )],
        None,
        &tag,
    );
    let consumer_wheel = wheel_tagged(
        &context,
        &language,
        "adjacencyconsumer",
        consumer_key,
        vec![provider_key],
        vec![
            ("pakro_adjacent_native/__init__.py", init),
            (
                "pakro_adjacent_native/native.so",
                extension_bytes.as_slice(),
            ),
        ],
        None,
        &tag,
    );
    let layout = prepare_layout(
        &context,
        &language,
        [83; 32],
        vec![consumer_wheel, provider_wheel],
    )
    .unwrap();
    let provider_package = layout
        .packages()
        .iter()
        .find(|package| package.manifest().key() == provider_key)
        .unwrap();
    let consumer_package = layout
        .packages()
        .iter()
        .find(|package| package.manifest().key() == consumer_key)
        .unwrap();
    assert_ne!(provider_package.root(), consumer_package.root());
    assert_eq!(
        consumer_package.manifest().facts.dependencies,
        [provider_key]
    );
    let output = import_from(&layout.bin().join("python"), None);
    assert!(
        output.status.success(),
        "the selected runtime dependency must remain reachable through @loader_path/lib across separate wheel allocations: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    use std::os::unix::fs::MetadataExt;
    let projected = layout
        .prefix()
        .join("site-packages/pakro_adjacent_native/native.so");
    let canonical = consumer_package
        .site_packages()
        .join("pakro_adjacent_native/native.so");
    let projected_metadata = std::fs::metadata(&projected).unwrap();
    let canonical_metadata = std::fs::metadata(&canonical).unwrap();
    assert_ne!(
        (projected_metadata.dev(), projected_metadata.ino()),
        (canonical_metadata.dev(), canonical_metadata.ino())
    );
    assert_eq!(projected_metadata.nlink(), 1);
    assert_eq!(std::fs::read(canonical).unwrap(), extension_bytes);
    assert_eq!(
        std::fs::read(
            provider_package
                .site_packages()
                .join("pakro_adjacent_native/lib/libpakro_layout_provider.dylib")
        )
        .unwrap(),
        provider_bytes
    );
    assert!(
        !layout
            .prefix()
            .join("site-packages/adjacencyconsumer-1.0.dist-info")
            .exists(),
        "canonical RECORD ownership stays in the package tree"
    );
    layout.check_binding().unwrap();
    language.check_binding().unwrap();
}
