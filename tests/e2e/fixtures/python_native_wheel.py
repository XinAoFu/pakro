#!/usr/bin/env python3
"""Build a real extension wheel fixture with the explicitly selected Python.

Only stdlib and the runtime's declared C compiler are used. The caller supplies
the independently expected wheel tag; this fixture is not a tag generator.
"""

import argparse
import base64
import csv
import hashlib
import io
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys
import sysconfig
import tempfile
import zipfile


SOURCE = r"""
#include <Python.h>

static PyObject *answer(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(42);
}
static PyMethodDef methods[] = {
    {"answer", answer, METH_NOARGS, "Return the independently expected answer."},
    {NULL, NULL, 0, NULL}
};
static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT, "_native", NULL, -1, methods
};
PyMODINIT_FUNC PyInit__native(void) {
    return PyModule_Create(&module);
}
"""


def build(destination, tag):
    if not re.fullmatch(r"[A-Za-z0-9_.]+-[A-Za-z0-9_.]+-[A-Za-z0-9_.]+", tag):
        raise ValueError("fixture tag must contain exactly three safe components")
    destination = Path(destination).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    suffix = sysconfig.get_config_var("EXT_SUFFIX")
    include = sysconfig.get_path("include")
    if not suffix or not include or not (Path(include) / "Python.h").is_file():
        raise RuntimeError("selected runtime has no observed extension suffix or C headers")
    shared = shlex.split(sysconfig.get_config_var("LDSHARED") or "")
    if not shared:
        raise RuntimeError("selected runtime declares no extension linker command")
    with tempfile.TemporaryDirectory(prefix="pakro-native-wheel-", dir=str(destination)) as scratch:
        scratch = Path(scratch)
        source = scratch / "native.c"
        extension = scratch / ("_native" + suffix)
        source.write_text(SOURCE, encoding="ascii")
        command = shared + ["-fPIC", "-I" + include, str(source), "-o", str(extension)]
        subprocess.run(command, check=True)
        # Prove this is a loadable extension before wrapping it in a ZIP. The
        # literal 42 is independent of any installer or wheel inspection code.
        check = subprocess.run(
            [sys.executable, "-E", "-s", "-S", "-c",
             "import sys;sys.path.insert(0,sys.argv[1]);import _native;assert _native.answer()==42",
             str(scratch)],
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        assert check.stdout == b"" and check.stderr == b""
        metadata = "pakro_native_demo-1.0.dist-info"
        data = "pakro_native_demo-1.0.data"
        files = {
            "pakro_native_demo/__init__.py": (
                b"from ._native import answer\n"
                b"def main():\n    import sys\n    print(answer())\n"
                b"    return 7 if sys.argv[1:] == ['exit-seven'] else 0\n"
            ),
            "pakro_native_demo/_native" + suffix: extension.read_bytes(),
            "pakro_native_demo/资源,说明.txt": "independent native fixture\n".encode(),
            metadata + "/METADATA": (
                b"Metadata-Version: 2.1\nName: pakro-native-demo\nVersion: 1.0\n"
                b"Requires-Python: >=3.0\n"
            ),
            metadata + "/WHEEL": (
                "Wheel-Version: 1.0\nGenerator: pakro-test-fixture\n"
                "Root-Is-Purelib: false\nTag: " + tag + "\n"
            ).encode("ascii"),
            metadata + "/entry_points.txt": (
                b"[console_scripts]\npakro-native-demo = pakro_native_demo:main\n"
            ),
            metadata + "/licenses/LICENSE": b"Test fixture authored for pakro.\n",
            data + "/purelib/pakro_native_pure.py": b"VALUE = 43\n",
            data + "/platlib/pakro_native_platform.py": b"VALUE = 44\n",
            data + "/headers/pakro_native_demo.h": b"#define PAKRO_NATIVE_FIXTURE 42\n",
            data + "/data/share/pakro-native-demo/data.txt": b"VALUE=45\n",
            data + "/scripts/pakro-archive-demo": (
                b"#!python\nfrom pakro_native_demo import main\nraise SystemExit(main())\n"
            ),
        }
        record_name = metadata + "/RECORD"
        record = io.StringIO(newline="")
        writer = csv.writer(record, lineterminator="\n")
        for name, body in sorted(files.items()):
            digest = base64.urlsafe_b64encode(hashlib.sha256(body).digest()).rstrip(b"=").decode()
            writer.writerow([name, "sha256=" + digest, str(len(body))])
        writer.writerow([record_name, "", ""])
        files[record_name] = record.getvalue().encode("utf-8")
        wheel = destination / ("pakro_native_demo-1.0-" + tag + ".whl")
        with zipfile.ZipFile(wheel, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name, body in sorted(files.items()):
                info = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = (0o100644 << 16)
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, body)
        result = {
            "filename": wheel.name,
            "sha256": hashlib.sha256(wheel.read_bytes()).hexdigest(),
            "interpreter": sys.executable,
            "language": sys.version.split()[0],
            "implementation": sys.implementation.name,
            "ext_suffix": suffix,
            "tag": tag,
            "compiler": command,
            "member_count": len(files),
        }
        (destination / "fixture-build.json").write_text(
            json.dumps(result, indent=2) + "\n", encoding="utf-8"
        )
        return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination")
    parser.add_argument("tag")
    options = parser.parse_args()
    print(json.dumps(build(options.destination, options.tag)))
