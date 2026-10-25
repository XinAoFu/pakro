//! Command names supplied by the CLI for diagnostics.
use std::sync::OnceLock;
pub struct Frontend {
    pub subcommand_names: fn() -> Vec<String>,
}
static FRONTEND: OnceLock<Frontend> = OnceLock::new();
pub fn register(frontend: Frontend) {
    let _ = FRONTEND.set(frontend);
}
pub(crate) fn subcommand_names() -> Vec<String> {
    (FRONTEND
        .get()
        .expect("frontend::register must run before diagnostics")
        .subcommand_names)()
}
