use std::path::Path;

use serde_json::{Value, json};

use crate::python_runtime::PythonRuntimeDescriptor;

#[derive(Clone, Copy)]
pub(in super::super) enum Flavor {
    Cpython39,
    Cpython311,
    Pypy312,
    Legacy31,
}

pub(in super::super) fn descriptor(executable: &Path, flavor: Flavor) -> PythonRuntimeDescriptor {
    let (minor, micro, implementation, implementation_full, implementation_version, soabi, suffix) =
        match flavor {
            Flavor::Cpython39 => (
                9,
                6,
                "cpython",
                "3.9.6",
                json!([3, 9, 6]),
                Some("cpython-39-darwin"),
                Some(".cpython-39-darwin.so"),
            ),
            Flavor::Cpython311 => (
                11,
                17,
                "cpython",
                "3.11.17",
                json!([3, 11, 17]),
                Some("cpython-311-darwin"),
                Some(".cpython-311-darwin.so"),
            ),
            Flavor::Pypy312 => (
                12,
                15,
                "pypy",
                "8.0.1",
                json!([8, 0, 1]),
                Some("pypy312-pp80-darwin"),
                Some(".pypy312-pp80-darwin.so"),
            ),
            Flavor::Legacy31 => (1, 5, "cpython", "3.1.5", json!([3, 1, 5]), None, None),
        };
    let version = |major: u32, minor: u32, micro: u32| json!({"major":major,"minor":minor,"micro":micro,"release_level":"final","serial":0});
    serde_json::from_value(json!({
        "schema":1,"observation_mode":"base_runtime_site_disabled",
        "probe_flags":["-E","-s","-S","-B","-c"],"executable":executable,
        "implementation":implementation,"language_version":version(3,minor,micro),
        "implementation_version":version(implementation_version[0].as_u64().unwrap() as u32,implementation_version[1].as_u64().unwrap() as u32,implementation_version[2].as_u64().unwrap() as u32),
        "implementation_version_source":if implementation=="pypy" {"pypy_version_info"} else {"sys_version_info"},
        "markers":{
            "implementation_name":implementation,"implementation_version":implementation_full,
            "os_name":"posix","platform_machine":"arm64",
            "platform_python_implementation":if implementation=="pypy" {"PyPy"} else {"CPython"},
            "platform_release":"25.0.0","platform_system":"Darwin","platform_version":"fixture",
            "python_full_version":format!("3.{minor}.{micro}"),"python_version":format!("3.{minor}"),"sys_platform":"darwin"
        },
        "soabi":soabi,"ext_suffix":suffix,"abiflags":"",
        "extension_suffixes":suffix.into_iter().collect::<Vec<_>>(),
        "gil_disabled":if implementation=="cpython" {Some(false)} else {None},
        "compatibility":null,"sysconfig_paths":{}
    })).unwrap()
}

pub(in super::super) fn layout(
    selected: &PythonRuntimeDescriptor,
    root: &Path,
    final_prefix: &Path,
    flavor: Flavor,
) -> Value {
    let leaf = match flavor {
        Flavor::Pypy312 => "pypy3.12",
        Flavor::Cpython39 => "python3.9",
        Flavor::Cpython311 => "python3.11",
        Flavor::Legacy31 => "python3.1",
    };
    let stdlib = root.join("lib").join(leaf);
    let include = root.join("include").join(leaf);
    let loader = root.join("bin/loader.dylib");
    let declared_library = if matches!(flavor, Flavor::Pypy312) {
        "libpypy3.11-c.dylib"
    } else {
        "libpython.dylib"
    };
    let mut value = json!({
        "schema":1,"observation_mode":"base_layout_site_disabled",
        "executable":selected.executable,
        "identity":{
            "implementation":selected.implementation,"language_version":selected.language_version,
            "implementation_version":selected.implementation_version,"soabi":selected.soabi,
            "ext_suffix":selected.ext_suffix,"abiflags":selected.abiflags,"gil_disabled":selected.gil_disabled
        },
        "prefix":root,"exec_prefix":root,"base_prefix":root,"base_exec_prefix":root,
        "sys_path":[stdlib,stdlib.join("lib-dynload")],"sysconfig_origin":stdlib.join("sysconfig.py"),
        "path_api":"sysconfig","scheme_names":["posix_prefix"],"selected_scheme":"posix_prefix",
        "base_paths":{"stdlib":stdlib,"platstdlib":stdlib,"purelib":stdlib.join("site-packages"),"platlib":stdlib.join("site-packages"),"include":include,"platinclude":include,"scripts":root.join("bin"),"data":root},
        "target_prefix":final_prefix,
        "target_paths":{"purelib":final_prefix.join("lib").join(leaf).join("site-packages"),"platlib":final_prefix.join("lib").join(leaf).join("site-packages"),"headers":final_prefix.join("include").join(leaf),"scripts":final_prefix.join("bin"),"data":final_prefix},
        "config_vars":{"LIBDIR":root.join("bin"),"LDLIBRARY":declared_library,"LIBRARY":"","INCLUDEPY":include,"SOABI":selected.soabi,"EXT_SUFFIX":selected.ext_suffix},
        "files":{
            "python_h":{"advertised":include.join("Python.h"),"status":{"state":"missing"}},
            "libdir_ldlibrary":{"advertised":root.join("bin").join(declared_library),"status":{"state":"missing"}}
        },
        "loader":{"state":"observed","images":[selected.executable,loader]}
    });
    if matches!(flavor, Flavor::Cpython39) {
        value["base_paths"]["purelib"] = json!("/Library/Python/3.9/site-packages");
        value["base_paths"]["platlib"] = json!("/Library/Python/3.9/site-packages");
        value["base_paths"]["scripts"] = json!("/usr/local/bin");
        value["config_vars"]["BINDIR"] = json!(
            "/Applications/Xcode.app/Contents/Developer/Library/Frameworks/Python3.framework/Versions/3.9/bin"
        );
    }
    if matches!(flavor, Flavor::Legacy31) {
        value["base_prefix"] = Value::Null;
        value["base_exec_prefix"] = Value::Null;
        value["path_api"] = json!("distutils_sysconfig");
        value["scheme_names"] = json!([]);
        value["selected_scheme"] = Value::Null;
    }
    value
}
