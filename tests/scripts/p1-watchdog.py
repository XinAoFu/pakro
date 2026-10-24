#!/usr/bin/env python3
"""Capability-checked execution. A process snapshot is not a tree-ownership proof.

The default tree mode refuses to launch until a strict platform backend exists.
macOS exposes no fork-follow/subreaper API: NOTE_TRACK is unsupported, and a
fast setsid/reparent can escape both process-group and snapshot discovery.
The explicit single-process mode uses a kernel NPROC hard limit of zero. It is
for trusted, unprivileged commands that do not need to spawn subprocesses.
"""
import argparse
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import sys
import threading
import time


def process_table():
    result = subprocess.run(['ps', '-axo', 'pid=,ppid=,pgid=,state=,lstart='],
                            env=dict(os.environ, LC_ALL='C'), text=True,
                            capture_output=True, check=True, timeout=1)
    rows = {}
    for line in result.stdout.splitlines():
        fields = line.split(None, 4)
        if len(fields) == 5:
            rows[int(fields[0])] = (int(fields[1]), int(fields[2]), fields[3], fields[4])
    return rows


def unsupported(reason, mode):
    return dict(exit=125, supported=False, launched=False, mode=mode,
                coverage_complete=False, timed_out=False, survivors=None,
                elapsed_seconds=0.0, owned_processes=0, signalled_processes=0,
                reason=reason)


def supervise(command, output, timeout=600, env=None, cwd=None, *, mode='tree'):
    if not 0.5 <= timeout <= 600:
        raise ValueError('the complete group budget must be between 0.5 and 600 seconds')
    if mode == 'tree':
        return unsupported(
            'Strict descendant ownership is unavailable in this runner on {}. '
            'No command was launched; run long groups in the user terminal or '
            'provide a separately validated strict platform backend.'.format(sys.platform), mode)
    if mode != 'single-process':
        raise ValueError('unknown supervision mode')
    # This backend has been verified only on the current macOS target. In
    # particular Linux NPROC includes threads, and privileged tasks can bypass
    # NPROC: neither is silently treated as an equivalent guarantee.
    if sys.platform != 'darwin' or os.geteuid() == 0 or os.getuid() != os.geteuid():
        return unsupported('Single-process NPROC enforcement is not verified for this platform/identity', mode)
    if threading.active_count() != 1 or threading.current_thread() is not threading.main_thread():
        return unsupported('Single-process setup requires an unthreaded main-thread caller', mode)
    if not command:
        raise ValueError('a command is required')

    started = time.monotonic()
    deadline = started + timeout
    grace = min(2.0, timeout / 5)
    reap = min(1.0, timeout / 5)
    execution_deadline = deadline - grace - reap
    child = None
    timed_out = False
    signalled = False
    previous_handlers = {}

    def interrupted(signum, frame):
        raise KeyboardInterrupt('owned single-process supervisor interrupted')

    def no_children():
        resource.setrlimit(resource.RLIMIT_NPROC, (0, 0))

    def end():
        nonlocal signalled
        if child is None or child.poll() is not None:
            return
        child.terminate();signalled = True
        try:
            child.wait(timeout=max(0.01, min(grace, deadline-time.monotonic()-reap)))
        except subprocess.TimeoutExpired:
            child.kill()
            try:
                child.wait(timeout=max(0.01, deadline-time.monotonic()))
            except subprocess.TimeoutExpired:
                pass

    for signum in (signal.SIGTERM, signal.SIGINT):
        previous_handlers[signum] = signal.signal(signum, interrupted)
    try:
        child = subprocess.Popen(command, stdout=output, stderr=subprocess.STDOUT,
                                 env=env, cwd=cwd, start_new_session=True,
                                 preexec_fn=no_children)
        try:
            child.wait(timeout=max(0.01, execution_deadline-time.monotonic()))
        except subprocess.TimeoutExpired:
            timed_out = True
        end()
        alive = child.poll() is None
        code = 124 if timed_out else (125 if alive else child.returncode)
        return dict(exit=code, supported=True, launched=True, mode=mode,
                    coverage_complete=True, timed_out=timed_out,
                    survivors=[child.pid] if alive else [],
                    elapsed_seconds=time.monotonic()-started,
                    owned_processes=1, signalled_processes=int(signalled),
                    enforcement='RLIMIT_NPROC soft=hard=0 before exec; no descendants admitted')
    except BaseException:
        if child is not None and child.poll() is None:
            child.kill()
            try:child.wait(timeout=1)
            except subprocess.TimeoutExpired:pass
        raise
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--timeout',type=float,default=600)
    parser.add_argument('--log',type=Path,required=True)
    parser.add_argument('--mode',choices=['tree','single-process'],default='tree')
    parser.add_argument('command',nargs=argparse.REMAINDER)
    args=parser.parse_args()
    command=args.command[1:] if args.command[:1]==['--'] else args.command
    if not command:parser.error('a command is required')
    args.log.parent.mkdir(parents=True,exist_ok=True)
    with args.log.open('w') as output:
        output.write('group_limit_seconds={} mode={}\n'.format(args.timeout,args.mode));output.flush()
        result=supervise(command,output,args.timeout,mode=args.mode)
        output.write('\nP1_SUPERVISION '+json.dumps(result)+'\n')
        output.write('P1_GROUP elapsed_seconds={:.3f} exit={}\n'.format(result['elapsed_seconds'],result['exit']))
    print('{}: {}'.format(args.log,json.dumps(result)),flush=True)
    return result['exit'] if result['exit']>=0 else 128-result['exit']


if __name__=='__main__':sys.exit(main())
