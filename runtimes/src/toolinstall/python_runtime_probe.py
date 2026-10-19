import sys

# -c leaves an empty cwd entry; remove it before importing other modules.
sys.path[:] = [entry for entry in sys.path if entry]
import json
import os
import platform
import struct

if sys.version_info[:2] >= (3, 2):
    import sysconfig
else:
    from distutils import sysconfig

if sys.version_info[:2] >= (3, 3):
    from importlib.machinery import EXTENSION_SUFFIXES
else:
    import imp
    EXTENSION_SUFFIXES = [suffix for suffix, mode, kind in imp.get_suffixes()
                          if kind == imp.C_EXTENSION]


def version_data(value):
    return {"major": value[0], "minor": value[1], "micro": value[2],
            "release_level": value[3], "serial": value[4]}


def full_version(value):
    result = "%s.%s.%s" % (value[0], value[1], value[2])
    if value[3] != "final":
        result += {"alpha": "a", "beta": "b", "candidate": "rc"}[value[3]]
        result += str(value[4])
    return result


MAX_HEADER_BYTES = 1024 * 1024
MAX_INTERPRETER_BYTES = 4096
MAX_POLICY_ROWS = 2048
MAX_LIBC_MINOR = 1024


def bool_config(key):
    value = sysconfig.get_config_var(key)
    if value is None:
        return None
    if type(value) not in (bool, int, str):
        return None
    if value is True or value == 1 or value == "1":
        return True
    if value is False or value == 0 or value == "0":
        return False
    return None


def unicode_width():
    if sys.version_info[:2] >= (3, 3):
        return None
    value = sysconfig.get_config_var("Py_UNICODE_SIZE")
    if value in (2, 4, "2", "4"):
        return int(value)
    maximum = getattr(sys, "maxunicode", None)
    if maximum == 65535:
        return 2
    if maximum == 1114111:
        return 4
    return None


def numeric_version(text):
    parts = text.split(".")
    if len(parts) not in (2, 3):
        return None
    if any(not part or not all("0" <= char <= "9" for char in part) for part in parts):
        return None
    if any(len(part) > 4 for part in parts):
        return None
    values = [int(part) for part in parts]
    if any(value > 1024 for value in values):
        return None
    return values


def mac_product_version():
    try:
        import plistlib
        with open("/System/Library/CoreServices/SystemVersion.plist", "rb") as stream:
            data = stream.read(MAX_HEADER_BYTES + 1)
        if len(data) > MAX_HEADER_BYTES:
            return None
        if hasattr(plistlib, "loads"):
            product = plistlib.loads(data)
        else:
            product = plistlib.readPlistFromBytes(data)
        text = product.get("ProductVersion")
        if not isinstance(text, str):
            return None
        values = numeric_version(text)
        if values is None or values[0] < 10:
            return None
        return {"major": values[0], "minor": values[1],
                "micro": values[2] if len(values) == 3 else 0}
    except Exception:
        return None


def macho_cpu_type():
    try:
        import ctypes
        library = ctypes.CDLL(None)
        header = library._dyld_get_image_header
        header.argtypes = [ctypes.c_uint32]
        header.restype = ctypes.c_void_p
        address = header(0)
        if not address:
            return None
        data = ctypes.string_at(address, 8)
        if data[:4] in (b"\xce\xfa\xed\xfe", b"\xcf\xfa\xed\xfe"):
            return struct.unpack("<i", data[4:8])[0]
        if data[:4] in (b"\xfe\xed\xfa\xce", b"\xfe\xed\xfa\xcf"):
            return struct.unpack(">i", data[4:8])[0]
        return None
    except Exception:
        return None


def read_elf(stream):
    data = stream.read(MAX_HEADER_BYTES + 1)
    if len(data) > MAX_HEADER_BYTES:
        # Only the first bounded header region is relevant; large executables
        # are normal. No segment outside that region is assumed readable.
        data = data[:MAX_HEADER_BYTES]
    if len(data) < 52 or data[:4] != b"\x7fELF" or data[6] != 1:
        return None
    elf_class = data[4]
    byte_order = data[5]
    if elf_class not in (1, 2) or byte_order not in (1, 2):
        return None
    order = "<" if byte_order == 1 else ">"
    if elf_class == 1:
        flags = struct.unpack(order + "I", data[36:40])[0]
        offset = struct.unpack(order + "I", data[28:32])[0]
        size, count = struct.unpack(order + "HH", data[42:46])
        expected_size = 32
    else:
        if len(data) < 64:
            return None
        flags = struct.unpack(order + "I", data[48:52])[0]
        offset = struct.unpack(order + "Q", data[32:40])[0]
        size, count = struct.unpack(order + "HH", data[54:58])
        expected_size = 56
    if count > 256 or (count and size != expected_size):
        return None
    if offset > len(data) or offset + size * count > len(data):
        return None
    interpreter = None
    for index in range(count):
        entry = offset + index * size
        kind = struct.unpack(order + "I", data[entry:entry + 4])[0]
        if kind != 3:
            continue
        if interpreter is not None:
            return None
        if elf_class == 1:
            location = struct.unpack(order + "I", data[entry + 4:entry + 8])[0]
            length = struct.unpack(order + "I", data[entry + 16:entry + 20])[0]
        else:
            location = struct.unpack(order + "Q", data[entry + 8:entry + 16])[0]
            length = struct.unpack(order + "Q", data[entry + 32:entry + 40])[0]
        if length < 2 or length > MAX_INTERPRETER_BYTES:
            return None
        if location > len(data) or location + length > len(data):
            return None
        raw = data[location:location + length]
        if raw[-1:] != b"\x00" or b"\x00" in raw[:-1]:
            return None
        try:
            interpreter = raw[:-1].decode("utf-8")
        except UnicodeError:
            return None
        if not os.path.isabs(interpreter):
            return None
    return {"class": elf_class, "little_endian": byte_order == 1,
            "machine": struct.unpack(order + "H", data[18:20])[0],
            "flags": flags, "interpreter": interpreter}


def linux_arches(elf, machine):
    if elf is None:
        return []
    value = (elf["class"], elf["machine"], elf["little_endian"])
    if value == (2, 62, True):
        return ["x86_64"]
    if value == (1, 3, True) and machine in ("x86_64", "i686"):
        return ["i686"]
    if value == (2, 183, True):
        return ["aarch64"]
    if value == (1, 40, True):
        if elf["flags"] & 0xff000000 != 0x05000000 or not elf["flags"] & 0x400:
            return []
        if machine in ("armv8l", "aarch64"):
            return ["armv8l", "armv7l"]
        if machine == "armv7l":
            return ["armv7l"]
    names = {(2, 21, True): "ppc64le", (2, 21, False): "ppc64",
             (2, 22, False): "s390x", (2, 258, True): "loongarch64",
             (2, 243, True): "riscv64"}
    name = names.get(value)
    return [name] if name is not None else []


def libc_observation(elf):
    try:
        import ctypes
        library = ctypes.CDLL(None)
        function = library.gnu_get_libc_version
        function.argtypes = []
        function.restype = ctypes.c_char_p
        value = function()
        if value is not None:
            version = numeric_version(value.decode("ascii"))
            if version is not None and version[0] > 0:
                return {"family": "glibc", "major": version[0], "minor": version[1]}
    except Exception:
        pass
    if elf is not None:
        loader = elf["interpreter"]
        if loader is not None:
            basename = os.path.basename(loader)
            if basename.startswith("ld-musl-") and basename.endswith(".so.1"):
                return {"family": "musl", "version": None, "loader": loader}
    return {"family": "unknown"}


def manylinux_observation(libc, arches):
    if libc["family"] != "glibc" or libc["major"] != 2 or not arches:
        return {"state": "unknown"}
    if libc["minor"] > MAX_LIBC_MINOR:
        return {"state": "unknown"}
    try:
        if "_manylinux" not in sys.modules:
            if sys.version_info[:2] >= (3, 4):
                from importlib.util import find_spec
                if find_spec("_manylinux") is None:
                    return {"state": "default"}
            else:
                import imp
                try:
                    stream, filename, description = imp.find_module("_manylinux")
                except ImportError:
                    return {"state": "default"}
                if stream is not None:
                    stream.close()
        module = __import__("_manylinux")
    except Exception:
        return {"state": "unknown"}
    missing = object()
    try:
        callback = getattr(module, "manylinux_compatible", missing)
    except Exception:
        return {"state": "unknown"}
    if callback is not missing and not callable(callback):
        return {"state": "unknown"}
    rows = []
    legacy = {5: "manylinux1_compatible", 12: "manylinux2010_compatible",
              17: "manylinux2014_compatible"}
    try:
        for arch in arches:
            minimum = 5 if arch in ("x86_64", "i686") else 17
            for minor in range(libc["minor"], minimum - 1, -1):
                if len(rows) >= MAX_POLICY_ROWS:
                    return {"state": "unknown"}
                if callback is not missing:
                    compatible = callback(2, minor, arch)
                    if compatible is None:
                        compatible = True
                else:
                    key = legacy.get(minor)
                    compatible = getattr(module, key, missing) if key is not None else missing
                    if compatible is missing:
                        compatible = True
                if not isinstance(compatible, bool):
                    return {"state": "unknown"}
                rows.append({"arch": arch, "major": 2, "minor": minor,
                             "compatible": compatible})
    except Exception:
        return {"state": "unknown"}
    return {"state": "overrides", "values": rows}


def compatibility_observation(machine):
    bits = struct.calcsize("P") * 8
    debug = bool_config("Py_DEBUG")
    if debug is None and hasattr(sys, "gettotalrefcount"):
        debug = True
    if sys.platform == "darwin":
        observation = {"os": "macos", "cpu_type": macho_cpu_type(),
                       "product_version": mac_product_version()}
    else:
        try:
            with open("/proc/self/exe", "rb") as stream:
                elf = read_elf(stream)
        except (OSError, IOError):
            elf = None
        libc = libc_observation(elf)
        observation = {"os": "linux", "elf": elf, "libc": libc,
                       "manylinux": manylinux_observation(libc, linux_arches(elf, machine))}
    return {"pointer_bits": bits, "debug": debug,
            "pymalloc": bool_config("WITH_PYMALLOC"),
            "unicode_size": unicode_width(), "platform": observation}

display = platform.python_implementation()
name = display.lower()
implementation = getattr(sys, "implementation", None)
if name == "pypy":
    implementation_version = getattr(sys, "pypy_version_info", None)
    if implementation_version is None:
        raise RuntimeError("PyPy has no pypy_version_info")
    implementation_source = "pypy_version_info"
elif name == "cpython":
    implementation_version = getattr(implementation, "version", None)
    if implementation_version is None:
        implementation_version = sys.version_info
        implementation_source = "sys_version_info"
    else:
        implementation_source = "sys_implementation_version"
else:
    raise RuntimeError("unsupported Python implementation: " + display)

if hasattr(sysconfig, "get_paths"):
    paths = sysconfig.get_paths()
else:
    paths = {
        "stdlib": sysconfig.get_python_lib(standard_lib=True),
        "platstdlib": sysconfig.get_python_lib(plat_specific=True, standard_lib=True),
        "purelib": sysconfig.get_python_lib(),
        "platlib": sysconfig.get_python_lib(plat_specific=True),
        "include": sysconfig.get_python_inc(),
        "platinclude": sysconfig.get_python_inc(plat_specific=True),
        "scripts": os.path.join(sys.prefix, "bin"),
        "data": sys.prefix,
    }
gil = bool_config("Py_GIL_DISABLED")
if gil is None and name == "cpython" and sys.version_info[:2] < (3, 13):
    gil = False
system, nodename, release, platform_version, machine = os.uname()
markers = {
    "implementation_name": name,
    "implementation_version": full_version(implementation_version),
    "os_name": os.name,
    "platform_machine": machine,
    "platform_python_implementation": display,
    "platform_release": release,
    "platform_system": system,
    "platform_version": platform_version,
    "python_full_version": full_version(sys.version_info),
    "python_version": "%s.%s" % (sys.version_info[0], sys.version_info[1]),
    "sys_platform": sys.platform,
}
result = {
    "schema": 1,
    "observation_mode": "base_runtime_site_disabled",
    "probe_flags": ["-E", "-s", "-S", "-B", "-c"],
    "executable": os.path.realpath(sys.executable),
    "implementation": name,
    "language_version": version_data(sys.version_info),
    "implementation_version": version_data(implementation_version),
    "implementation_version_source": implementation_source,
    "markers": markers,
    "soabi": sysconfig.get_config_var("SOABI"),
    "ext_suffix": sysconfig.get_config_var("EXT_SUFFIX"),
    "abiflags": getattr(sys, "abiflags", None),
    "extension_suffixes": list(EXTENSION_SUFFIXES),
    "gil_disabled": gil,
    "compatibility": compatibility_observation(machine),
    "sysconfig_paths": dict((key, str(value)) for key, value in paths.items()
                           if value is not None),
}
print(json.dumps(result, ensure_ascii=True, sort_keys=True))
