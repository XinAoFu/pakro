//! Local glob expansion for directory-aware configuration observation.

use eyre::Result;
use std::path::{Path, PathBuf};

pub(crate) fn glob(root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    if patterns.is_empty() {
        return Ok(vec![]);
    }
    let opts = glob::MatchOptions {
        require_literal_separator: true,
        ..Default::default()
    };
    Ok(patterns
        .iter()
        .map(|pattern| root.join(pattern).to_string_lossy().to_string())
        .filter_map(|pattern| glob::glob_with(&pattern, opts).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .flat_map(|paths| paths.filter_map(|p| p.ok()))
        .collect())
}
