#!/usr/bin/env python3
"""Prepare only the three trusted P6 Linux C fixtures, never product builds.

Use an already installed native cc and system Python. The output directory must
be new; failures retain their private evidence and are never retried in place.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import struct
import subprocess
import sys
import time


SOURCES = {
    "runtime_probe.c": "58b9bc5de8db184175ed1e22743bb5ab8634a24b1fbbe80aa37f918b2bb50900",
    "provider.c": "1d40e43fc67c3f3938d300bb72879aa815e266e018ed7a0c3a65864c512f71f5",
    "consumer.c": "9a29155505253786fb9bc096cd66c49330d047ea963b1c8a5ea234eab2443e08",
}
# These are the three compile argv from the original owned build-input.json.
# There is no shell expansion, including the literal $ORIGIN linker argument.
COMMANDS = (
    ("-O2", "-Wall", "-Wextra", "-o", "runtime_probe", "runtime_probe.c"),
    ("-shared", "-fPIC", "-Wl,-soname,libpakro_provider.so", "-o",
     "reference/sharednative/lib/libpakro_provider.so", "provider.c"),
    ("-shared", "-fPIC", "-Wl,-z,defs", "-Wl,-z,origin", "-Wl,--enable-new-dtags",
     "-Wl,-rpath,$ORIGIN/lib", "-o", "reference/sharednative/native.so", "consumer.c",
     "-Lreference/sharednative/lib", "-lpakro_provider"),
)
PRODUCTS = (
    "runtime_probe", "reference/sharednative/lib/libpakro_provider.so",
    "reference/sharednative/native.so",
)


def remaining(deadline):
    value = deadline - time.monotonic()
    if value <= 0:
        raise TimeoutError("the entire trusted fixture preparation exceeded 120 seconds")
    return value


def stamp(value):
    return (value.st_dev, value.st_ino, value.st_uid, value.st_mode, value.st_nlink,
            value.st_size, value.st_mtime_ns, value.st_ctime_ns)


def describe(path, deadline, keep=False):
    remaining(deadline)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise ValueError(f"not a regular fixture/tool file: {path}")
        digest = hashlib.sha256()
        body = bytearray()
        while True:
            remaining(deadline)
            block = os.read(descriptor, 65536)
            if not block:
                break
            digest.update(block)
            if keep:
                if len(body) + len(block) > 1024 * 1024:
                    raise ValueError("trusted fixture source exceeds its 1 MiB input limit")
                body.extend(block)
        if stamp(os.fstat(descriptor)) != stamp(before) or stamp(os.lstat(path)) != stamp(before):
            raise ValueError(f"file changed during the fixture observation: {path}")
        return {
            "path": str(path), "sha256": digest.hexdigest(), "size": before.st_size,
            "owner": before.st_uid, "mode": oct(stat.S_IMODE(before.st_mode)),
            "nlink": before.st_nlink, "device": before.st_dev, "inode": before.st_ino,
        }, bytes(body)
    finally:
        os.close(descriptor)


def elf_facts(path, deadline):
    remaining(deadline)
    if path.stat().st_size > 16 * 1024 * 1024:
        raise ValueError("compiled fixture exceeds its 16 MiB limit")
    data = path.read_bytes()
    if data[:4] != b"\x7fELF" or len(data) < 64 or data[4] not in (1, 2) or data[5] not in (1, 2):
        raise ValueError(f"compiler did not produce an ordinary ELF fixture: {path}")
    endian = "<" if data[5] == 1 else ">"
    wide = data[4] == 2
    header = struct.unpack_from(endian + ("HHIQQQIHHHHHH" if wide else "HHIIIIIHHHHHH"), data, 16)
    offset, size, count = header[4], header[8], header[9]
    program_format = endian + ("IIQQQQQQ" if wide else "IIIIIIII")
    if count > 1024 or size < struct.calcsize(program_format) or offset + size * count > len(data):
        raise ValueError("compiled ELF program header bounds are invalid")
    interpreter = None
    for index in range(count):
        program = struct.unpack_from(program_format, data, offset + index * size)
        if program[0] == 3:  # PT_INTERP: retain the real compiler-selected loader.
            start, length = (program[2], program[5]) if wide else (program[1], program[4])
            raw = data[start:start + length]
            if interpreter is not None or length < 2 or length > 4096 or len(raw) != length or raw[-1:] != b"\0" or b"\0" in raw[:-1]:
                raise ValueError("compiled ELF has an invalid PT_INTERP")
            interpreter = raw[:-1].decode("utf-8")
    remaining(deadline)
    return {"class": 64 if wide else 32, "machine": header[1], "type": header[0],
            "pt_interp": interpreter}


def main():
    started = time.monotonic()
    deadline = started + 120
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output_dir", type=Path, help="a new absolute private output directory; its parent must exist")
    parser.add_argument("--cc", default="cc", help="already installed native C compiler, default: cc")
    parser.add_argument("--expect-projection-method", required=True,
                        choices=("linux-reflink", "verified-copy"),
                        help="explicit assertion for the later test filesystem; this preparation does not probe it")
    args = parser.parse_args()
    if not sys.platform.startswith("linux"):
        parser.error("this trusted fixture preparation is Linux-only")
    if not args.output_dir.is_absolute() or ".." in args.output_dir.parts or not args.output_dir.name:
        parser.error("output_dir must be a new absolute path without '..'")
    compiler_name = shutil.which(args.cc)
    if compiler_name is None:
        parser.error("a native cc is required; prepare it yourself (this script installs nothing)")
    compiler = Path(compiler_name).resolve(strict=True)
    system_python = Path(getattr(sys, "_base_executable", None) or sys.executable).resolve(strict=True)
    source_dir = Path(__file__).resolve().parents[1] / "support/fixtures/p6_linux"
    parent = args.output_dir.parent.resolve(strict=True)
    output = parent / args.output_dir.name
    receipt = {"schema": 1, "purpose": "trusted test fixtures only; no product worker or build acceptance",
               "limit_seconds": 120, "output_dir": str(output), "complete": False,
               "started_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
               "sources": {}, "commands": [], "products": {}}
    receipt["requested_projection_method_assertion"] = args.expect_projection_method
    parent_fd = root_fd = None
    current = None
    old_umask = os.umask(0o077)
    try:
        receipt["compiler"], _ = describe(compiler, deadline)
        if compiler.open("rb").read(4) != b"\x7fELF":
            raise ValueError("cc must resolve to the actual native ELF compiler, not a manager or shell wrapper")
        receipt["system_python"], _ = describe(system_python, deadline)
        parent_fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        os.mkdir(output.name, 0o700, dir_fd=parent_fd)  # EEXIST is always fatal, including symlinks.
        root_fd = os.open(output.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent_fd)
        root_identity = os.fstat(root_fd)

        def check_output():
            remaining(deadline)
            actual = os.stat(output.name, dir_fd=parent_fd, follow_symlinks=False)
            if not stat.S_ISDIR(actual.st_mode) or actual.st_uid != os.geteuid() or stat.S_IMODE(actual.st_mode) != 0o700 or (actual.st_dev, actual.st_ino) != (root_identity.st_dev, root_identity.st_ino):
                raise ValueError("the owned private output directory was replaced or changed")

        for name, expected in SOURCES.items():
            facts, body = describe(source_dir / name, deadline, keep=True)
            if facts["sha256"] != expected:
                raise ValueError(f"trusted source fixture changed: {name}")
            with (output / name).open("xb") as destination:
                destination.write(body)
            facts["snapshot_path"] = str(output / name)
            receipt["sources"][name] = facts
        (output / "reference/sharednative/lib").mkdir(parents=True, mode=0o700)
        (output / "tmp").mkdir(mode=0o700)
        environment = {"PATH": os.defpath, "HOME": str(output), "TMPDIR": str(output / "tmp"),
                       "LANG": "C", "LC_ALL": "C", "SOURCE_DATE_EPOCH": "315532800"}
        receipt["compiler_environment"] = environment
        for index, fixed_arguments in enumerate(COMMANDS):
            check_output()
            target = output / PRODUCTS[index]
            if target.exists() or target.is_symlink():
                raise ValueError("a compiler output already exists; it will not be overwritten")
            argv = [str(compiler), *fixed_arguments]
            command = {"argv": argv, "cwd": str(output), "returncode": None,
                       "stdout": str(output / f"compile-{index}.stdout"),
                       "stderr": str(output / f"compile-{index}.stderr")}
            receipt["commands"].append(command)
            with Path(command["stdout"]).open("xb") as stdout, Path(command["stderr"]).open("xb") as stderr:
                # Only these fixed three trusted commands run, serially. Reserve
                # five seconds of the shared 120-second budget for kill/reap.
                budget = remaining(deadline) - 5
                if budget <= 0:
                    raise TimeoutError("insufficient time remains for the next fixed fixture compile")
                current = subprocess.Popen(argv, cwd=output, env=environment, stdin=subprocess.DEVNULL,
                                           stdout=stdout, stderr=stderr, start_new_session=True)
                command["returncode"] = current.wait(timeout=budget)
                current = None
            if command["returncode"] != 0:
                raise RuntimeError(f"trusted fixture compile {index} failed; inspect its saved stderr")
            check_output()
        for name in SOURCES:
            facts, _ = describe(output / name, deadline)
            if facts["sha256"] != SOURCES[name]:
                raise ValueError("a private source snapshot changed during compilation")
        after_compiler, _ = describe(compiler, deadline)
        if after_compiler != receipt["compiler"]:
            raise ValueError("the actual compiler changed during fixture preparation")
        for relative in PRODUCTS:
            facts, _ = describe(output / relative, deadline)
            if facts["owner"] != os.geteuid() or facts["nlink"] != 1 or int(facts["mode"], 8) & 0o022:
                raise ValueError("compiled fixture is not an independently owned regular file")
            facts["elf"] = elf_facts(output / relative, deadline)
            receipt["products"][relative] = facts
        probe, provider, consumer = (receipt["products"][name] for name in PRODUCTS)
        if not probe["elf"]["pt_interp"] or not probe["elf"]["pt_interp"].startswith("/"):
            raise ValueError("the native runtime fixture must retain its real GNU/musl PT_INTERP")
        if any((value["elf"]["class"], value["elf"]["machine"]) != (probe["elf"]["class"], probe["elf"]["machine"]) for value in (provider, consumer)):
            raise ValueError("the three native fixtures do not have the same real ELF target")
        receipt["test_environment"] = {
            "PAKRO_TEST_LINUX_PROTOCOL_FIXTURE": str(output / "runtime_probe"),
            "PAKRO_TEST_LINUX_PROTOCOL_SHA256": probe["sha256"],
            "PAKRO_TEST_ELF_FIXTURE_DIR": str(output / "reference/sharednative"),
            "PAKRO_TEST_ELF_CONSUMER_SHA256": consumer["sha256"],
            "PAKRO_TEST_ELF_PROVIDER_SHA256": provider["sha256"],
            "PAKRO_TEST_SYSTEM_PYTHON": str(system_python),
            "PAKRO_TEST_EXPECT_PROJECTION_METHOD": args.expect_projection_method,
        }
        receipt["test_name"] = "libraries::package::layouts::projection::method_tests::elf::p6_linux_precompiled_elf_loads_from_shared_projection"
        check_output()
        receipt["complete"] = True
    except BaseException as error:
        if current is not None:
            try:
                os.killpg(current.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                current.wait(timeout=max(0.01, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                pass
        receipt["error"] = f"{type(error).__name__}: {error}"[:4096]
    finally:
        receipt["elapsed_seconds"] = round(time.monotonic() - started, 6)
        if root_fd is not None:
            descriptor = os.open("receipt.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=root_fd)
            with os.fdopen(descriptor, "w", encoding="utf-8") as destination:
                json.dump(receipt, destination, indent=2, sort_keys=True)
                destination.write("\n")
                destination.flush()
                os.fsync(destination.fileno())
            os.fsync(root_fd)
            os.close(root_fd)
        if parent_fd is not None:
            os.close(parent_fd)
        os.umask(old_umask)
    print(json.dumps(receipt, sort_keys=True))
    return 0 if receipt["complete"] else 1


if __name__ == "__main__":
    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGHUP, interrupted)
    raise SystemExit(main())
