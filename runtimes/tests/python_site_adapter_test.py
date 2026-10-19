import importlib._bootstrap_external as external
import importlib.machinery
import importlib.util
import os
import pathlib
import sys
import tempfile
import types
import unittest

SCRIPT = pathlib.Path(__file__).parents[1] / "src/toolinstall/python_site_adapter.py"
HELPERS = {"__name__": "pakro_python_site_adapter_test"}
exec(compile(SCRIPT.read_bytes(), str(SCRIPT), "exec"), HELPERS)
SiteCodeError = HELPERS["SiteCodeError"]
attest_code = HELPERS["attest_code"]
validate_site_top_level = HELPERS["validate_site_top_level"]


SOURCE_PATH = "/trusted/runtime/stdlib/site.py"
TRUSTED = b"import sys\ndef main():\n    raise RuntimeError('unwanted automatic main')\nif not sys.flags.no_site:\n    main()\n"


class SnapshotLoader(importlib.machinery.SourceFileLoader):
    """Actual stdlib cache selection, with all data sourced from owned bytes."""

    def __init__(self, source, cache):
        super().__init__("site", SOURCE_PATH)
        self.source = source
        self.cache = cache
        self.reads = []

    def path_stats(self, path):
        if path != SOURCE_PATH:
            raise AssertionError("unexpected loader stat")
        return {"mtime": 123456, "size": len(self.source)}

    def get_data(self, path):
        self.reads.append(path)
        if path == SOURCE_PATH:
            return self.source
        if path == importlib.util.cache_from_source(SOURCE_PATH):
            if self.cache is None:
                raise FileNotFoundError(path)
            return self.cache
        raise AssertionError("unexpected loader read")

    def set_data(self, *args, **kwargs):
        raise AssertionError("attestation must never write a cache")


def compile_site(source=TRUSTED):
    return compile(source, SOURCE_PATH, "exec", dont_inherit=True, optimize=sys.flags.optimize)


def timestamp_cache(code):
    return external._code_to_timestamp_pyc(code, mtime=123456, source_size=len(TRUSTED))


def frozen_filename(code):
    constants = tuple(frozen_filename(value) if isinstance(value, types.CodeType) else value for value in code.co_consts)
    return code.replace(co_consts=constants, co_filename="<frozen site>")


def frozen_fixture():
    # Reproduce only the two reviewed function/constant positions; this reduced
    # fixture is not a digest pin or a claim about a complete runtime's site.
    prefix = "\n".join("a = 'pad%d'" % index for index in range(5)) + "\n"
    prefix += "def abs_paths(m):\n    marker = 'marker'\n    return m.__name__ not in {'_frozen_importlib', '_frozen_importlib_external'}\n"
    suffix = "def _getuserbase(platform):\n    marker = 'marker'\n    return platform in {'emscripten', 'vxworks', 'wasi'}\n"
    # Older compilers additionally keep a function qualname constant. Observe
    # that layout in this fixture; the product still permits exactly slots 5/11.
    for padding in range(8):
        text = prefix + "\n".join("a = 'pad%d'" % index for index in range(5, 5 + padding)) + "\n" + suffix
        expected = compile_site(text)
        if len(expected.co_consts) > 11 and isinstance(expected.co_consts[11], types.CodeType):
            if expected.co_consts[11].co_name == "_getuserbase":
                break
    else:
        raise AssertionError("fixture could not construct the reviewed constant slots")
    assert expected.co_consts[5].co_name == "abs_paths"
    assert expected.co_consts[11].co_name == "_getuserbase"
    actual = frozen_filename(expected)
    constants = list(actual.co_consts)
    for index in (5, 11):
        function = constants[index]
        values = list(function.co_consts)
        assert isinstance(values[2], frozenset)
        values[2] = tuple(sorted(values[2]))
        constants[index] = function.replace(co_consts=tuple(values))
    return expected, actual.replace(co_consts=tuple(constants))


class SiteCodeTests(unittest.TestCase):
    def test_known_guarded_top_level_is_safe_before_hook_replacement(self):
        validate_site_top_level(TRUSTED, SOURCE_PATH)
        namespace = {"__name__": "site"}
        attest_code(compile_site(), compile_site()).execute(namespace)
        self.assertIn("main", namespace)

    def test_unguarded_top_level_trap_is_rejected_before_exec(self):
        with tempfile.TemporaryDirectory() as owner:
            trap = pathlib.Path(owner) / "trap"
            for source in [b"main()\n" + TRUSTED, TRUSTED + ("open(%r, 'w').write('executed')\n" % str(trap)).encode()]:
                with self.assertRaisesRegex(SiteCodeError, "unknown site adapter"):
                    validate_site_top_level(source, SOURCE_PATH)
                self.assertFalse(trap.exists())

    def test_decorators_defaults_and_annotations_cannot_execute_before_hooks(self):
        for source in [
            b"@untrusted()\ndef other():\n    pass\n" + TRUSTED,
            b"def other(value=untrusted()):\n    pass\n" + TRUSTED,
            b"def other(value: untrusted()):\n    pass\n" + TRUSTED,
        ]:
            with self.assertRaisesRegex(SiteCodeError, "unknown site adapter"):
                validate_site_top_level(source, SOURCE_PATH)

    def test_unknown_import_alias_or_dynamic_global_is_not_a_site_adapter(self):
        for source in [b"import third_party\n" + TRUSTED, b"import os as sys\n" + TRUSTED, b"PREFIXES = untrusted()\n" + TRUSTED, b"class Other(untrusted()):\n    pass\n" + TRUSTED]:
            with self.assertRaisesRegex(SiteCodeError, "unknown site adapter"):
                validate_site_top_level(source, SOURCE_PATH)

    def test_matching_source_cache_is_executed_only_after_attestation(self):
        expected = compile_site()
        loader = SnapshotLoader(TRUSTED, timestamp_cache(expected))
        actual = loader.get_code("site")
        self.assertIn(importlib.util.cache_from_source(SOURCE_PATH), loader.reads)
        namespace = {"__name__": "site"}
        attest_code(expected, actual).execute(namespace)
        self.assertIn("main", namespace)

    def test_valid_timestamp_pyc_with_unguarded_trap_is_rejected_before_exec(self):
        with tempfile.TemporaryDirectory() as owner:
            trap = pathlib.Path(owner) / "trap"
            evil = compile_site("open(%r, 'w').write('executed')\n" % str(trap))
            loader = SnapshotLoader(TRUSTED, timestamp_cache(evil))
            actual = loader.get_code("site")
            self.assertIn(importlib.util.cache_from_source(SOURCE_PATH), loader.reads)
            self.assertNotEqual(actual.co_code, compile_site().co_code)
            with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
                attest_code(compile_site(), actual)
            self.assertFalse(trap.exists())

    def test_valid_unchecked_hash_pyc_is_rejected_before_exec(self):
        with tempfile.TemporaryDirectory() as owner:
            trap = pathlib.Path(owner) / "trap"
            evil = compile_site("open(%r, 'w').write('executed')\n" % str(trap))
            cache = external._code_to_hash_pyc(evil, importlib.util.source_hash(TRUSTED), checked=False)
            actual = SnapshotLoader(TRUSTED, cache).get_code("site")
            self.assertNotEqual(actual.co_code, compile_site().co_code)
            with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
                attest_code(compile_site(), actual)
            self.assertFalse(trap.exists())

    def test_nested_function_constant_mismatch_is_not_hidden_by_equal_module_code(self):
        expected = compile_site()
        actual = compile_site(TRUSTED.replace(b"unwanted automatic main", b"different automatic main"))
        self.assertEqual(expected.co_code, actual.co_code)
        with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
            attest_code(expected, actual)

    def test_argument_flags_names_and_closure_fields_are_compared(self):
        expected = compile_site()
        different_flags = expected.replace(co_flags=expected.co_flags ^ 0x80)
        self.assertNotEqual(expected.co_flags, different_flags.co_flags)
        for actual in [different_flags, expected.replace(co_names=("other",) + expected.co_names[1:]), expected.replace(co_argcount=1, co_nlocals=1, co_varnames=("argument",))]:
            with self.subTest(actual=actual):
                with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
                    attest_code(expected, actual)

    def test_exception_table_is_compared_when_the_runtime_exposes_it(self):
        expected = compile_site()
        if not hasattr(expected, "co_exceptiontable"):
            self.skipTest("selected code format has no exception table field")
        with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
            attest_code(expected, expected.replace(co_exceptiontable=b"\x01"))

    def test_file_source_filename_and_line_information_are_not_normalized(self):
        expected = compile_site()
        for actual in [expected.replace(co_filename="/other/site.py"), expected.replace(co_firstlineno=2)]:
            with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
                attest_code(expected, actual)

    def test_only_the_two_reviewed_frozen_membership_constants_are_normalized(self):
        expected, actual = frozen_fixture()
        namespace = {"__name__": "site"}
        attest_code(expected, actual, loader_kind="frozen").execute(namespace)
        module = types.SimpleNamespace(__name__="_frozen_importlib")
        self.assertFalse(namespace["abs_paths"](module))
        self.assertTrue(namespace["_getuserbase"]("wasi"))

    def test_frozen_membership_value_change_or_duplicate_is_rejected(self):
        expected, original = frozen_fixture()
        for replacement in [("emscripten", "vxworks", "other"), ("emscripten", "vxworks", "wasi", "wasi")]:
            constants = list(original.co_consts)
            function = constants[11]
            values = list(function.co_consts)
            values[2] = replacement
            constants[11] = function.replace(co_consts=tuple(values))
            with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
                attest_code(expected, original.replace(co_consts=tuple(constants)), loader_kind="frozen")

    def test_membership_normalization_is_not_available_for_source_loaders(self):
        expected, actual = frozen_fixture()
        actual = actual.replace(co_filename=SOURCE_PATH)
        with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
            attest_code(expected, actual, loader_kind="source")

    def test_nonmembership_frozen_constant_and_arbitrary_filename_are_rejected(self):
        expected, actual = frozen_fixture()
        constants = list(actual.co_consts)
        constants[0] = "different padding literal"
        for modified in [actual.replace(co_consts=tuple(constants)), actual.replace(co_filename="<other frozen>")]:
            with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
                attest_code(expected, modified, loader_kind="frozen")

    def test_unknown_loader_is_rejected_before_any_execution(self):
        expected = compile_site()
        with self.assertRaisesRegex(SiteCodeError, "unknown loader"):
            attest_code(expected, expected, loader_kind="custom")

    def test_code_graph_and_literal_budgets_are_checked_before_attestation(self):
        expected = compile_site()
        for limits in [{"max_nodes": 0}, {"max_nodes": 1}, {"max_depth": 0}, {"max_code_bytes": 1}, {"max_literal_bytes": 1}]:
            with self.subTest(limits=limits):
                with self.assertRaisesRegex(SiteCodeError, "code limit"):
                    attest_code(expected, expected, limits=limits)


if __name__ == "__main__":
    unittest.main()
