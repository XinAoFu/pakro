//! Patch `sysconfig` data in a Python installation.
//!
//! Inspired by: <https://github.com/bluss/sysconfigpatcher/blob/c1ebf8ab9274dcde255484d93ce0f1fd1f76a248/src/sysconfigpatcher.py#L137C1-L140C100>,
//! available under the MIT license:
//!
//! ```text
//! Copyright 2024 Ulrik Sverdrup "bluss"
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy of
//! this software and associated documentation files (the "Software"), to deal in
//! the Software without restriction, including without limitation the rights to
//! use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
//! the Software, and to permit persons to whom the Software is furnished to do so,
//! subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
//! FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
//! COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
//! IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
//! CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
//! ```

use crate::sysconfig::parser::{Error as ParseError, SysconfigData, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::LazyLock;

mod cursor;
mod parser;

/// Replacement mode for sysconfig values.
#[derive(Debug)]
enum ReplacementMode {
    Partial { from: String },
    Full,
}

/// A replacement entry to patch in sysconfig data.
#[derive(Debug)]
struct ReplacementEntry {
    mode: ReplacementMode,
    to: String,
}

impl ReplacementEntry {
    /// Patches a sysconfig value either partially (replacing a specific word) or fully.
    fn patch(&self, entry: &str) -> String {
        match &self.mode {
            ReplacementMode::Partial { from } => entry
                .split_whitespace()
                .map(|word| if word == from { &self.to } else { word })
                .collect::<Vec<_>>()
                .join(" "),
            ReplacementMode::Full => self.to.clone(),
        }
    }
}

/// Mapping for sysconfig keys to lookup and replace with the appropriate entry.
static DEFAULT_VARIABLE_UPDATES: LazyLock<BTreeMap<String, ReplacementEntry>> =
    LazyLock::new(|| {
        BTreeMap::from_iter([
            (
                "CC".to_string(),
                ReplacementEntry {
                    mode: ReplacementMode::Partial {
                        from: "clang".to_string(),
                    },
                    to: "cc".to_string(),
                },
            ),
            (
                "CXX".to_string(),
                ReplacementEntry {
                    mode: ReplacementMode::Partial {
                        from: "clang++".to_string(),
                    },
                    to: "c++".to_string(),
                },
            ),
            (
                "BLDSHARED".to_string(),
                ReplacementEntry {
                    mode: ReplacementMode::Partial {
                        from: "clang".to_string(),
                    },
                    to: "cc".to_string(),
                },
            ),
            (
                "LDSHARED".to_string(),
                ReplacementEntry {
                    mode: ReplacementMode::Partial {
                        from: "clang".to_string(),
                    },
                    to: "cc".to_string(),
                },
            ),
            (
                "LDCXXSHARED".to_string(),
                ReplacementEntry {
                    mode: ReplacementMode::Partial {
                        from: "clang++".to_string(),
                    },
                    to: "c++".to_string(),
                },
            ),
            (
                "LINKCC".to_string(),
                ReplacementEntry {
                    mode: ReplacementMode::Partial {
                        from: "clang".to_string(),
                    },
                    to: "cc".to_string(),
                },
            ),
            (
                "AR".to_string(),
                ReplacementEntry {
                    mode: ReplacementMode::Full,
                    to: "ar".to_string(),
                },
            ),
        ])
    });

/// Update the `sysconfig` data in a Python installation.
pub fn update_sysconfig(
    install_root: &Path,
    major: u8,
    minor: u8,
    suffix: &str,
) -> Result<(), Error> {
    // Find the `_sysconfigdata_` file in the Python installation.
    let real_prefix = std::path::absolute(install_root)?;
    let canonical_root = real_prefix.canonicalize()?;
    let sysconfigdata = find_sysconfigdata(&real_prefix, major, minor, suffix)?.canonicalize()?;
    let metadata = std::fs::symlink_metadata(&sysconfigdata)?;
    if !sysconfigdata.starts_with(&canonical_root) || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sysconfigdata must be a regular file contained in the Python install root",
        )
        .into());
    }
    trace!(
        "Discovered `sysconfig` data at: {}",
        sysconfigdata.display()
    );

    // Update the `_sysconfigdata_` file in-memory.
    let contents = std::fs::read_to_string(&sysconfigdata)?;
    let contents = patch_sysconfig_contents(&contents, &real_prefix)?;

    // Replace the selected name instead of truncating its inode: the original
    // may also be linked from an immutable store or another installation.
    let parent = sysconfigdata.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sysconfigdata parent is missing",
        )
    })?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(contents.as_bytes())?;
    file.flush()?;
    file.as_file().set_permissions(metadata.permissions())?;
    file.as_file().sync_all()?;
    file.persist(&sysconfigdata).map_err(|error| error.error)?;

    Ok(())
}

/// Transform already owned sysconfig bytes without opening or writing a path.
pub fn patch_sysconfig_contents(contents: &str, real_prefix: &Path) -> Result<String, Error> {
    let data = SysconfigData::from_str(contents)?;
    let data = patch_sysconfigdata(data, real_prefix);
    Ok(data.to_string_pretty()?)
}

/// Find the `_sysconfigdata_` file in a Python installation.
///
/// For example, on macOS, returns `{real_prefix}/lib/python3.12/_sysconfigdata__darwin_darwin.py"`.
fn find_sysconfigdata(
    real_prefix: &Path,
    major: u8,
    minor: u8,
    suffix: &str,
) -> Result<PathBuf, Error> {
    // Find the `lib` directory in the Python installation.
    // Try candidates in order: with suffix, without suffix, and with "t" suffix
    // (free-threaded Python uses e.g. `python3.14t` as its lib directory name).
    let candidates = [
        format!("python{major}.{minor}{suffix}"),
        format!("python{major}.{minor}"),
        format!("python{major}.{minor}t"),
    ];
    let lib = candidates
        .iter()
        .map(|c| real_prefix.join("lib").join(c))
        .find(|p| p.exists())
        .ok_or(Error::MissingLib)?;

    // Probe the `lib` directory for `_sysconfigdata_`.
    for entry in lib.read_dir()? {
        let entry = entry?;

        if entry.path().extension().is_none_or(|ext| ext != "py") {
            continue;
        }

        if !entry
            .path()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.starts_with("_sysconfigdata_"))
        {
            continue;
        }

        let metadata = entry.metadata()?;
        // Resolve and validate the selected path centrally before reading or
        // writing it. An outside symlink must fail, rather than be skipped in
        // favor of a different candidate, while a contained alias is safe.
        if metadata.is_file() || metadata.is_symlink() {
            return Ok(entry.path());
        }
    }

    Err(Error::MissingSysconfigdata)
}

/// Patch the given `_sysconfigdata_` contents.
fn patch_sysconfigdata(mut data: SysconfigData, real_prefix: &Path) -> SysconfigData {
    /// Update the `/install` prefix in a whitespace-separated string.
    fn update_prefix(s: &str, real_prefix: &Path) -> String {
        s.split_whitespace()
            .map(|part| {
                if let Some(rest) = part.strip_prefix("/install") {
                    if rest.is_empty() {
                        real_prefix.display().to_string()
                    } else {
                        real_prefix.join(&rest[1..]).display().to_string()
                    }
                } else {
                    part.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Remove any references to `-isysroot` in a whitespace-separated string.
    fn remove_isysroot(s: &str) -> String {
        // If we see `-isysroot`, drop it and the next part.
        let mut parts = s.split_whitespace().peekable();
        let mut result = Vec::with_capacity(parts.size_hint().0);
        while let Some(part) = parts.next() {
            if part == "-isysroot" {
                parts.next();
            } else {
                result.push(part);
            }
        }
        result.join(" ")
    }

    // Patch each value, as needed.
    let mut count = 0;
    for (key, value) in data.iter_mut() {
        let Value::String(value) = value else {
            continue;
        };
        let patched = update_prefix(value, real_prefix);
        let mut patched = remove_isysroot(&patched);

        if let Some(replacement_entry) = DEFAULT_VARIABLE_UPDATES.get(key) {
            patched = replacement_entry.patch(&patched);
        }

        if *value != patched {
            trace!("Updated `{key}` from `{value}` to `{patched}`");
            count += 1;
            *value = patched;
        }
    }

    match count {
        0 => trace!("No updates required"),
        1 => trace!("Updated 1 value"),
        n => trace!("Updated {n} values"),
    }

    // Mark the Python installation as standalone.
    data.insert("PYTHON_BUILD_STANDALONE".to_string(), Value::Int(1));

    data
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Python installation is missing a `lib` directory")]
    MissingLib,
    #[error("Python installation is missing a `_sysconfigdata_` file")]
    MissingSysconfigdata,
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    const CONFIG: &str = concat!(
        "# system configuration generated and used by the sysconfig module\n",
        "build_time_vars = {\"BINDIR\": \"/install/bin\"}\n",
    );

    #[test]
    fn update_sysconfig_accepts_regular_and_contained_alias_files() {
        for alias in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let lib = root.path().join("lib/python3.15");
            std::fs::create_dir_all(&lib).unwrap();
            let selected = lib.join("_sysconfigdata_fixture.py");
            let config = if alias {
                let target = root.path().join("contained-config.py");
                std::fs::write(&target, CONFIG).unwrap();
                std::os::unix::fs::symlink(&target, &selected).unwrap();
                target
            } else {
                std::fs::write(&selected, CONFIG).unwrap();
                selected.clone()
            };
            update_sysconfig(root.path(), 3, 15, "").unwrap();
            let patched = std::fs::read_to_string(config).unwrap();
            assert!(patched.contains("PYTHON_BUILD_STANDALONE"));
            assert!(!patched.contains("/install/bin"));
            if alias {
                assert!(std::fs::symlink_metadata(selected).unwrap().is_symlink());
            }
        }
    }

    #[test]
    fn update_sysconfig_rejects_escaping_lib_and_stdlib_directories_before_writing() {
        for whole_lib in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let external = if whole_lib {
                outside.path().join("python3.15/_sysconfigdata_fixture.py")
            } else {
                outside.path().join("_sysconfigdata_fixture.py")
            };
            std::fs::create_dir_all(external.parent().unwrap()).unwrap();
            std::fs::write(&external, CONFIG).unwrap();
            let alias = if whole_lib {
                root.path().join("lib")
            } else {
                std::fs::create_dir(root.path().join("lib")).unwrap();
                root.path().join("lib/python3.15")
            };
            std::os::unix::fs::symlink(outside.path(), alias).unwrap();
            let error = update_sysconfig(root.path(), 3, 15, "").unwrap_err();
            assert!(error.to_string().contains("contained"), "{error}");
            assert_eq!(std::fs::read_to_string(external).unwrap(), CONFIG);
        }
    }

    #[test]
    fn update_sysconfig_rejects_an_escaping_config_file_alias_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let external = outside.path().join("external-config.py");
        std::fs::write(&external, CONFIG).unwrap();
        let lib = root.path().join("lib/python3.15");
        std::fs::create_dir_all(&lib).unwrap();
        let alias = lib.join("_sysconfigdata_fixture.py");
        std::os::unix::fs::symlink(&external, &alias).unwrap();
        let error = update_sysconfig(root.path(), 3, 15, "").unwrap_err();
        assert!(error.to_string().contains("contained"), "{error}");
        assert_eq!(std::fs::read_to_string(external).unwrap(), CONFIG);
        assert!(std::fs::symlink_metadata(alias).unwrap().is_symlink());
    }

    #[test]
    fn update_sysconfig_requires_a_regular_file_after_resolving_an_alias() {
        let root = tempfile::tempdir().unwrap();
        let lib = root.path().join("lib/python3.15");
        std::fs::create_dir_all(&lib).unwrap();
        let directory = root.path().join("not-config");
        std::fs::create_dir(&directory).unwrap();
        std::os::unix::fs::symlink(&directory, lib.join("_sysconfigdata_fixture.py")).unwrap();
        let error = update_sysconfig(root.path(), 3, 15, "").unwrap_err();
        assert!(error.to_string().contains("regular file"), "{error}");
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 0);
    }

    #[test]
    fn update_sysconfig_breaks_external_hardlinks_and_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let external = outside.path().join("shared-config.py");
        std::fs::write(&external, CONFIG).unwrap();
        std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o640)).unwrap();
        let lib = root.path().join("lib/python3.15");
        std::fs::create_dir_all(&lib).unwrap();
        let selected = lib.join("_sysconfigdata_fixture.py");
        std::fs::hard_link(&external, &selected).unwrap();

        update_sysconfig(root.path(), 3, 15, "").unwrap();

        assert_eq!(
            std::fs::read_to_string(&external).unwrap(),
            CONFIG,
            "patching an install must not mutate an outside hardlinked inode"
        );
        let patched = std::fs::read_to_string(&selected).unwrap();
        assert!(patched.contains("PYTHON_BUILD_STANDALONE"));
        assert!(!patched.contains("/install/bin"));
        assert_eq!(
            std::fs::metadata(&selected).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            std::fs::metadata(&external).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn update_real_prefix() -> Result<(), Error> {
        let sysconfigdata = [
            ("BASEMODLIBS", ""),
            ("BINDIR", "/install/bin"),
            ("BINLIBDEST", "/install/lib/python3.10"),
            ("BLDLIBRARY", "-L. -lpython3.10"),
            ("BUILDPYTHON", "python.exe"),
            ("prefix", "/install/prefix"),
            ("exec_prefix", "/install/exec_prefix"),
            ("base", "/install/base"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect::<SysconfigData>();

        let real_prefix = Path::new("/real/prefix");
        let data = patch_sysconfigdata(sysconfigdata, real_prefix);

        insta::assert_snapshot!(data.to_string_pretty()?, @r###"
        # system configuration generated and used by the sysconfig module
        build_time_vars = {
            "BASEMODLIBS": "",
            "BINDIR": "/real/prefix/bin",
            "BINLIBDEST": "/real/prefix/lib/python3.10",
            "BLDLIBRARY": "-L. -lpython3.10",
            "BUILDPYTHON": "python.exe",
            "PYTHON_BUILD_STANDALONE": 1,
            "base": "/real/prefix/base",
            "exec_prefix": "/real/prefix/exec_prefix",
            "prefix": "/real/prefix/prefix"
        }
        "###);

        Ok(())
    }

    #[test]
    fn test_replacements() -> Result<(), Error> {
        let sysconfigdata = [
            ("CC", "clang -pthread"),
            ("CXX", "clang++ -pthread"),
            ("AR", "/tools/llvm/bin/llvm-ar"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect::<SysconfigData>();

        let real_prefix = Path::new("/real/prefix");
        let data = patch_sysconfigdata(sysconfigdata, real_prefix);

        insta::assert_snapshot!(data.to_string_pretty()?, @r###"
        # system configuration generated and used by the sysconfig module
        build_time_vars = {
            "AR": "ar",
            "CC": "cc -pthread",
            "CXX": "c++ -pthread",
            "PYTHON_BUILD_STANDALONE": 1
        }
        "###);

        Ok(())
    }

    #[test]
    fn remove_isysroot() -> Result<(), Error> {
        let sysconfigdata = [
            ("BLDSHARED", "clang -bundle -undefined dynamic_lookup -arch arm64 -isysroot /Applications/MacOSX14.2.sdk"),
        ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect::<SysconfigData>();

        let real_prefix = Path::new("/real/prefix");
        let data = patch_sysconfigdata(sysconfigdata, real_prefix);

        insta::assert_snapshot!(data.to_string_pretty()?, @r###"
        # system configuration generated and used by the sysconfig module
        build_time_vars = {
            "BLDSHARED": "cc -bundle -undefined dynamic_lookup -arch arm64",
            "PYTHON_BUILD_STANDALONE": 1
        }
        "###);

        Ok(())
    }
}
