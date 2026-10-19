# PAKRO_RUNTIME_LAYOUT_V1: fixed, site-disabled selected-runtime observation.
import sys
sys.path[:] = [path for path in sys.path if path]
import errno
import json
import os
import stat

MAX_PATHS = 4096
MAX_PATH_BYTES = 16384
MAX_IMAGES = 512
MAX_MAP_BYTES = 1024 * 1024


def find_sysconfig():
    try:
        from importlib.util import find_spec
    except ImportError:
        import imp
        try:
            located = imp.find_module("sysconfig")
        except ImportError:
            located = None
        if located is not None and located[0] is not None:
            located[0].close()
        found = located is not None
    else:
        found = find_spec("sysconfig") is not None
    if found:
        import sysconfig
    else:
        from distutils import sysconfig
    return sysconfig


sysconfig = find_sysconfig()
final_prefix = sys.argv[1]
try:
    final_prefix.encode("ascii")
except UnicodeEncodeError:
    raise RuntimeError("invalid non-ASCII final prefix")
if not os.path.isabs(final_prefix):
    raise RuntimeError("invalid explicit final prefix")


def version(value):
    return {"major": value[0], "minor": value[1], "micro": value[2],
            "release_level": value[3], "serial": value[4]}


def config_bool(key):
    value = sysconfig.get_config_var(key)
    if value is True or value == 1 or value == "1":
        return True
    if value is False or value == 0 or value == "0":
        return False
    return None


def path_fact(path):
    try:
        value = os.stat(path)
    except (OSError, IOError) as error:
        if error.errno in (errno.ENOENT, errno.ENOTDIR):
            status = {"state": "missing"}
        else:
            status = {"state": "error", "kind": type(error).__name__}
    else:
        status = {"state": "present", "canonical": os.path.realpath(path),
                  "regular_file": stat.S_ISREG(value.st_mode),
                  "size": value.st_size, "mode": value.st_mode & 511}
    return {"advertised": path, "status": status}


def loader_images():
    try:
        if sys.platform == "darwin":
            import ctypes
            library = ctypes.CDLL(None)
            count = library._dyld_image_count
            count.argtypes = []
            count.restype = ctypes.c_uint32
            name = library._dyld_get_image_name
            name.argtypes = [ctypes.c_uint32]
            name.restype = ctypes.c_char_p
            total = count()
            if total > MAX_IMAGES:
                raise RuntimeError("loader image count exceeds limit")
            images = []
            for index in range(total):
                raw = name(index)
                if raw is None or len(raw) > MAX_PATH_BYTES:
                    raise RuntimeError("loader image path unavailable")
                path = raw.decode(sys.getfilesystemencoding())
                if not os.path.isabs(path):
                    raise RuntimeError("loader image is not absolute")
                images.append(os.path.realpath(path))
        elif sys.platform.startswith("linux"):
            import re
            with open("/proc/self/maps", "rb") as stream:
                raw = stream.read(MAX_MAP_BYTES + 1)
            if len(raw) > MAX_MAP_BYTES:
                raise RuntimeError("loader mapping exceeds limit")
            images = []
            for row in raw.splitlines():
                fields = row.split(None, 5)
                if len(fields) != 6 or b"x" not in fields[1] or not fields[5].startswith(b"/"):
                    continue
                if fields[5].endswith(b" (deleted)"):
                    raise RuntimeError("loaded image was deleted")
                name = re.sub(b"\\\\([0-7]{3})", lambda match: bytes(bytearray([int(match.group(1), 8)])), fields[5])
                if len(name) > MAX_PATH_BYTES:
                    raise RuntimeError("loader image path exceeds limit")
                images.append(os.path.realpath(name.decode(sys.getfilesystemencoding())))
                if len(images) > MAX_IMAGES:
                    raise RuntimeError("loader image count exceeds limit")
        else:
            return {"state": "unknown", "reason": "unsupported_loader_observation"}
        return {"state": "observed", "images": sorted(set(images))}
    except Exception:
        return {"state": "unknown", "reason": "loader_observation_unavailable"}


if hasattr(sysconfig, "get_paths"):
    path_api = "sysconfig"
    names = list(sysconfig.get_scheme_names())
    if len(names) > 64:
        raise RuntimeError("scheme observation exceeds limit")
    if "posix_prefix" in names:
        scheme = "posix_prefix"
    elif "venv" in names:
        scheme = "venv"
    else:
        raise RuntimeError("explicit prefix scheme is unavailable")
    base_paths = sysconfig.get_paths()
    projected = sysconfig.get_paths(scheme=scheme, vars={"base": final_prefix, "platbase": final_prefix})
    headers = sysconfig.get_paths(scheme=scheme, vars={"base": final_prefix, "platbase": final_prefix,
                                                    "installed_base": final_prefix, "installed_platbase": final_prefix})["include"]
    target_paths = dict((key, projected[key]) for key in ("purelib", "platlib", "scripts", "data"))
    target_paths["headers"] = headers
else:
    path_api = "distutils_sysconfig"
    names = []
    scheme = None
    from distutils.command.install import INSTALL_SCHEMES
    from distutils.util import subst_vars
    if "unix_prefix" not in INSTALL_SCHEMES:
        raise RuntimeError("legacy prefix scheme is unavailable")
    legacy = INSTALL_SCHEMES["unix_prefix"]

    def legacy_paths(prefix):
        variables = {"base": prefix, "platbase": prefix,
                     "py_version_short": sysconfig.get_python_version(), "dist_name": ""}
        return {"stdlib": sysconfig.get_python_lib(standard_lib=True, prefix=prefix),
                "platstdlib": sysconfig.get_python_lib(plat_specific=True, standard_lib=True, prefix=prefix),
                "purelib": sysconfig.get_python_lib(prefix=prefix),
                "platlib": sysconfig.get_python_lib(plat_specific=True, prefix=prefix),
                "include": sysconfig.get_python_inc(prefix=prefix),
                "platinclude": sysconfig.get_python_inc(plat_specific=True, prefix=prefix),
                "scripts": os.path.normpath(subst_vars(legacy["scripts"], variables)),
                "data": os.path.normpath(subst_vars(legacy["data"], variables))}
    base_paths = legacy_paths(sys.prefix)
    projected = legacy_paths(final_prefix)
    target_paths = dict((key, projected[key]) for key in ("purelib", "platlib", "scripts", "data"))
    target_paths["headers"] = projected["include"]

keys = ("BINDIR", "LIBDIR", "LIBPL", "LDLIBRARY", "LIBRARY", "INSTSONAME",
        "INCLUDEPY", "CONFINCLUDEPY", "SOABI", "EXT_SUFFIX", "ABIFLAGS", "VERSION", "LDVERSION",
        "platlibdir", "Py_ENABLE_SHARED", "Py_GIL_DISABLED", "PYTHONFRAMEWORK",
        "PYTHONFRAMEWORKPREFIX", "PYTHONFRAMEWORKDIR", "PYTHONFRAMEWORKINSTALLDIR",
        "LIBDEST", "BINLIBDEST", "MULTIARCH", "prefix", "exec_prefix", "base", "platbase")
config = dict((key, sysconfig.get_config_var(key)) for key in keys)
files = {}
for key, path_key in (("python_h", "include"), ("plat_python_h", "platinclude")):
    if path_key in base_paths:
        files[key] = path_fact(os.path.join(base_paths[path_key], "Python.h"))
for directory in ("LIBDIR", "LIBPL"):
    for filename in ("LDLIBRARY", "LIBRARY", "INSTSONAME"):
        if isinstance(config[directory], str) and isinstance(config[filename], str):
            files[directory.lower() + "_" + filename.lower()] = path_fact(os.path.join(config[directory], config[filename]))

if hasattr(sys, "pypy_version_info"):
    implementation = "pypy"
    implementation_version = sys.pypy_version_info
else:
    implementation = "cpython"
    implementation_version = getattr(getattr(sys, "implementation", None), "version", sys.version_info)
gil = config_bool("Py_GIL_DISABLED")
if gil is None and implementation == "cpython" and sys.version_info[:2] < (3, 13):
    gil = False
if len(sys.path) > MAX_PATHS:
    raise RuntimeError("module search paths exceed limit")

result = {"schema": 1, "observation_mode": "base_layout_site_disabled",
          "executable": os.path.realpath(sys.executable),
          "identity": {"implementation": implementation, "language_version": version(sys.version_info),
                       "implementation_version": version(implementation_version), "soabi": config["SOABI"],
                       "ext_suffix": config["EXT_SUFFIX"], "abiflags": getattr(sys, "abiflags", None),
                       "gil_disabled": gil},
          "prefix": sys.prefix, "exec_prefix": sys.exec_prefix,
          "base_prefix": getattr(sys, "base_prefix", None), "base_exec_prefix": getattr(sys, "base_exec_prefix", None),
          "sys_path": list(sys.path), "sysconfig_origin": os.path.realpath(sysconfig.__file__) if getattr(sysconfig, "__file__", None) else None,
          "path_api": path_api, "scheme_names": names, "selected_scheme": scheme,
          "base_paths": base_paths, "target_prefix": final_prefix, "target_paths": target_paths,
          "config_vars": config, "files": files, "loader": loader_images()}
print(json.dumps(result, ensure_ascii=True, sort_keys=True))
