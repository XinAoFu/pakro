//! The unit-test fixture both test binaries start from: the library's
//! (`src/core/util/test.rs`) and the `pakro` binary's (`src/main.rs`). Each calls [`init`]
//! from its own constructor, since `cfg(test)` does not reach across crates.

use std::path::PathBuf;

use indoc::indoc;

use crate::{env, file};

/// Set once the harness has written the fixture tree; see [`init`].
const FIXTURES_READY_ENV: &str = "__PAKRO_TEST_FIXTURES_READY";

/// Point pakro at the `tests/support/` fixture tree and write its config files.
pub fn init() {
    // Invocations that only list tests (`--list`, which nextest runs twice per
    // binary, concurrently) must not reset the shared fixture tree: one
    // process's remove_all() unlinks the directory another just chdir'd into,
    // and that process then aborts on its next current_dir() call.
    if std::env::args_os().any(|a| a == "--list") {
        return;
    }
    pakro_util::testing::enable(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support"));
    crate::register_util_hooks();
    // Tests must start from the environment nextest gives their process, not
    // from an activation diff inherited from the process that launched it.
    // This has to happen before the first access to env::HOME initializes
    // PRISTINE_ENV.
    env::remove_var("__PAKRO_DIFF");
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "debug")
    }
    console::set_colors_enabled(false);
    console::set_colors_enabled_stderr(false);
    env::set_var(
        "HOME",
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support"),
    );
    // The embedded aube writes its adaptive concurrency state under
    // `$XDG_CACHE_HOME`, else `~/Library/Caches` on macOS, which would leave
    // an untracked file in the fixture tree. Pin it to the Linux default,
    // which `tests/support/.gitignore` already covers, and away from the developer's
    // own cache when their shell sets XDG_CACHE_HOME.
    env::set_var("XDG_CACHE_HOME", env::HOME.join(".cache"));
    env::remove_var("PAKRO_TRUSTED_CONFIG_PATHS");
    env::remove_var("PAKRO_DISABLE_TOOLS");
    env::set_var("NO_COLOR", "1");
    env::set_var("PAKRO_CACHE_PRUNE_AGE", "0");
    env::set_var("PAKRO_CACHE_DIR", env::HOME.join("data").join("cache"));
    env::set_var("PAKRO_CONFIG_DIR", env::HOME.join("config"));
    env::set_var("PAKRO_ENV", "");
    env::set_var("PAKRO_DATA_DIR", env::HOME.join("data"));
    env::set_var("PAKRO_GLOBAL_CONFIG_FILE", "~/config/config.toml");
    env::set_var("PAKRO_SYSTEM_CONFIG_FILE", "nonexistent");
    env::set_var(
        "PAKRO_OVERRIDE_CONFIG_FILENAMES",
        ".test.pakro.toml:test.config.toml",
    );
    env::set_var(
        "PAKRO_OVERRIDE_TOOL_VERSIONS_FILENAMES",
        ".test-tool-versions",
    );
    env::set_var("PAKRO_STATE_DIR", env::HOME.join("state"));
    env::set_var("PAKRO_USE_TOML", "0");
    env::set_var("PAKRO_YES", "1");
    // A test that re-runs this binary as a child (to get fresh process-wide
    // statics) inherits this variable. The child must reuse the fixture tree
    // rather than reset it: remove_all() would unlink the directory the parent
    // is still in, and every later test that touches the cwd would fail. It
    // also keeps the cwd it inherited, which is the one the parent chose.
    if env::var_os(FIXTURES_READY_ENV).is_some() {
        return;
    }
    file::remove_all(&*env::HOME.join("cwd")).unwrap();
    file::create_dir_all(&*env::HOME.join("cwd").join(".pakro").join("tasks")).unwrap();
    env::set_current_dir(env::HOME.join("cwd")).unwrap();
    file::write(
        env::HOME.join("config").join("config.toml"),
        indoc! {r#"
            [env]
            TEST_ENV_VAR = 'test-123'

            [alias.tiny.versions]
            "my/alias" = '3.0'

            [tasks.configtask]
            run = 'echo "configtask:"'
            [tasks.lint]
            run = 'echo "linting!"'
            [tasks.test]
            run = 'echo "testing!"'
            [settings]
            always_keep_download = true
            always_keep_install = true
            idiomatic_version_file = true
            plugin_autoupdate_last_check_duration = "20m"
            jobs = 2
            "#},
    )
    .unwrap();
    file::write(
        env::HOME.join(".test-tool-versions"),
        indoc! {r#"
            tiny  2
            dummy ref:master
            "#},
    )
    .unwrap();
    file::write(
        env::current_dir().unwrap().join(".test-tool-versions"),
        indoc! {r#"
            tiny 3
            "#},
    )
    .unwrap();
    file::write(
        ".pakro/tasks/filetask",
        indoc! {r#"#!/usr/bin/env bash
        #PAKRO alias="ft"
        #PAKRO description="This is a test build script"
        #PAKRO depends=["lint", "test"]
        #PAKRO sources=[".test-tool-versions"]
        #PAKRO outputs=["$PAKRO_PROJECT_ROOT/test/test-build-output.txt"]
        #PAKRO env={TEST_BUILDSCRIPT_ENV_VAR = "VALID", BOOLEAN_VAR = true}

        #USAGE flag "--user <user>" help="The user to run as"

        set -exo pipefail
        cd "$PAKRO_PROJECT_ROOT" || exit 1
        echo "running test-build script"
        echo "TEST_BUILDSCRIPT_ENV_VAR: $TEST_BUILDSCRIPT_ENV_VAR" > test-build-output.txt
        echo "user=$usage_user"
        "#},
    )
    .unwrap();
    file::make_executable(".pakro/tasks/filetask").unwrap();
    env::set_var(FIXTURES_READY_ENV, "1");
}
