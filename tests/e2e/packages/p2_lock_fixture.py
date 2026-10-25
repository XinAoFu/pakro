"""Frozen original PBS transport plus deterministic wheel/source distributions."""
import base64
import hashlib
import io
import json
import runpy
import sys
import tarfile
import zipfile
from pathlib import Path
from urllib.parse import urlsplit

HERE = Path(__file__).resolve().parent


def load_handler(path, argument):
    original = sys.argv
    try:
        sys.argv = [str(path), str(argument)]
        return runpy.run_path(str(path), init_globals={"TestHandler": TestHandler})["Handler"]
    finally:
        sys.argv = original


def make_wheel(name, version, module, dependencies=(), entry=None):
    info = f"{name}-{version}.dist-info"
    files = {f"{name}.py": module.encode(), f"{info}/METADATA": (
        f"Metadata-Version: 2.4\nName: {name}\nVersion: {version}\nRequires-Python: >=3.13\n"
        + "".join(f"Requires-Dist: {value}\n" for value in dependencies)).encode(),
        f"{info}/WHEEL": b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n"}
    if entry:
        files[f"{info}/entry_points.txt"] = ("[console_scripts]\n" + entry + "\n").encode()
    records = [f"{path},sha256={base64.urlsafe_b64encode(hashlib.sha256(body).digest()).rstrip(b'=').decode()},{len(body)}"
               for path, body in files.items()]
    files[f"{info}/RECORD"] = ("\n".join(records + [f"{info}/RECORD,,"]) + "\n").encode()
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        for path, body in files.items():
            item = zipfile.ZipInfo(path)
            item.external_attr = 0o100644 << 16
            archive.writestr(item, body)
    return buffer.getvalue()


def prepare(root):
    root.mkdir(parents=True, exist_ok=True)
    runpy.run_path(str(HERE / "f3_shared_wheel_fixture.py"))["prepare"](root)
    (root / "a_dep-1.1-py3-none-any.whl").write_bytes(make_wheel("a_dep", "1.1", "VALUE='unexpected-new-selection'\n"))
    backend = '''import base64, hashlib, io, zipfile
from pathlib import Path
def get_requires_for_build_wheel(config_settings=None):
    import a_dep
    assert a_dep.VALUE == 'shared-dependency'
    return ['z-root==1.0']
def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    import a_dep, z_root
    assert a_dep.VALUE == 'shared-dependency'
    info='p2_source-1.0.dist-info'
    files={'p2_source.py': b"import a_dep\\ndef main():\\n print('source:' + a_dep.VALUE)\\n",
           info+'/METADATA': b'Metadata-Version: 2.4\\nName: p2_source\\nVersion: 1.0\\nRequires-Python: >=3.13\\nRequires-Dist: a-dep>=1,<2\\n',
           info+'/WHEEL': b'Wheel-Version: 1.0\\nRoot-Is-Purelib: true\\nTag: py3-none-any\\n',
           info+'/entry_points.txt': b'[console_scripts]\\np2-source-cli = p2_source:main\\n'}
    records=[f"{p},sha256={base64.urlsafe_b64encode(hashlib.sha256(b).digest()).rstrip(b'=').decode()},{len(b)}" for p,b in files.items()]
    files[info+'/RECORD']=('\\n'.join(records+[info+'/RECORD,,'])+'\\n').encode()
    out=io.BytesIO()
    with zipfile.ZipFile(out,'w') as archive:
        for p,b in files.items():
            item=zipfile.ZipInfo(p); item.external_attr=0o100644 << 16
            archive.writestr(item,b)
    name='p2_source-1.0-py3-none-any.whl'
    Path(wheel_directory,name).write_bytes(out.getvalue())
    return name
'''
    members = {"pyproject.toml": b'[build-system]\nrequires=["a-dep==1.0"]\nbuild-backend="backend"\nbackend-path=["."]\n',
               "backend.py": backend.encode()}
    # Both tar headers and gzip mtime are fixed so source/output identity is
    # reproducible across entirely separate data and cache roots.
    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for name, body in members.items():
            item = tarfile.TarInfo("p2_source-1.0/" + name)
            item.mode, item.size, item.mtime = 0o644, len(body), 0
            archive.addfile(item, io.BytesIO(body))
    import gzip
    (root / "p2_source-1.0.tar.gz").write_bytes(gzip.compress(payload.getvalue(), mtime=0))


if "TestHandler" in globals():
    runtime = load_handler(HERE.parent / "cli/p1_exact_python_fixture.py", sys.argv[1])
    fixture_root = Path(sys.argv[2])
    wheel_handler = load_handler(HERE / "f3_shared_wheel_fixture.py", fixture_root)

    class Handler(runtime):
        def do_GET(self):
            path = urlsplit(self.path).path
            if path == "/simple/p2-source/" or (path == "/simple/a-dep/" and (fixture_root / "new-candidates").exists()):
                self.log_headers()
                name = path.split("/")[2]
                names = ["p2_source-1.0.tar.gz"] if name == "p2-source" else ["a_dep-1.0-py3-none-any.whl", "a_dep-1.1-py3-none-any.whl"]
                files = [{"filename": filename, "url": "/files/" + filename,
                          "hashes": {"sha256": hashlib.sha256((fixture_root / filename).read_bytes()).hexdigest()},
                          "requires-python": ">=3.13"} for filename in names]
                body = json.dumps({"meta": {"api-version": "1.0"}, "name": name, "files": files}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/vnd.pypi.simple.v1+json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            elif path.startswith(("/simple/", "/files/")):
                wheel_handler.do_GET(self)
            else:
                super().do_GET()
elif __name__ == "__main__":
    prepare(Path(sys.argv[1]))
