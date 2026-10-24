"""Fixed official PBS metadata plus the original verified local archive.

Loaded by the existing HTTP harness; it never publishes library content itself.
"""
import hashlib
import json
import sys
from pathlib import Path
from urllib.parse import quote, unquote, urlsplit

VERSION = "3.13.16"
BUILD = "20261013"
FILENAME = f"cpython-{VERSION}+{BUILD}-aarch64-apple-darwin-install_only_stripped.tar.gz"
SHA256 = "16a5332fb0dcb77f163fc8f8cbccd95be1644cc53e68e5d2a79f50afd6680a0e"
TAG_PATH = f"/repositories/162334160/releases/tags/{BUILD}"
DOWNLOAD_PATH = f"/astral-sh/python-build-standalone/releases/download/{BUILD}/"
CANONICAL_ARCHIVE = f"https://github.com{DOWNLOAD_PATH}{quote(FILENAME, safe='')}"


def verified_archive(value):
    path = Path(value).resolve(strict=True)
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    if digest.hexdigest() != SHA256:
        raise ValueError("local PBS fixture does not have the approved original SHA256")
    return path


if "TestHandler" in globals():
    archive = verified_archive(sys.argv[1])

    class Handler(TestHandler):
        def do_GET(self):
            self.reply(False)

        def do_HEAD(self):
            self.reply(True)

        def reply(self, head):
            self.log_headers()
            path = unquote(urlsplit(self.path).path)
            if path == TAG_PATH:
                # Deliberately omit API digest to exercise the actual
                # same-release checksum manifest lookup before acquisition.
                body = json.dumps({
                    "tag_name": BUILD,
                    "draft": False,
                    "prerelease": False,
                    "published_at": "2026-10-14T02:06:09Z",
                    "assets": [{"name": FILENAME, "browser_download_url": CANONICAL_ARCHIVE}],
                }).encode()
                media = "application/json"
            elif path == DOWNLOAD_PATH + "SHA256SUMS":
                body = f"{SHA256}  {FILENAME}\n".encode()
                media = "text/plain"
            elif path == DOWNLOAD_PATH + FILENAME:
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(archive.stat().st_size))
                self.end_headers()
                if not head:
                    with archive.open("rb") as source:
                        for block in iter(lambda: source.read(64 * 1024), b""):
                            self.wfile.write(block)
                return
            else:
                self.send_error(404, "not a frozen PBS fixture route")
                return
            self.send_response(200)
            self.send_header("Content-Type", media)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if not head:
                self.wfile.write(body)
else:
    verified_archive(sys.argv[1])
    print("P1 exact Python original archive SHA256 verified")
