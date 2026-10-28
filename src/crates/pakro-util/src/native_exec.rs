//! Embedded native exec gate and fixed compiler wrapper for supervised build sessions.

pub const SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "macos"));

#[cfg(target_os = "linux")]
pub const GATE_LIBRARY_NAME: &str = "libpakro_native_gate.so";
#[cfg(target_os = "macos")]
pub const GATE_LIBRARY_NAME: &str = "libpakro_native_gate.dylib";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub const GATE_LIBRARY_NAME: &str = "native-exec-gate-unavailable";

#[cfg(target_os = "linux")]
pub const GATE_LIBRARY_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/libpakro_native_gate.so"));
#[cfg(target_os = "macos")]
pub const GATE_LIBRARY_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/libpakro_native_gate.dylib"));
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub const GATE_LIBRARY_BYTES: &[u8] = &[];

pub const COMPILER_WRAPPER_NAME: &str = "pakro-native-compiler-wrapper";
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub const COMPILER_WRAPPER_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/pakro-native-compiler-wrapper"));
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub const COMPILER_WRAPPER_BYTES: &[u8] = &[];
