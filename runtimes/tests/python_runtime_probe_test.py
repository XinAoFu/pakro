"""Stdlib-only behavioral fixtures for the fixed, isolated runtime probe."""

import builtins
import ctypes
import io
from pathlib import Path
import struct
import sys
import types
import unittest
from unittest import mock


SCRIPT = Path(__file__).parents[1] / "src/toolinstall/python_runtime_probe.py"
HELPERS = SCRIPT.read_text().split("display = platform.python_implementation()", 1)[0]


def elf_fixture(bits=64, little=True, machine=183, loader="/lib/ld-musl-aarch64.so.1"):
    data = bytearray(512)
    data[:4] = b"\x7fELF"
    data[4:7] = bytes((1 if bits == 32 else 2, 1 if little else 2, 1))
    order = "<" if little else ">"
    struct.pack_into(order + "H", data, 18, machine)
    if bits == 32:
        struct.pack_into(order + "I", data, 28, 64)
        struct.pack_into(order + "HH", data, 42, 32, 1)
        struct.pack_into(order + "I", data, 64, 3)
        struct.pack_into(order + "I", data, 68, 256)
        struct.pack_into(order + "I", data, 80, len(loader) + 1)
    else:
        struct.pack_into(order + "Q", data, 32, 64)
        struct.pack_into(order + "HH", data, 54, 56, 1)
        struct.pack_into(order + "I", data, 64, 3)
        struct.pack_into(order + "Q", data, 72, 256)
        struct.pack_into(order + "Q", data, 96, len(loader) + 1)
    data[256:256 + len(loader)] = loader.encode("ascii")
    return bytes(data)


class ProbeTests(unittest.TestCase):
    def setUp(self):
        self.namespace = {}
        exec(compile(HELPERS, str(SCRIPT), "exec"), self.namespace)

    def call(self, name, *args):
        return self.namespace[name](*args)

    def test_bool_config_keeps_unknown_values(self):
        config = {"false": 0, "true": "1", "invalid": "unexpected"}
        with mock.patch.object(self.namespace["sysconfig"], "get_config_var", config.get):
            self.assertEqual([self.call("bool_config", key) for key in config],
                             [False, True, None])

    def test_legacy_unicode_uses_observed_config(self):
        with mock.patch.object(sys, "version_info", (3, 2, 6, "final", 0)):
            with mock.patch.object(self.namespace["sysconfig"], "get_config_var", lambda key: 4):
                self.assertEqual(self.call("unicode_width"), 4)

    def test_numeric_version_is_bounded(self):
        self.assertEqual(self.call("numeric_version", "14.5.0"), [14, 5, 0])
        self.assertEqual(self.call("numeric_version", "26.0"), [26, 0])
        for value in ("14.x", "999999999.2", "14.5.-1", "2", "2.1.3.4"):
            self.assertIsNone(self.call("numeric_version", value))

    def test_elf_reads_real_class_and_endianness(self):
        for bits, little, machine in ((32, True, 3), (64, True, 183), (64, False, 21)):
            self.assertEqual(self.call("read_elf", io.BytesIO(elf_fixture(bits, little, machine))),
                             {"class": 1 if bits == 32 else 2, "little_endian": little,
                              "machine": machine, "flags": 0,
                              "interpreter": "/lib/ld-musl-aarch64.so.1"})

    def test_elf_rejects_headers_and_interpreters_outside_bounds(self):
        for source in (b"not ELF", elf_fixture()[:51]):
            self.assertIsNone(self.call("read_elf", io.BytesIO(source)))
        for field, value in ((32, 2**64 - 1), (96, 4097), (72, 2**64 - 1)):
            data = bytearray(elf_fixture())
            struct.pack_into("<Q", data, field, value)
            self.assertIsNone(self.call("read_elf", io.BytesIO(data)))

    def test_arches_use_elf_width_instead_of_host_machine(self):
        elf = {"class": 1, "little_endian": True, "machine": 3, "flags": 0}
        self.assertEqual(self.call("linux_arches", elf, "x86_64"), ["i686"])
        arm = {"class": 1, "little_endian": True, "machine": 40, "flags": 0x05000400}
        self.assertEqual(self.call("linux_arches", arm, "aarch64"), ["armv8l", "armv7l"])
        arm["flags"] = 0x05000200
        self.assertEqual(self.call("linux_arches", arm, "aarch64"), [])

    def test_manylinux_callback_records_every_floor(self):
        module = types.ModuleType("_manylinux")
        module.manylinux_compatible = lambda major, minor, arch: False if minor == 17 else None
        with mock.patch.dict(sys.modules, {"_manylinux": module}):
            self.assertEqual(self.call("manylinux_observation", {"family": "glibc", "major": 2, "minor": 18}, ["aarch64"]),
                             {"state": "overrides", "values": [
                                 {"arch": "aarch64", "major": 2, "minor": 18, "compatible": True},
                                 {"arch": "aarch64", "major": 2, "minor": 17, "compatible": False}]})

    def test_manylinux_legacy_flags_keep_protocol_meaning(self):
        module = types.ModuleType("_manylinux")
        module.manylinux2014_compatible = False
        with mock.patch.dict(sys.modules, {"_manylinux": module}):
            observed = self.call("manylinux_observation", {"family": "glibc", "major": 2, "minor": 18}, ["aarch64"])
        self.assertEqual(observed, {"state": "overrides", "values": [
            {"arch": "aarch64", "major": 2, "minor": 18, "compatible": True},
            {"arch": "aarch64", "major": 2, "minor": 17, "compatible": False}]})

    def test_manylinux_failure_is_unknown(self):
        module = types.ModuleType("_manylinux")
        def broken(*args):
            raise RuntimeError("fixture callback broken")
        module.manylinux_compatible = broken
        with mock.patch.dict(sys.modules, {"_manylinux": module}):
            self.assertEqual(self.call("manylinux_observation", {"family": "glibc", "major": 2, "minor": 18}, ["aarch64"]), {"state": "unknown"})
            module.manylinux_compatible = lambda *args: "false"
            self.assertEqual(self.call("manylinux_observation", {"family": "glibc", "major": 2, "minor": 18}, ["aarch64"]), {"state": "unknown"})

    def test_manylinux_attribute_failure_is_unknown(self):
        module = types.ModuleType("_manylinux")
        def broken_attribute(name):
            raise RuntimeError("fixture module attribute broken")
        module.__getattr__ = broken_attribute
        with mock.patch.dict(sys.modules, {"_manylinux": module}):
            try:
                result = self.call("manylinux_observation", {"family": "glibc", "major": 2, "minor": 18}, ["aarch64"])
            except RuntimeError:
                self.fail("module attribute failures must retain unknown capability")
            self.assertEqual(result, {"state": "unknown"})

    def test_manylinux_present_invalid_hook_does_not_mean_absence(self):
        libc = {"family": "glibc", "major": 2, "minor": 18}
        module = types.ModuleType("_manylinux")
        module.manylinux_compatible = None
        with mock.patch.dict(sys.modules, {"_manylinux": module}):
            self.assertEqual(self.call("manylinux_observation", libc, ["aarch64"]), {"state": "unknown"})
            del module.manylinux_compatible
            module.manylinux2014_compatible = None
            self.assertEqual(self.call("manylinux_observation", libc, ["aarch64"]), {"state": "unknown"})

    def test_manylinux_absence_differs_from_broken_dependency(self):
        import importlib.util
        libc = {"family": "glibc", "major": 2, "minor": 18}
        with mock.patch.dict(sys.modules):
            sys.modules.pop("_manylinux", None)
            with mock.patch.object(importlib.util, "find_spec", lambda name: None):
                self.assertEqual(self.call("manylinux_observation", libc, ["aarch64"]), {"state": "default"})
            original_import = builtins.__import__
            def broken_import(name, *args, **kwargs):
                if name == "_manylinux":
                    raise ImportError("fixture dependency missing")
                return original_import(name, *args, **kwargs)
            with mock.patch.object(importlib.util, "find_spec", lambda name: object()):
                with mock.patch.object(builtins, "__import__", broken_import):
                    self.assertEqual(self.call("manylinux_observation", libc, ["aarch64"]), {"state": "unknown"})

    def test_macos_version_reads_bounded_product_plist(self):
        product = b'<?xml version="1.0"?><plist version="1.0"><dict><key>ProductVersion</key><string>26.0</string></dict></plist>'
        with mock.patch.object(builtins, "open", lambda *args: io.BytesIO(product)):
            self.assertEqual(self.call("mac_product_version"), {"major": 26, "minor": 0, "micro": 0})
        with mock.patch.object(builtins, "open", lambda *args: io.BytesIO(b"x" * (1024 * 1024 + 1))):
            self.assertIsNone(self.call("mac_product_version"))

    def test_macho_reads_loaded_image_cpu(self):
        class Function:
            def __call__(self, *args):
                return 1
        library = types.SimpleNamespace(_dyld_get_image_header=Function())
        for cpu in (16777223, 16777228):
            with mock.patch.object(ctypes, "CDLL", lambda value: library):
                with mock.patch.object(ctypes, "string_at", lambda *args: b"\xcf\xfa\xed\xfe" + struct.pack("<i", cpu)):
                    self.assertEqual(self.call("macho_cpu_type"), cpu)

    def test_libc_uses_current_process_symbol_and_elf_loader(self):
        class Function:
            def __call__(self, *args):
                return b"2.31"
        library = types.SimpleNamespace(gnu_get_libc_version=Function())
        with mock.patch.object(ctypes, "CDLL", lambda value: library):
            self.assertEqual(self.call("libc_observation", None), {"family": "glibc", "major": 2, "minor": 31})
            del library.gnu_get_libc_version
            self.assertEqual(self.call("libc_observation", {"interpreter": "/lib/ld-musl-aarch64.so.1"}),
                             {"family": "musl", "version": None, "loader": "/lib/ld-musl-aarch64.so.1"})
            self.assertEqual(self.call("libc_observation", {"interpreter": "/lib/unknown.so"}), {"family": "unknown"})


if __name__ == "__main__":
    unittest.main()
