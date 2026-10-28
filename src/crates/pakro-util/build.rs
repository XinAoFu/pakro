use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=native/native_exec.c");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target != "linux" && target != "macos" {
        return;
    }
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let compiler = cc::Build::new().get_compiler();
    let mut library = compiler.to_command();
    library.args(["-std=c11", "-O2", "-Wall", "-Wextra", "-fPIC"]);
    library.arg(if target == "macos" {
        "-dynamiclib"
    } else {
        "-shared"
    });
    library
        .arg("native/native_exec.c")
        .arg("-o")
        .arg(output.join(if target == "macos" {
            "libpakro_native_gate.dylib"
        } else {
            "libpakro_native_gate.so"
        }));
    if target == "linux" {
        library.arg("-ldl");
    } else {
        library.arg("-Wl,-install_name,@rpath/libpakro_native_gate.dylib");
    }
    assert!(
        library.status().unwrap().success(),
        "native exec gate compilation failed"
    );
    let mut wrapper = compiler.to_command();
    wrapper
        .args([
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-DPAKRO_NATIVE_WRAPPER=1",
        ])
        .arg("native/native_exec.c")
        .arg("-o")
        .arg(output.join("pakro-native-compiler-wrapper"));
    assert!(
        wrapper.status().unwrap().success(),
        "native compiler wrapper compilation failed"
    );
}
