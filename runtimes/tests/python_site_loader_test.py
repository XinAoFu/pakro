"""Owned-byte loader replay only, separate from Rust/runtime admission."""

import importlib._bootstrap_external as external
import importlib.util
from pathlib import Path
import sys
import struct
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).parents[1] / "src/toolinstall/python_site_adapter.py"
HELPERS = {"__name__": "pakro_python_site_loader_test"}
exec(compile(SCRIPT.read_bytes(), str(SCRIPT), "exec"), HELPERS)
SiteCodeError = HELPERS["SiteCodeError"]
load_attested_source = HELPERS["load_attested_source"]

SOURCE_PATH = "/owned-runtime/stdlib/site.py"
SOURCE = b"import sys\nmarker = 'source'\ndef main():\n    raise RuntimeError('automatic site main')\nif not sys.flags.no_site:\n    main()\n"
MTIME = 123456


def compile_site(source=SOURCE):
    return compile(source, SOURCE_PATH, "exec", dont_inherit=True, optimize=sys.flags.optimize)


def cache_for(code=None, mtime=MTIME, size=None):
    return bytes(external._code_to_timestamp_pyc(
        compile_site() if code is None else code,
        mtime=mtime,
        source_size=len(SOURCE) if size is None else size,
    ))


class SiteLoaderTests(unittest.TestCase):
    def test_matching_cache_is_selected_and_same_attested_object_executes(self):
        result = load_attested_source(SOURCE, cache_for(), SOURCE_PATH, MTIME)
        self.assertEqual(result.selection(), "cache")
        namespace = {"__name__": "site"}
        result.execute(namespace)
        self.assertEqual(namespace["marker"], "source")

    def test_missing_and_stale_cache_use_owned_source_without_path_io(self):
        for cache in [None, cache_for(mtime=MTIME - 1), cache_for(size=len(SOURCE) + 1)]:
            with mock.patch("builtins.open", side_effect=AssertionError("unexpected filesystem IO")):
                result = load_attested_source(SOURCE, cache, SOURCE_PATH, MTIME)
            self.assertEqual(result.selection(), "source")
            namespace = {"__name__": "site"}
            result.execute(namespace)
            self.assertEqual(namespace["marker"], "source")

    def test_bad_magic_or_truncated_cache_uses_actual_loader_fallback(self):
        for cache in [b"bad", b"BAD!" + cache_for()[4:]]:
            result = load_attested_source(SOURCE, cache, SOURCE_PATH, MTIME)
            self.assertEqual(result.selection(), "source")

    def test_valid_timestamp_evil_cache_is_rejected_before_any_execution(self):
        with tempfile.TemporaryDirectory() as owner:
            trap = Path(owner) / "trap"
            evil = compile_site("open(%r, 'w').write('executed')" % str(trap))
            with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
                load_attested_source(SOURCE, cache_for(evil), SOURCE_PATH, MTIME)
            self.assertFalse(trap.exists())

    def test_valid_unchecked_hash_evil_cache_is_not_ignored(self):
        with tempfile.TemporaryDirectory() as owner:
            trap = Path(owner) / "trap"
            evil = compile_site("open(%r, 'w').write('executed')" % str(trap))
            cache = bytes(external._code_to_hash_pyc(evil, importlib.util.source_hash(SOURCE), checked=False))
            with self.assertRaisesRegex(SiteCodeError, "code mismatch"):
                load_attested_source(SOURCE, cache, SOURCE_PATH, MTIME)
            self.assertFalse(trap.exists())

    def test_checked_hash_cache_obeys_selected_runtime_hash_validation(self):
        good = bytes(external._code_to_hash_pyc(compile_site(), importlib.util.source_hash(SOURCE), checked=True))
        self.assertEqual(load_attested_source(SOURCE, good, SOURCE_PATH, MTIME).selection(), "cache")
        wrong = bytes(external._code_to_hash_pyc(compile_site(), b"abcdefgh", checked=True))
        self.assertEqual(load_attested_source(SOURCE, wrong, SOURCE_PATH, MTIME).selection(), "source")

    def test_source_cache_and_code_budgets_are_enforced_before_loader_reads(self):
        for limits in [
            {"max_source_bytes": 0}, {"max_source_bytes": len(SOURCE) - 1},
            {"max_cache_bytes": 1}, {"max_code_bytes": 1},
            {"max_nodes": 1}, {"unknown": 1},
        ]:
            with self.assertRaisesRegex(SiteCodeError, "code limit"):
                load_attested_source(SOURCE, cache_for(), SOURCE_PATH, MTIME, limits=limits)

    def test_mutable_bytes_bad_paths_and_noninteger_mtime_are_rejected(self):
        for source, cache, path, mtime in [
            (bytearray(SOURCE), None, SOURCE_PATH, MTIME),
            (SOURCE, bytearray(cache_for()), SOURCE_PATH, MTIME),
            (SOURCE, None, "relative/site.py", MTIME),
            (SOURCE, None, "/runtime/../site.py", MTIME),
            (SOURCE, None, "/runtime/si\0te.py", MTIME),
            (SOURCE, None, SOURCE_PATH, True),
            (SOURCE, None, SOURCE_PATH, 1 << 128),
        ]:
            with self.assertRaisesRegex(SiteCodeError, "snapshot input"):
                load_attested_source(source, cache, path, mtime)

    def test_bytecode_writing_disabled_is_a_required_fixed_runner_flag(self):
        with mock.patch.object(sys, "dont_write_bytecode", False):
            with self.assertRaisesRegex(SiteCodeError, "bytecode writing"):
                load_attested_source(SOURCE, None, SOURCE_PATH, MTIME)

    def test_declared_container_allocations_are_bounded_before_marshal(self):
        import marshal
        ordinary = cache_for()
        header_size = len(ordinary) - len(marshal.dumps(compile_site()))
        self.assertGreater(header_size, 0)
        for tag in [b"(", b"s"]:
            payload = ordinary[:header_size] + tag + struct.pack("<I", 1 << 30)
            # This seam prevents deserializing a huge declaration even while
            # testing the broken baseline. It is never a production patch.
            with mock.patch.object(external, "_compile_bytecode", side_effect=SiteCodeError("unsafe marshal seam reached")) as decoder:
                with self.assertRaisesRegex(SiteCodeError, "marshal budget"):
                    load_attested_source(SOURCE, payload, SOURCE_PATH, MTIME)
                decoder.assert_not_called()

    def test_legitimate_noncanonical_marshal_and_build_filename_are_accepted(self):
        import marshal
        ordinary = cache_for()
        header_size = len(ordinary) - len(marshal.dumps(compile_site()))
        alternatives = [
            ordinary[:header_size] + marshal.dumps(compile_site(), 2),
            bytes(external._code_to_timestamp_pyc(compile(SOURCE, "/build-time/stdlib/site.py", "exec", dont_inherit=True), mtime=MTIME, source_size=len(SOURCE))),
        ]
        for cache in alternatives:
            result = load_attested_source(SOURCE, cache, SOURCE_PATH, MTIME)
            self.assertEqual(result.selection(), "cache")


if __name__ == "__main__":
    unittest.main()
