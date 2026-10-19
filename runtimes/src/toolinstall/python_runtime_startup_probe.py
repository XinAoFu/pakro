# Fixed site observer. The helper definitions above are repository-owned code;
# request source/cache bytes must be bound by the Rust admission boundary.
import sys
sys.path[:] = [path for path in sys.path if path]
import base64
import hashlib
import importlib.machinery
import importlib.util
import json
import os
import stat
import types


def _observe_startup():
    raw = sys.stdin.buffer.read(32 * 1024 * 1024 + 1)
    if len(raw) > 32 * 1024 * 1024:
        raise SiteCodeError("startup input limit")
    request = json.loads(raw.decode("utf-8"))
    if request["schema"] != 1 or request["mode"] not in ("cfg_recognition_intercepted", "normal_empty_environment"):
        raise SiteCodeError("unknown startup observation mode")
    normal = request["mode"] == "normal_empty_environment"
    if bool(sys.flags.no_site) == normal or not sys.flags.ignore_environment or not sys.dont_write_bytecode:
        raise SiteCodeError("startup flags disagree")
    if not normal and "site" in sys.modules:
        raise SiteCodeError("site executed before code admission")
    validate_startup_finders([])
    module = sys.modules.get("site") if normal else None
    if normal and type(module) is not types.ModuleType:
        raise SiteCodeError("normal site module is missing")
    spec = module.__spec__ if normal else importlib.util.find_spec("site")
    if spec is None:
        raise SiteCodeError("site origin is missing")
    source_request = request["source"]
    path = source_request["path"]
    limits = request["code_limits"]
    if len(source_request["bytes"]) > 4 * ((limits["max_source_bytes"] + 2) // 3):
        raise SiteCodeError("source encoding exceeds budget")
    source = base64.b64decode(source_request["bytes"], validate=True)
    if len(source) != source_request["size"] or hashlib.sha256(source).hexdigest() != source_request["sha256"]:
        raise SiteCodeError("startup source digest disagrees")
    validate_site_top_level(source, path, limits)
    cache_request = request["cache"]
    cache = None
    cache_path = None
    cache_sha = None
    if request["loader"] == "source":
        if type(spec.loader) is not importlib.machinery.SourceFileLoader or os.path.realpath(spec.origin) != os.path.realpath(path):
            raise SiteCodeError("startup loader or source origin disagrees")
        cache_path = importlib.util.cache_from_source(spec.origin)
        if cache_request is None or os.path.realpath(cache_path) != os.path.realpath(cache_request["path"]):
            raise SiteCodeError("startup cache origin disagrees")
        if cache_request["state"] == "pinned":
            if len(cache_request["bytes"]) > 4 * ((limits["max_cache_bytes"] + 2) // 3):
                raise SiteCodeError("cache encoding exceeds budget")
            cache = base64.b64decode(cache_request["bytes"], validate=True)
            cache_sha = hashlib.sha256(cache).hexdigest()
            if len(cache) != cache_request["size"] or cache_sha != cache_request["sha256"]:
                raise SiteCodeError("startup cache digest disagrees")
        elif cache_request["state"] != "missing":
            raise SiteCodeError("unknown startup cache state")
        admitted = load_attested_source(source, cache, path, source_request["mtime"], limits)
    elif request["loader"] == "frozen":
        if spec.loader is not importlib.machinery.FrozenImporter or spec.origin != "frozen" or cache_request is not None:
            raise SiteCodeError("startup frozen origin disagrees")
        expected = compile(source, path, "exec", dont_inherit=True, optimize=sys.flags.optimize)
        admitted = attest_code(expected, spec.loader.get_code("site"), loader_kind="frozen", limits={key: limits[key] for key in _DEFAULT_LIMITS})
    else:
        raise SiteCodeError("unknown startup loader")
    final = request["final_prefix"]
    cfg_path = os.path.join(final, "pyvenv.cfg")
    cfg_fd = os.open(cfg_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(cfg_fd)
        if not stat.S_ISREG(info.st_mode) or info.st_size > 64 * 1024:
            raise SiteCodeError("startup cfg type or size disagrees")
        with os.fdopen(cfg_fd, "rb", closefd=False) as stream:
            cfg = stream.read(64 * 1024 + 1)
    finally:
        os.close(cfg_fd)
    if hashlib.sha256(cfg).hexdigest() != request["cfg_sha256"]:
        raise SiteCodeError("startup cfg digest disagrees")
    if not normal:
        module = types.ModuleType("site")
        module.__file__ = spec.origin if spec.origin != "frozen" else path
        module.__loader__ = spec.loader
        module.__spec__ = spec
        module.__package__ = ""
        sys.modules["site"] = module
        admitted.execute(module.__dict__)
    required = ("addsitepackages", "addsitedir", "addusersitepackages", "execsitecustomize", "execusercustomize", "enablerlcompleter")
    if any(not callable(getattr(module, name, None)) for name in required) or not callable(getattr(module, "main", None)):
        raise SiteCodeError("unknown site hook adapter")
    requested = []

    def record(path):
        path = os.path.abspath(path)
        if os.path.commonpath((final, path)) != final or path == final:
            raise SiteCodeError("outside site request")
        if path not in requested:
            if len(requested) >= request["max_paths"] or len(path.encode("utf-8")) > limits["max_path_bytes"]:
                raise SiteCodeError("site request limit")
            requested.append(path)

    def addsites(known, prefixes=None):
        for directory in module.getsitepackages(prefixes):
            record(directory)
        return known

    def adddir(directory, known_paths=None):
        record(directory)
        return known_paths if known_paths is not None else set()

    def adduser(known):
        if module.ENABLE_USER_SITE:
            raise SiteCodeError("user site request")
        return known

    if normal:
        for directory in module.getsitepackages([sys.prefix]):
            record(directory)
    else:
        module.addsitepackages = addsites
        module.addsitedir = adddir
        module.addusersitepackages = adduser
        module.execsitecustomize = lambda: None
        module.execusercustomize = lambda: None
        module.enablerlcompleter = lambda: None
        if hasattr(module, "addpackage"):
            module.addpackage = lambda *args, **kwargs: (_ for _ in ()).throw(SiteCodeError("direct pth entry is forbidden"))
        module.main()
    ensure_no_customization(list(sys.path) + requested, request["max_paths"], limits["max_path_bytes"])
    import sysconfig
    names = sysconfig.get_scheme_names()
    if "posix_prefix" not in names:
        raise SiteCodeError("explicit final scheme unavailable")
    projected = sysconfig.get_paths(scheme="posix_prefix", vars={"base": final, "platbase": final})
    headers = sysconfig.get_paths(scheme="posix_prefix", vars={"base": final, "platbase": final, "installed_base": final, "installed_platbase": final})["include"]
    target = {key: projected[key] for key in ("purelib", "platlib", "scripts", "data")}
    target["headers"] = headers
    output = {
        "schema": 1, "observation_mode": request["mode"],
        "executable": os.path.realpath(sys.executable), "prefix": sys.prefix,
        "exec_prefix": sys.exec_prefix, "base_prefix": sys.base_prefix,
        "base_exec_prefix": sys.base_exec_prefix, "loader": request["loader"],
        "site_origin": spec.origin, "cache_path": cache_path,
        "source_sha256": source_request["sha256"], "cache_sha256": cache_sha,
        "code_selection": "frozen" if request["loader"] == "frozen" else admitted.selection(),
        "cfg_path": cfg_path, "home": getattr(sys, "_home", None),
        "user_site_enabled": module.ENABLE_USER_SITE, "no_site": bool(sys.flags.no_site),
        "ignore_environment": bool(sys.flags.ignore_environment), "dont_write_bytecode": bool(sys.dont_write_bytecode),
        "requested_sites": requested, "suppressed_hooks": [] if normal else list(required),
        "sys_path": list(sys.path), "default_paths": sysconfig.get_paths(), "final_paths": target,
    }
    encoded = json.dumps(output, ensure_ascii=True, separators=(",", ":"))
    if len(encoded) > 1024 * 1024:
        raise SiteCodeError("startup output limit")
    sys.stdout.write(encoded)


_observe_startup()
