# SPDX-License-Identifier: Apache-2.0
"""Plan, licence and entitlement checks may live only in rust/src/core/hive/."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("check_local_free", ROOT / "scripts/check-local-free.py")
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

HEADER = 'schema = "leanctx.local-free-allowlist/v1"\n'


class LocalFreeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.write("rust/src/core/cache.rs", "fn cache() {}\n")
        self.write("rust/src/core/hive/plan.rs", 'fn gate() { require_pro(); std::env::var("LEAN_CTX_PLAN"); }\n')
        self.allow("")

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def allow(self, entries):
        self.write("product/local-free-allowlist.toml", HEADER + entries)

    def errors(self):
        return "\n".join(check.verify(self.root)["errors"])

    def test_gates_under_core_hive_pass(self):
        self.assertEqual(self.errors(), "")

    def test_a_gate_outside_core_hive_fails_until_allowlisted_with_owner(self):
        self.write("rust/src/tools/x.rs", "fn x() { if plan == Plan::Pro { capability_admitted(); } }\n")
        self.assertIn("rust/src/tools/x.rs: gating symbol(s) capability_admitted, paid plan", self.errors())
        self.allow('[[file]]\npath = "rust/src/tools/x.rs"\nowner = "Z"\nreason = ""\n')
        found = self.errors()
        self.assertIn("needs owner B or C", found)
        self.assertIn("needs a reason", found)
        self.allow('[[file]]\npath = "rust/src/tools/x.rs"\nowner = "B"\nreason = "moves to hive"\n')
        self.assertEqual(self.errors(), "")

    def test_comments_strings_and_test_files_are_not_gates(self):
        self.write("rust/src/core/doc.rs",
                   '//! The entitlement used to live here.\n/* LEAN_CTX_PLAN /* nested */ */\n'
                   'fn f() { let s = "// not a comment"; let r = r#"x"#; }\n')
        self.write("rust/src/core/thing_tests.rs", "fn t() { require_pro(); }\n")
        self.assertEqual(self.errors(), "")
        self.write("rust/src/core/doc.rs", 'fn f() { let s = "a // b"; minimum_plan(); }\n')
        self.assertIn("rust/src/core/doc.rs: gating symbol(s) minimum_plan", self.errors())

    def test_stale_allowlist_entry_fails(self):
        self.allow('[[file]]\npath = "rust/src/core/cache.rs"\nowner = "C"\nreason = "gone"\n')
        self.assertIn("rust/src/core/cache.rs has no gating symbol any more", self.errors())


if __name__ == "__main__":
    unittest.main()
