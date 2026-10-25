use crate::Result;
use crate::config::pakrorc;
use crate::file::replace_path;
use crate::shell::ShellType;
use crate::{args::ToolArg, file::display_path};
use eyre::Context;
use indexmap::IndexSet;
pub use pakro_util::env::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::LazyLock as Lazy;
use std::sync::RwLock;
use std::{path::Path, string::ToString};

pub static TOOL_ARGS: RwLock<Vec<ToolArg>> = RwLock::new(vec![]);
pub static PAKRO_SHELL: Lazy<Option<ShellType>> =
    Lazy::new(|| detect_shell(var("PAKRO_SHELL").ok(), var("SHELL").ok(), &SHELL));

/// Which shell pakro should speak, from the environment.
///
/// `SHELL` is consulted here but deliberately *not* through [`SHELL`], which on Windows reads
/// `COMSPEC`. Git Bash, MSYS2 and Cygwin all set `SHELL` to a real shell there, and
/// [`ShellType::from_str`] already understands the form they use, but nothing ever looked. Reading
/// it through [`SHELL`] instead would be wrong: [`SHELL_COMMAND_FLAG`] is `/c` on Windows and
/// `pakro exec -c` and `pakro en` pair the two, so `bash.exe /c …` is what that would run.
///
/// Split out from the `Lazy` because that is process-wide and reads the real environment, so the
/// precedence below cannot be exercised from a test any other way.
fn detect_shell(
    pakro_shell: Option<String>,
    shell_var: Option<String>,
    fallback: &str,
) -> Option<ShellType> {
    // What `pakro activate` exports, so it decides on its own. Set but unparseable is an answer,
    // not a reason to start guessing.
    if let Some(s) = pakro_shell {
        return s.parse().ok();
    }
    // On unix `SHELL` *is* the fallback, so consulting it separately would be the same lookup
    // twice. On Windows it is the one the fallback cannot reach.
    if cfg!(windows)
        && let Some(st) = shell_var.and_then(|s| s.parse().ok())
    {
        return Some(st);
    }
    fallback.parse().ok()
}

// paths and directories

// data subdirs

pub static PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME: Lazy<String> = Lazy::new(|| {
    var("PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME")
        .ok()
        .or(PAKRO_OVERRIDE_TOOL_VERSIONS_FILENAMES
            .as_ref()
            .and_then(|v| v.first().cloned()))
        .or(var("PAKRO_DEFAULT_TOOL_VERSIONS_FILENAME").ok())
        .unwrap_or_else(|| ".tool-versions".into())
});
pub static PAKRO_DEFAULT_CONFIG_FILENAME: Lazy<String> = Lazy::new(|| {
    var("PAKRO_DEFAULT_CONFIG_FILENAME")
        .ok()
        .or(PAKRO_OVERRIDE_CONFIG_FILENAMES.first().cloned())
        .unwrap_or_else(|| "pakro.toml".into())
});
pub(crate) static PAKRO_OVERRIDE_TOOL_VERSIONS_FILENAMES: Lazy<Option<IndexSet<String>>> =
    Lazy::new(|| match var("PAKRO_OVERRIDE_TOOL_VERSIONS_FILENAMES") {
        Ok(v) if v == "none" => Some([].into()),
        Ok(v) => Some(split_colon_list(&v)),
        Err(_) => {
            pakrorc::get_override_tool_versions_filenames().map(|v| v.iter().cloned().collect())
        }
    });
pub(crate) static PAKRO_OVERRIDE_CONFIG_FILENAMES: Lazy<IndexSet<String>> =
    Lazy::new(|| match var("PAKRO_OVERRIDE_CONFIG_FILENAMES") {
        Ok(v) => split_colon_list(&v),
        Err(_) => pakrorc::get_override_config_filenames()
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default(),
    });
pub static PAKRO_ENV: Lazy<Vec<String>> = Lazy::new(|| environment(&ARGS.read().unwrap()));

/// The tri-state auto_env setting: PAKRO_AUTO_ENV env var > .pakrorc.toml > unset
pub(crate) fn auto_env_setting() -> Option<bool> {
    if var_is_true("PAKRO_AUTO_ENV") {
        Some(true)
    } else if var_is_false("PAKRO_AUTO_ENV") {
        Some(false)
    } else {
        pakrorc::get_auto_env()
    }
}

/// The tri-state env_conf_d setting: PAKRO_ENV_CONF_D env var > .pakrorc.toml > unset.
pub(crate) fn env_conf_d_setting() -> Option<bool> {
    if var_is_true("PAKRO_ENV_CONF_D") {
        Some(true)
    } else if var_is_false("PAKRO_ENV_CONF_D") {
        Some(false)
    } else {
        pakrorc::get_env_conf_d()
    }
}

/// Keep dotted conf.d fragments unconditional through the deprecation window.
pub(crate) fn env_conf_d_default_for_version(v: &versions::Versioning) -> bool {
    *v >= versions::Versioning::new("2027.8.10").unwrap()
}

/// Whether `conf.d` filenames carry environment suffixes, resolving the
/// setting against the version-gated default.
pub(crate) fn env_conf_d() -> bool {
    env_conf_d_setting().unwrap_or_else(|| env_conf_d_default_for_version(&crate::version::V))
}

/// Default for auto_env when the setting is unset: off until pakro 2027.6.0
pub(crate) fn auto_env_default_for_version(v: &versions::Versioning) -> bool {
    *v >= versions::Versioning::new("2027.6.0").unwrap()
}

/// Platform-derived environment names, regardless of whether auto_env is enabled.
/// Ordered least to most specific: os family ("unix"), os, "{os}-{arch}".
/// On Windows the family equals the os so it dedupes to ["windows", "windows-{arch}"].
pub(crate) fn platform_env_names() -> Vec<String> {
    let mut names: Vec<String> = vec![];
    for name in [
        consts::FAMILY.to_string(),
        crate::platform::OS.to_string(),
        format!("{}-{}", *crate::platform::OS, *crate::platform::ARCH),
    ] {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Platform environments active for config file discovery and lockfile selection.
/// Empty unless auto_env is enabled. Names already in PAKRO_ENV are excluded so
/// explicit environments keep their user-specified (higher) precedence.
/// These are deliberately not part of PAKRO_ENV: they do not affect the
/// `{{ pakro_env }}` template variable or PAKRO_ENV propagation to subprocesses.
pub(crate) static AUTO_ENV_NAMES: Lazy<Vec<String>> = Lazy::new(|| {
    let enabled =
        auto_env_setting().unwrap_or_else(|| auto_env_default_for_version(&crate::version::V));
    if !enabled {
        return vec![];
    }
    platform_env_names()
        .into_iter()
        .filter(|name| !PAKRO_ENV.contains(name))
        .collect()
});

/// Auto platform envs followed by explicit PAKRO_ENV entries, for "later wins"
/// consumers like config filename enumeration.
pub(crate) static PAKRO_ENV_WITH_AUTO: Lazy<Vec<String>> = Lazy::new(|| {
    AUTO_ENV_NAMES
        .iter()
        .chain(PAKRO_ENV.iter())
        .cloned()
        .collect()
});

pub(crate) static PAKRO_IGNORED_CONFIG_PATHS: Lazy<Vec<PathBuf>> = Lazy::new(|| {
    env_ignored_config_paths()
        .or_else(|| {
            pakrorc::get_ignored_config_paths().map(|paths| paths.iter().cloned().collect())
        })
        .unwrap_or_default()
});
/// `ignored_config_paths` without project `.pakrorc.toml` files, for operations
/// over every tracked config whose result must not depend on the working directory.
pub(crate) static PAKRO_GLOBAL_IGNORED_CONFIG_PATHS: Lazy<Vec<PathBuf>> = Lazy::new(|| {
    env_ignored_config_paths()
        .or_else(|| {
            pakrorc::get_global_ignored_config_paths().map(|paths| paths.iter().cloned().collect())
        })
        .unwrap_or_default()
});
fn env_ignored_config_paths() -> Option<Vec<PathBuf>> {
    let invocation_cwd = pakrorc::invocation_cwd()
        .map(Path::to_path_buf)
        .or_else(|| current_dir().ok())
        .unwrap_or_default();
    var_os("PAKRO_IGNORED_CONFIG_PATHS").map(|v| {
        split_paths(&v)
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| pakrorc::resolve_ignored_config_path(p, &invocation_cwd))
            .collect()
    })
}
pub(crate) static PAKRO_CEILING_PATHS: Lazy<HashSet<PathBuf>> = Lazy::new(|| {
    var_os("PAKRO_CEILING_PATHS")
        .map(|v| {
            split_paths(&v)
                .filter(|p| !p.as_os_str().is_empty())
                .map(replace_path)
                .collect()
        })
        .or_else(|| {
            pakrorc::get_ceiling_paths()
                .map(|paths| paths.iter().cloned().map(replace_path).collect())
        })
        .unwrap_or_default()
});

// true if running inside a shim

// true if the current process is running as a shim (not direct pakro invocation)

pub(crate) static LINUX_DISTRO: Lazy<Option<String>> = Lazy::new(linux_distro);

/// Whether terminal-width presentation output should be truncated.
pub fn should_truncate() -> bool {
    crate::config::Settings::get().truncate
}

// python

fn environment(args: &[String]) -> Vec<String> {
    let arg_defs = HashSet::from(["--profile", "-P", "--env", "-E"]);

    // Get environment value from args or env vars
    // Precedence: CLI args > env vars > .pakrorc.toml
    let from_args = if *IS_RUNNING_AS_SHIM {
        // When running as shim, ignore command line args and use env vars only
        vec![]
    } else {
        // Subcommands where positional args accept hyphen values, so -E after the
        // first positional would be a task arg, not a global flag.
        let run_subcommands: HashSet<&str> = HashSet::from(["run", "r"]);
        // Try to get from command line args first
        // Handles `--env production`, `--env=production`, `-E production`, `-E=production`,
        // and `-Eproduction`.
        let mut values = Vec::new();
        let mut it = args.iter().take_while(|a| a.as_str() != "--");
        let mut in_run_subcommand = false;
        while let Some(arg) = it.next() {
            if arg.starts_with('-') {
                if arg_defs.contains(arg.as_str()) {
                    // Case: `-E production` or `--env production`
                    if let Some(next) = it.next() {
                        values.push(next.to_string());
                    }
                } else if let Some((prefix, rest)) = arg.split_at_checked(2)
                    && !rest.starts_with('=')
                    && arg_defs.contains(prefix)
                {
                    // Case: `-Eproduction`
                    values.push(rest.to_string());
                } else if let Some((flag, value)) = arg.split_once('=') {
                    // Case: `-E=production` or `--env=production`
                    if arg_defs.contains(flag) {
                        values.push(value.to_string());
                    }
                }
            } else {
                // After `run`/`r`, the first positional is the task name — everything
                // after that belongs to the task, so stop scanning for env flags.
                if in_run_subcommand {
                    break;
                }
                if run_subcommands.contains(arg.as_str()) {
                    in_run_subcommand = true;
                }
            }
        }
        values
            .into_iter()
            .flat_map(|s| {
                s.split(',')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    if !from_args.is_empty() {
        return from_args;
    }
    var("PAKRO_ENV")
        .ok()
        .or_else(|| var("PAKRO_PROFILE").ok())
        .or_else(|| var("PAKRO_ENVIRONMENT").ok())
        .map(|s| {
            s.split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .or_else(|| pakrorc::get_env().cloned())
        .unwrap_or_default()
}

fn linux_distro() -> Option<String> {
    crate::platform::linux_os_release().map(|release| release.id.clone())
}

/// Split a colon-separated string into a set, filtering empty segments.
/// Empty segments arise from empty strings, leading/trailing colons, or
/// consecutive colons — all of which should be ignored rather than
/// injected as empty paths into config discovery.
fn split_colon_list(value: &str) -> IndexSet<String> {
    value
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

pub fn set_current_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref();
    trace!("cd {}", display_path(path));
    std::env::set_current_dir(path)
        .wrap_err_with(|| format!("failed to set current directory to {}", display_path(path)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn test_var_path() {
        let _config = Config::get().await.unwrap();
        set_var("PAKRO_TEST_PATH", "/foo/bar");
        assert_eq!(
            var_path("PAKRO_TEST_PATH").unwrap(),
            PathBuf::from("/foo/bar")
        );
        remove_var("PAKRO_TEST_PATH");
    }

    /// An empty value is not a directory. Callers all join onto the result, so returning
    /// `Some("")` would hand them a relative path resolved against the cwd — e.g. an empty
    /// `XDG_CONFIG_HOME` turning `PAKRO_CONFIG_DIR` into the relative `pakro`.
    #[tokio::test]
    async fn test_var_path_treats_empty_as_unset() {
        let _config = Config::get().await.unwrap();
        set_var("PAKRO_TEST_EMPTY_PATH", "");
        assert_eq!(var_path("PAKRO_TEST_EMPTY_PATH"), None);
        remove_var("PAKRO_TEST_EMPTY_PATH");
    }

    /// vars_safe() must skip pairs whose key or value is not valid UTF-8 rather
    /// than panicking the way std::env::vars() does (#5370).
    #[cfg(unix)]
    #[test]
    fn test_vars_safe_skips_invalid_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        // 0xff can never appear in valid UTF-8.
        let bad_value_key = "PAKRO_TEST_VARS_SAFE_BAD_VALUE";
        let bad_key = OsString::from_vec(b"PAKRO_TEST_VARS_SAFE_BAD_KEY_\xff".to_vec());
        let good_key = "PAKRO_TEST_VARS_SAFE_GOOD";

        // the guard restores the previous environment on drop, so even a failing
        // assertion below cannot leak a non-UTF-8 var into the test process
        let mut guard = crate::test::EnvVarGuard::new();
        guard
            .set(bad_value_key, OsString::from_vec(vec![0xff]))
            .set(&bad_key, "ok")
            .set(good_key, "1");

        // Both malformed vars really are in the process environment...
        assert!(vars_os().any(|(k, _)| k == bad_value_key));
        assert!(vars_os().any(|(k, _)| k == bad_key));
        // ...and vars_safe() returns without panicking.
        let safe: Vec<(String, String)> = vars_safe().collect();

        assert!(!safe.iter().any(|(k, _)| k == bad_value_key));
        assert!(
            !safe
                .iter()
                .any(|(k, _)| k.starts_with("PAKRO_TEST_VARS_SAFE_BAD_KEY_"))
        );
        // Valid neighbours are still returned.
        assert!(safe.iter().any(|(k, v)| k == good_key && v == "1"));
    }

    #[test]
    fn test_auto_env_default_for_version() {
        let v = |s: &str| versions::Versioning::new(s).unwrap();
        assert!(!auto_env_default_for_version(&v("2026.6.2")));
        assert!(!auto_env_default_for_version(&v("2026.12.0")));
        assert!(!auto_env_default_for_version(&v("2027.5.9")));
        assert!(auto_env_default_for_version(&v("2027.6.0")));
        assert!(auto_env_default_for_version(&v("2028.1.0")));
    }

    #[test]
    fn test_env_conf_d_default_for_version() {
        let v = |s: &str| versions::Versioning::new(s).unwrap();
        assert!(!env_conf_d_default_for_version(&v("2026.8.10")));
        assert!(!env_conf_d_default_for_version(&v("2027.8.9")));
        assert!(env_conf_d_default_for_version(&v("2027.8.10")));
    }

    #[cfg(unix)]
    #[test]
    fn test_platform_env_names_unix() {
        let names = platform_env_names();
        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "unix");
        assert_eq!(names[1], *crate::platform::OS);
        assert_eq!(
            names[2],
            format!("{}-{}", *crate::platform::OS, *crate::platform::ARCH)
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_platform_env_names_windows() {
        // os family == os on windows, so the list dedupes to two entries
        let names = platform_env_names();
        assert_eq!(
            names,
            vec![
                "windows".to_string(),
                format!("windows-{}", *crate::platform::ARCH)
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_is_path_key_accepts_any_casing_on_windows() {
        for spelling in ["PATH", "Path", "path", "pAtH"] {
            assert!(is_path_key(spelling), "{spelling}");
            assert_eq!(normalize_path_key(spelling.to_string()), *PATH_KEY);
        }

        assert!(!is_path_key("PATHEXT"));
        assert!(!is_path_key("TEMP"));
        assert!(!is_path_key(""));
        // A name that is not PATH keeps the spelling the config gave it.
        assert_eq!(normalize_path_key("Temp".to_string()), "Temp");
    }

    #[cfg(unix)]
    #[test]
    fn test_is_path_key_is_exact_on_unix() {
        assert!(is_path_key("PATH"));
        assert_eq!(normalize_path_key("PATH".to_string()), "PATH");

        // `Path` is a variable of its own here, so folding it would drop what was asked for.
        assert!(!is_path_key("Path"));
        assert!(!is_path_key("path"));
        assert_eq!(normalize_path_key("Path".to_string()), "Path");

        assert!(!is_path_key("PATHEXT"));
        assert!(!is_path_key(""));
    }

    #[test]
    fn test_split_colon_list() {
        let cases: Vec<(&str, Vec<&str>)> = vec![
            ("", vec![]),    // empty string — was causing panic
            (":", vec![]),   // colon only
            (":::", vec![]), // multiple colons
            ("pakro.toml", vec!["pakro.toml"]),
            ("a:b", vec!["a", "b"]),
            (":a:b:", vec!["a", "b"]), // leading/trailing colons
            ("a::b", vec!["a", "b"]),  // consecutive colons
        ];
        for (input, expected) in cases {
            let result = split_colon_list(input);
            let expected: IndexSet<String> = expected.into_iter().map(|s| s.to_string()).collect();
            assert_eq!(result, expected, "input: {input:?}");
        }
    }

    #[test]
    fn test_is_pakro_binary() {
        // The spellings pakro is actually invoked under, on every platform.
        assert!(is_pakro_binary("pakro"));
        assert!(is_pakro_binary("pakro.exe"));
        assert!(is_pakro_binary("pakro.cmd"));
        assert!(is_pakro_binary("pakro-doctor"));
        // The controls. Real shim names must stay shims, or this stops being a test of anything:
        // a version that always returned true would pass every assertion above.
        assert!(!is_pakro_binary("node"));
        assert!(!is_pakro_binary("node.exe"));
        assert!(!is_pakro_binary("pakrox"));
        assert!(!is_pakro_binary("pakrox.exe"));
    }

    /// Windows resolves `PAKRO.EXE` to the same file as `pakro.exe` and hands the process `argv[0]`
    /// with the casing the caller wrote, so the test has to ignore case there. See the pair below.
    #[cfg(windows)]
    #[test]
    fn test_is_pakro_binary_ignores_case_on_windows() {
        assert!(is_pakro_binary("PAKRO.EXE"));
        assert!(is_pakro_binary("Pakro.exe"));
        assert!(is_pakro_binary("PAKRO"));
        assert!(is_pakro_binary("PAKRO-DOCTOR"));
        // Case-insensitivity must not widen what counts as pakro.
        assert!(!is_pakro_binary("NODE.EXE"));
        assert!(!is_pakro_binary("PAKROX.EXE"));
    }

    /// The other half. On unix `PAKRO` is a different file from `pakro`, so a shim by that name has
    /// to keep being treated as a shim; ignoring case here would make pakro run itself instead.
    #[cfg(unix)]
    #[test]
    fn test_is_pakro_binary_is_case_sensitive_on_unix() {
        assert!(!is_pakro_binary("PAKRO"));
        assert!(!is_pakro_binary("Pakro"));
        assert!(!is_pakro_binary("PAKRO-DOCTOR"));
    }

    fn detect(pakro_shell: Option<&str>, shell_var: Option<&str>, fallback: &str) -> String {
        detect_shell(
            pakro_shell.map(str::to_string),
            shell_var.map(str::to_string),
            fallback,
        )
        .map(|st| st.to_string())
        .unwrap_or_else(|| "(none)".to_string())
    }

    /// `pakro activate` exports `PAKRO_SHELL`, so it decides on its own — including deciding that
    /// the answer is nothing. Falling back after an unparseable value would start guessing at a
    /// shell the session has already named.
    #[test]
    fn pakro_shell_wins_and_an_unparseable_one_is_still_the_answer() {
        assert_eq!(detect(Some("zsh"), Some("/bin/bash"), "/bin/bash"), "zsh");
        assert_eq!(
            detect(Some("nonsense"), Some("/bin/bash"), "/bin/bash"),
            "(none)"
        );
    }

    /// The fix: Git Bash, MSYS2 and Cygwin set `SHELL` on Windows, where the fallback reads
    /// `COMSPEC` and so can never see it.
    #[cfg(windows)]
    #[test]
    fn shell_is_consulted_on_windows() {
        for shell_var in [
            r"C:\Program Files\Git\bin\bash.exe",
            "/bin/bash.exe",
            r"C:\msys64\usr\bin\zsh.exe",
        ] {
            let expected = match shell_var.contains("zsh") {
                true => "zsh",
                false => "bash",
            };
            assert_eq!(
                detect(None, Some(shell_var), r"C:\WINDOWS\system32\cmd.exe"),
                expected,
                "{shell_var}"
            );
        }
        // Unchanged where there is nothing to find: cmd.exe is not a shell pakro generates for.
        assert_eq!(detect(None, None, r"C:\WINDOWS\system32\cmd.exe"), "(none)");
    }

    /// The control. On unix `SHELL` *is* the fallback, so the extra lookup must not exist —
    /// otherwise this test would pass for the wrong reason on every platform.
    #[cfg(unix)]
    #[test]
    fn the_fallback_is_the_only_second_source_on_unix() {
        // A `SHELL` that disagrees with the fallback is ignored: unix passes the same value as
        // both, so anything else would mean the Windows branch had leaked.
        assert_eq!(detect(None, Some("/bin/zsh"), "/bin/bash"), "bash");
        assert_eq!(detect(None, None, "/bin/zsh"), "zsh");
        assert_eq!(detect(None, None, "sh"), "bash");
    }

    /// The names `self-replace` generates, which pakro used to report as broken shims. Measured with
    /// `TEMP` at 199 and 201 characters, where the copy's own init hook stops recognising itself.
    #[test]
    fn a_self_replace_copy_of_this_binary_is_recognised() {
        let rand = "qzcdgqhxhqwhzdwyqchsmdxcqouxxche";
        assert!(is_self_replace_helper(
            &format!(".pakro.{rand}.__selfdelete__.exe"),
            "pakro"
        ));
        assert!(is_self_replace_helper(
            &format!(".pakro.{rand}.__relocated__.exe"),
            "pakro"
        ));
        // The stem follows the binary, so a renamed pakro is still recognised.
        assert!(is_self_replace_helper(
            &format!(".pakro-dev.{rand}.__selfdelete__.exe"),
            "pakro-dev"
        ));
    }

    /// The controls. Two of them are the reason the stem is a parameter at all.
    #[test]
    fn ordinary_names_and_other_applications_are_not() {
        let rand = "qzcdgqhxhqwhzdwyqchsmdxcqouxxche";
        // pakro itself, and a genuine shim.
        assert!(!is_self_replace_helper("pakro.exe", "pakro"));
        assert!(!is_self_replace_helper("node.exe", "pakro"));
        // Another application's orphan, sitting in the same TEMP. Matching the suffix alone would
        // claim it — and the sweep would delete it.
        assert!(!is_self_replace_helper(
            &format!(".node.{rand}.__selfdelete__.exe"),
            "pakro"
        ));
        // Ours by name, but not one of these copies.
        assert!(!is_self_replace_helper(".pakro.something.exe", "pakro"));
    }

    /// The segment between the stem and the suffix has to be the one `self-replace` generates —
    /// 32 characters from `fastrand`'s `lowercase()`. The caller acting on a `true` here **deletes
    /// the file**, so a name that merely looks similar must not qualify.
    #[test]
    fn a_name_that_only_resembles_a_generated_one_is_left_alone() {
        let ok = "qzcdgqhxhqwhzdwyqchsmdxcqouxxche";
        assert_eq!(ok.len(), SELF_REPLACE_RANDOM_LEN, "premise");
        assert!(is_self_replace_helper(
            &format!(".pakro.{ok}.__selfdelete__.exe"),
            "pakro"
        ));

        // No segment at all.
        assert!(!is_self_replace_helper(
            ".pakro..__selfdelete__.exe",
            "pakro"
        ));
        assert!(!is_self_replace_helper(
            ".pakro.__selfdelete__.exe",
            "pakro"
        ));
        // Too short, and too long.
        assert!(!is_self_replace_helper(
            ".pakro.abc.__selfdelete__.exe",
            "pakro"
        ));
        assert!(!is_self_replace_helper(
            &format!(".pakro.{ok}x.__selfdelete__.exe"),
            "pakro"
        ));
        // Right length, wrong alphabet — a digit and an uppercase letter.
        let digits = format!("{}1", &ok[..SELF_REPLACE_RANDOM_LEN - 1]);
        assert!(!is_self_replace_helper(
            &format!(".pakro.{digits}.__selfdelete__.exe"),
            "pakro"
        ));
        let upper = format!("{}A", &ok[..SELF_REPLACE_RANDOM_LEN - 1]);
        assert!(!is_self_replace_helper(
            &format!(".pakro.{upper}.__selfdelete__.exe"),
            "pakro"
        ));
    }

    /// The stem carries whatever casing the caller used to start pakro — measured: `current_exe()`
    /// returns `CASEPROBE` for a `caseprobe.exe` started as `CASEPROBE.EXE`. So an update launched
    /// one way writes an orphan the next one, launched the other way, has to still recognise.
    #[cfg(windows)]
    #[test]
    fn an_orphan_from_a_differently_cased_launch_is_still_ours() {
        let rand = "qzcdgqhxhqwhzdwyqchsmdxcqouxxche";
        for stem in ["pakro", "PAKRO", "Pakro"] {
            for written in ["pakro", "PAKRO", "Pakro"] {
                assert!(
                    is_self_replace_helper(&format!(".{written}.{rand}.__selfdelete__.exe"), stem),
                    "stem {stem} should claim an orphan written as {written}"
                );
            }
        }
        // Still not another application's, however it is cased.
        assert!(!is_self_replace_helper(
            &format!(".NODE.{rand}.__selfdelete__.exe"),
            "pakro"
        ));
    }

    /// The control for the rule above: unix filesystems are case-sensitive, so `PAKRO` and `pakro`
    /// are different files and an orphan of one is not an orphan of the other.
    #[cfg(unix)]
    #[test]
    fn casing_still_separates_names_on_unix() {
        let rand = "qzcdgqhxhqwhzdwyqchsmdxcqouxxche";
        assert!(is_self_replace_helper(
            &format!(".pakro.{rand}.__selfdelete__.exe"),
            "pakro"
        ));
        assert!(!is_self_replace_helper(
            &format!(".PAKRO.{rand}.__selfdelete__.exe"),
            "pakro"
        ));
    }
}
