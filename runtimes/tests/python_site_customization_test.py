import importlib._bootstrap_external as external
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest import mock


HELPERS = {}
SCRIPT = Path(__file__).parents[1] / "src/toolinstall/python_site_adapter.py"
exec(compile(SCRIPT.read_bytes(), str(SCRIPT), "exec"), HELPERS)
ensure_no_customization = HELPERS["ensure_no_customization"]


class HookTests(unittest.TestCase):
    def test_base_stdlib_customize_trap_is_found_without_import_even_with_empty_final_site(self):
        with tempfile.TemporaryDirectory() as owner:
            root = Path(owner)
            base, site = root / "base-stdlib", root / "final-site"
            base.mkdir()
            site.mkdir()
            trap = root / "executed"
            (base / "sitecustomize.py").write_text("open(%r, 'w').write('trap')" % str(trap))
            with self.assertRaisesRegex(ValueError, "customize hook"):
                ensure_no_customization([str(base), str(site)])
            self.assertFalse(trap.exists())

    def test_sourceless_usercustomize_cache_is_found_without_deserialization(self):
        with tempfile.TemporaryDirectory() as owner:
            root = Path(owner)
            trap = root / "executed"
            code = compile("open(%r, 'w').write('trap')" % str(trap), "/build/usercustomize.py", "exec")
            (root / "usercustomize.pyc").write_bytes(bytes(external._code_to_timestamp_pyc(code)))
            with self.assertRaisesRegex(ValueError, "customize hook"):
                ensure_no_customization([str(root)])
            self.assertFalse(trap.exists())

    def test_unknown_meta_or_path_finder_is_rejected_without_calling_it(self):
        with tempfile.TemporaryDirectory() as owner:
            root = Path(owner)
            trap = root / "executed"
            class Unknown:
                def __eq__(self, other):
                    trap.write_text("equality called")
                    return False
                def find_spec(self, *args, **kwargs):
                    trap.write_text("finder called")
                    raise AssertionError("unknown finder executed")
                def __call__(self, *args, **kwargs):
                    trap.write_text("hook called")
                    raise AssertionError("unknown hook executed")
            for surface in ("meta_path", "path_hooks"):
                with mock.patch.object(sys, surface, list(getattr(sys, surface)) + [Unknown()]):
                    with self.assertRaisesRegex(ValueError, "unknown startup finder"):
                        ensure_no_customization([str(root)])
                self.assertFalse(trap.exists())


if __name__ == "__main__":
    unittest.main()
