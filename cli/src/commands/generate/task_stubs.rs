use crate::Result;
use crate::config::Config;
use crate::file;
use crate::file::display_path;
use crate::task::Task;
use eyre::bail;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Generate shims to run mise tasks
///
/// By default, this will build shims like ./bin/<task>. These can be paired with `mise generate install-script`
/// so contributors to a project can execute mise tasks without installing mise into their system.
/// When a parent and nested task both exist, the parent stub is written to `<parent>/_default`.
#[derive(Debug, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    example(
        r###"mise tasks add test -- echo 'running tests'
mise generate task-stubs
./bin/test
running tests"###
    )
)]
pub(super) struct TaskStubs {
    /// Directory to create task stubs inside of
    #[usage(long, short, verbatim_doc_comment, default="bin", value_hint=ValueHint::DirPath)]
    dir: PathBuf,

    /// Path to a mise bin to use when running the task stub.
    ///
    /// Use `--mise-bin=./bin/mise` to use a mise bin generated from `mise generate install-script`
    ///
    /// On Windows a path is run as written, so that script needs its own launcher beside it:
    /// generate it with `mise generate install-script --write ./bin/mise --windows`. The default
    /// `mise` is a bare name and resolves off PATH, which needs nothing extra.
    #[usage(long, short, verbatim_doc_comment, default = "mise")]
    mise_bin: PathBuf,

    /// What to write beside each stub for Windows to launch
    ///
    /// `cmd` writes `<task>.cmd`. cmd.exe re-parses the whole line before `%*` expands, so an
    /// argument containing `& ^ | " %VAR%` does not reach the task intact when the launcher is
    /// called from PowerShell.
    #[usage(long, verbatim_doc_comment, value_enum, default = "cmd")]
    windows_launcher: WindowsLauncher,
}

#[derive(Debug, Default, Clone, Copy, usage_rs::ValueEnum)]
enum WindowsLauncher {
    #[default]
    #[usage()]
    Cmd,
}

impl TaskStubs {
    pub(super) async fn run(self) -> eyre::Result<()> {
        let config = Config::get().await?;
        let launchers = match self.windows_launcher {
            WindowsLauncher::Cmd => Launchers,
        };
        let tasks = config.tasks().await?;
        // Two paths per task, and they differ only for a task that came from a file: `name` keeps
        // the file's extension, `display_name` does not. The stub is named after the task, and the
        // file-named path is kept so a stub written under the old spelling can be migrated away.
        let task_paths = tasks.values().map(Task::name_to_path).collect::<Vec<_>>();
        let base_paths = stub_base_paths(&tasks, &task_paths);
        let paths = resolve_stub_paths(&self.dir, &base_paths)?;
        let stubs = tasks
            .values()
            .zip(task_paths)
            .zip(paths)
            .map(|((task, legacy_path), path)| {
                Ok(TaskStub {
                    task,
                    legacy_path: self.dir.join(legacy_path),
                    path,
                    output: self.generate(task)?,
                    legacy_output: self.generate_legacy(task)?,
                    launcher: self.generate_launcher(task),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let migrations = validate_stub_paths(&self.dir, &stubs, &launchers)?;

        for migration in migrations {
            match migration {
                StubMigration::File(path) => {
                    // The launcher goes with the stub it belongs to, or it keeps running a task
                    // that no longer has a stub here. Only one mise wrote is removed, so a
                    // hand-written .cmd is left alone.
                    remove_generated_launcher(&path, &launchers)?;
                    file::remove_file(path)?
                }
                StubMigration::Directory(path) => file::remove_all(path)?,
            }
        }
        // Only when this run actually writes one. A task set that is empty, or whose names all
        // already end in an executable extension, gets no launcher at all — and a warning about
        // launchers that were not written would describe something that did not happen.
        if stubs.iter().any(|s| launchers.path(&s.path).is_some()) {
            warn_if_windows_cannot_run(&self.mise_bin);
        }

        for stub in &stubs {
            if let Some(parent) = stub.path.parent() {
                file::create_dir_all(parent)?;
            }
            file::write(&stub.path, &stub.output)?;
            file::make_executable(&stub.path)?;
            miseprintln!("Wrote to {}", display_path(&stub.path));
            // Windows will not execute the `#!/bin/sh` stub, so it needs something it can launch.
            // The `.cmd` is written on every host: stubs are committed, and the contributor who
            // runs one on Windows is not the person who generated it.
            //
            // Reported like the stub is: it is a second committed file, and a run that names one
            // path while leaving two behind is how it goes unnoticed into a commit.
            if let Some(launcher_path) = launchers.path(&stub.path) {
                // A legacy native launcher may shadow the new script. Keep unknown executables
                // and report the conflict rather than deleting a file we cannot identify.
                let other = launchers.other_path(&stub.path);
                remove_owned_launcher(&other, &launchers)?;
                launchers.write(&launcher_path, &stub.launcher)?;
                miseprintln!("Wrote to {}", display_path(&launcher_path));
                warn_if_shadowed(&launcher_path, &other);
            }
        }
        Ok(())
    }

    /// The Windows launcher body for `task`, mirroring what the stub itself runs.
    ///
    /// `mise_bin` is embedded as given. With the default `mise` it resolves off PATH, where
    /// PATHEXT finds a `.cmd`, `.bat` or `.exe` for a bare name. A path is run as written, so a
    /// `--mise-bin` pointing at a `mise generate install-script` script needs that script's own
    /// Windows launcher beside it — `--windows`, which is opt-in because it is a second committed
    /// file. [`warn_if_windows_cannot_run`] says so when it is missing.
    fn generate_launcher(&self, task: &Task) -> String {
        let mise_bin = super::cmd_quote(&self.mise_bin.to_string_lossy());
        // The task name goes through the same quoting: it is interpolated into the same cmd line,
        // so a `&` or `%` in it breaks the launcher exactly the way one in the path does.
        let display_name = super::cmd_quote(&task.display_name);
        super::windows_launcher_body(&format!("{mise_bin} run {display_name}"))
    }

    fn generate(&self, task: &Task) -> Result<String> {
        let mise_bin = self.mise_bin.to_string_lossy();
        let mise_bin = shell_words::quote(&mise_bin);
        let display_name = &task.display_name;
        let script = format!(
            r#"
#!/bin/sh
# generated by mise task-stubs
exec {mise_bin} run {display_name} "$@"
"#
        );
        Ok(script.trim().to_string())
    }

    fn generate_legacy(&self, task: &Task) -> Result<String> {
        let mise_bin = self.mise_bin.to_string_lossy();
        let mise_bin = shell_words::quote(&mise_bin);
        let display_name = &task.display_name;
        let script = format!(
            r#"
#!/bin/sh
exec {mise_bin} run {display_name} "$@"
"#
        );
        Ok(script.trim().to_string())
    }
}

/// Say so when the launchers this run writes cannot run the mise they were told to use.
///
/// The launcher runs `--mise-bin` as written, so a path has to name something Windows can execute.
/// `mise generate install-script --write ./bin/mise` writes a `#!/usr/bin/env bash` script, which
/// Windows cannot execute at all; its own launcher is opt-in (`--windows`), deliberately, because
/// it is a second committed file. Pairing the two commands the way the help suggests therefore
/// produces a `bin/<task>.cmd` that fails with `'"./bin/mise"' is not recognized` — and nothing
/// said so at the point where it could still be fixed.
///
/// Warned on every host, like the `.cmd` itself is written on every host: whoever generated `bin/`
/// on Linux is exactly the person who will not see the failure.
fn warn_if_windows_cannot_run(mise_bin: &Path) {
    if windows_can_run(mise_bin) {
        return;
    }
    warn!(
        "{} is a path with no Windows launcher beside it, so the generated launchers cannot run it. \
         Write one with `mise generate install-script --write {} --windows`, or drop --mise-bin to \
         resolve mise off PATH.",
        display_path(mise_bin),
        mise_bin.display()
    );
}

/// Whether Windows can execute `mise_bin` as the generated launcher spells it.
///
/// Answered by Windows' rules, not the generating host's: the value is interpreted by cmd, and the
/// launcher is written on every platform for a contributor on another one.
fn windows_can_run(mise_bin: &Path) -> bool {
    // `\` as well as `/`, because cmd takes both as separators. `Path::components` on unix reads
    // `.\bin\mise` as a single component and would call it a bare name — the exact case where the
    // warning is needed, since cmd will run it as a path.
    if !mise_bin.to_string_lossy().contains(['/', '\\']) {
        return true;
    }
    if windows_runnable_extension(mise_bin) {
        return true;
    }
    let (Some(parent), Some(name)) = (mise_bin.parent(), mise_bin.file_name()) else {
        return true;
    };
    // Matched case-insensitively, as Windows matches: on a case-sensitive generating host a
    // `mise.CMD` beside the script is a launcher Windows would find and a lowercase-only probe
    // would not, which would warn about a gap that is not there.
    let Ok(entries) = fs::read_dir(parent) else {
        return false;
    };
    entries.filter_map(|e| e.ok()).any(|entry| {
        let file_name = entry.file_name();
        let sibling = Path::new(&file_name);
        sibling
            .file_stem()
            .is_some_and(|stem| stem.eq_ignore_ascii_case(name))
            && windows_runnable_extension(sibling)
    })
}

/// Whether the name ends in an extension Windows runs from a path alone.
fn windows_runnable_extension(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        ["cmd", "bat", "exe"]
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

/// Script launchers written beside task stubs, with conservative ownership checks.
struct Launchers;

impl Launchers {
    fn path(&self, stub: &Path) -> Option<PathBuf> {
        super::windows_launcher_path(stub)
    }

    fn other_path(&self, stub: &Path) -> Option<PathBuf> {
        super::windows_exe_launcher_path(stub)
    }

    fn write(&self, launcher: &Path, cmd_body: &str) -> Result<()> {
        file::write(launcher, cmd_body)
    }

    /// Only script markers prove ownership; native executables are left untouched.
    fn owns(&self, launcher: &Path) -> bool {
        if is_exe_path(launcher) {
            return false;
        }
        file::read_to_string(launcher)
            .inspect_err(|err| debug!("keeping {}: {err}", display_path(launcher)))
            .is_ok_and(|contents| super::is_generated_launcher(&contents))
    }
}

fn is_exe_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

/// Report a native executable that would shadow the script just written.
/// Unknown executables are never removed automatically.
fn warn_if_shadowed(written: &Path, other: &Option<PathBuf>) {
    let Some(other) = other else { return };
    if is_exe_path(written) || !is_exe_path(other) || !other.is_file() {
        return;
    }
    warn!(
        "{} is still there and mise cannot show it wrote it, so Windows will run it instead of {}. Remove it by hand if it is no longer needed.",
        display_path(other),
        display_path(written)
    );
}

/// Remove `launcher` if it is there and mise wrote it. A path that holds nothing, or something
/// mise cannot show it wrote, is left exactly as it is.
fn remove_owned_launcher(launcher: &Option<PathBuf>, launchers: &Launchers) -> Result<()> {
    let Some(launcher) = launcher else {
        return Ok(());
    };
    if !fs::symlink_metadata(launcher).is_ok_and(|m| m.file_type().is_file()) {
        return Ok(());
    }
    if launchers.owns(launcher) {
        file::remove_file(launcher)?;
    }
    Ok(())
}

struct TaskStub<'a> {
    task: &'a Task,
    legacy_path: PathBuf,
    path: PathBuf,
    output: String,
    legacy_output: String,
    launcher: String,
}

/// Remove the Windows launchers beside a stub that is being migrated away.
///
/// A `.cmd` is recognised by [`super::is_generated_launcher`] rather than by comparing against the
/// launchers this run would produce. Those bodies embed `--mise-bin` and the task name, so a run
/// that changes either would not recognise the launcher it wrote last time and would leave
/// `<task>.cmd` behind, still runnable and still pointing at the old mise. Anything mise did not
/// write stays put, and a missing file is fine: most stubs never had one.
///
/// Both forms are checked whichever one this run writes: the stub is going away, so any launcher
/// mise left for it is going away too.
fn remove_generated_launcher(stub_path: &Path, launchers: &Launchers) -> Result<()> {
    remove_owned_launcher(&super::windows_launcher_path(stub_path), launchers)?;
    remove_owned_launcher(&super::windows_exe_launcher_path(stub_path), launchers)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum StubMigration {
    File(PathBuf),
    Directory(PathBuf),
}

/// The path each stub should take, named for the task, falling back where that is ambiguous.
///
/// A file task is *named* after its file — `build.sh` — while everything else about it, including
/// the command the stub runs, uses the extensionless display name. Naming the stub after the file
/// produced `bin/build.sh`, and on Windows a `#!/bin/sh` script called `bin/build.bat`, which
/// cmd.exe runs as a batch file line by line.
///
/// Two file tasks that differ only by extension share a display name, and on platforms other than
/// Windows both survive as separate tasks — `prefer_windows_file_task_siblings` collapses the pair
/// only there. Those keep their file-named paths, because renaming both onto one path would take a
/// working project and fail its `task-stubs` run. Windows never reaches that branch, so the case
/// this exists to fix is always unambiguous.
fn stub_base_paths(tasks: &BTreeMap<String, Task>, task_paths: &[PathBuf]) -> Vec<PathBuf> {
    let display_paths = tasks
        .values()
        .map(Task::display_name_to_path)
        .collect::<Vec<_>>();
    let mut counts: HashMap<&PathBuf, usize> = HashMap::new();
    for path in &display_paths {
        *counts.entry(path).or_default() += 1;
    }
    display_paths
        .iter()
        .zip(task_paths)
        .map(|(display_path, task_path)| {
            if counts.get(display_path).copied().unwrap_or_default() > 1 {
                task_path.clone()
            } else {
                display_path.clone()
            }
        })
        .collect()
}

fn resolve_stub_paths(dir: &Path, task_paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let base_paths = task_paths
        .iter()
        .map(|path| dir.join(path))
        .collect::<Vec<_>>();
    let paths = base_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            if base_paths.iter().enumerate().any(|(other_index, other)| {
                index != other_index && other != path && other.starts_with(path)
            }) {
                path.join("_default")
            } else {
                path.clone()
            }
        })
        .collect::<Vec<_>>();

    let mut seen = HashSet::new();
    for path in &paths {
        if !seen.insert(path) {
            bail!(
                "multiple tasks map to task stub path {}",
                display_path(path)
            );
        }
    }
    Ok(paths)
}

fn validate_stub_paths(
    dir: &Path,
    stubs: &[TaskStub<'_>],
    launchers: &Launchers,
) -> Result<Vec<StubMigration>> {
    let mut migrations = HashSet::new();
    for stub in stubs.iter().filter(|stub| stub.legacy_path != stub.path) {
        match fs::symlink_metadata(&stub.legacy_path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                let existing = file::read_to_string(&stub.legacy_path)?;
                if existing != stub.output && existing != stub.legacy_output {
                    bail!(
                        "cannot create nested task stubs because {} is not the generated stub for task {}",
                        display_path(&stub.legacy_path),
                        stub.task.display_name
                    );
                }
                migrations.insert(StubMigration::File(stub.legacy_path.clone()));
            }
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => bail!(
                "cannot create nested task stubs because {} is not a directory",
                display_path(&stub.legacy_path)
            ),
            Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
            Err(err) => return Err(err.into()),
        }
    }

    for stub in stubs {
        match fs::symlink_metadata(&stub.path) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                validate_generated_stub_directory(&stub.path, &stub.output, stub.task, launchers)?;
                migrations.insert(StubMigration::Directory(stub.path.clone()));
            }
            Ok(metadata) if metadata.file_type().is_symlink() => bail!(
                "cannot write task stub because {} is a symbolic link",
                display_path(&stub.path)
            ),
            Ok(metadata) if metadata.file_type().is_file() => {
                let existing = file::read_to_string(&stub.path)?;
                let legacy_leaf = stub.legacy_path == stub.path && existing == stub.legacy_output;
                if existing != stub.output && !legacy_leaf {
                    bail!(
                        "cannot write task stub because {} is not a generated task stub",
                        display_path(&stub.path)
                    );
                }
            }
            Ok(_) => bail!(
                "cannot write task stub because {} is not a regular file",
                display_path(&stub.path)
            ),
            Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
            Err(err) => return Err(err.into()),
        }
        validate_launcher_path(stub, launchers)?;
        for parent in stub.path.ancestors().skip(1) {
            match fs::symlink_metadata(parent) {
                Ok(metadata)
                    if metadata.file_type().is_dir()
                        || migrations.contains(&StubMigration::File(parent.to_path_buf())) => {}
                Ok(_) => bail!(
                    "cannot create task stub directory because {} is not a directory",
                    display_path(parent)
                ),
                Err(err)
                    if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
                Err(err) => return Err(err.into()),
            }
            if parent == dir {
                break;
            }
        }
    }
    Ok(migrations.into_iter().collect())
}

/// Refuse to replace a launcher beside a stub that mise did not write.
///
/// The stub path is the user's choice, so `bin/<task>.cmd` is a name a project may already be
/// using for a script of its own — and unlike the stub itself, nothing about the name says mise
/// owns it. `bin/<task>.exe` is the same problem with less to go on, which is why ownership there
/// rests on the bytes. Checked during validation rather than at the write, so a launcher that is
/// not ours stops the whole run instead of leaving a half-generated `bin/`.
fn validate_launcher_path(stub: &TaskStub<'_>, launchers: &Launchers) -> Result<()> {
    let Some(launcher) = launchers.path(&stub.path) else {
        return Ok(());
    };
    match fs::symlink_metadata(&launcher) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if !launchers.owns(&launcher) {
                bail!(
                    "cannot write Windows launcher because {} is not a generated launcher",
                    display_path(&launcher)
                );
            }
        }
        Ok(_) => bail!(
            "cannot write Windows launcher because {} is not a regular file",
            display_path(&launcher)
        ),
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

fn validate_generated_stub_directory(
    path: &Path,
    expected: &str,
    task: &Task,
    launchers: &Launchers,
) -> Result<()> {
    let default = path.join("_default");
    match fs::symlink_metadata(&default) {
        Ok(metadata)
            if metadata.file_type().is_file() && file::read_to_string(&default)? == expected => {}
        _ => bail!(
            "cannot replace task stub directory because {} does not contain the generated stub for task {}",
            display_path(path),
            task.display_name
        ),
    }

    validate_generated_stub_tree(path, launchers)?;
    Ok(())
}

fn validate_generated_stub_tree(path: &Path, launchers: &Launchers) -> Result<usize> {
    let mut files = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path)?;
        if metadata.file_type().is_dir() {
            let child_files = validate_generated_stub_tree(&entry_path, launchers)?;
            if child_files == 0 {
                bail!(
                    "cannot replace task stub directory because {} is empty",
                    display_path(&entry_path)
                );
            }
            files += child_files;
        } else if metadata.file_type().is_file() && is_exe_path(&entry_path) {
            // A native launcher of ours. Checked before anything reads the file as text, which a
            // binary is not. Not counted towards `files`, same as the `.cmd`: a directory holding
            // nothing but launchers has no stubs left and should still be reported as empty.
            if !launchers.owns(&entry_path) {
                bail!(
                    "cannot replace task stub directory because {} is not a generated task stub",
                    display_path(&entry_path)
                );
            }
        } else if metadata.file_type().is_file()
            && is_generated_task_stub(&file::read_to_string(&entry_path)?)
        {
            files += 1;
        } else if metadata.file_type().is_file()
            && super::is_generated_launcher(&file::read_to_string(&entry_path)?)
        {
            // Our own Windows launcher. Not counted towards `files`: a directory holding nothing
            // but launchers has no stubs left and should still be reported as empty.
        } else {
            bail!(
                "cannot replace task stub directory because {} is not a generated task stub",
                display_path(&entry_path)
            );
        }
    }
    Ok(files)
}

fn is_generated_task_stub(contents: &str) -> bool {
    let mut lines = contents.lines();
    matches!(lines.next(), Some("#!/bin/sh"))
        && matches!(lines.next(), Some("# generated by mise task-stubs"))
        && lines
            .next()
            .and_then(|line| line.strip_prefix("exec "))
            .and_then(|line| line.strip_suffix(" \"$@\""))
            .is_some_and(|line| line.contains(" run "))
        && lines.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_parent_and_nested_task_paths() {
        let paths = resolve_stub_paths(
            Path::new("bin"),
            &[
                PathBuf::from("foo"),
                PathBuf::from("foo/bar"),
                PathBuf::from("foo/bar/baz"),
                PathBuf::from("foobar"),
            ],
        )
        .unwrap();

        assert_eq!(
            paths,
            [
                PathBuf::from("bin/foo/_default"),
                PathBuf::from("bin/foo/bar/_default"),
                PathBuf::from("bin/foo/bar/baz"),
                PathBuf::from("bin/foobar"),
            ]
        );
    }

    #[test]
    fn script_launcher_paths_preserve_the_native_conflict_path() {
        let stub = Path::new("bin/hello");
        assert_eq!(Launchers.path(stub), Some(PathBuf::from("bin/hello.cmd")));
        assert_eq!(
            Launchers.other_path(stub),
            Some(PathBuf::from("bin/hello.exe"))
        );
    }

    #[test]
    fn native_executables_are_not_owned() {
        // Native launchers have been removed; existing executables are left untouched.
        let dir = tempfile::tempdir().unwrap();
        let stray = dir.path().join("hello.exe");
        fs::write(&stray, b"anything").unwrap();
        assert!(!Launchers.owns(&stray));
    }

    #[test]
    fn a_cmd_launcher_is_still_judged_by_its_marker() {
        // `owns` covers both forms, so the `.cmd` rule has to survive the same call.
        let dir = tempfile::tempdir().unwrap();
        let launchers = Launchers;

        let ours = dir.path().join("hello.cmd");
        fs::write(&ours, super::super::windows_launcher_body("mise run hello")).unwrap();
        assert!(launchers.owns(&ours));

        let theirs = dir.path().join("theirs.cmd");
        fs::write(&theirs, "@echo off\r\nmise run hello %*\r\n").unwrap();
        assert!(!launchers.owns(&theirs));
    }

    #[test]
    fn a_launcher_that_cannot_be_read_is_not_ours() {
        // `file::read_to_string` takes UTF-8 only, so a `.cmd` a project keeps in CP932 or
        // Latin-1 fails to read. That has to answer "not mine" rather than fail the run: the
        // file was only ever going to be left in place.
        let dir = tempfile::tempdir().unwrap();
        let launchers = Launchers;

        let theirs = dir.path().join("theirs.cmd");
        // Lone 0x93/0x94 -- CP932 curly quotes, and not valid UTF-8.
        fs::write(&theirs, b"@echo off\r\necho \x93hi\x94\r\n").unwrap();
        assert!(!launchers.owns(&theirs));

        // And cleanup leaves it, rather than erroring out over it.
        remove_owned_launcher(&Some(theirs.clone()), &launchers).unwrap();
        assert!(theirs.exists());
    }

    #[test]
    fn removing_a_launcher_leaves_what_is_not_ours() {
        let dir = tempfile::tempdir().unwrap();
        let launchers = Launchers;

        let ours = dir.path().join("hello.cmd");
        fs::write(&ours, super::super::windows_launcher_body("mise run hello")).unwrap();
        remove_owned_launcher(&Some(ours.clone()), &launchers).unwrap();
        assert!(!ours.exists());

        let theirs = dir.path().join("theirs.cmd");
        fs::write(&theirs, "@echo off\r\necho mine\r\n").unwrap();
        remove_owned_launcher(&Some(theirs.clone()), &launchers).unwrap();
        assert!(theirs.exists());

        // A launcher that was never written is not an error: most stubs never had one.
        let missing = dir.path().join("missing.cmd");
        remove_owned_launcher(&Some(missing), &launchers).unwrap();
        remove_owned_launcher(&None, &launchers).unwrap();
    }

    #[test]
    fn a_bare_mise_bin_needs_nothing_beside_it() {
        // The default. cmd resolves a bare name through PATH, where PATHEXT finds `mise.exe`, so
        // warning about it would fire on nearly every run of this command and mean nothing.
        for bin in ["mise", "mise.exe"] {
            assert!(windows_can_run(Path::new(bin)), "{bin}");
        }
    }

    #[test]
    fn a_windows_spelled_path_is_a_path_on_every_host() {
        // `Path::components` on unix reads this as one component, which would classify it as a
        // bare name. cmd does not: it runs `.\bin\mise` as a path, and that is the case the
        // warning exists for. Judged by Windows' rules because the launcher is written on every
        // platform for someone on another one.
        let dir = tempfile::tempdir().unwrap();
        assert!(!windows_can_run(&dir.path().join(".\\bin\\mise")));
    }

    #[test]
    fn a_path_mise_bin_needs_something_windows_can_run() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        // What `mise generate install-script --write bin/mise` leaves without `--windows`: a
        // shebang script, which cmd cannot execute at all.
        let script = bin.join("mise");
        fs::write(&script, "#!/usr/bin/env bash\n").unwrap();
        assert!(!windows_can_run(&script));

        // What `--windows` adds. Checked by name, not by content: that is the file cmd would find.
        fs::write(bin.join("mise.cmd"), "@echo off\r\n").unwrap();
        assert!(windows_can_run(&script));

        // A path that already names something Windows runs needs no sibling at all.
        for name in ["other.exe", "other.CMD", "other.bat"] {
            assert!(windows_can_run(&bin.join(name)), "{name}");
        }
    }

    #[test]
    fn a_sibling_is_matched_the_way_windows_matches_it() {
        // On a case-sensitive host `mise.CMD` is a different filename from `mise.cmd`; on Windows,
        // where the launcher runs, it is the same one and cmd would find it. Warning here would be
        // about a gap that is not there.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("mise");
        fs::write(&script, "#!/usr/bin/env bash\n").unwrap();
        fs::write(dir.path().join("mise.CMD"), "@echo off\r\n").unwrap();
        assert!(windows_can_run(&script));
    }

    #[test]
    fn a_sibling_that_is_not_there_does_not_count() {
        // The control for the two tests above: the sibling check has to be able to answer "no", or
        // it would be satisfied by any path at all.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        // A sibling that is not runnable is not a launcher either -- `mise.txt` beside `mise`
        // must not count.
        fs::write(bin.join("mise"), "#!/usr/bin/env bash\n").unwrap();
        fs::write(bin.join("mise.txt"), "notes\n").unwrap();
        assert!(!windows_can_run(&bin.join("mise")));
        // And a directory that does not exist cannot hold one.
        assert!(!windows_can_run(&dir.path().join("nested").join("mise")));
    }

    #[test]
    fn rejects_duplicate_resolved_paths() {
        let err = resolve_stub_paths(
            Path::new("bin"),
            &[PathBuf::from("foo"), PathBuf::from("foo/_default")],
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("multiple tasks map to task stub path"));
        assert!(message.contains("_default"));
    }
}
