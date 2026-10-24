//! Bounded process-local capacity for simultaneously protected package files.

/// Reserve file-descriptor capacity without changing a caller's hard limit.
/// Higher inherited soft limits and the parent shell's settings stay intact.
pub fn ensure_file_descriptor_capacity() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use nix::libc;
        let mut current = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: getrlimit writes one valid rlimit on success.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, current.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the successful call above initialized the complete value.
        let current = unsafe { current.assume_init() };
        let target = current.rlim_max.min(4096);
        if current.rlim_cur < target {
            let requested = libc::rlimit {
                rlim_cur: target,
                rlim_max: current.rlim_max,
            };
            // SAFETY: this raises only our own soft limit, within the existing
            // hard ceiling. The CLI calls this before starting its threads.
            if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &requested) } != 0 {
                let error = std::io::Error::last_os_error();
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "raising process file-descriptor soft limit from {} to {target} (hard {}): {error}",
                        current.rlim_cur, current.rlim_max
                    ),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use nix::libc;
    use std::process::Command;

    fn limits() -> libc::rlimit {
        let mut value = std::mem::MaybeUninit::uninit();
        // SAFETY: getrlimit initializes the output on success.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, value.as_mut_ptr()) },
            0
        );
        unsafe { value.assume_init() }
    }

    fn check(test: &str, soft: libc::rlim_t, hard: libc::rlim_t, expected: libc::rlim_t) {
        const CHILD: &str = "PAKRO_FD_CAPACITY_TEST_CHILD";
        let original = limits();
        if std::env::var(CHILD).ok().as_deref() == Some(test) {
            assert!(
                original.rlim_max >= hard,
                "test host hard limit is insufficient"
            );
            let requested = libc::rlimit {
                rlim_cur: soft,
                rlim_max: hard,
            };
            // SAFETY: only this isolated child changes its own valid limits.
            assert_eq!(
                unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &requested) },
                0
            );
            ensure_file_descriptor_capacity().unwrap();
            let actual = limits();
            assert_eq!(
                actual.rlim_cur, expected,
                "process soft capacity was not prepared"
            );
            assert_eq!(
                actual.rlim_max, hard,
                "the caller's hard ceiling must remain unchanged"
            );
            ensure_file_descriptor_capacity().unwrap();
            let repeated = limits();
            assert_eq!(
                (repeated.rlim_cur, repeated.rlim_max),
                (actual.rlim_cur, actual.rlim_max),
                "capacity initialization must be idempotent"
            );
            return;
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, test)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let after = limits();
        assert_eq!(
            (after.rlim_cur, after.rlim_max),
            (original.rlim_cur, original.rlim_max)
        );
    }

    #[test]
    fn s4_fd_capacity_raises_only_the_child_soft_limit() {
        check(
            "process_limits::tests::s4_fd_capacity_raises_only_the_child_soft_limit",
            256,
            8192,
            4096,
        );
    }
    #[test]
    fn s4_fd_capacity_caps_the_request_at_the_hard_limit() {
        check(
            "process_limits::tests::s4_fd_capacity_caps_the_request_at_the_hard_limit",
            64,
            128,
            128,
        );
    }
    #[test]
    fn s4_fd_capacity_preserves_a_higher_soft_limit() {
        check(
            "process_limits::tests::s4_fd_capacity_preserves_a_higher_soft_limit",
            8192,
            8192,
            8192,
        );
    }
    #[test]
    fn s4_fd_capacity_respects_an_equal_restrictive_hard_limit() {
        check(
            "process_limits::tests::s4_fd_capacity_respects_an_equal_restrictive_hard_limit",
            128,
            128,
            128,
        );
    }
}
