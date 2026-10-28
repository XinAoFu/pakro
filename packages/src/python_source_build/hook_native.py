"""PEP 517 recipe 2: fixed inputs, isolated console scripts and native transport."""
import importlib
import json
import os
import pathlib
import resource
import runpy
import shutil
import site
import subprocess
import sys
import types


def prepare(request):
    if request["protocol"] != 2:
        raise RuntimeError("unsupported native protocol")
    resource.setrlimit(resource.RLIMIT_FSIZE, (request["max_file_bytes"],) * 2)
    for directory in request["dependency_sites"]:
        site.addsitedir(directory)
    transport = {key: value for key, value in os.environ.items()
                 if key.startswith("PAKRO_NATIVE_") or key in ("LD_PRELOAD", "DYLD_INSERT_LIBRARIES")}
    original = subprocess.Popen
    scripts = request["console_scripts"]

    class NativePopen(original):
        def __init__(self, args, *positional, **keywords):
            env = dict(os.environ if keywords.get("env") is None else keywords["env"])
            env.update(transport)
            gate = env.get("PAKRO_NATIVE_GATE_LIBRARY")
            if gate:
                env["DYLD_INSERT_LIBRARIES" if sys.platform == "darwin" else "LD_PRELOAD"] = gate
            keywords["env"] = env
            if not keywords.get("shell") and not keywords.get("executable"):
                words = list(args) if isinstance(args, (list, tuple)) else [args]
                if words and isinstance(words[0], (str, bytes, os.PathLike)):
                    path = shutil.which(words[0], path=env.get("PATH"))
                    resolved = str(pathlib.Path(os.fsdecode(path)).resolve()) if path else None
                    if resolved == str(pathlib.Path(sys.executable).resolve()):
                        # Keep interpreter options with the interpreter, then
                        # enter the same isolated, fixed-site driver for code.
                        offset = 1
                        safe_path = False
                        probe = False
                        valid_options = True
                        while offset < len(words):
                            option = os.fsdecode(words[offset])
                            if option in ("-c", "-m", "--", "-") or not option.startswith("-"):
                                break
                            if option.startswith(("-c", "-m")):
                                words[offset:offset + 1] = [option[:2], option[2:]]
                                break
                            if option in ("--help", "--help-env", "--help-xoptions", "--help-all", "--version"):
                                probe = True
                            if not option.startswith(("--", "-W", "-X")):
                                safe_path |= "I" in option[1:] or "P" in option[1:]
                                probe |= "h" in option[1:] or "V" in option[1:]
                            if option in ("-W", "-X", "--check-hash-based-pycs"):
                                if offset + 1 == len(words):
                                    valid_options = False
                                    break
                                offset += 2
                            else:
                                offset += 1
                        if not probe and valid_options:
                            execution = words[offset:] if offset < len(words) else ["--stdin-default"]
                            args = [sys.executable, *words[1:offset], "-I", "-S", "-B",
                                    request["native_driver"], "--python-safe" if safe_path else "--python", *execution]
                    elif resolved in scripts:
                        # Wheel console scripts point at the immutable build-env
                        # adapter. Run them with the held interpreter directly,
                        # retaining -I/-S and gate injection across macOS /bin/sh.
                        words = [sys.executable, "-I", "-S", "-B", request["native_driver"],
                                 "--console", resolved, *words[1:]]
                        args = words
            super().__init__(args, *positional, **keywords)

    subprocess.Popen = NativePopen


def main(request):
    sys.path[:0] = request["backend_paths"]
    module_name, separator, object_name = request["backend"].partition(":")
    module = importlib.import_module(module_name)
    if request["backend_paths"]:
        origin = getattr(module, "__file__", None)
        if not origin:
            raise ImportError("in-tree backend has no fixed origin")
        origin = pathlib.Path(origin).resolve()
        contained = False
        for root in request["backend_paths"]:
            try:
                origin.relative_to(pathlib.Path(root))
                contained = True
                break
            except ValueError:
                pass
        if not contained:
            raise ImportError("in-tree backend did not load from backend-path")
    backend = module
    if separator:
        for component in object_name.split("."):
            backend = getattr(backend, component)
    hook = getattr(backend, request["hook"], None)
    if hook is None:
        if request["hook"] == "build_wheel":
            raise AttributeError("required wheel hook absent")
        return {"ok": True, "missing": True, "value": None}
    if request["hook"] == "get_requires_for_build_wheel":
        value = hook(config_settings=request["config_settings"])
        if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
            raise TypeError("build requirements must be a list of strings")
    elif request["hook"] == "prepare_metadata_for_build_wheel":
        value = hook(request["directory"], config_settings=request["config_settings"])
    else:
        value = hook(request["directory"], config_settings=request["config_settings"],
                     metadata_directory=request["metadata_directory"])
    return {"ok": True, "missing": False, "value": value}


if __name__ == "__main__":
    if sys.argv[1:2] in (["--console"], ["--python"], ["--python-safe"]):
        mode = sys.argv[1]
        with open(os.environ["PAKRO_NATIVE_HOOK_REQUEST"], encoding="utf-8") as stream:
            request = json.load(stream)
        prepare(request)
        if mode == "--console":
            script = str(pathlib.Path(sys.argv[2]).resolve())
            if script not in request["console_scripts"]:
                raise RuntimeError("unregistered build console script")
            sys.argv = [script, *sys.argv[3:]]
            runpy.run_path(script, run_name="__main__")
        else:
            words = sys.argv[2:]
            default_stdin = words[:1] == ["--stdin-default"]
            if default_stdin:
                words = ["-"]
            elif words[:1] == ["--"]:
                words = words[1:]
                if not words:
                    default_stdin = True
                    words = ["-"]
            if mode == "--python" and words[:1] in (["-c"], ["-m"], ["-"]):
                sys.path.insert(0, os.getcwd() if words[0] == "-m" else "")
            if words[:1] in (["-c"], ["-"]):
                command = words[0] == "-c"
                code = words[1] if command else sys.stdin.read()
                sys.argv = ["-c", *words[2:]] if command else ([""] if default_stdin else words)
                module = types.ModuleType("__main__")
                sys.modules["__main__"] = module
                exec(compile(code, "<string>" if command else "<stdin>", "exec"), module.__dict__)
            elif words[:1] == ["-m"]:
                sys.argv = words[1:]
                runpy.run_module(words[1], run_name="__main__", alter_sys=True)
            else:
                script = str(pathlib.Path(words[0]).resolve())
                sys.argv = [script, *words[1:]]
                if mode == "--python":
                    sys.path.insert(0, str(pathlib.Path(script).parent))
                runpy.run_path(script, run_name="__main__")
    else:
        request_path, result_path = sys.argv[1:]
        try:
            with open(request_path, encoding="utf-8") as stream:
                request = json.load(stream)
            prepare(request)
            response = main(request)
            status = 0
        except BaseException as error:
            response = {"ok": False, "missing": False, "value": None,
                        "error": type(error).__name__}
            status = 1
        with open(result_path, "x", encoding="utf-8") as stream:
            json.dump(response, stream, ensure_ascii=True)
        raise SystemExit(status)
