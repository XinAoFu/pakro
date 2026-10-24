"""Fixed PEP 517 transport. Only the backend and explicit fixed inputs vary."""
import importlib
import json
import os
import pathlib
import resource
import site
import sys


def main(request, result):
    if request["protocol"] != 1:
        raise RuntimeError("unsupported protocol")
    maximum = request["max_file_bytes"]
    resource.setrlimit(resource.RLIMIT_FSIZE, (maximum, maximum))
    for directory in request["dependency_sites"]:
        site.addsitedir(directory)
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
        value = hook(config_settings=None)
        if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
            raise TypeError("build requirements must be a list of strings")
    elif request["hook"] == "prepare_metadata_for_build_wheel":
        value = hook(request["directory"], config_settings=None)
    else:
        value = hook(request["directory"], config_settings=None,
                     metadata_directory=request["metadata_directory"])
    return {"ok": True, "missing": False, "value": value}


if __name__ == "__main__":
    request_path, result_path = sys.argv[1:]
    try:
        with open(request_path, "r", encoding="utf-8") as stream:
            request = json.load(stream)
        response = main(request, result_path)
        status = 0
    except BaseException as error:
        response = {"ok": False, "missing": False, "value": None,
                    "error": type(error).__name__}
        status = 1
    with open(result_path, "x", encoding="utf-8") as stream:
        json.dump(response, stream, ensure_ascii=True)
    raise SystemExit(status)
