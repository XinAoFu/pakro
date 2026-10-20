"""Original HTTP harness handler plus genuine wheel-byte fixture producer."""
import base64
import hashlib
import io
import json
import sys
import zipfile
from pathlib import Path


def wheel(name, dependencies, members, entry=None):
    info = f"{name}-1.0.dist-info"
    files = dict(members)
    files[f"{info}/METADATA"] = (
        f"Metadata-Version: 2.4\nName: {name}\nVersion: 1.0\nRequires-Python: >=3.13\n"
        + "".join(f"Requires-Dist: {dep}\n" for dep in dependencies)
    ).encode()
    files[f"{info}/WHEEL"] = b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n"
    if entry:
        files[f"{info}/entry_points.txt"] = ("[console_scripts]\n" + entry + "\n").encode()
    records = []
    for path, body in files.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(body).digest()).rstrip(b"=").decode()
        records.append(f"{path},sha256={digest},{len(body)}")
    files[f"{info}/RECORD"] = ("\n".join(records + [f"{info}/RECORD,,"]) + "\n").encode()
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        for path, body in files.items():
            item = zipfile.ZipInfo(path)
            item.external_attr = 0o100644 << 16
            archive.writestr(item, body)
    return buffer.getvalue()


def prepare(root):
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    (root / "a_dep-1.0-py3-none-any.whl").write_bytes(wheel("a_dep", [], {
        "a_dep.py": b"VALUE = 'shared-dependency'\n",
        "extra/pth_marker.py": b"VALUE = 'pth-visible'\n",
        "extra-path.pth": b"extra\n",
    }))
    (root / "z_root-1.0-py3-none-any.whl").write_bytes(wheel("z_root", ["a-dep>=1,<2"], {
        "z_root.py": b"import a_dep, pth_marker\ndef main():\n print(a_dep.VALUE + ':' + pth_marker.VALUE)\nif __name__ == '__main__': main()\n",
    }, "z-root-cli = z_root:main"))


if "TestHandler" in globals():
    fixture_root = Path(sys.argv[1])

    class Handler(TestHandler):
        def do_GET(self):
            self.log_headers()
            if self.path in ("/simple/z-root/", "/simple/a-dep/"):
                name = self.path.split("/")[2]
                filename = name.replace("-", "_") + "-1.0-py3-none-any.whl"
                wheel_body = (fixture_root / filename).read_bytes()
                body = json.dumps({"meta": {"api-version": "1.0"}, "name": name, "files": [{
                    "filename": filename, "url": f"/files/{filename}",
                    "hashes": {"sha256": hashlib.sha256(wheel_body).hexdigest()}, "requires-python": ">=3.13",
                }]}).encode()
                media = "application/vnd.pypi.simple.v1+json"
            elif self.path.startswith("/files/") and (fixture_root / self.path.split("/")[-1]).is_file():
                body = (fixture_root / self.path.split("/")[-1]).read_bytes()
                media = "application/octet-stream"
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", media)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
elif __name__ == "__main__":
    prepare(sys.argv[1])
