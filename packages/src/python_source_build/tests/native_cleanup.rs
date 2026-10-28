use super::*;
use pakro_util::{native_build::{NativeBuildConfig, NativeBuildSession}, sandbox::SandboxConfig};
use std::{fs::File, os::unix::fs::{MetadataExt, PermissionsExt}, process::Command};
use tokio_util::sync::CancellationToken;

fn worker(root: &Path) -> PathBuf {
    let input = root.join("cleanup-worker.c");
    let output = root.join("cleanup-worker");
    fs::write(&input, br#"
#define _DEFAULT_SOURCE
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
int main(int argc,char **argv) {
  if(argc!=3 || strcmp(argv[1],"hang")) return 0;
  int fd=open(argv[2],O_WRONLY|O_APPEND); if(fd<0)return 20;
  pid_t child=fork(); if(child<0)return 21;
  if(!child) { if(setpgid(0,0))_exit(22); for(;;){if(write(fd,"x",1)!=1)_exit(23);usleep(1000);} }
  close(fd); for(;;)pause();
}
"#).unwrap();
    let compiler = std::env::var_os("PAKRO_TEST_NATIVE_CC").unwrap_or_else(|| "/usr/bin/cc".into());
    let result = Command::new(compiler).args(["-std=c11", "-O0"]).arg(input).arg("-o").arg(&output).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    output
}

fn config(workspace: &Path, writable: &Path, program: &Path) -> NativeBuildConfig {
    let mut reads = vec![workspace.to_owned(), program.to_owned()];
    for path in ["/lib", "/lib64", "/usr/lib", "/System/Library", "/dev/null", "/dev/urandom"] {
        if Path::new(path).exists() { reads.push(path.into()); }
    }
    NativeBuildConfig {
        bootstrap: std::env::var_os("PAKRO_TEST_BINARY").expect("newly built main required").into(),
        fixed_programs: vec![program.to_owned()],
        sandbox: SandboxConfig { allow_read: reads, allow_write: vec![writable.to_owned()], ..Default::default() },
        workspace: workspace.to_owned(), exec_pool: workspace.join("native-pool"),
        generated_roots: vec![writable.to_owned()], requests: workspace.join("native-requests"),
        responses: workspace.join("native-responses"), unshare: Some("/usr/bin/unshare".into()),
    }
}

/// Holding the real watcher stopped makes the pre-cleanup destructor boundary
/// deterministic: the worker is demonstrably still writing when owners drop.
struct StoppedWatcher(i32);
impl StoppedWatcher {
    fn stop(config: &NativeBuildConfig) -> Self {
        let output = Command::new("/bin/ps").args(["-ax", "-o", "pid=", "-o", "command="]).output().unwrap();
        assert!(output.status.success());
        let suffix = format!("__pakro_native_build_v1 watcher {}", config.responses.join("supervisor.json").display());
        let pids = String::from_utf8(output.stdout).unwrap().lines()
            .filter(|line| line.trim_end().ends_with(&suffix))
            .map(|line| line.split_whitespace().next().unwrap().parse::<i32>().unwrap()).collect::<Vec<_>>();
        assert_eq!(pids.len(), 1, "exactly this private watcher must be found");
        assert_eq!(unsafe { nix::libc::kill(pids[0], nix::libc::SIGSTOP) }, 0);
        let mut status = 0;
        assert_eq!(unsafe { nix::libc::waitpid(pids[0], &mut status, nix::libc::WUNTRACED) }, pids[0]);
        assert!(nix::libc::WIFSTOPPED(status));
        Self(pids[0])
    }
}
impl Drop for StoppedWatcher {
    fn drop(&mut self) { unsafe { nix::libc::kill(self.0, nix::libc::SIGCONT); } }
}

async fn removed_and_stopped(workspace: &Path, ticks: &File) {
    let until = Instant::now() + Duration::from_secs(5);
    while workspace.exists() && Instant::now() < until { tokio::time::sleep(Duration::from_millis(10)).await; }
    assert!(!workspace.exists(), "watcher did not remove its private workspace");
    let bytes = ticks.metadata().unwrap().len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(ticks.metadata().unwrap().len(), bytes, "descendant survived workspace removal");
}

#[tokio::test(flavor = "current_thread")]
async fn p6_native_source_production_destructors_wait_for_watcher() {
    use crate::libraries::language::repository::LanguageRepository;
    use crate::libraries::language::repository::tests::{archive, install_spec, operation_context, prepare};
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().canonicalize().unwrap();
    let program = worker(&root);
    let runtime = archive("3.15.0");
    let spec = install_spec(&runtime);
    let fixture_context = operation_context(&root, true, Duration::from_secs(30));
    drop(prepare(&spec, &fixture_context, &runtime).await.unwrap());
    let bytes = bundle(&[("demo-1.0/pyproject.toml", b"[build-system]\nrequires=[]\nbuild-backend='fixture'\n")]);
    let scratch = root.join("builds"); fs::create_dir(&scratch).unwrap();
    for mode in ["workspace-error", "future-drop", "finish", "startup-error"] {
        // Each case is a new build operation. Its held runtime must carry the
        // same fresh operation deadline, rather than the fixture's old lease.
        let context = operation_context(&root, true, Duration::from_secs(30));
        let language = LanguageRepository::open_readonly(&context).unwrap().acquire_existing(&spec).unwrap().unwrap();
        eprintln!("P6_NATIVE_DESTRUCTOR_CASE {mode}");
        let mut source_owner = SourceBuildSession::prepare(source(&bytes, &root), "demo", "1.0", language,
            &scratch, context.policy().deadline, Default::default()).unwrap();
        let workspace = source_owner.workspace_path().to_owned();
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
        let work = source_owner.extracted.root.clone();
        let mut environment = source_owner.environment(Vec::new()).unwrap();
        environment.prefix.delegate_cleanup_to_workspace(&workspace).unwrap();
        let prefix_witness = environment.prefix.final_path().join("bin/.pakro python");
        source_owner.initial_environment = Some(environment);
        let mut configuration = config(&workspace, &work, &program);
        if mode == "startup-error" { configuration.bootstrap = program.clone(); }
        let started = NativeBuildSession::start_owned(configuration.clone(), source_owner.extracted.native_cleanup_owner()).await;
        if mode == "startup-error" {
            assert!(started.is_err(), "fixture bootstrap cannot emit watcher readiness");
            drop(source_owner);
            assert!(prefix_witness.is_file(), "failed startup lost unconfirmed workspace ownership");
            fs::remove_dir_all(&workspace).unwrap();
            continue;
        }
        let mut session = started.unwrap();
        if mode == "finish" {
            let result = session.run(&program, &[], &work, &BTreeMap::new(), Instant::now()+Duration::from_secs(5),
                4096, &CancellationToken::new()).await.unwrap();
            assert!(result.status.success());
            session.close().await.unwrap();
            assert!(workspace.exists());
            drop(source_owner);
            assert!(!workspace.exists(), "confirmed Finish did not return destructor cleanup");
            continue;
        }
        let ticks_path = work.join("ticks"); fs::write(&ticks_path, []).unwrap();
        let ticks = File::open(&ticks_path).unwrap();
        let args = ["hang".into(), ticks_path.to_string_lossy().into_owned()];
        let environment = BTreeMap::new(); let cancellation = CancellationToken::new();
        let mut execute = Box::pin(session.run(&program, &args, &work, &environment,
            Instant::now()+Duration::from_secs(10), 4096, &cancellation));
        let until = Instant::now()+Duration::from_secs(5);
        while ticks.metadata().unwrap().len() == 0 {
            assert!(Instant::now() < until);
            tokio::select! { result = &mut execute => panic!("child exited before destructor case: {result:?}"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {} }
        }
        let stopped = StoppedWatcher::stop(&configuration);
        let readonly = work.join("readonly"); fs::create_dir(&readonly).unwrap();
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o555)).unwrap();
        if mode == "workspace-error" {
            let error = worker::workspace_usage(&source_owner.extracted,
                SourceBuildLimits { max_workspace_bytes: 1, ..Default::default() }, context.policy().deadline).err().expect("real workspace monitor must reject its byte budget");
            assert!(format!("{error:#}").contains("workspace byte budget"));
        }
        drop(execute); // Actual NativeBuildSession::run future Drop closes owner input.
        drop(session);
        drop(source_owner); // Actual SourceBuildSession -> BuildEnvironment + RawSourceTree destructors.
        assert!(prefix_witness.is_file(), "nested prefix destructor raced the watcher");
        assert_eq!(fs::metadata(&readonly).unwrap().mode() & 0o777, 0o555, "RawSourceTree chmod raced the watcher");
        let before = ticks.metadata().unwrap().len();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(ticks.metadata().unwrap().len() > before, "fixture must still run while cleanup is unconfirmed");
        drop(stopped);
        removed_and_stopped(&workspace, &ticks).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn p6_native_language_owner_destructor_waits_for_watcher() {
    use crate::libraries::language::repository::{LanguageRepository, LanguageReservation};
    use crate::libraries::language::repository::tests::{archive, install_spec, operation_context};
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap(); let program = worker(&root);
    let context = operation_context(&root, true, Duration::from_secs(20));
    let mut owner = match LanguageRepository::open_writer(&context).unwrap().reserve_install(&install_spec(&archive("3.15.0"))).unwrap() {
        LanguageReservation::Build(owner) => owner, LanguageReservation::Existing(_) => panic!("private allocation must be new"),
    };
    let mut file = owner.create_file_exclusive(Path::new("witness"), 0o600).unwrap(); file.write_all(b"owned").unwrap(); file.finish().unwrap();
    let witness = owner.final_path().join("witness");
    let allocation = owner.final_path().parent().unwrap().to_owned();
    let work = allocation.join("validation-work"); fs::create_dir(&work).unwrap();
    let configuration = config(&allocation, &work, &program);
    let mut session = NativeBuildSession::start_owned(configuration.clone(), owner.native_cleanup_owner()).await.unwrap();
    let ticks_path = work.join("ticks"); fs::write(&ticks_path, []).unwrap(); let ticks = File::open(&ticks_path).unwrap();
    let args = ["hang".into(), ticks_path.to_string_lossy().into_owned()];
    let environment = BTreeMap::new(); let cancellation = CancellationToken::new();
    let mut execute = Box::pin(session.run(&program, &args, &work, &environment,
        Instant::now()+Duration::from_secs(10), 4096, &cancellation));
    let until = Instant::now()+Duration::from_secs(5);
    while ticks.metadata().unwrap().len() == 0 {
        assert!(Instant::now()<until);
        tokio::select! { result = &mut execute => panic!("child exited before owner Drop: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {} }
    }
    let stopped = StoppedWatcher::stop(&configuration);
    drop(execute); drop(session); drop(owner);
    assert_eq!(fs::read(witness).unwrap(), b"owned", "language owner destructor removed a live validation input");
    drop(stopped);
    removed_and_stopped(&allocation, &ticks).await;
}
