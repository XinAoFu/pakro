"""Pre-execution site-code comparison; this alone is not a bound adapter.

The fixed caller must own and pin its source/cache/runtime inputs. This helper
does not discover a runtime, read its paths, import site, or authorize a normal
startup. It retains the same actual code object that passed comparison.
"""

import dis
import ast
import _imp
import importlib._bootstrap_external
import importlib.machinery
import importlib.util
import os
import struct
import sys
import types
import zipimport


_FIELDS = (
    "co_argcount", "co_posonlyargcount", "co_kwonlyargcount", "co_nlocals",
    "co_stacksize", "co_flags", "co_names", "co_varnames", "co_freevars",
    "co_cellvars", "co_name", "co_qualname", "co_firstlineno", "co_code",
    "co_linetable", "co_lnotab", "co_exceptiontable",
)
_DERIVED = frozenset(("co_lines", "co_positions"))
_DEFAULT_LIMITS = {
    "max_nodes": 65536,
    "max_depth": 64,
    "max_code_bytes": 8 * 1024 * 1024,
    "max_literal_bytes": 8 * 1024 * 1024,
}
_FROZEN_MEMBERSHIP = {
    (5, 2): ("abs_paths", frozenset(("_frozen_importlib", "_frozen_importlib_external"))),
    (11, 2): ("_getuserbase", frozenset(("emscripten", "vxworks", "wasi"))),
}
_ABSENT = object()


class SiteCodeError(ValueError):
    pass


class _AttestedCode:
    def __init__(self, actual, selection=None):
        self._actual = actual
        self._selection = selection

    def selection(self):
        return self._selection

    def execute(self, namespace):
        exec(self._actual, namespace)


def attest_code(expected, actual, loader_kind="source", limits=None):
    if loader_kind not in ("source", "frozen"):
        raise SiteCodeError("unknown loader")
    configured = dict(_DEFAULT_LIMITS)
    if limits is not None:
        if not isinstance(limits, dict) or any(key not in configured for key in limits):
            raise SiteCodeError("code limit: unknown configuration")
        configured.update(limits)
    if any(type(value) is not int or value <= 0 for value in configured.values()):
        raise SiteCodeError("code limit: values must be positive integers")
    if type(expected) is not types.CodeType or type(actual) is not types.CodeType:
        raise SiteCodeError("code mismatch: non-code input")
    source_filename = expected.co_filename
    if loader_kind == "frozen" and not os.path.isabs(source_filename):
        raise SiteCodeError("code mismatch: frozen reference has no source binding")
    _preflight(expected, configured)
    _preflight(actual, configured)
    _compare_code(expected, actual, loader_kind, source_filename, ())
    return _AttestedCode(actual)


def load_attested_source(source, cache, source_path, source_mtime, limits=None):
    """Replay the selected runtime's source-loader rules over owned bytes.

    The caller must separately bind its actual spec/loader/origin and the
    immutable inputs. This function never discovers or opens a source path.
    """
    configured = dict(_DEFAULT_LIMITS)
    configured.update({
        "max_source_bytes": 8 * 1024 * 1024,
        "max_cache_bytes": 8 * 1024 * 1024,
        "max_path_bytes": 16384,
    })
    if limits is not None:
        if not isinstance(limits, dict) or any(key not in configured for key in limits):
            raise SiteCodeError("code limit: unknown replay configuration")
        configured.update(limits)
    if any(type(value) is not int or value <= 0 for value in configured.values()):
        raise SiteCodeError("code limit: replay values must be positive integers")
    if (type(source) is not bytes or (cache is not None and type(cache) is not bytes)
            or type(source_path) is not str or type(source_mtime) is not int
            or not -(1 << 63) <= source_mtime < (1 << 63)):
        raise SiteCodeError("snapshot input has an invalid type, path or mtime")
    if (len(source) > configured["max_source_bytes"]
            or (cache is not None and len(cache) > configured["max_cache_bytes"])
            or len(source_path) > configured["max_path_bytes"]):
        raise SiteCodeError("code limit: replay input bytes exceeded")
    if len(source_path.encode("utf-8", "surrogatepass")) > configured["max_path_bytes"]:
        raise SiteCodeError("code limit: replay path bytes exceeded")
    if (not os.path.isabs(source_path) or "\0" in source_path
            or any(part in (".", "..") for part in source_path.split("/"))):
        raise SiteCodeError("snapshot input has an invalid type, path or mtime")
    if not sys.dont_write_bytecode:
        raise SiteCodeError("bytecode writing must be disabled before replay")
    cache_path = importlib.util.cache_from_source(source_path)
    if len(cache_path.encode("utf-8", "surrogatepass")) > configured["max_path_bytes"]:
        raise SiteCodeError("code limit: replay cache path bytes exceeded")

    class SnapshotLoader(importlib.machinery.SourceFileLoader):
        def __init__(self):
            super().__init__("site", source_path)
            self.compiled_source = False

        def path_stats(self, path):
            if path != source_path:
                raise SiteCodeError("snapshot input requested an unexpected stat path")
            return {"mtime": source_mtime, "size": len(source)}

        def get_data(self, path):
            if path == source_path:
                return source
            if path == cache_path:
                if cache is None:
                    raise FileNotFoundError(path)
                _preflight_selected_cache(cache, source, source_mtime, expected, configured)
                return cache
            raise SiteCodeError("snapshot input requested an unexpected read path")

        def source_to_code(self, *args, **kwargs):
            self.compiled_source = True
            return super().source_to_code(*args, **kwargs)

        def set_data(self, *args, **kwargs):
            raise SiteCodeError("bytecode writing is forbidden during replay")

    # All raw input budgets precede compilation and marshal/cache parsing.
    expected = compile(source, source_path, "exec", dont_inherit=True, optimize=sys.flags.optimize)
    _preflight(expected, {key: configured[key] for key in _DEFAULT_LIMITS})
    loader = SnapshotLoader()
    actual = loader.get_code("site")
    attested = attest_code(expected, actual, limits={key: configured[key] for key in _DEFAULT_LIMITS})
    attested._selection = "source" if loader.compiled_source else "cache"
    return attested


def validate_site_top_level(source, source_path, limits=None):
    """Admit only the reviewed pre-hook shape, not arbitrary Python safety."""
    configured = dict(_DEFAULT_LIMITS)
    if limits is not None:
        configured.update({key: value for key, value in limits.items() if key in configured})
    if type(source) is not bytes or len(source) > (limits or {}).get("max_source_bytes", 8 * 1024 * 1024):
        raise SiteCodeError("code limit: site source exceeded")
    try:
        tree = ast.parse(source, source_path, "exec")
    except (ValueError, SyntaxError):
        raise SiteCodeError("unknown site adapter: invalid source")
    for count, _ in enumerate(ast.walk(tree), 1):
        if count > configured["max_nodes"]:
            raise SiteCodeError("code limit: adapter AST nodes exceeded")
    dump = lambda node: ast.dump(node, include_attributes=False)
    assignments = {
        "PREFIXES": dump(ast.parse("[sys.prefix, sys.exec_prefix]", mode="eval").body),
        "ENABLE_USER_SITE": dump(ast.parse("None", mode="eval").body),
        "USER_SITE": dump(ast.parse("None", mode="eval").body),
        "USER_BASE": dump(ast.parse("None", mode="eval").body),
        "is_pypy": dump(ast.parse("sys.implementation.name == 'pypy'", mode="eval").body),
    }
    guards = {
        dump(ast.parse("if not sys.flags.no_site:\n    main()\n").body[0]): "main",
        dump(ast.parse("if __name__ == '__main__':\n    _script()\n").body[0]): "script",
    }
    imports = frozenset(("sys", "os", "builtins", "_sitebuiltins", "io", "stat"))
    functions = set()
    seen = set()
    for index, node in enumerate(tree.body):
        if index == 0 and isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant) and type(node.value.value) is str:
            continue
        if isinstance(node, ast.Import):
            if all(alias.name in imports and alias.asname is None for alias in node.names):
                continue
        elif isinstance(node, ast.Assign):
            if (len(node.targets) == 1 and isinstance(node.targets[0], ast.Name)
                    and assignments.get(node.targets[0].id) == dump(node.value)):
                continue
        elif isinstance(node, ast.FunctionDef):
            arguments = list(node.args.args) + list(node.args.kwonlyargs) + list(getattr(node.args, "posonlyargs", ()))
            arguments += [arg for arg in (node.args.vararg, node.args.kwarg) if arg is not None]
            if (node.name in functions or node.decorator_list or node.returns is not None
                    or getattr(node, "type_params", ()) or any(arg.annotation is not None for arg in arguments)):
                raise SiteCodeError("unknown site adapter: eager function definition")
            for default in list(node.args.defaults) + [value for value in node.args.kw_defaults if value is not None]:
                try:
                    ast.literal_eval(default)
                except (ValueError, TypeError, SyntaxError):
                    raise SiteCodeError("unknown site adapter: eager default")
            functions.add(node.name)
            continue
        elif isinstance(node, ast.If):
            guard = guards.get(dump(node))
            if guard is not None and guard not in seen:
                seen.add(guard)
                continue
        raise SiteCodeError("unknown site adapter: eager top-level statement")
    if "main" not in functions or "main" not in seen:
        raise SiteCodeError("unknown site adapter: missing no-site guard")


def _startup_loader_details():
    return (
        (importlib.machinery.ExtensionFileLoader, importlib.machinery.EXTENSION_SUFFIXES),
        (importlib.machinery.SourceFileLoader, importlib.machinery.SOURCE_SUFFIXES),
        (importlib.machinery.SourcelessFileLoader, importlib.machinery.BYTECODE_SUFFIXES),
    )


def _same_hook_value(actual, expected):
    if actual is expected:
        return True
    if type(actual) is not type(expected):
        return False
    if type(actual) in (tuple, list):
        return len(actual) == len(expected) and all(_same_hook_value(a, b) for a, b in zip(actual, expected))
    return type(actual) is str and actual == expected


def validate_startup_finders(search_paths):
    known = (importlib.machinery.BuiltinImporter, importlib.machinery.FrozenImporter, importlib.machinery.PathFinder)
    if len(sys.meta_path) != len(known) or any(actual is not expected for actual, expected in zip(sys.meta_path, known)):
        raise SiteCodeError("unknown startup finder: meta path")
    expected = importlib.machinery.FileFinder.path_hook(*_startup_loader_details())
    if len(sys.path_hooks) != 2 or sys.path_hooks[0] is not zipimport.zipimporter:
        raise SiteCodeError("unknown startup finder: path hooks")
    actual = sys.path_hooks[1]
    if (type(actual) is not types.FunctionType or actual.__code__ is not expected.__code__
            or actual.__closure__ is None or len(actual.__closure__) != len(expected.__closure__)
            or not all(_same_hook_value(a.cell_contents, b.cell_contents) for a, b in zip(actual.__closure__, expected.__closure__))):
        raise SiteCodeError("unknown startup finder: path hook implementation")
    for path in search_paths:
        cached = sys.path_importer_cache.get(path)
        if cached is not None and type(cached) not in (importlib.machinery.FileFinder, zipimport.zipimporter):
            raise SiteCodeError("unknown startup finder: importer cache")


def ensure_no_customization(search_paths, max_paths=4096, max_path_bytes=16384):
    if len(search_paths) > max_paths:
        raise SiteCodeError("code limit: customization paths exceeded")
    for path in search_paths:
        if type(path) is not str or len(path) > max_path_bytes or not os.path.isabs(path) or len(path.encode("utf-8", "surrogatepass")) > max_path_bytes:
            raise SiteCodeError("unknown startup finder: invalid search path")
    # Validate all extension points before invoking a finder or path hook. Use
    # fresh known finders for inspection, never an arbitrary cached object's IO.
    validate_startup_finders(search_paths)
    for name in ("sitecustomize", "usercustomize"):
        if name in sys.modules or importlib.machinery.BuiltinImporter.find_spec(name) is not None or importlib.machinery.FrozenImporter.find_spec(name) is not None:
            raise SiteCodeError("customize hook is already available")
    for path in dict.fromkeys(search_paths):
        if os.path.isdir(path):
            finder = importlib.machinery.FileFinder(path, *_startup_loader_details())
        else:
            try:
                finder = zipimport.zipimporter(path)
            except zipimport.ZipImportError:
                continue
        for name in ("sitecustomize", "usercustomize"):
            if finder.find_spec(name) is not None:
                raise SiteCodeError("customize hook is available on the normal search path")


def _preflight_selected_cache(cache, source, mtime, expected, limits):
    external = importlib._bootstrap_external
    details = {"name": "site", "path": expected.co_filename}
    try:
        flags = external._classify_pyc(cache, "site", details)
        if flags & 1:
            policy = _imp.check_hash_based_pycs
            if policy != "never" and ((flags & 2) or policy == "always"):
                external._validate_hash_pyc(cache, importlib.util.source_hash(source), "site", details)
        else:
            external._validate_timestamp_pyc(cache, mtime, len(source), "site", details)
    except (ImportError, EOFError):
        # Preserve the actual loader's stale/bad-header source fallback. Such
        # bytes will never enter marshal in its unchanged get_code algorithm.
        return
    # Determine the payload boundary and code grammar from this trusted runtime,
    # not host/minor guesses. Nothing in the cache is deserialized here.
    sample = external._code_to_timestamp_pyc(expected, mtime, len(source))
    serialized = marshal.dumps(expected)
    header_size = len(sample) - len(serialized)
    if header_size <= 0 or header_size > len(cache):
        raise SiteCodeError("marshal budget: unknown cache header")
    profile = identify_profile(expected, limits)
    Scanner(memoryview(cache)[header_size:], profile, limits).complete()


def _preflight(root, limits):
    used = {"nodes": 0, "code_bytes": 0, "literal_bytes": 0}
    active = set()

    def account(key, count, maximum):
        used[key] += count
        if used[key] > limits[maximum]:
            raise SiteCodeError("code limit: " + key + " exceeded")

    def visit(value, depth):
        account("nodes", 1, "max_nodes")
        if depth > limits["max_depth"]:
            raise SiteCodeError("code limit: depth exceeded")
        kind = type(value)
        if kind is types.CodeType:
            if id(value) in active:
                raise SiteCodeError("code mismatch: recursive code graph")
            active.add(id(value))
            unknown = {
                name for name in dir(value)
                if name.startswith("co_")
                and name not in _FIELDS
                and name not in _DERIVED
                and name not in ("co_consts", "co_filename")
            }
            if unknown:
                raise SiteCodeError("code mismatch: unknown code field")
            for name in _FIELDS:
                if name == "co_lnotab" and hasattr(value, "co_linetable"):
                    # A derived compatibility view can expand encoded deltas.
                    # The authoritative raw co_linetable is bounded and exact.
                    continue
                field = getattr(value, name, _ABSENT)
                if field is _ABSENT:
                    continue
                if name in ("co_code", "co_linetable", "co_lnotab", "co_exceptiontable"):
                    if type(field) is not bytes:
                        raise SiteCodeError("code mismatch: invalid bytecode field")
                    account("code_bytes", len(field), "max_code_bytes")
                else:
                    visit(field, depth + 1)
            visit(value.co_filename, depth + 1)
            for constant in value.co_consts:
                visit(constant, depth + 1)
            active.remove(id(value))
        elif kind in (tuple, frozenset):
            if id(value) in active:
                raise SiteCodeError("code mismatch: recursive constant graph")
            active.add(id(value))
            for item in value:
                visit(item, depth + 1)
            active.remove(id(value))
        elif kind is str:
            if len(value) > limits["max_literal_bytes"]:
                raise SiteCodeError("code limit: literal bytes exceeded")
            account("literal_bytes", len(value.encode("utf-8", "surrogatepass")), "max_literal_bytes")
        elif kind is bytes:
            account("literal_bytes", len(value), "max_literal_bytes")
        elif kind is int:
            account("literal_bytes", max(1, (value.bit_length() + 7) // 8), "max_literal_bytes")
        elif kind is float:
            account("literal_bytes", 8, "max_literal_bytes")
        elif kind is complex:
            account("literal_bytes", 16, "max_literal_bytes")
        elif value is None or value is Ellipsis or kind is bool:
            account("literal_bytes", 1, "max_literal_bytes")
        else:
            raise SiteCodeError("code mismatch: unknown constant type")

    visit(root, 0)


def _compare_code(expected, actual, loader_kind, source_filename, path):
    if type(expected) is not types.CodeType or type(actual) is not types.CodeType:
        raise SiteCodeError("code mismatch: code object type")
    for name in _FIELDS:
        if name == "co_lnotab" and hasattr(expected, "co_linetable") and hasattr(actual, "co_linetable"):
            continue
        left = getattr(expected, name, _ABSENT)
        right = getattr(actual, name, _ABSENT)
        if left is _ABSENT or right is _ABSENT:
            if left is not right:
                raise SiteCodeError("code mismatch: absent field " + name)
        elif type(left) is not type(right) or left != right:
            raise SiteCodeError("code mismatch: " + name)
    if loader_kind == "frozen":
        if expected.co_filename != source_filename or actual.co_filename != "<frozen site>":
            raise SiteCodeError("code mismatch: frozen filename")
    elif expected.co_filename != actual.co_filename:
        raise SiteCodeError("code mismatch: source filename")
    if len(expected.co_consts) != len(actual.co_consts):
        raise SiteCodeError("code mismatch: constant count")
    for index, (left, right) in enumerate(zip(expected.co_consts, actual.co_consts)):
        point = path + (index,)
        if _allowed_frozen_membership(left, right, actual, loader_kind, point, index):
            continue
        _compare_value(left, right, loader_kind, source_filename, point)


def _compare_value(left, right, loader_kind, source_filename, path):
    kind = type(left)
    if kind is not type(right):
        raise SiteCodeError("code mismatch: constant type")
    if kind is types.CodeType:
        _compare_code(left, right, loader_kind, source_filename, path)
    elif kind is tuple:
        if len(left) != len(right):
            raise SiteCodeError("code mismatch: tuple constant length")
        for index, (a, b) in enumerate(zip(left, right)):
            _compare_value(a, b, loader_kind, source_filename, path + (index,))
    elif kind is frozenset:
        # Code objects cannot be compared through their broad Python equality.
        # Membership normalization is handled only at the two explicit slots.
        if any(type(value) not in (str, bytes, int, bool, type(None)) for value in left | right):
            raise SiteCodeError("code mismatch: unsupported frozenset element")
        typed_left = {(type(value), value) for value in left}
        typed_right = {(type(value), value) for value in right}
        if typed_left != typed_right:
            raise SiteCodeError("code mismatch: frozenset constant")
    elif kind is float:
        if struct.pack(">d", left) != struct.pack(">d", right):
            raise SiteCodeError("code mismatch: float constant")
    elif kind is complex:
        if struct.pack(">dd", left.real, left.imag) != struct.pack(">dd", right.real, right.imag):
            raise SiteCodeError("code mismatch: complex constant")
    elif left != right:
        raise SiteCodeError("code mismatch: constant value")


def _allowed_frozen_membership(left, right, actual, loader_kind, path, index):
    if loader_kind != "frozen" or path not in _FROZEN_MEMBERSHIP:
        return False
    function_name, members = _FROZEN_MEMBERSHIP[path]
    if (actual.co_name != function_name or type(left) is not frozenset
            or left != members or type(right) is not tuple
            or any(type(value) is not str for value in right)
            or len(right) != len(members) or frozenset(right) != members):
        return False
    instructions = list(dis.get_instructions(actual))
    found = False
    for position, instruction in enumerate(instructions):
        if instruction.opname == "LOAD_CONST" and instruction.arg == index:
            found = True
            if position + 1 == len(instructions) or instructions[position + 1].opname != "CONTAINS_OP":
                return False
    return found
"""Allocation-bounded marshal structure traversal; never deserialize objects."""

import marshal
import struct


PROFILES = ((6, 8, 1), (5, 8, 2), (6, 9, 2), (5, 8, 1))


class BudgetError(SiteCodeError):
    pass


class Scanner:
    def __init__(self, raw, profile, limits):
        self.raw = raw
        self.profile = profile
        self.limits = limits
        self.position = 0
        self.nodes = 0
        self.literal_bytes = 0
        self.code_bytes = 0
        self.refs = []
        self.interned = []

    def fail(self, why):
        raise BudgetError("marshal budget: " + why)

    def take(self, count):
        if count < 0 or count > len(self.raw) - self.position:
            self.fail("truncated field")
        self.position += count

    def byte(self):
        if self.position == len(self.raw):
            self.fail("truncated tag")
        value = self.raw[self.position]
        self.position += 1
        return value

    def integer(self):
        if len(self.raw) - self.position < 4:
            self.fail("truncated integer")
        value = struct.unpack_from("<i", self.raw, self.position)[0]
        self.position += 4
        return value

    def literal(self, count):
        if count < 0 or count > self.limits["max_literal_bytes"] - self.literal_bytes:
            self.fail("declared literal allocation")
        self.literal_bytes += count
        self.take(count)

    def collection(self, count):
        if count < 0 or count > self.limits["max_nodes"] - self.nodes:
            self.fail("declared container allocation")
        if count > len(self.raw) - self.position:
            self.fail("container cannot fit encoded bytes")

    def value(self, depth=0, allow_null=False):
        self.nodes += 1
        if self.nodes > self.limits["max_nodes"] or depth > self.limits["max_depth"]:
            self.fail("node or depth limit")
        encoded = self.byte()
        flag, tag = encoded & 128, chr(encoded & 127)
        slot = None
        if flag:
            if len(self.refs) >= self.limits["max_nodes"]:
                self.fail("reference table limit")
            slot = len(self.refs)
            self.refs.append(None)
        if tag == "0":
            if not allow_null or flag:
                self.fail("invalid null object")
            result = ("null", 0, False)
        elif tag in ("N", "F", "T", "S", "."):
            result = ("scalar", 0, False)
        elif tag in ("i", "I"):
            self.take(4 if tag == "i" else 8)
            result = ("scalar", 0, False)
        elif tag == "l":
            count = self.integer()
            self.literal(abs(count) * 2)
            result = ("scalar", 0, False)
        elif tag in ("g", "y"):
            self.literal(8 if tag == "g" else 16)
            result = ("scalar", 0, False)
        elif tag in ("f", "x"):
            self.literal(self.byte())
            if tag == "x":
                self.literal(self.byte())
            result = ("scalar", 0, False)
        elif tag in ("s", "t", "u", "a", "A", "z", "Z"):
            count = self.byte() if tag in ("z", "Z") else self.integer()
            self.literal(count)
            result = ("bytes" if tag == "s" else "str", count, False)
            if tag in ("t", "A", "Z"):
                if len(self.interned) >= self.limits["max_nodes"]:
                    self.fail("interned table limit")
                self.interned.append(result)
        elif tag in ("r", "R"):
            if flag:
                self.fail("flagged reference")
            index = self.integer()
            table = self.refs if tag == "r" else self.interned
            if index < 0 or index >= len(table) or table[index] is None:
                self.fail("invalid or forward reference")
            result = table[index]
        elif tag in ("(", ")", "[", "<", ">"):
            count = self.byte() if tag == ")" else self.integer()
            self.collection(count)
            strings = True
            for _ in range(count):
                strings = self.value(depth + 1)[0] == "str" and strings
            result = ("tuple" if tag in ("(", ")") else "container", count, strings)
        elif tag == "{":
            count = 0
            while True:
                key = self.value(depth + 1, allow_null=True)
                if key[0] == "null":
                    break
                self.value(depth + 1)
                count += 1
            result = ("container", count, False)
        elif tag == "c":
            result = self.code(depth)
        else:
            self.fail("unknown type tag")
        if slot is not None:
            self.refs[slot] = result
        return result

    def code(self, depth):
        integers, object_count, tail_count = self.profile
        counts = [self.integer() for _ in range(integers)]
        if any(count < 0 or count > self.limits["max_nodes"] for count in counts[:-1]) or counts[-1] < 0:
            self.fail("code count or flags")
        fields = [self.value(depth + 1) for _ in range(object_count)]
        firstline = self.integer()
        if firstline < 0:
            self.fail("negative first line")
        tails = [self.value(depth + 1) for _ in range(tail_count)]
        if fields[0][0] != "bytes" or fields[1][0] != "tuple" or fields[2][0] != "tuple" or not fields[2][2]:
            self.fail("invalid code, constants or names")
        if self.profile == (5, 8, 2):
            if fields[3][0] != "tuple" or not fields[3][2] or fields[4][0] != "bytes" or fields[3][1] != fields[4][1]:
                self.fail("invalid locals-plus fields")
            if any(field[0] != "str" for field in fields[5:]):
                self.fail("invalid code filename or name")
            locals_count = fields[3][1]
        else:
            if any(field[0] != "tuple" or not field[2] for field in fields[3:6]):
                self.fail("invalid local or closure names")
            if any(field[0] != "str" for field in fields[6:]):
                self.fail("invalid code filename or name")
            locals_count = fields[3][1]
            if integers == 6 and counts[3] != locals_count:
                self.fail("local count disagrees")
            if integers == 5 and counts[2] != locals_count:
                self.fail("legacy local count disagrees")
        if counts[0] > locals_count:
            self.fail("argument count exceeds locals")
        if integers == 6 or self.profile == (5, 8, 2):
            if counts[1] > counts[0] or counts[0] + counts[2] > locals_count:
                self.fail("argument layout disagrees")
        if any(field[0] != "bytes" for field in tails):
            self.fail("invalid code line or exception data")
        self.code_bytes += fields[0][1] + sum(field[1] for field in tails)
        if self.code_bytes > self.limits["max_code_bytes"]:
            self.fail("declared code bytes")
        return ("code", 0, False)

    def complete(self):
        if self.value()[0] != "code" or self.position != len(self.raw):
            self.fail("non-code root or trailing bytes")
        return self.profile


def identify_profile(expected, limits):
    tiny = compile("", expected.co_filename, "exec", dont_inherit=True)
    samples = (marshal.dumps(tiny), marshal.dumps(expected))
    matches = []
    for profile in PROFILES:
        try:
            for sample in samples:
                Scanner(sample, profile, limits).complete()
        except BudgetError:
            continue
        matches.append(profile)
    if len(matches) != 1:
        raise BudgetError("marshal budget: unknown or ambiguous runtime code grammar")
    return matches[0]
