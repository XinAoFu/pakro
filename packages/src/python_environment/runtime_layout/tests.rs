use super::test_fixtures::{Flavor, descriptor, layout};
use super::*;

use serde_json::{Value, json};

fn selected(flavor: Flavor) -> PythonRuntimeDescriptor {
    descriptor(Path::new("/runtime/bin/python"), flavor)
}
fn read(selected: &PythonRuntimeDescriptor, value: &Value) -> Result<RuntimeLayoutObservation> {
    decode_runtime_layout(
        &serde_json::to_vec(value).unwrap(),
        selected,
        Path::new("/environments/g-final"),
        RuntimeLayoutLimits::default(),
    )
}
fn fixture(selected: &PythonRuntimeDescriptor, flavor: Flavor) -> Value {
    layout(
        selected,
        Path::new("/runtime"),
        Path::new("/environments/g-final"),
        flavor,
    )
}
fn error(result: Result<RuntimeLayoutObservation>, diagnostic: &str) {
    let error = result.unwrap_err();
    assert!(error.to_string().contains(diagnostic), "{error:#}");
}

#[test]
fn actual_cpython_and_pypy_target_shapes_preserve_the_selected_layout() {
    for (flavor, leaf) in [
        (Flavor::Cpython39, "python3.9"),
        (Flavor::Cpython311, "python3.11"),
        (Flavor::Pypy312, "pypy3.12"),
    ] {
        let selected = selected(flavor);
        let observed = read(&selected, &fixture(&selected, flavor)).unwrap();
        assert_eq!(
            observed.target_paths["purelib"],
            Path::new("/environments/g-final/lib")
                .join(leaf)
                .join("site-packages")
        );
        assert_eq!(observed.identity.implementation, selected.implementation);
        assert_eq!(observed.identity.ext_suffix, selected.ext_suffix);
        assert_eq!(
            observed.target_paths["data"],
            Path::new("/environments/g-final")
        );
    }
}

#[test]
fn legacy_distutils_projection_does_not_invent_modern_base_prefix_or_scheme() {
    let selected = selected(Flavor::Legacy31);
    let observed = read(&selected, &fixture(&selected, Flavor::Legacy31)).unwrap();
    assert_eq!(observed.path_api, RuntimeLayoutPathApi::DistutilsSysconfig);
    assert_eq!(observed.base_prefix, None);
    assert_eq!(observed.base_exec_prefix, None);
    assert_eq!(observed.selected_scheme, None);
    assert_eq!(
        observed.target_paths["purelib"],
        Path::new("/environments/g-final/lib/python3.1/site-packages")
    );
}

#[test]
fn clt_global_defaults_and_stale_build_paths_do_not_become_target_scheme() {
    let selected = selected(Flavor::Cpython39);
    let observed = read(&selected, &fixture(&selected, Flavor::Cpython39)).unwrap();
    assert_eq!(
        observed.base_paths["purelib"],
        Path::new("/Library/Python/3.9/site-packages")
    );
    assert_eq!(
        observed.target_paths["scripts"],
        Path::new("/environments/g-final/bin")
    );
    assert_eq!(
        observed.config_vars["BINDIR"],
        json!(
            "/Applications/Xcode.app/Contents/Developer/Library/Frameworks/Python3.framework/Versions/3.9/bin"
        )
    );
}

#[test]
fn stale_declared_pypy_library_is_missing_while_actual_loaded_image_stays_distinct() {
    let selected = selected(Flavor::Pypy312);
    let observed = read(&selected, &fixture(&selected, Flavor::Pypy312)).unwrap();
    assert_eq!(observed.config_vars["LDLIBRARY"], "libpypy3.11-c.dylib");
    assert_eq!(
        observed.files["libdir_ldlibrary"].status,
        RuntimeLayoutPathStatus::Missing
    );
    assert_eq!(
        observed.loader,
        RuntimeLoaderObservation::Observed {
            images: vec![
                PathBuf::from("/runtime/bin/python"),
                PathBuf::from("/runtime/bin/loader.dylib")
            ]
        }
    );
}

#[test]
fn base_compile_headers_are_separate_from_contained_wheel_header_destinations() {
    let selected = selected(Flavor::Cpython311);
    let observed = read(&selected, &fixture(&selected, Flavor::Cpython311)).unwrap();
    assert_eq!(
        observed.base_paths["include"],
        Path::new("/runtime/include/python3.11")
    );
    assert_eq!(
        observed.target_paths["headers"],
        Path::new("/environments/g-final/include/python3.11")
    );
    assert_eq!(
        observed.files["python_h"].status,
        RuntimeLayoutPathStatus::Missing
    );
}

#[test]
fn unavailable_loader_observation_stays_unknown_without_host_fallback() {
    let selected = selected(Flavor::Cpython311);
    let mut value = fixture(&selected, Flavor::Cpython311);
    value["loader"] = json!({"state":"unknown","reason":"ctypes_missing"});
    let observed = read(&selected, &value).unwrap();
    assert_eq!(
        observed.loader,
        RuntimeLoaderObservation::Unknown {
            reason: "ctypes_missing".into()
        }
    );
}

#[test]
fn schema_mode_and_unknown_fields_are_rejected() {
    let selected = selected(Flavor::Cpython311);
    for (field, value) in [
        ("schema", json!(2)),
        ("observation_mode", json!("activated")),
        ("unexpected", json!(true)),
    ] {
        let mut raw = fixture(&selected, Flavor::Cpython311);
        raw[field] = value;
        error(read(&selected, &raw), "layout protocol");
    }
}

#[test]
fn full_language_implementation_and_abi_identity_must_match_the_bound_descriptor() {
    let selected = selected(Flavor::Pypy312);
    for (field, value) in [
        ("implementation", json!("cpython")),
        (
            "language_version",
            json!({"major":3,"minor":12,"micro":14,"release_level":"final","serial":0}),
        ),
        (
            "implementation_version",
            json!({"major":7,"minor":3,"micro":17,"release_level":"final","serial":0}),
        ),
        ("ext_suffix", json!(".pypy312-pp73-darwin.so")),
        ("gil_disabled", json!(true)),
    ] {
        let mut raw = fixture(&selected, Flavor::Pypy312);
        raw["identity"][field] = value;
        error(read(&selected, &raw), "runtime identity");
    }
}

#[test]
fn another_executable_or_final_allocation_cannot_rebind_the_observation() {
    let selected = selected(Flavor::Cpython311);
    let mut raw = fixture(&selected, Flavor::Cpython311);
    raw["executable"] = json!("/different/bin/python");
    error(read(&selected, &raw), "executable binding");
    let mut raw = fixture(&selected, Flavor::Cpython311);
    raw["target_prefix"] = json!("/environments/another");
    error(read(&selected, &raw), "final prefix");
}

#[test]
fn target_escape_relative_paths_missing_or_extra_scheme_keys_are_rejected() {
    let selected = selected(Flavor::Cpython311);
    for value in [
        json!("relative/site"),
        json!("/outside/site"),
        json!("/environments/g-final/../../outside"),
    ] {
        let mut raw = fixture(&selected, Flavor::Cpython311);
        raw["target_paths"]["purelib"] = value;
        error(read(&selected, &raw), "target scheme");
    }
    let mut raw = fixture(&selected, Flavor::Cpython311);
    raw["target_paths"]
        .as_object_mut()
        .unwrap()
        .remove("headers");
    error(read(&selected, &raw), "target scheme");
    raw["target_paths"]["headers"] = json!("/environments/g-final/include");
    raw["target_paths"]["unknown"] = json!("/environments/g-final/unknown");
    error(read(&selected, &raw), "target scheme");
}

#[test]
fn metadata_file_facts_cannot_advertise_unrelated_files_as_runtime_headers_or_libraries() {
    let selected = selected(Flavor::Cpython311);
    for key in ["python_h", "libdir_ldlibrary"] {
        let mut raw = fixture(&selected, Flavor::Cpython311);
        raw["files"][key]["advertised"] = json!("/unrelated/looks-valid");
        raw["files"][key]["status"] = json!({"state":"present","canonical":"/unrelated/looks-valid","regular_file":true,"size":1,"mode":420});
        error(read(&selected, &raw), "file fact binding");
    }
}

#[test]
fn unknown_config_keys_non_scalar_values_and_unreported_scheme_are_rejected() {
    let selected = selected(Flavor::Cpython311);
    let mut raw = fixture(&selected, Flavor::Cpython311);
    raw["config_vars"]["arbitrary"] = json!("not a whitelisted observation");
    error(read(&selected, &raw), "config observation");
    let mut raw = fixture(&selected, Flavor::Cpython311);
    raw["config_vars"]["LIBDIR"] = json!(["not", "scalar"]);
    error(read(&selected, &raw), "config observation");
    let mut raw = fixture(&selected, Flavor::Cpython311);
    raw["selected_scheme"] = json!("not_reported");
    error(read(&selected, &raw), "scheme observation");
}

#[test]
fn output_path_loader_and_scheme_budgets_fail_without_partial_observations() {
    let selected = selected(Flavor::Cpython311);
    let raw = serde_json::to_vec(&fixture(&selected, Flavor::Cpython311)).unwrap();
    for limits in [
        RuntimeLayoutLimits {
            max_stdout_bytes: raw.len() - 1,
            ..Default::default()
        },
        RuntimeLayoutLimits {
            max_path_bytes: 8,
            ..Default::default()
        },
        RuntimeLayoutLimits {
            max_loader_images: 1,
            ..Default::default()
        },
        RuntimeLayoutLimits {
            max_schemes: 0,
            ..Default::default()
        },
    ] {
        error(
            decode_runtime_layout(&raw, &selected, Path::new("/environments/g-final"), limits),
            "layout limit",
        );
    }
}
