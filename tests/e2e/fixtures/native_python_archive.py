#!/usr/bin/env python3
"""Controlled Linux/aarch64 runtime archive for native CLI transport tests.

This is a process protocol fixture, not a real Python distribution. Its wrapper
records every execution and accepts only the stdlib probe or --version.
"""

import hashlib
import io
import json
import shlex
import sys
import tarfile
from pathlib import Path


def create(archive_path, calls_path):
    calls_path = str(Path(calls_path).resolve())
    host_python = shlex.quote(sys.executable)
    command = (
        '#!/bin/sh\n'
        'runtime_root=$(CDPATH= cd -P -- "$(dirname -- "$0")/.." && pwd) || exit 97\n'
        f'exec {host_python} -E -s -S -B '
        '"$runtime_root/libexec/native_python_archive.py" runtime "$0" '
        f'{shlex.quote(calls_path)} "$@"\n'
    )
    files = {
        "python/bin/python": (command.encode(), 0o755),
        "python/libexec/native_python_archive.py": (Path(__file__).read_bytes(), 0o644),
    }
    with tarfile.open(archive_path, "w:gz") as archive:
        for name, (body, mode) in files.items():
            member = tarfile.TarInfo(name)
            member.size = len(body)
            member.mode = mode
            archive.addfile(member, io.BytesIO(body))
    body = Path(archive_path).read_bytes()
    print(hashlib.sha256(body).hexdigest(), len(body))


def observe(executable):
    executable = Path(executable).resolve()
    root = executable.parent.parent
    language = {"major": 3, "minor": 13, "micro": 5, "release_level": "final", "serial": 0}
    soabi = "cpython-313-aarch64-linux-gnu"
    suffix = f".{soabi}.so"
    return {
        "schema": 1,
        "observation_mode": "base_runtime_site_disabled",
        "probe_flags": ["-E", "-s", "-S", "-B", "-c"],
        "executable": str(executable),
        "implementation": "cpython",
        "language_version": language,
        "implementation_version": language,
        "implementation_version_source": "sys_version_info",
        "markers": {
            "implementation_name": "cpython", "implementation_version": "3.13.5",
            "os_name": "posix", "platform_machine": "aarch64",
            "platform_python_implementation": "CPython", "platform_release": "6.1.0",
            "platform_system": "Linux", "platform_version": "fixture kernel",
            "python_full_version": "3.13.5", "python_version": "3.13", "sys_platform": "linux",
        },
        "soabi": soabi, "ext_suffix": suffix,
        "abiflags": "", "extension_suffixes": [suffix, ".so"], "gil_disabled": False,
        "compatibility": {
            "pointer_bits": 64, "debug": False, "pymalloc": True, "unicode_size": None,
            "platform": {
                "os": "linux",
                "elf": {"class": 2, "little_endian": True, "machine": 183, "flags": 0,
                        "interpreter": "/lib/ld-linux-aarch64.so.1"},
                "libc": {"family": "glibc", "major": 2, "minor": 17},
                "manylinux": {"state": "default"},
            },
        },
        "sysconfig_paths": {"stdlib": str(root / "lib/python3.13")},
    }


def runtime(executable, calls_path, args):
    probe = len(args) == 6 and args[:5] == ["-E", "-s", "-S", "-B", "-c"]
    version = args == ["--version"]
    with Path(calls_path).open("a") as calls:
        calls.write(json.dumps({"kind": "probe" if probe else "version" if version else "unexpected",
                                "args": args[:5]}) + "\n")
    if probe:
        print(json.dumps(observe(executable)))
    elif version:
        print("Python 3.13.5 (native archive fixture)")
    else:
        raise SystemExit("unexpected runtime execution")


if __name__ == "__main__":
    if sys.argv[1] == "create":
        create(sys.argv[2], sys.argv[3])
    elif sys.argv[1] == "runtime":
        runtime(sys.argv[2], sys.argv[3], sys.argv[4:])
    else:
        raise SystemExit("unknown fixture command")
