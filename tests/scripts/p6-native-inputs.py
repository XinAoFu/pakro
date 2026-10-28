#!/usr/bin/env python3
"""Prepare only the fixed public inputs for the P6 native backend fixtures."""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tarfile
import time
import tomllib
import urllib.parse
import urllib.request


PINNED = {
    "setuptools": "80.9.0", "wheel": "0.45.1", "packaging": "25.0",
    "pathspec": "0.12.1", "scikit-build-core": "0.11.6",
    "meson-python": "0.18.0", "meson": "1.7.2", "pyproject-metadata": "0.9.1",
    "maturin": "1.8.6", "setuptools-rust": "1.11.1", "semantic-version": "2.10.0",
}
CASES = {
    "setuptools": ("p6-setuptools-native", "setuptools.build_meta", "import p6_native_demo as m; assert m.answer()==42 and len(m.nonce())==32"),
    "scikit": ("p6-scikit-native", "scikit_build_core.build", "import p6_native_demo as m; assert m.answer()==42"),
    "meson": ("p6-meson-native", "mesonpy", "import p6_native_demo as m; assert m.answer()==42"),
    "maturin": ("p6-maturin-native", "maturin", "import p6_rust_demo as m; assert m.answer()==42"),
    "setuptools-rust": ("p6-setuptools-rust-native", "setuptools.build_meta", "import p6_rust_demo as m; assert m.answer()==42"),
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="new private directory; never overwrite an existing directory")
    parser.add_argument("--platform", choices=("macos-arm64", "linux-x86_64"), required=True)
    parser.add_argument("--cargo", type=Path, required=True, help="actual installed Cargo executable, not a rustup proxy")
    args = parser.parse_args()
    output = args.output
    if not output.is_absolute() or ".." in output.parts or output.exists() or output.is_symlink():
        parser.error("--output must name a new absolute directory without '..'")
    cargo = args.cargo.resolve(strict=True)
    if not cargo.is_file() or not os.access(cargo, os.X_OK) or cargo.name == "rustup":
        parser.error("--cargo must be an actual installed Cargo executable")
    templates = Path(__file__).resolve().parents[1] / "support" / "fixtures" / "p6-native"
    output.mkdir(mode=0o700)
    assets = output / "assets"
    sources = output / "sources"
    assets.mkdir(mode=0o700)
    sources.mkdir(mode=0o700)
    started = time.monotonic()
    deadline = started + 480
    receipt = {"schema": 1, "complete": False, "platform": args.platform, "assets": [], "cases": [], "cargo_locks": []}
    with cargo.open("rb") as stream:
        receipt["cargo"] = {"path": str(cargo), "sha256": hashlib.file_digest(stream, "sha256").hexdigest()}
    active = None
    total = 0

    def remaining():
        value = deadline - time.monotonic()
        if value <= 0:
            raise TimeoutError("the complete fixed-input preparation exceeded 480 seconds")
        return value

    def fetch(url, maximum, destination=None, expected=None):
        nonlocal total
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != "https" or parsed.hostname not in {"pypi.org", "files.pythonhosted.org", "static.crates.io"}:
            raise ValueError("input URL is outside the fixed public upstreams")
        request = urllib.request.Request(url, headers={"User-Agent": "Pakro-P6-fixed-test-inputs/1"})
        digest = hashlib.sha256()
        count = 0
        result = bytearray()
        stream = destination.open("xb") if destination else None
        try:
            with urllib.request.urlopen(request, timeout=min(30, remaining())) as response:
                if urllib.parse.urlparse(response.url).hostname not in {"pypi.org", "files.pythonhosted.org", "static.crates.io"}:
                    raise ValueError("public input redirected outside its upstream")
                while True:
                    remaining()
                    chunk = response.read(65536)
                    if not chunk:
                        break
                    count += len(chunk)
                    total += len(chunk)
                    if count > maximum or total > 256 * 1024 * 1024:
                        raise ValueError("fixed-input download byte budget exceeded")
                    digest.update(chunk)
                    if stream:
                        stream.write(chunk)
                    else:
                        result.extend(chunk)
        finally:
            if stream:
                stream.close()
        actual = digest.hexdigest()
        if expected and actual != expected:
            raise ValueError("public input does not match its official SHA256")
        return bytes(result), count, actual

    def add_pypi(name, version, source=False):
        url = f"https://pypi.org/pypi/{name}/{version}/json"
        raw, _, metadata_sha = fetch(url, 2 * 1024 * 1024)
        metadata = json.loads(raw)
        if metadata["info"]["version"] != version:
            raise ValueError("PyPI returned a different exact release")
        candidates = [file for file in metadata["urls"] if not file.get("yanked") and file["packagetype"] == ("sdist" if source else "bdist_wheel")]
        if source:
            candidates = [file for file in candidates if file["filename"].endswith(".tar.gz")]
        elif name == "maturin":
            if args.platform == "macos-arm64":
                candidates = [file for file in candidates if "macosx" in file["filename"] and "arm64" in file["filename"]]
            else:
                candidates = [file for file in candidates if "manylinux" in file["filename"] and "x86_64" in file["filename"]]
        else:
            candidates = [file for file in candidates if file["filename"].endswith("-none-any.whl")
                          and "py3" in file["filename"].rsplit("-", 3)[-3].split(".")]
        if not candidates:
            raise ValueError(f"no fixed supported input for {name}=={version}")
        file = sorted(candidates, key=lambda file: file["filename"])[0]
        filename = file["filename"]
        if Path(filename).name != filename:
            raise ValueError("upstream filename is not one component")
        expected = file["digests"]["sha256"]
        _, size, actual = fetch(file["url"], 64 * 1024 * 1024, assets / filename, expected)
        if size != file["size"]:
            raise ValueError("public input size differs from official metadata")
        record = {"name": name.lower().replace("_", "-"), "version": version, "filename": filename,
                  "path": f"assets/{filename}", "sha256": actual, "size": size,
                  "url": file["url"], "metadata_url": url, "metadata_sha256": metadata_sha,
                  "requires_python": file.get("requires_python") or ">=3", "kind": "sdist" if source else "wheel"}
        receipt["assets"].append(record)
        print(f"verified {name}=={version}: {filename}", flush=True)
        return record

    try:
        for name, version in PINNED.items():
            add_pypi(name, version)
        public = add_pypi("MarkupSafe", "3.0.2", True)
        receipt["cases"].append({"case": "markupsafe", "name": "markupsafe", "version": "3.0.2", "filename": public["filename"],
                                 "backend": "setuptools.build_meta", "import": "import markupsafe, markupsafe._speedups; assert str(markupsafe.escape('<&>'))=='&lt;&amp;&gt;'"})
        prepare_home = output / "prepare-home"
        prepare_home.mkdir(mode=0o700)
        cargo_home = output / "cargo-preparation"
        cargo_home.mkdir(mode=0o700)
        for case, (name, backend, import_code) in CASES.items():
            remaining()
            source = sources / case
            shutil.copytree(templates / case, source, symlinks=False)
            if case in {"maturin", "setuptools-rust"}:
                argv = [str(cargo), "generate-lockfile", "--manifest-path", str(source / "Cargo.toml")]
                log = output / f"cargo-{case}.log"
                environment = {"PATH": str(cargo.parent) + os.pathsep + os.defpath, "HOME": str(prepare_home),
                               "CARGO_HOME": str(cargo_home), "LANG": "C", "LC_ALL": "C", "CARGO_TERM_COLOR": "never"}
                with log.open("xb") as stream:
                    active = subprocess.Popen(argv, cwd=source, env=environment, stdin=subprocess.DEVNULL,
                                              stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
                    code = active.wait(timeout=max(0.001, remaining() - 5))
                active = None
                if code:
                    raise RuntimeError(f"fixed Cargo lock preparation failed ({code}); see {log.name}")
                lock_bytes = (source / "Cargo.lock").read_bytes()
                lock = tomllib.loads(lock_bytes.decode())
                crates = [package for package in lock["package"] if "source" in package]
                if not crates or any(package["source"] != "registry+https://github.com/rust-lang/crates.io-index" or len(package.get("checksum", "")) != 64 for package in crates):
                    raise ValueError("prepared Rust fixture lacks complete fixed crates.io checksums")
                receipt["cargo_locks"].append({"case": case, "argv": argv, "exit": code, "sha256": hashlib.sha256(lock_bytes).hexdigest(), "registry_packages": len(crates), "log": log.name})
                for package in crates:
                    filename = f"{package['name']}-{package['version']}.crate"
                    existing = next((file for file in receipt["assets"] if file["filename"] == filename), None)
                    if existing:
                        if existing["sha256"] != package["checksum"]:
                            raise ValueError("the two fixed Cargo locks disagree on one crate's checksum")
                        continue
                    url = f"https://static.crates.io/crates/{package['name']}/{filename}"
                    _, size, actual = fetch(url, 64 * 1024 * 1024, assets / filename, package["checksum"])
                    receipt["assets"].append({"kind": "crate", "name": package["name"], "version": package["version"], "filename": filename,
                                              "path": f"assets/{filename}", "sha256": actual, "size": size, "url": url})
            filename = name.replace("-", "_") + "-1.0.0.tar.gz"
            archive = assets / filename
            with archive.open("xb") as target, gzip.GzipFile(fileobj=target, mode="wb", filename="", mtime=0) as compressed, tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as tar:
                bundle = name.replace("-", "_") + "-1.0.0"
                for member in [source, *sorted(source.rglob("*"))]:
                    if member.is_symlink():
                        raise ValueError("trusted source template unexpectedly contains a symlink")
                    relative = member.relative_to(source)
                    info = tarfile.TarInfo(bundle + ("/" + relative.as_posix() if relative.parts else ""))
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = 315532800
                    info.mode = 0o755 if member.is_dir() else 0o644
                    if member.is_dir():
                        info.type = tarfile.DIRTYPE
                        tar.addfile(info)
                    elif member.is_file():
                        info.size = member.stat().st_size
                        with member.open("rb") as stream:
                            tar.addfile(info, stream)
                    else:
                        raise ValueError("trusted source template is not a regular file/directory")
            raw = archive.read_bytes()
            receipt["assets"].append({"kind": "sdist", "name": name, "version": "1.0.0", "filename": filename,
                                      "path": f"assets/{filename}", "sha256": hashlib.sha256(raw).hexdigest(), "size": len(raw), "requires_python": ">=3.11"})
            receipt["cases"].append({"case": case, "name": name, "version": "1.0.0", "filename": filename, "backend": backend, "import": import_code})
        receipt["complete"] = True
        receipt["marker"] = "P6_NATIVE_INPUTS_COMPLETE"
    except BaseException as error:
        if active is not None and active.poll() is None:
            os.killpg(active.pid, signal.SIGKILL)
            active.wait(timeout=5)
        receipt["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        receipt["elapsed_seconds"] = time.monotonic() - started
        receipt["downloaded_bytes"] = total
        (output / "assets.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
        print(json.dumps({key: receipt[key] for key in ("complete", "elapsed_seconds", "downloaded_bytes")}), flush=True)


if __name__ == "__main__":
    main()
