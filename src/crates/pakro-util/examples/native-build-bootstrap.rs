//! Small executable for the native supervisor's component tests.
fn main() -> std::process::ExitCode {
    pakro_util::native_build::try_handle_internal().unwrap_or(std::process::ExitCode::FAILURE)
}
