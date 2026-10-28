"""Real PBS transport and deterministic wheels for the P3 lifecycle contract."""
import base64
import hashlib
import io
import json
import os
import runpy
import stat
import subprocess
import sys
import zipfile
from pathlib import Path
from urllib.parse import unquote, urlsplit

HERE = Path(__file__).resolve().parent
SECOND_VERSION = "3.14.9"
SECOND_BUILD = "20261021"
SECOND_FILENAME = f"cpython-{SECOND_VERSION}+{SECOND_BUILD}-aarch64-apple-darwin-install_only_stripped.tar.gz"
SECOND_SHA256 = "9f8c3480b8f0db553c270db548e8a21ae8e39df412e5661d66e8050c5c6ed2a8"
SECOND_SIZE = 26705294
SECOND_DOWNLOAD = f"/astral-sh/python-build-standalone/releases/download/{SECOND_BUILD}/"
SECOND_URL = "https://github.com" + SECOND_DOWNLOAD + SECOND_FILENAME.replace("+", "%2B")
PACKAGES = {
    "p3-dep": {"1.0": (), "1.1": (), "2.0": ()},
    "p3-root": {"1.0": ("p3-dep==1.0",), "1.1": ("p3-dep>=1,<2",), "2.0": ("p3-dep>=2,<3",)},
    "p3-added": {"1.0": ()},
    "p3-extra": {"1.0": ("p3-feature==1.0; extra == 'feature'",)},
    "p3-feature": {"1.0": ()},
    "p3-active": {"1.0": ()},
    "p3-broken": {"1.0": ()},
}


def wheel(name, version, dependencies):
    module = name.replace("-", "_")
    info = f"{module}-{version}.dist-info"
    body = f"VALUE = '{name}:{version}'\n"
    if name == "p3-root":
        body = f"import p3_dep\nVALUE = '{version}:' + p3_dep.VALUE\n"
    metadata = f"Metadata-Version: 2.4\nName: {name}\nVersion: {version}\nRequires-Python: >=3.13\n"
    if name == "p3-extra":
        metadata += "Provides-Extra: feature\n"
    metadata += "".join(f"Requires-Dist: {dependency}\n" for dependency in dependencies)
    files = {
        f"{module}.py": body.encode(),
        f"{info}/METADATA": metadata.encode(),
        f"{info}/WHEEL": b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    }
    records = [f"{path},sha256={base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b'=').decode()},{len(data)}"
               for path, data in files.items()]
    files[f"{info}/RECORD"] = ("\n".join(records + [f"{info}/RECORD,,"]) + "\n").encode()
    result = io.BytesIO()
    with zipfile.ZipFile(result, "w") as archive:
        for path, data in files.items():
            item = zipfile.ZipInfo(path)
            item.external_attr = 0o100644 << 16
            archive.writestr(item, data)
    return result.getvalue()


def filename(name, version):
    return f"{name.replace('-', '_')}-{version}-py3-none-any.whl"


def prepare(root):
    root.mkdir(parents=True, exist_ok=True)
    for name, versions in PACKAGES.items():
        for version, dependencies in versions.items():
            (root / filename(name, version)).write_bytes(wheel(name, version, dependencies))


def verified_second_archive(value):
    path = Path(value).resolve(strict=True)
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    assert path.stat().st_size == SECOND_SIZE and digest.hexdigest() == SECOND_SHA256, \
        "second archive differs from the verified official PBS 3.14.9+20261021 artifact"
    return path


def snapshot(roots):
    digest = hashlib.sha256()
    for label, raw in enumerate(roots):
        root = Path(raw)
        paths = [root] + sorted(root.rglob("*")) if root.exists() else []
        for path in paths:
            info = path.lstat()
            row = [label, str(path.relative_to(root)), info.st_dev, info.st_ino,
                   info.st_mode, info.st_size, info.st_mtime_ns]
            if stat.S_ISREG(info.st_mode):
                row.append(hashlib.sha256(path.read_bytes()).hexdigest())
            elif stat.S_ISLNK(info.st_mode):
                row.append(os.readlink(path))
            digest.update(json.dumps(row, separators=(",", ":")).encode() + b"\0")
    return digest.hexdigest()


def lock_data(path):
    # Called with the actual verified CPython 3.13, without a host TOML dependency.
    import tomllib
    with Path(path).open("rb") as source:
        return tomllib.load(source)


def graph(path):
    document = lock_data(path)
    groups = document["python"]["groups"]
    assert len(groups) == 1, "updated lock retained a stale Python target graph"
    return document, groups[0]


def project(catalog_path, root):
    catalog = json.loads(Path(catalog_path).read_text())
    canonical = str(Path(root).resolve())
    registrations = [row for row in catalog["projects"] if row["root"] == canonical]
    assert len(registrations) == 1, f"no unique project registration for {canonical}"
    registration = registrations[0]
    selections = [row for row in catalog["selections"] if row["key"]["project"] == registration["project"]]
    return catalog, registration, selections


def source_manifest(root):
    root = Path(root)
    names = subprocess.run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
                           cwd=root, check=True, stdout=subprocess.PIPE).stdout.split(b"\0")
    rows = []
    for raw in sorted(set(names)):
        if not raw:
            continue
        name = os.fsdecode(raw)
        if name.startswith("docs/") or name in ("AGENTS.md", "README.md"):
            continue
        path = root / name
        try:
            info = path.lstat()
        except FileNotFoundError:
            rows.append([name, "deleted"])
            continue
        if stat.S_ISREG(info.st_mode):
            data, kind = path.read_bytes(), "file"
        elif stat.S_ISLNK(info.st_mode):
            data, kind = os.fsencode(os.readlink(path)), "symlink"
        else:
            raise ValueError(f"unsupported source entry: {name}")
        rows.append([name, kind, info.st_mode & 0o777, hashlib.sha256(data).hexdigest()])
    return rows


def probe(arguments):
    mode, *args = arguments
    if mode == "prepare":
        prepare(Path(args[0]))
    elif mode == "verify-second":
        print(f"P3_SECOND_ARCHIVE_VERIFIED {verified_second_archive(args[0])} sha256={SECOND_SHA256}")
    elif mode == "snapshot":
        print(snapshot(args))
    elif mode == "source-manifest":
        print(json.dumps(source_manifest(args[0]), separators=(",", ":")))
    elif mode == "assert-nodes":
        _, selected = graph(args[0])
        actual = {node["name"]: node["version"] for node in selected.get("nodes", [])}
        assert actual == json.loads(args[1]), f"locked nodes differ: {actual}"
        print(f"P3_LOCK_NODES_VERIFIED {json.dumps(actual, sort_keys=True)}")
    elif mode == "pin":
        document, selected = graph(args[0])
        node = next(node for node in selected["nodes"] if node["name"] == args[1])
        pin = next(pin for pin in document["python"]["pins"] if pin["id"] == node["source_pin"])
        print(json.dumps(pin, sort_keys=True, separators=(",", ":")))
    elif mode == "runtime":
        document = lock_data(args[0])
        print(json.dumps([{key: entry[key] for key in ("version", "platforms")}
                          for entry in document["tools"]["python"]], sort_keys=True, separators=(",", ":")))
    elif mode == "assert-intent":
        _, selected = graph(args[0])
        declarations = selected["root_declarations"]
        assert any("p3-extra[feature]==1.0" in row for row in declarations), "extras were lost from lock intent"
        assert any("p3-inactive==1.0" in row and "python_version" in row for row in declarations), "inactive marker intent was lost"
        names = {node["name"] for node in selected.get("nodes", [])}
        assert {"p3-feature", "p3-active"} <= names, "active extra or target marker did not install its package"
        assert "p3-inactive" not in names, "inactive declaration entered the installed graph"
        print("P3_COMPLETE_INTENT_AND_TARGET_ACTIVATION_VERIFIED")
    elif mode in ("project-id", "project-state", "assert-language-only"):
        _, registration, selections = project(args[0], args[1])
        if mode == "project-id":
            print(registration["project"])
        elif mode == "project-state":
            print(json.dumps([registration, selections], sort_keys=True, separators=(",", ":")))
        else:
            active = [row["stamp"] for row in selections if row["stamp"]["language"] or row["stamp"]["generation"]]
            assert len(active) == 1 and active[0]["language"] and active[0]["generation"] is None, "last-root removal did not commit a language-only selection"
            print("P3_LANGUAGE_ONLY_SELECTION_VERIFIED")
    elif mode == "assert-forgotten":
        catalog = json.loads(Path(args[0]).read_text())
        identity = args[1]
        assert not any(row["project"] == identity for row in catalog["projects"]), "forget kept the project registration"
        selections = [row for row in catalog["selections"] if row["key"]["project"] == identity]
        assert selections and all(row["stamp"]["revision"] > 0 and row["stamp"]["language"] is None and row["stamp"]["generation"] is None for row in selections), "forget discarded revisions or kept an active selection"
        clear_slots = [{"key": row["key"], "revision": row["stamp"]["revision"]} for row in selections]
        print(json.dumps(clear_slots, sort_keys=True, separators=(",", ":")))
    elif mode == "assert-reregistered":
        _, registration, selections = project(args[0], args[1])
        assert registration["project"] == args[2], "reregistration changed the path's exact project ID"
        def key(value):
            return value["project"], value["group"], value["target"]
        clear_slots = {key(row["key"]): row["revision"] for row in json.loads(args[3])}
        current = {key(row["key"]): row["stamp"] for row in selections}
        assert clear_slots and current.keys() == clear_slots.keys(), "reregistration discarded or invented a selection slot"
        active = []
        for slot, stamp in current.items():
            assert stamp["revision"] >= clear_slots[slot], "reregistration rewound an old cleared slot"
            if stamp["language"] or stamp["generation"]:
                assert stamp["revision"] > clear_slots[slot], "reregistration reused the active slot's cleared revision"
                active.append(stamp)
        assert active, "reregistration did not publish an active selection"
        print("P3_REREGISTRATION_REVISION_VERIFIED")
    else:
        raise SystemExit(f"unknown fixture probe: {mode}")


if "TestHandler" in globals():
    original_argv = sys.argv
    try:
        sys.argv = [str(HERE.parent / "cli/p1_exact_python_fixture.py"), original_argv[1]]
        runtime = runpy.run_path(sys.argv[0], init_globals={"TestHandler": TestHandler})["Handler"]
    finally:
        sys.argv = original_argv
    fixture_root = Path(sys.argv[2])
    second_archive = verified_second_archive(sys.argv[3])

    class Handler(runtime):
        def do_GET(self):
            path = unquote(urlsplit(self.path).path)
            if path in (f"/repositories/162334160/releases/tags/{SECOND_BUILD}",
                        f"/repos/astral-sh/python-build-standalone/releases/tags/{SECOND_BUILD}"):
                self.log_headers()
                body = json.dumps({"tag_name": SECOND_BUILD, "draft": False, "prerelease": False,
                                   "published_at": "2026-10-21T19:23:37Z", "assets": [
                                       {"name": SECOND_FILENAME, "browser_download_url": SECOND_URL,
                                        "size": SECOND_SIZE, "digest": "sha256:" + SECOND_SHA256}]}).encode()
                media = "application/json"
            elif path == SECOND_DOWNLOAD + "SHA256SUMS":
                self.log_headers()
                body = f"{SECOND_SHA256}  {SECOND_FILENAME}\n".encode()
                media = "text/plain"
            elif path == SECOND_DOWNLOAD + SECOND_FILENAME:
                self.log_headers()
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(SECOND_SIZE))
                self.end_headers()
                with second_archive.open("rb") as source:
                    for block in iter(lambda: source.read(64 * 1024), b""):
                        self.wfile.write(block)
                return
            elif path.startswith("/simple/"):
                self.log_headers()
                name = path.removeprefix("/simple/").strip("/")
                if name not in PACKAGES:
                    self.send_error(404, "not a prepared P3 package")
                    return
                files = [{"filename": filename(name, version), "url": "/files/" + filename(name, version),
                          "hashes": {"sha256": hashlib.sha256((fixture_root / filename(name, version)).read_bytes()).hexdigest()},
                          "requires-python": ">=3.13"} for version in PACKAGES[name]]
                body = json.dumps({"meta": {"api-version": "1.0"}, "name": name, "files": files}).encode()
                media = "application/vnd.pypi.simple.v1+json"
            elif path.startswith("/files/"):
                self.log_headers()
                name = path.removeprefix("/files/")
                if "/" in name or not (fixture_root / name).is_file():
                    self.send_error(404, "not a prepared P3 artifact")
                    return
                body = (fixture_root / name).read_bytes()
                if name.startswith("p3_broken-"):
                    body = b"deliberately not the advertised wheel bytes"
                media = "application/octet-stream"
            else:
                return super().do_GET()
            self.send_response(200)
            self.send_header("Content-Type", media)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
elif __name__ == "__main__":
    probe(sys.argv[1:])
