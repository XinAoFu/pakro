//! Filesystem helpers shared by materializers.

use std::fs;
use std::path::Path;

/// Recursively recreates `src` at `dst`: directories are created, regular
/// files are hardlinked when the filesystem allows it (same volume) and
/// copied otherwise, and symlinks are recreated pointing at their original
/// targets. Hardlinks keep the store the single owner of the bytes while
/// making the copy behave like a real directory tree for tools that resolve
/// realpaths (node's resolver).
pub fn hardlink_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = entry.metadata()?;
        if meta.is_dir() {
            hardlink_tree(&from, &to)?;
        } else if entry.file_type()?.is_symlink() {
            let target = fs::read_link(&from)?;
            if fs::symlink_metadata(&to).is_ok() {
                fs::remove_file(&to)?;
            }
            std::os::unix::fs::symlink(target, &to)?;
        } else if fs::hard_link(&from, &to).is_err() {
            // cross-device or filesystem without hardlinks: fall back to a
            // copy, still correct, just not deduplicated
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
