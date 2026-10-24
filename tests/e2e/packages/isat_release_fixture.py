"""Serve a checked release corpus through a loopback-only test mirror.

The corpus contains original Simple JSON and original release archives. It is
input to the real resolver and source builder, never an installed environment
or a preselected dependency plan. No request can fetch an absent input online.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
import io
import json
import os
import re
import socket
import stat
import tempfile
import threading
import time
import urllib.parse
import zipfile
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PurePosixPath


FORMAT = "pakro-isat-release-fixture-v1"
_MAX_MANIFEST = 8 * 1024 * 1024
_MAX_PAGE = 64 * 1024 * 1024
_MAX_ARTIFACT = 2 * 1024 * 1024 * 1024
_MAX_CORPUS = 8 * 1024 * 1024 * 1024
_MAX_METADATA = 16 * 1024 * 1024
_MAX_REQUESTS = 131072
_SIMPLE_MEDIA = "application/vnd.pypi.simple.v1+json"


class FixtureError(ValueError):
    """A corpus or bounded loopback operation is invalid."""


def _require(condition, message):
    if not condition:
        raise FixtureError(message)


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        _require(key not in result, "JSON contains duplicate object keys")
        result[key] = value
    return result


def _json(raw):
    try:
        return json.loads(
            raw,
            object_pairs_hook=_unique_object,
            parse_constant=lambda _: (_ for _ in ()).throw(FixtureError("non-finite JSON value")),
        )
    except (UnicodeError, ValueError, RecursionError):
        raise FixtureError("invalid corpus JSON") from None


def _sha256(value):
    _require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value),
             "invalid corpus SHA-256")
    return value


def _size(value, maximum):
    _require(type(value) is int and 0 < value <= maximum, "invalid corpus file size")
    return value


def _name(value):
    _require(isinstance(value, str) and re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", value),
             "invalid normalized project name")
    return value


def _filename(value):
    _require(isinstance(value, str) and 0 < len(value) <= 512
             and value not in (".", "..")
             and all(32 < ord(c) < 127 and c not in "/\\?#" for c in value),
             "invalid release filename")
    return value


def _artifact_path(url, filename):
    _require(isinstance(url, str) and len(url) <= 8192
             and all(32 < ord(c) < 127 for c in url), "invalid release URL")
    try:
        parsed = urllib.parse.urlsplit(url)
        decoded = urllib.parse.unquote_to_bytes(parsed.path).decode("utf-8")
        port = parsed.port
    except (ValueError, UnicodeError):
        raise FixtureError("invalid release URL") from None
    _require(parsed.scheme == "https" and parsed.netloc == "files.pythonhosted.org"
             and parsed.hostname == "files.pythonhosted.org" and port is None
             and parsed.username is None and parsed.password is None
             and not parsed.query and not parsed.fragment,
             "release URL must be a public query-free PyPI file URL")
    _require(parsed.path.startswith("/packages/") and decoded.startswith("/packages/")
             and not any(part in ("", ".", "..") for part in decoded.split("/")[1:])
             and "\\" not in decoded and PurePosixPath(decoded).name == filename
             and all(32 < ord(c) < 127 for c in parsed.path),
             "invalid release URL path")
    return parsed.path


@dataclass(frozen=True)
class _Blob:
    file: object
    size: int
    sha256: str

    def read(self):
        return os.pread(self.file.fileno(), self.size, 0)


def _open_regular(root_fd, relative):
    _require(isinstance(relative, str) and len(relative) <= 1024 and "\\" not in relative,
             "invalid corpus-relative path")
    parts = relative.split("/")
    _require(parts and all(part not in ("", ".", "..") for part in parts),
             "corpus file must remain inside the corpus")
    current = os.dup(root_fd)
    try:
        for part in parts[:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                            dir_fd=current)
            os.close(current)
            current = child
        fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
                     dir_fd=current)
        try:
            _require(stat.S_ISREG(os.fstat(fd).st_mode), "corpus input is not a regular file")
        except BaseException:
            os.close(fd)
            raise
        return os.fdopen(fd, "rb")
    except OSError:
        raise FixtureError("corpus input cannot be opened without following symlinks") from None
    finally:
        os.close(current)


def _snapshot(root_fd, relative, digest, size, stack):
    """Retain private checked bytes, so later source-path mutation is harmless."""
    snapshot = stack.enter_context(tempfile.TemporaryFile())
    actual = hashlib.sha256()
    count = 0
    with _open_regular(root_fd, relative) as source:
        _require(os.fstat(source.fileno()).st_size == size, "corpus size differs from manifest")
        while True:
            chunk = source.read(min(1024 * 1024, size - count + 1))
            if not chunk:
                break
            count += len(chunk)
            _require(count <= size, "corpus input exceeds manifest size")
            actual.update(chunk)
            snapshot.write(chunk)
    _require(count == size and actual.hexdigest() == digest,
             "corpus bytes differ from manifest SHA-256 or size")
    snapshot.flush()
    return _Blob(snapshot, size, digest)


def _metadata_hashes(records):
    hashes = {}
    for record in records:
        for key in ("core-metadata", "dist-info-metadata"):
            value = record.get(key)
            if not isinstance(value, dict):
                continue
            for algorithm, digest in value.items():
                _require(isinstance(algorithm, str) and isinstance(digest, str),
                         "invalid advertised metadata hash")
                try:
                    length = hashlib.new(algorithm).digest_size * 2
                except (ValueError, TypeError):
                    raise FixtureError("unsupported advertised metadata hash") from None
                _require(length > 0 and len(digest) == length
                         and re.fullmatch(r"[0-9a-f]+", digest),
                         "invalid advertised metadata hash")
                _require(algorithm not in hashes or hashes[algorithm] == digest,
                         "conflicting advertised metadata hashes")
                hashes[algorithm] = digest
    return hashes


def _wheel_metadata(blob, records):
    try:
        # ZipFile owns an independent offset; HTTP reads use pread throughout.
        with os.fdopen(os.dup(blob.file.fileno()), "rb") as archive:
            with zipfile.ZipFile(archive) as wheel:
                matches = [entry for entry in wheel.infolist()
                           if entry.filename.endswith(".dist-info/METADATA")
                           and len(PurePosixPath(entry.filename).parts) == 2]
                _require(len(matches) == 1 and not matches[0].is_dir()
                         and len(PurePosixPath(matches[0].filename).parts) == 2
                         and matches[0].file_size <= _MAX_METADATA,
                         "wheel must contain one bounded original METADATA")
                metadata = wheel.read(matches[0])
    except (OSError, zipfile.BadZipFile, RuntimeError, NotImplementedError):
        raise FixtureError("original wheel METADATA cannot be read") from None
    for algorithm, expected in _metadata_hashes(records).items():
        _require(hashlib.new(algorithm, metadata).hexdigest() == expected,
                 "original wheel METADATA differs from its advertised hash")
    return metadata


def _load_corpus(corpus, stack):
    try:
        root_fd = os.open(os.fspath(corpus),
                          os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    except OSError:
        raise FixtureError("corpus root must be an existing non-symlink directory") from None
    stack.callback(os.close, root_fd)
    with _open_regular(root_fd, "manifest.json") as source:
        raw = source.read(_MAX_MANIFEST + 1)
    _require(len(raw) <= _MAX_MANIFEST, "corpus manifest exceeds its size limit")
    manifest = _json(raw)
    _require(isinstance(manifest, dict) and manifest.get("format") == FORMAT
             and isinstance(manifest.get("pages"), list)
             and isinstance(manifest.get("artifacts"), list), "invalid corpus manifest")
    _require(len(manifest["pages"]) <= 4096 and len(manifest["artifacts"]) <= 8192,
             "corpus contains too many inputs")
    total = 0
    responses = {}
    originals = {}
    snapshots = {}

    def checked(entry, maximum):
        nonlocal total
        _require(isinstance(entry, dict), "invalid corpus file entry")
        digest = _sha256(entry.get("sha256"))
        size = _size(entry.get("size"), maximum)
        total += size
        _require(total <= _MAX_CORPUS, "corpus exceeds its total size limit")
        relative = entry.get("path")
        if relative in snapshots:
            blob = snapshots[relative]
            _require(blob.size == size and blob.sha256 == digest,
                     "conflicting corpus file identities")
        else:
            blob = _snapshot(root_fd, relative, digest, size, stack)
            snapshots[relative] = blob
        return blob

    for entry in manifest["pages"]:
        _require(isinstance(entry, dict), "invalid page entry")
        name = _name(entry.get("name"))
        _require(entry.get("path") == f"pages/{name}.json", "unexpected corpus page path")
        path = f"/simple/{name}/"
        _require(path not in responses, "duplicate corpus project page")
        blob = checked(entry, _MAX_PAGE)
        page = _json(blob.read())
        _require(isinstance(page, dict) and isinstance(page.get("name"), str)
                 and re.sub(r"[-_.]+", "-", page["name"]).lower() == name
                 and isinstance(page.get("files"), list), "page project identity is invalid")
        responses[path] = (blob, _SIMPLE_MEDIA)
        for record in page["files"]:
            _require(isinstance(record, dict), "invalid original Simple file record")
            filename = _filename(record.get("filename"))
            url = record.get("url")
            _artifact_path(url, filename)
            hashes = record.get("hashes")
            if isinstance(hashes, dict) and "sha256" in hashes:
                digest = _sha256(hashes["sha256"])
                originals.setdefault((filename, url, digest), []).append(record)

    for entry in manifest["artifacts"]:
        _require(isinstance(entry, dict), "invalid artifact entry")
        filename = _filename(entry.get("filename"))
        url = entry.get("url")
        path = _artifact_path(url, filename)
        digest = _sha256(entry.get("sha256"))
        _require(entry.get("path") == f"artifacts/{digest}", "unexpected corpus artifact path")
        records = originals.get((filename, url, digest))
        _require(records, "artifact has no matching original Simple file record")
        _require(path not in responses, "duplicate corpus artifact URL path")
        blob = checked(entry, _MAX_ARTIFACT)
        for record in records:
            _require(record.get("size") is None or record["size"] == blob.size,
                     "artifact size differs from original Simple file record")
        responses[path] = (blob, "application/octet-stream")
        if filename.endswith(".whl"):
            metadata = _wheel_metadata(blob, records)
            _require(path + ".metadata" not in responses, "duplicate metadata response path")
            responses[path + ".metadata"] = (metadata, "application/octet-stream")
    return responses


class ReleaseFixture:
    """A loopback origin and snapshot lists of safe request/missing paths."""

    def __init__(self):
        self.origin = ""
        self._lock = threading.Lock()
        self._requests = []
        self._missing = []

    @property
    def requests(self):
        with self._lock:
            return list(self._requests)

    @property
    def missing(self):
        with self._lock:
            return list(self._missing)


def _safe_request_target(target):
    try:
        parsed = urllib.parse.urlsplit(target)
    except ValueError:
        return "/<invalid-request-target>", False
    path = parsed.path
    safe = (len(path) <= 8192 and path.startswith("/")
            and all(32 < ord(c) < 127 for c in path))
    return (path if safe else "/<invalid-request-target>",
            safe and not parsed.scheme and not parsed.netloc
            and not parsed.query and not parsed.fragment)


class _Server(ThreadingHTTPServer):
    daemon_threads = True
    block_on_close = False

    def __init__(self, handler):
        self._active_lock = threading.Lock()
        self._active = {}
        super().__init__(("127.0.0.1", 0), handler)

    def process_request(self, request, client_address):
        request.settimeout(3)

        def serve():
            try:
                self.finish_request(request, client_address)
            except (OSError, ValueError):
                pass
            finally:
                self.shutdown_request(request)
                with self._active_lock:
                    self._active.pop(threading.current_thread(), None)

        thread = threading.Thread(target=serve, daemon=True, name="release-fixture-request")
        with self._active_lock:
            if len(self._active) >= 64:
                self.shutdown_request(request)
                return
            self._active[thread] = request
        try:
            thread.start()
        except BaseException:
            with self._active_lock:
                self._active.pop(thread, None)
            self.shutdown_request(request)
            raise

    def close_requests(self, deadline):
        with self._active_lock:
            active = list(self._active.items())
        for _, request in active:
            try:
                request.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            request.close()
        for thread, _ in active:
            thread.join(max(0, deadline - time.monotonic()))
        _require(not any(thread.is_alive() for thread, _ in active),
                 "fixture request thread did not stop within its deadline")

    def handle_error(self, request, client_address):
        # BaseHTTPRequestHandler errors must not print raw targets or headers.
        pass


@contextmanager
def start_fixture(corpus: Path):
    """Check all inputs before serving; shutdown and thread joins are bounded."""
    with ExitStack() as stack:
        responses = _load_corpus(Path(corpus), stack)
        fixture = ReleaseFixture()

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.0"

            def log_message(self, *_):
                pass

            def do_GET(self):
                self._reply(False)

            def do_HEAD(self):
                self._reply(True)

            def _reply(self, head_only):
                path, valid = _safe_request_target(self.path)
                response = responses.get(path) if valid else None
                with fixture._lock:
                    _require(len(fixture._requests) < _MAX_REQUESTS,
                             "fixture request audit limit exceeded")
                    fixture._requests.append(path)
                    if response is None:
                        fixture._missing.append(path)
                body, media = response if response is not None else (b"not prepared\n", "text/plain")
                size = body.size if isinstance(body, _Blob) else len(body)
                self.send_response(200 if response is not None else 404)
                self.send_header("Content-Type", media)
                self.send_header("Content-Length", str(size))
                self.send_header("Connection", "close")
                self.end_headers()
                if head_only:
                    return
                if isinstance(body, _Blob):
                    offset = 0
                    while offset < body.size:
                        chunk = os.pread(body.file.fileno(), min(64 * 1024, body.size - offset), offset)
                        _require(chunk, "checked corpus snapshot ended unexpectedly")
                        self.wfile.write(chunk)
                        offset += len(chunk)
                else:
                    self.wfile.write(body)

        server = _Server(Handler)
        stack.callback(server.server_close)
        fixture.origin = f"http://127.0.0.1:{server.server_port}"
        thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.05),
                                  daemon=True, name="release-fixture-server")
        thread.start()
        try:
            yield fixture
        finally:
            deadline = time.monotonic() + 5
            shutdown = threading.Thread(target=server.shutdown, daemon=True)
            shutdown.start()
            shutdown.join(max(0, deadline - time.monotonic()))
            server.server_close()
            server.close_requests(deadline)
            thread.join(max(0, deadline - time.monotonic()))
            _require(not shutdown.is_alive() and not thread.is_alive(),
                     "fixture server did not stop within its deadline")


def _self_test_corpus(root, wrong_metadata=False, metadata_key="core-metadata"):
    root.mkdir(parents=True)
    (root / "pages").mkdir()
    (root / "artifacts").mkdir()
    metadata = b"Metadata-Version: 2.4\nName: demo_pkg\nVersion: 1.0\n"
    members = {
        "demo_pkg/__init__.py": b"VALUE = 42\n",
        "demo_pkg/_vendor/vendor-1.0.dist-info/METADATA":
            b"Metadata-Version: 2.4\nName: vendor\nVersion: 1.0\n",
        "demo_pkg-1.0.dist-info/METADATA": metadata,
        "demo_pkg-1.0.dist-info/WHEEL":
            b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    }
    record = "".join(f"{name},sha256={base64.urlsafe_b64encode(hashlib.sha256(body).digest()).rstrip(b'=').decode()},{len(body)}\n"
                     for name, body in members.items())
    members["demo_pkg-1.0.dist-info/RECORD"] = (record + "demo_pkg-1.0.dist-info/RECORD,,\n").encode()
    wheel = io.BytesIO()
    with zipfile.ZipFile(wheel, "w", zipfile.ZIP_STORED) as archive:
        for name, body in members.items():
            item = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            item.external_attr = 0o100644 << 16
            archive.writestr(item, body)
    wheel = wheel.getvalue()
    digest = hashlib.sha256(wheel).hexdigest()
    filename = "demo_pkg-1.0-py3-none-any.whl"
    url = "https://files.pythonhosted.org/packages/ab/" + filename
    file = {"filename": filename, "url": url, "hashes": {"sha256": digest},
            "size": len(wheel), metadata_key: {
                "sha256": "0" * 64 if wrong_metadata else hashlib.sha256(metadata).hexdigest()}}
    page = json.dumps({"meta": {"api-version": "1.0"}, "name": "demo-pkg",
                       "files": [file]}, separators=(",", ":")).encode()
    (root / "pages/demo-pkg.json").write_bytes(page)
    (root / "artifacts" / digest).write_bytes(wheel)
    manifest = {"format": FORMAT,
                "pages": [{"name": "demo-pkg", "path": "pages/demo-pkg.json",
                           "sha256": hashlib.sha256(page).hexdigest(), "size": len(page)}],
                "artifacts": [{"filename": filename, "url": url,
                               "path": "artifacts/" + digest, "sha256": digest, "size": len(wheel)}]}
    (root / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    return page, wheel, metadata, urllib.parse.urlsplit(url).path, digest


def self_test():
    """Only loopback traffic and owned temporary files; no package install."""
    def get(fixture, path):
        address = urllib.parse.urlsplit(fixture.origin)
        connection = http.client.HTTPConnection(address.hostname, address.port, timeout=3)
        try:
            connection.request("GET", path)
            response = connection.getresponse()
            return response.status, response.read()
        finally:
            connection.close()

    def rejects(corpus):
        try:
            with start_fixture(corpus):
                raise AssertionError("invalid corpus reached the serving phase")
        except FixtureError:
            return
        raise AssertionError("invalid corpus was accepted")

    with tempfile.TemporaryDirectory(prefix="isat-fixture-self-test-") as temporary:
        root = Path(temporary)
        corpus = root / "valid"
        page, wheel, metadata, path, digest = _self_test_corpus(corpus)
        with start_fixture(corpus) as fixture:
            assert get(fixture, "/simple/demo-pkg/") == (200, page)
            assert get(fixture, path) == (200, wheel)
            assert get(fixture, path + ".metadata") == (200, metadata)
            assert get(fixture, "/simple/absent/")[0] == 404
            assert get(fixture, "/packages/absent.whl")[0] == 404
            assert fixture.missing == ["/simple/absent/", "/packages/absent.whl"]
            assert len(fixture.requests) == 5
            # Serving continues from the private verified bytes after mutation.
            (corpus / "artifacts" / digest).write_bytes(b"tampered")
            assert get(fixture, path) == (200, wheel)
        rejects(corpus)
        bad_metadata = root / "bad-metadata"
        _self_test_corpus(bad_metadata, wrong_metadata=True)
        rejects(bad_metadata)
        bad_legacy_metadata = root / "bad-legacy-metadata"
        _self_test_corpus(bad_legacy_metadata, wrong_metadata=True,
                          metadata_key="dist-info-metadata")
        rejects(bad_legacy_metadata)
        symlink = root / "symlink"
        _self_test_corpus(symlink)
        (symlink / "pages/demo-pkg.json").unlink()
        (symlink / "pages/demo-pkg.json").symlink_to(corpus / "pages/demo-pkg.json")
        rejects(symlink)
    print("release fixture original bytes, metadata, missing404, tamper and symlink checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", required=True)
    parser.parse_args()
    self_test()
