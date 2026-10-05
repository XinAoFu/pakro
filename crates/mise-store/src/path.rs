use std::path::{Path, PathBuf};

use crate::identity::PackageIdentity;

pub fn entry_dir(root: &Path, id: &PackageIdentity) -> PathBuf {
    root.join(id.eco.dir_name())
        .join(id.dir_name())
        .join(&id.version)
        .join(id.digest.to_string())
}
