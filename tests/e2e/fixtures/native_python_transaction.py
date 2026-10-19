#!/usr/bin/env python3
"""Ordinary-error recovery regression driver; not a real Linux runtime.

Reuse native_python_archive's protocol fixture without changing its positive
tests. Each case owns separate work/data/cache/state/config directories.
"""

import contextlib
import hashlib
import io
import json
import os
import re
import shlex
import stat
import subprocess
import sys
import tarfile
from pathlib import Path

sys.dont_write_bytecode = True
import native_python_archive as base


FILENAME = "cpython-3.13.5+20261013-aarch64-unknown-linux-gnu-install_only.tar.gz"
CANONICAL = "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/" + FILENAME
VERSION = "Python 3.13.5 (native archive fixture)"
FAILURES = {
    "cache": "artifact digest mismatch",
    "unpack": "extracting verified Python runtime archive",
    "probe": "probing extracted Python runtime",
    "patch": "patching validated PBS Python sysconfig",
    "postinstall": "fixture forced postinstall failure",
}


def create(archive_path, calls_path, control_path):
    # The ordinary fixture supplies the wrapper, worker and observation schema.
    with contextlib.redirect_stdout(io.StringIO()):
        base.create(archive_path, calls_path)
    with tarfile.open(archive_path) as archive:
        entries = [(entry, archive.extractfile(entry).read()) for entry in archive]
    wrapper, body = entries[0]
    assert wrapper.name == "python/bin/python"
    body = body.decode().replace(
        '"$runtime_root/libexec/native_python_archive.py" runtime "$0" ',
        '"$runtime_root/libexec/native_python_transaction.py" runtime "$0" '
        + shlex.quote(str(Path(control_path).resolve())) + " ",
    ).encode()
    entries[0] = (wrapper, body)
    for name, payload in [
        ("python/libexec/native_python_transaction.py", Path(__file__).read_bytes()),
        ("python/lib/python3.13/_sysconfigdata_fixture.py",
         b'# system configuration generated and used by the sysconfig module\n'
         b'build_time_vars = {"BINDIR": "/install/bin"}\n'),
    ]:
        member = tarfile.TarInfo(name)
        member.mode = 0o644
        entries.append((member, payload))
    with tarfile.open(archive_path, "w:gz") as archive:
        for entry, payload in entries:
            entry.size = len(payload)
            archive.addfile(entry, io.BytesIO(payload))


def runtime(executable, control_path, calls_path, args):
    probe = len(args) == 6 and args[:5] == ["-E", "-s", "-S", "-B", "-c"]
    mode = Path(control_path).read_text().strip()
    if probe and mode == "probe":
        with Path(calls_path).open("a") as calls:
            calls.write(json.dumps({"kind": "probe", "mode": mode}) + "\n")
        raise SystemExit("fixture forced probe failure")
    if probe and mode == "patch":
        config = Path(executable).resolve().parent.parent / "lib/python3.13/_sysconfigdata_fixture.py"
        config.write_text("build_time_vars = {\n")
    base.runtime(executable, calls_path, args)


def run(command, env, cwd, expected=None):
    result = subprocess.run(command, env=env, cwd=cwd, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=90)
    if expected is None:
        assert result.returncode == 0, (command, result.returncode, result.stdout)
    else:
        assert result.returncode != 0, ("failure was reported as success", command, result.stdout)
        assert expected in result.stdout, ("wrong failure phase", expected, result.stdout)
    return result.stdout.strip()


def tree_snapshot(root):
    assert root.is_dir(), f"old working prefix was removed: {root}"
    found = {}
    for path in [root] + sorted(root.rglob("*")):
        info = path.lstat()
        key = str(path.relative_to(root))
        if stat.S_ISLNK(info.st_mode):
            value = ("link", os.readlink(path))
        elif stat.S_ISREG(info.st_mode):
            value = ("file", stat.S_IMODE(info.st_mode), hashlib.sha256(path.read_bytes()).hexdigest())
        else:
            assert stat.S_ISDIR(info.st_mode), path
            value = ("dir", stat.S_IMODE(info.st_mode))
        found[key] = value
    return found


def aliases(tool_root):
    if not tool_root.exists():
        return {}
    return {entry.name: os.readlink(entry) for entry in tool_root.iterdir() if entry.is_symlink()}


def owned_catalog(installs, prefix):
    receipt = prefix / ".pakro-install.toml"
    if not receipt.exists():
        return {}
    digest = re.search(r'^digest = "([a-z2-7]+)"$', receipt.read_text(), re.MULTILINE).group(1)
    metadata = installs / ".pakro"
    records = list((metadata / "identities").glob(f"*/{digest}.toml"))
    assert len(records) == 1, records
    paths = records + [metadata / "names" / prefix.name]
    paths += [path for path in (metadata / "selections").glob("*.toml")
              if re.search(r'^selected = "' + digest + r'"$', path.read_text(), re.MULTILINE)]
    # Only this installation's records; never capture/restore the global catalog.
    return {str(path): path.read_bytes() for path in paths}


def write_config(work, postinstall=None):
    options = 'version = "3.13.5"'
    if postinstall is not None:
        options += ", postinstall = " + json.dumps(postinstall)
    (work / "pakro.toml").write_text("[tools]\npython = { " + options + " }\n")


def write_lock(work, archive):
    payload = archive.read_bytes()
    checksum = hashlib.sha256(payload).hexdigest()
    (work / "pakro.lock").write_text(
        '[[tools.python]]\nversion = "3.13.5"\nbackend = "core:python"\n'
        f'"platforms.linux-arm64" = {{ checksum = "sha256:{checksum}", size = {len(payload)}, '
        f'url = "{CANONICAL}", provenance = "github-attestations" }}\n'
    )
    return checksum


def case(layout, phase, workspace, server_base, request_log):
    root = workspace / f"transaction-{layout}-{phase}"
    root.mkdir()
    work = root / "work"
    work.mkdir()
    calls = root / "runtime-calls.jsonl"
    control = root / "control"
    control.write_text("normal")
    archive = root / FILENAME
    create(archive, calls, control)
    write_config(work)
    checksum = write_lock(work, archive)
    env = dict(os.environ)
    for variable, child in [("PAKRO_DATA_DIR", "data"), ("PAKRO_CACHE_DIR", "cache"),
                            ("PAKRO_CONFIG_DIR", "config"), ("PAKRO_STATE_DIR", "state"),
                            ("PAKRO_SYSTEM_DIR", "system")]:
        env[variable] = str(root / child)
    if layout == "legacy-relative":
        env["PAKRO_DATA_DIR"] = "../data"
        env["PAKRO_CACHE_DIR"] = "../cache"
    env.update(PAKRO_OS="linux", PAKRO_ARCH="arm64", PAKRO_LOCKFILE="1",
               PAKRO_PYTHON_COMPILE="0", PAKRO_GITHUB_ATTESTATIONS="1",
               PAKRO_PYTHON_GITHUB_ATTESTATIONS="1", PAKRO_GITHUB_TOKEN="invalid",
               GITHUB_TOKEN="invalid", PAKRO_USE_VERSIONS_HOST="0", PAKRO_YES="1",
               PAKRO_EXPERIMENTAL="1" if layout == "identity" else "0",
               PAKRO_INSTALL_LAYOUT="identity" if layout == "identity" else "legacy")
    locator = f"{server_base}/{root.name}/{FILENAME}"
    env["PAKRO_URL_REPLACEMENTS"] = json.dumps({
        CANONICAL: locator,
        "https://api.github.com": server_base + "/unexpected-api",
        "https://downloads.python.org": server_base + "/unexpected-python",
        "https://pypy.org": server_base + "/unexpected-pypy",
    })
    command = ["pakro", "install", "--locked", "python", "-f"]
    if layout == "into":
        prefix = root / "explicit-prefix"
        command = ["pakro", "install-into", "python@3.13.5", str(prefix)]
    run(command, env, work)
    data = Path(env["PAKRO_DATA_DIR"])
    if not data.is_absolute():
        data = (work / data).resolve()
    installs = data / "installs"
    tool_root = installs / "python"
    if layout != "into":
        slot = tool_root / "3.13.5"
        assert slot.is_symlink() == (layout == "identity"), (layout, slot)
        prefix = slot.resolve()
    prefix = prefix.resolve()
    if layout == "identity":
        assert prefix.parent == installs and prefix.name.startswith("python-"), prefix
        assert (prefix / ".pakro-install.toml").is_file(), prefix
        # Keep a real selector to the old receipt as well as its exact pin.
        # The locked force itself must not retarget or delete that selector.
        run(["pakro", "installs", "select", prefix.name], env, work)
    else:
        assert not (prefix / ".pakro-install.toml").exists(), prefix
    (prefix / "keep-old-tree").write_text("old working installation\n")
    assert run([str(prefix / "bin/python"), "--version"], env, work) == VERSION
    before_tree = tree_snapshot(prefix)
    before_aliases = aliases(tool_root)
    before_catalog = owned_catalog(installs, prefix)
    cache = Path(env["PAKRO_CACHE_DIR"])
    if not cache.is_absolute():
        cache = (work / cache).resolve()
    before_markers = {str(path): path.read_bytes() for path in cache.rglob("incomplete")}
    assert not before_markers, before_markers

    failed_env = dict(env)
    if phase == "cache":
        entries = [path for path in cache.rglob(FILENAME)
                   if path.parent.name == "sha256-" + checksum and path.parent.parent.name == "artifacts"]
        assert len(entries) == 1, entries
        entries[0].write_bytes(b"corrupted cached archive")
        failed_env["PAKRO_OFFLINE"] = "1"
    elif phase == "unpack":
        archive.write_bytes(b"not a gzip or tar archive")
        write_lock(work, archive)
    elif phase in ("probe", "patch"):
        control.write_text(phase)
        failed_env["PAKRO_OFFLINE"] = "1"
        if phase == "probe":
            # Keeping failed new files must not suppress restoration of a
            # previously working installation or retain its incomplete marker.
            failed_env["PAKRO_ALWAYS_KEEP_INSTALL"] = "1"
    elif phase == "postinstall":
        write_config(work, "echo fixture forced postinstall failure >&2; exit 73")
        failed_env["PAKRO_OFFLINE"] = "1"
    run(command, failed_env, work, FAILURES[phase])
    print(f"REACHED {layout}/{phase}: expected failure; checking previous installation", flush=True)
    assert run([str(prefix / "bin/python"), "--version"], env, work) == VERSION
    assert tree_snapshot(prefix) == before_tree, "old payload/receipt/modes changed"
    assert aliases(tool_root) == before_aliases, "old exact compatibility/runtime aliases changed"
    for path, payload in before_catalog.items():
        assert Path(path).is_file() and Path(path).read_bytes() == payload, ("old owned record changed", path)
    after_markers = {str(path): path.read_bytes() for path in cache.rglob("incomplete")}
    assert after_markers == before_markers, ("old marker state changed", after_markers)
    assert not list(prefix.parent.glob(".pakro-runtime-rollback-*")), "ordinary rollback left a backup"
    all_requests = [json.loads(path.read_text()) for path in request_log.glob("request_*.json")]
    assert not any(item["path"].startswith("/unexpected-") for item in all_requests), all_requests
    requests = [item for item in all_requests if item["path"].startswith("/" + root.name + "/")]
    assert len(requests) == (2 if phase == "unpack" else 1), requests
    assert all(item["method"] == "GET" for item in requests), requests


def suite(layout, workspace, server_base, request_log):
    failures = []
    for phase in FAILURES:
        try:
            case(layout, phase, workspace, server_base, request_log)
        except Exception as error:
            failures.append((phase, repr(error)))
            print(f"FAIL {layout}/{phase}: {error!r}", flush=True)
        else:
            print(f"PASS {layout}/{phase}", flush=True)
    if failures:
        raise SystemExit(f"{len(failures)}/5 {layout} recovery cases failed")


if __name__ == "__main__":
    if sys.argv[1] == "runtime":
        runtime(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5:])
    elif sys.argv[1] == "suite":
        suite(sys.argv[2], Path(sys.argv[3]).resolve(), sys.argv[4], Path(sys.argv[5]).resolve())
    else:
        raise SystemExit("unknown fixture command")
