use crate::config::{Config, Settings, SettingsExt};
use clx::progress;
use eyre::Result;
use std::collections::HashSet;
use std::fs::{File, OpenOptions, create_dir_all};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::{io::Write, sync::OnceLock};

use crate::{env, ui};
use log::{Level, LevelFilter, Metadata, Record};

#[derive(Debug)]
struct Logger {
    level: Mutex<LevelFilter>,
    term_level: Mutex<LevelFilter>,
    file_level: LevelFilter,
    log_file: Option<Mutex<File>>,
}

/// Root crate names of third-party dependencies that emit very noisy debug
/// and trace logs (often per HTTP/2 frame, per socket read, etc.) and would
/// otherwise overwhelm `-v`/`-vv` output. Debug and Trace records from these
/// crates are dropped entirely unless `PAKRO_LOG_VERBOSE_DEPS=1` is set.
/// Info/Warn/Error still pass through — those are rare and worth seeing.
static NOISY_DEP_TARGETS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "h2",
        "hyper",
        "hyper_util",
        "mio",
        "reqwest",
        "rustls",
        "tokio_util",
        "tower",
        "want",
    ]
    .into_iter()
    .collect()
});

fn is_noisy_dep_target(target: &str) -> bool {
    // `log` targets default to the module path (e.g. "h2::proto::streams").
    // Match on the crate-root segment so we don't accidentally match an
    // unrelated crate whose name happens to start with one of ours
    // (e.g. "h2extra") — zero allocation: just splits the input slice.
    let root = target.split_once("::").map_or(target, |(r, _)| r);
    NOISY_DEP_TARGETS.contains(root)
}

// Kept separate from optional noisy dependency logging: parser tokens can
// contain unregistered signed URLs and private index metadata.
fn is_sensitive_dependency_record(level: Level, target: &str) -> bool {
    matches!(level, Level::Debug | Level::Trace)
        && target.split_once("::").map_or(target, |(root, _)| root) == "html5ever"
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        !is_sensitive_dependency_record(metadata.level(), metadata.target())
            && metadata.level() <= *self.level.lock().unwrap()
    }

    fn log(&self, record: &Record) {
        if is_sensitive_dependency_record(record.level(), record.target()) {
            return;
        }
        // Drop Debug/Trace spam from noisy third-party crates (e.g. h2 logging
        // every received DATA frame) regardless of terminal/file level, and
        // the solver's per-step info narration, which is never terminal
        // material. Opt back in with PAKRO_LOG_VERBOSE_DEPS=1.
        if !*env::PAKRO_LOG_VERBOSE_DEPS {
            let noisy_dep = matches!(record.level(), Level::Debug | Level::Trace)
                && is_noisy_dep_target(record.target());
            let solver_chatter =
                record.level() == Level::Info && record.target().starts_with("pubgrub");
            if noisy_dep || solver_chatter {
                return;
            }
        }

        let term_level = *self.term_level.lock().unwrap();
        let will_log_file = record.level() <= self.file_level && self.log_file.is_some();
        let will_log_term = record.level() <= term_level;

        if !will_log_file && !will_log_term {
            return;
        }

        // Redact once for all outputs (Aho-Corasick makes this efficient)
        let args = record.args().to_string();
        // maybe_get instead of is_loaded + get_: another thread may unload the
        // config (Config::reset) between the two calls, and get_ panics on None
        let args = match Config::maybe_get() {
            Some(config) => config.redact(&args),
            None => args,
        };

        if will_log_file && let Some(log_file) = &self.log_file {
            let mut log_file = log_file.lock().unwrap();
            let out = self.render(record, self.file_level, &args);
            if !out.is_empty() {
                let _ = writeln!(log_file, "{}", console::strip_ansi_codes(&out));
            }
        }
        if will_log_term {
            let out = self.render(record, term_level, &args);
            if !out.is_empty() {
                // Use clx pause/resume for clean logging during progress display
                progress::pause();
                safe_eprintln!("{out}");
                progress::resume();
            }
        }
    }

    fn flush(&self) {}
}

impl Logger {
    fn init(term_level: LevelFilter, file_level: LevelFilter) -> Self {
        let mut logger = Logger {
            level: Mutex::new(std::cmp::max(term_level, file_level)),
            file_level,
            term_level: Mutex::new(term_level),
            log_file: None,
        };

        if !Settings::is_package_query()
            && let Some(log_file) = &*env::PAKRO_LOG_FILE
        {
            if let Ok(log_file) = init_log_file(log_file) {
                logger.log_file = Some(Mutex::new(log_file));
            } else {
                safe_eprintln!("pakro: could not open log file: {log_file:?}");
            }
        }

        logger
    }

    fn render(&self, record: &Record, level: LevelFilter, args: &str) -> String {
        match level {
            LevelFilter::Off => "".to_string(),
            LevelFilter::Trace => {
                let level = record.level();
                let file = record.file().unwrap_or("<unknown>");
                if level == LevelFilter::Trace && file.contains("/expr-lang") {
                    return "".to_string();
                };
                let meta = ui::style::edim(format!(
                    "{thread_id:>2} [{file}:{line}]",
                    thread_id = thread_id(),
                    line = record.line().unwrap_or(0),
                ));
                format!("{level} {meta} {args}", level = self.styled_level(level),)
            }
            LevelFilter::Debug => {
                format!("{level} {args}", level = self.styled_level(record.level()),)
            }
            _ => {
                let pakro = match record.level() {
                    Level::Error => ui::style::ered("pakro"),
                    Level::Warn => ui::style::eyellow("pakro"),
                    _ => ui::style::edim("pakro"),
                };
                match record.level() {
                    Level::Info => format!("{pakro} {args}"),
                    _ => format!(
                        "{pakro} {level} {args}",
                        level = self.styled_level(record.level()),
                    ),
                }
            }
        }
    }

    fn styled_level(&self, level: Level) -> String {
        let level = match level {
            Level::Error => ui::style::ered("ERROR").to_string(),
            Level::Warn => ui::style::eyellow("WARN").to_string(),
            Level::Info => ui::style::ecyan("INFO").to_string(),
            Level::Debug => ui::style::emagenta("DEBUG").to_string(),
            Level::Trace => ui::style::edim("TRACE").to_string(),
        };
        console::pad_str(&level, 5, console::Alignment::Left, None).to_string()
    }
}

pub(crate) fn thread_id() -> String {
    let id = format!("{:?}", thread::current().id());
    let id = id.replace("ThreadId(", "");
    id.replace(")", "")
}

pub fn init() {
    static LOGGER: OnceLock<Logger> = OnceLock::new();
    let settings = Settings::try_get();
    if let Some(logger) = LOGGER.get() {
        // Re-init. A settings build that failed says nothing about the level, so the default is the
        // wrong answer here: `quiet` resolves to `log_level = "error"` inside `try_get`, and
        // resetting to `info` would *raise* the bar and let the warnings flushed below through.
        //
        // The parsed CLI flags are the exception. Once they are part of the build it can keep
        // failing — `--cd` naming a directory that `validate_cd_path` accepts but the `chdir`
        // refuses is the case `Cli::run` propagates — and then no later build succeeds either, so
        // waiting for one would mean `--quiet` never applying to this run at all. `cli_log_level`
        // reads the flags without a build, and answers `None` when they say nothing about
        // verbosity — then the level in force came from a build that could see the config files,
        // and is still the better answer.
        let term_level = match &settings {
            Ok(settings) => Some(settings.log_level()),
            Err(_) => Settings::cli_log_level(),
        };
        if let Some(term_level) = term_level {
            *logger.term_level.lock().unwrap() = term_level;
            *logger.level.lock().unwrap() = std::cmp::max(term_level, logger.file_level);
            log::set_max_level(term_level);
        }
    } else {
        // First init: nothing is in force yet, so the default is all there is. A logger at `info`
        // beats no logger at all — a warning printed too loudly still beats one that vanishes.
        let term_level = match settings {
            Ok(settings) => settings.log_level(),
            Err(_) => Settings::cli_log_level().unwrap_or(LevelFilter::Info),
        };
        let file_level = env::PAKRO_LOG_FILE_LEVEL.unwrap_or(term_level);
        let logger = LOGGER.get_or_init(|| Logger::init(term_level, file_level));
        if let Err(err) = log::set_logger(logger) {
            safe_eprintln!("pakro: could not initialize logger: {err}");
        }
        log::set_max_level(term_level);
    }
    Settings::flush_pending_warnings();
}

fn init_log_file(log_file: &Path) -> Result<File> {
    if let Some(log_dir) = log_file.parent() {
        create_dir_all(log_dir)?;
    }
    Ok(OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log as _;

    #[test]
    fn parser_payload_levels_are_always_sensitive_but_unrelated_targets_are_not() {
        for target in [
            "html5ever",
            "html5ever::tree_builder",
            "html5ever::tokenizer",
        ] {
            for level in [Level::Debug, Level::Trace] {
                assert!(
                    is_sensitive_dependency_record(level, target),
                    "{level} {target}"
                );
            }
            for level in [Level::Error, Level::Warn, Level::Info] {
                assert!(!is_sensitive_dependency_record(level, target));
            }
        }
        for target in [
            "html5ever_extra",
            "pakro::html5ever",
            "h2::proto",
            "pakro_index",
        ] {
            assert!(!is_sensitive_dependency_record(Level::Trace, target));
        }
    }

    #[test]
    fn sensitive_dependency_records_are_disabled_even_at_trace_level() {
        let logger = Logger {
            level: Mutex::new(LevelFilter::Trace),
            term_level: Mutex::new(LevelFilter::Off),
            file_level: LevelFilter::Trace,
            log_file: None,
        };
        assert!(
            !logger.enabled(
                &Metadata::builder()
                    .level(Level::Debug)
                    .target("html5ever::tree_builder")
                    .build()
            )
        );
        assert!(
            logger.enabled(
                &Metadata::builder()
                    .level(Level::Warn)
                    .target("html5ever::tree_builder")
                    .build()
            )
        );
        assert!(
            logger.enabled(
                &Metadata::builder()
                    .level(Level::Debug)
                    .target("html5ever_extra")
                    .build()
            )
        );
    }

    #[test]
    fn direct_log_calls_cannot_write_raw_parser_payloads_to_a_trace_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let logger = Logger {
            level: Mutex::new(LevelFilter::Trace),
            term_level: Mutex::new(LevelFilter::Off),
            file_level: LevelFilter::Trace,
            log_file: Some(Mutex::new(file.as_file().try_clone().unwrap())),
        };
        logger.log(&Record::builder()
            .level(Level::Debug)
            .target("html5ever::tree_builder")
            .args(format_args!("href=https://private.example/artifact?token=unregistered-test-token yanked=private-reason"))
            .build());
        assert!(
            std::fs::read(file.path()).unwrap().is_empty(),
            "raw parser token reached the log file"
        );
        logger.log(
            &Record::builder()
                .level(Level::Info)
                .target("pakro_index")
                .args(format_args!("retained application message"))
                .build(),
        );
        assert!(
            std::fs::read_to_string(file.path())
                .unwrap()
                .contains("retained application message")
        );
    }
    #[tokio::test]
    async fn test_init() {
        let _config = Config::get().await.unwrap();
        init();
    }
}
