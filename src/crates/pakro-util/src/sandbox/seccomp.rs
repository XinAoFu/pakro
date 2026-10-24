use eyre::{Result, eyre};
use nix::libc;
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;

fn syscall_number<T: Into<i64>>(number: T) -> i64 {
    number.into()
}

/// Apply a seccomp-bpf filter that blocks network and/or process syscalls.
///
/// Blocks AF_INET and AF_INET6 sockets while allowing AF_UNIX (needed by many tools).
/// Based on the syscall list from OpenAI's codex-linux-sandbox.
pub(super) fn apply_seccomp_filter(deny_net: bool, deny_process: bool) -> Result<()> {
    apply_filter(deny_net, deny_process, false)
}

pub(super) fn apply_worker_filter(deny_process: bool) -> Result<()> {
    apply_filter(true, deny_process, true)
}

fn apply_filter(deny_net: bool, deny_process: bool, build_worker: bool) -> Result<()> {
    // Must set PR_SET_NO_NEW_PRIVS before installing seccomp filter
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        return Err(eyre!(
            "failed to set PR_SET_NO_NEW_PRIVS: {}",
            std::io::Error::last_os_error()
        ));
    }

    let arch = std::env::consts::ARCH;
    let target_arch = match arch {
        "x86_64" => TargetArch::x86_64,
        "aarch64" => TargetArch::aarch64,
        _ => return Err(eyre!("unsupported architecture for seccomp: {arch}")),
    };

    // Block socket() and socketpair() for AF_INET (2) and AF_INET6 (10)
    // Allow AF_UNIX (1) — needed by many tools for IPC
    let socket_rule_inet = SeccompRule::new(vec![SeccompCondition::new(
        0, // first arg: domain/family
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Eq,
        libc::AF_INET as u64,
    )?])?;

    let socket_rule_inet6 = SeccompRule::new(vec![SeccompCondition::new(
        0,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Eq,
        libc::AF_INET6 as u64,
    )?])?;

    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    if build_worker {
        // Include Unix sockets and operations on inherited descriptors. Also
        // disallow asynchronous socket creation outside the syscall filter.
        for syscall in [
            syscall_number(libc::SYS_socket),
            syscall_number(libc::SYS_socketpair),
            syscall_number(libc::SYS_connect),
            syscall_number(libc::SYS_accept),
            syscall_number(libc::SYS_accept4),
            syscall_number(libc::SYS_bind),
            syscall_number(libc::SYS_listen),
            syscall_number(libc::SYS_sendto),
            syscall_number(libc::SYS_sendmsg),
            syscall_number(libc::SYS_sendmmsg),
            syscall_number(libc::SYS_recvfrom),
            syscall_number(libc::SYS_recvmsg),
            syscall_number(libc::SYS_recvmmsg),
            syscall_number(libc::SYS_shutdown),
            syscall_number(libc::SYS_io_uring_setup),
            syscall_number(libc::SYS_io_uring_enter),
            syscall_number(libc::SYS_io_uring_register),
            syscall_number(libc::SYS_memfd_create),
            // A compiler child cannot detach from the group supervised by
            // the bounded runner or change another process's state.
            syscall_number(libc::SYS_setsid),
            syscall_number(libc::SYS_setpgid),
            syscall_number(libc::SYS_ptrace),
            syscall_number(libc::SYS_process_vm_readv),
            syscall_number(libc::SYS_process_vm_writev),
            syscall_number(libc::SYS_pidfd_getfd),
            syscall_number(libc::SYS_unshare),
            syscall_number(libc::SYS_setns),
            syscall_number(libc::SYS_kill),
            syscall_number(libc::SYS_tkill),
            syscall_number(libc::SYS_tgkill),
        ] {
            rules.insert(syscall, Vec::new());
        }
        if !deny_process {
            // clone3's pointed-to flags cannot be inspected by a classic BPF
            // rule. Ordinary fixed compiler children may fork/clone, but may
            // not create a new namespace that changes process ownership.
            rules.insert(syscall_number(libc::SYS_clone3), Vec::new());
            let mut namespace_rules = Vec::new();
            for flag in [
                libc::CLONE_NEWCGROUP,
                libc::CLONE_NEWIPC,
                libc::CLONE_NEWNET,
                libc::CLONE_NEWNS,
                libc::CLONE_NEWPID,
                libc::CLONE_NEWUSER,
                libc::CLONE_NEWUTS,
            ] {
                namespace_rules.push(SeccompRule::new(vec![SeccompCondition::new(
                    0,
                    SeccompCmpArgLen::Qword,
                    SeccompCmpOp::MaskedEq(flag as u64),
                    flag as u64,
                )?])?);
            }
            rules.insert(syscall_number(libc::SYS_clone), namespace_rules);
        }
    } else if deny_net {
        // Block socket() and socketpair() for inet families. Without an inet
        // socket the process cannot initiate network traffic.
        for syscall in [
            syscall_number(libc::SYS_socket),
            syscall_number(libc::SYS_socketpair),
        ] {
            rules.insert(
                syscall,
                vec![socket_rule_inet.clone(), socket_rule_inet6.clone()],
            );
        }
    }

    if deny_process {
        // An empty rule chain means the syscall number alone triggers the
        // filter's match action.
        let deny = Vec::<SeccompRule>::new();
        // Linux generic syscall ABI has no fork/vfork; aarch64 implements fork via clone.
        for syscall in [
            syscall_number(libc::SYS_clone),
            syscall_number(libc::SYS_clone3),
            #[cfg(not(any(
                target_arch = "aarch64",
                target_arch = "riscv64",
                target_arch = "loongarch64"
            )))]
            syscall_number(libc::SYS_fork),
            #[cfg(not(any(
                target_arch = "aarch64",
                target_arch = "riscv64",
                target_arch = "loongarch64"
            )))]
            syscall_number(libc::SYS_vfork),
            syscall_number(libc::SYS_kill),
            syscall_number(libc::SYS_tkill),
            syscall_number(libc::SYS_tgkill),
            syscall_number(libc::SYS_ptrace),
        ] {
            rules.insert(syscall, deny.clone());
        }
    }

    let filter: BpfProgram = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                     // default: allow everything
        SeccompAction::Errno(libc::EPERM as u32), // blocked syscalls return EPERM
        target_arch,
    )?
    .try_into()?;

    seccompiler::apply_filter(&filter).map_err(|e| eyre!("failed to apply seccomp filter: {e}"))?;

    Ok(())
}
