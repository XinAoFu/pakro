//! Fixed startup runners must supervise input and output together.

use super::*;

use std::os::unix::fs::PermissionsExt;

fn executable(script: &str) -> (tempfile::TempDir, PythonRuntimeHandle) {
    let owner = tempfile::tempdir().unwrap();
    let path = owner.path().join("runtime");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let handle = PythonRuntimeHandle::from_existing_executable(&path).unwrap();
    (owner, handle)
}

#[tokio::test]
async fn startup_stdin_is_exact_and_not_in_argv_or_inherited_environment() {
    let (_owner, handle) = executable(
        "#!/bin/sh\n[ \"$1 $2 $3 $4 $5\" = '-E -s -S -B -c' ] || exit 71\n[ \"$#\" = 6 ] || exit 72\n[ -z \"${PYTHONPATH+x}\" ] || exit 73\n/bin/cat\n",
    );
    let input = b"{\"private_source\":\"owned snapshot bytes\"}".to_vec();
    let result = probe_startup_intercept(
        &handle,
        input.clone(),
        1024,
        1024,
        Instant::now() + Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(result, input);
}

#[tokio::test]
async fn startup_stdin_above_pipe_capacity_is_pumped_under_the_same_deadline() {
    let (_owner, handle) = executable("#!/bin/sh\n/usr/bin/wc -c\n");
    let size = 512 * 1024;
    let result = probe_startup_intercept(
        &handle,
        vec![b'X'; size],
        1024,
        1024,
        Instant::now() + Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(
        String::from_utf8(result)
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap(),
        size
    );
}

#[tokio::test]
async fn startup_stdin_budget_is_checked_before_launch() {
    let owner = tempfile::tempdir().unwrap();
    let path = owner.path().join("runtime");
    let marker = owner.path().join("ran");
    std::fs::write(
        &path,
        format!("#!/bin/sh\nprintf ran > '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let handle = PythonRuntimeHandle::from_existing_executable(&path).unwrap();
    let error = probe_startup_intercept(
        &handle,
        vec![0; 32 * 1024 * 1024 + 1],
        1024,
        1024,
        Instant::now() + Duration::from_secs(3),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("startup input limit"),
        "{error:#}"
    );
    assert!(!marker.exists());
}

#[tokio::test]
async fn startup_blocked_input_is_killed_and_reaped_by_the_absolute_deadline() {
    let owner = tempfile::tempdir().unwrap();
    let path = owner.path().join("runtime");
    let pid_file = owner.path().join("pid");
    std::fs::write(&path, format!("#!/bin/sh\n[ \"$1\" = --warmup ] && exit 0\nprintf '%s' \"$$\" > '{}'\nexec /bin/sleep 30\n", pid_file.display())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        Command::new(&path)
            .arg("--warmup")
            .status()
            .await
            .unwrap()
            .success()
    );
    let handle = PythonRuntimeHandle::from_existing_executable(&path).unwrap();
    let error = probe_startup_intercept(
        &handle,
        vec![b'X'; 8 * 1024 * 1024],
        1024,
        1024,
        Instant::now() + Duration::from_millis(100),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error:#}");
    let pid: i32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
    assert_eq!(
        nix::sys::wait::waitpid(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ECHILD)
    );
}

#[tokio::test]
async fn normal_empty_runner_uses_its_distinct_fixed_site_enabled_flags() {
    let (_owner, handle) = executable(
        "#!/bin/sh\n[ \"$1 $2 $3 $4\" = '-E -s -B -c' ] || exit 74\n[ \"$#\" = 5 ] || exit 75\n/bin/cat\n",
    );
    let input = b"empty-owned-prefix".to_vec();
    let result = probe_empty_environment(
        &handle,
        input.clone(),
        1024,
        1024,
        Instant::now() + Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(result, input);
}
