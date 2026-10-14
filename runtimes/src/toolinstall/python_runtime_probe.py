import sys

# -c leaves an empty cwd entry; remove it before importing other modules.
sys.path[:] = [entry for entry in sys.path if entry]
import json
import os
import platform

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
gil = sysconfig.get_config_var("Py_GIL_DISABLED")
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
    "gil_disabled": None if gil is None else str(gil).lower() in ("1", "true"),
    "sysconfig_paths": dict((key, str(value)) for key, value in paths.items()
                           if value is not None),
}
print(json.dumps(result, ensure_ascii=True, sort_keys=True))
