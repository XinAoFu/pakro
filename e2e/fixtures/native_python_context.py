#!/usr/bin/env python3
import base64
import hashlib
import io
import json
import platform
import shlex
import sys
import zipfile
from pathlib import Path


def wheel_bytes(name):
    buffer = io.BytesIO()
    files = {
        f"{name}/__init__.py": b"VALUE = 42\n",
        f"{name}-1.0.dist-info/METADATA": f"Metadata-Version: 2.1\nName: {name}\nVersion: 1.0\n".encode(),
        f"{name}-1.0.dist-info/WHEEL": b"Wheel-Version: 1.0\nGenerator: fixture\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    }
    records = []
    for filename, body in files.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(body).digest()).rstrip(b"=").decode()
        records.append(f"{filename},sha256={digest},{len(body)}")
    record_name = f"{name}-1.0.dist-info/RECORD"
    files[record_name] = ("\n".join(records + [f"{record_name},,"]) + "\n").encode()
    with zipfile.ZipFile(buffer, "w") as archive:
        for filename, body in files.items():
            archive.writestr(filename, body)
    return buffer.getvalue()


def prepare(install_root, fixture_root, micro, implementation):
    install_root = Path(install_root)
    fixture_root = Path(fixture_root)
    executable = install_root / "bin/python"
    executable.parent.mkdir(parents=True, exist_ok=True)
    fixture_root.mkdir(parents=True, exist_ok=True)
    executable.touch()
    executable = executable.resolve()
    language = {"major": 3, "minor": 10, "micro": micro, "release_level": "final", "serial": 0}
    pypy = implementation == "pypy"
    full = f"3.10.{micro}"
    descriptor = {
        "schema": 1, "observation_mode": "base_runtime_site_disabled",
        "probe_flags": ["-E", "-s", "-S", "-B", "-c"], "executable": str(executable),
        "implementation": implementation, "language_version": language,
        "implementation_version": {"major": 7, "minor": 3, "micro": 17, "release_level": "final", "serial": 0} if pypy else language,
        "implementation_version_source": "pypy_version_info" if pypy else "sys_version_info",
        "markers": {
            "implementation_name": implementation, "implementation_version": "7.3.17" if pypy else full,
            "os_name": "posix", "platform_machine": platform.machine(),
            "platform_python_implementation": "PyPy" if pypy else "CPython",
            "platform_release": platform.release(), "platform_system": platform.system(),
            "platform_version": platform.version(), "python_full_version": full,
            "python_version": "3.10", "sys_platform": sys.platform,
        },
        "soabi": "pypy310-pp73" if pypy else "cpython-310", "ext_suffix": ".so",
        "abiflags": "", "extension_suffixes": [".so"], "gil_disabled": False,
        "sysconfig_paths": {"stdlib": str(install_root / "lib/python3.10")},
    }
    command = "#!/bin/sh\nif [ \"$#\" -eq 6 ] && [ \"${1:-}\" = '-E' ] && [ \"${2:-}\" = '-s' ] && [ \"${3:-}\" = '-S' ] && [ \"${4:-}\" = '-B' ] && [ \"${5:-}\" = '-c' ]; then\n  printf '%s' " + shlex.quote(json.dumps(descriptor)) + "\nelse\n  printf '%s\\n' 'unexpected runtime execution' >&2\n  exit 97\nfi\n"
    executable.write_text(command)
    executable.chmod(0o700)
    for name in ("demo", "patch13", "patch14"):
        (fixture_root / f"{name}-1.0-py3-none-any.whl").write_bytes(wheel_bytes(name))


if "TestHandler" in globals():
    fixture_root = Path(sys.argv[1])

    class Handler(TestHandler):
        def do_GET(self):
            self.log_headers()
            name = self.path.split("/")[1]
            if self.path.endswith("/json") and name in ("demo", "patch13", "patch14"):
                filename = f"{name}-1.0-py3-none-any.whl"
                if self.path == f"/{name}/json":
                    value = {"info": {"name": name, "version": "1.0"},
                             "releases": {"1.0": [{"filename": filename, "requires_python": ">=3.0"}]}}
                else:
                    wheel = (fixture_root / filename).read_bytes()
                    deps = ["patch13==1.0; python_full_version < '3.10.14'",
                            "patch14==1.0; python_full_version >= '3.10.14'"] if name == "demo" else []
                    value = {"info": {"name": name, "version": "1.0", "requires_python": ">=3.0", "requires_dist": deps},
                             "urls": [{"filename": filename, "url": f"http://{self.headers['Host']}/{filename}",
                                       "digests": {"sha256": hashlib.sha256(wheel).hexdigest()},
                                       "requires_python": ">=3.0", "packagetype": "bdist_wheel"}]}
                body = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
            elif self.path.endswith(".whl") and (fixture_root / self.path.lstrip("/")).is_file():
                body = (fixture_root / self.path.lstrip("/")).read_bytes()
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
            else:
                self.send_error(404)
                return
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
elif __name__ == "__main__":
    prepare(sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4])
