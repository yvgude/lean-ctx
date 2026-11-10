# SPDX-License-Identifier: Apache-2.0
"""Tests for scripts/change-class (task 156, L/I/G change classifier)."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import io
import json
import os
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "change-class"


def load_module():
    path = str(ROOT / "scripts" / "change-class")
    loader = importlib.machinery.SourceFileLoader("change_class", path)
    spec = importlib.util.spec_from_loader("change_class", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


cc = load_module()


class SnapshotPairTests(unittest.TestCase):
    """Fixture snapshot pairs, one per class."""

    def classify_pair(self, name: str):
        base = cc.read_snapshot_dir(str(FIXTURES / name / "base"))
        head = cc.read_snapshot_dir(str(FIXTURES / name / "head"))
        reasons = cc.api_reasons(cc.diff_api(base, head))
        return cc.classify(reasons), reasons

    def test_local_pair(self):
        cls, reasons = self.classify_pair("local")
        self.assertEqual(cls, "L")
        self.assertEqual(reasons, [])

    def test_interface_pair(self):
        cls, reasons = self.classify_pair("interface")
        self.assertEqual(cls, "I")
        self.assertEqual([r["class"] for r in reasons], ["I"])
        self.assertIn("crate::Widget::resize", reasons[0]["items"])

    def test_global_pair(self):
        cls, reasons = self.classify_pair("global")
        self.assertEqual(cls, "G")
        text = " | ".join(r["reason"] for r in reasons)
        self.assertIn("1 public item(s) changed", text)
        self.assertIn("1 public item(s) removed", text)
        changed = next(r for r in reasons if "changed" in r["reason"])
        self.assertEqual(changed["items"], ["crate::Widget::new"])


SOURCE = """\
//! Demo crate. A `pub fn in_doc()` in a comment is not an item.
#![allow(dead_code)]
pub mod api;
mod hidden;
mod reexported;
pub use reexported::Visible;
pub(crate) fn crate_only() {}
pub fn greet(name: &str,) -> String { let s = "pub fn in_string() {"; format!("{s}{name}") }
#[derive(Debug, Clone)]
pub struct Point { pub x: i32, pub y: i32 }
pub struct Opaque { pub id: u32, secret: u64 }
pub struct Wrapper(pub u8, u16);
pub enum Shape { Circle(f64), Square { side: f64 } }
#[non_exhaustive]
pub enum Open { A, B }
pub trait Draw { fn draw(&self); fn name(&self) -> &str { "draw" } }
impl Point { pub fn origin() -> Self { Point { x: 0, y: 0 } } fn private(&self) {} }
impl Draw for Point { fn draw(&self) {} }
impl Hidden { pub fn not_listed(&self) {} }
pub const LIMIT: usize = { 4 * 1024 };
#[cfg(feature = "extra")]
pub fn extra() {}
#[macro_export]
macro_rules! shout { ($e:expr) => { $e }; }
#[cfg(test)]
mod tests { pub fn test_helper() {} }
#[cfg(test)]
pub fn test_only() {}
"""


class ExtractorTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        src = Path(self.tmp.name) / "src"
        (src / "api").mkdir(parents=True)
        (src / "lib.rs").write_text(SOURCE)
        (src / "api" / "mod.rs").write_text("pub fn call() {}\nmod inner;\n")
        (src / "api" / "inner.rs").write_text("pub fn deep() {}\n")
        (src / "hidden.rs").write_text("pub struct Hidden;\npub fn invisible() {}\n")
        (src / "reexported.rs").write_text("pub struct Visible;\n")
        self.lines = cc.extract_crate(cc.FsTree(self.tmp.name), "src/lib.rs")
        self.keys = {line.split("\t", 1)[0] for line in self.lines}
        self.text = "\n".join(self.lines)

    def tearDown(self):
        self.tmp.cleanup()

    def test_records_public_items(self):
        for key in (
            "crate::greet",
            "crate::Point",
            "crate::Point derive Clone",
            "crate::Point::origin",
            "crate::Point impl Draw",
            "crate::Draw",
            "crate::Draw::name",
            "crate::LIMIT",
            "crate::extra",
            "crate::shout!",
            "crate::api",
            "crate::api::call",
            "crate::reexported::Visible",
            "crate::use reexported::Visible",
            "crate::Opaque.id",
            "crate::Open::A",
        ):
            self.assertIn(key, self.keys)

    def test_skips_private_test_and_comment_items(self):
        for needle in (
            "crate_only",
            "in_doc",
            "in_string",
            "test_helper",
            "test_only",
            "private",
            "invisible",
            "not_listed",
            "deep",
            "secret",
        ):
            self.assertNotIn(needle, self.text)

    def test_signatures_are_normalized(self):
        lines = dict(line.split("\t", 1) for line in self.lines)
        self.assertEqual(lines["crate::greet"], "pub fn greet(name: &str) -> String")
        self.assertEqual(lines["crate::Wrapper"], "pub struct Wrapper(pub u8, _)")
        self.assertEqual(lines["crate::LIMIT"], "pub const LIMIT: usize")
        self.assertEqual(lines["crate::extra"], '#[cfg(feature = "extra")] pub fn extra()')
        self.assertEqual(lines["crate::Draw"], "pub trait Draw { fn draw(&self) }")
        self.assertEqual(
            lines["crate::Shape"], "pub enum Shape { Circle(f64), Square { side: f64 } }"
        )


def git(repo: Path, *args: str) -> str:
    env = dict(
        os.environ,
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@localhost",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@localhost",
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
    )
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, capture_output=True, text=True, env=env
    ).stdout.strip()


class GitEndToEndTests(unittest.TestCase):
    """Real commits in a throwaway repository, classified through main()."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        git(self.repo, "init", "-q")
        self.write("rust/Cargo.toml", '[workspace]\nmembers = ["crates/demo"]\n')
        self.write(
            "rust/crates/demo/Cargo.toml",
            '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\n',
        )
        self.write("rust/crates/demo/src/lib.rs", "pub fn run(x: u8) -> u8 { x }\n")
        self.base = self.commit("base")

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, rel: str, text: str) -> None:
        path = self.repo / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def commit(self, message: str) -> str:
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", message)
        return git(self.repo, "rev-parse", "HEAD")

    def classify(self, *extra: str) -> str:
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = cc.main([self.base, "HEAD", "--repo", str(self.repo), *extra])
        self.assertEqual(code, 0)
        self.stderr = err.getvalue()
        return out.getvalue()

    def test_body_change_is_local_and_stdout_is_one_value(self):
        self.write("rust/crates/demo/src/lib.rs", "pub fn run(x: u8) -> u8 {\n    x + 1\n}\n")
        self.commit("body")
        self.assertEqual(self.classify(), "L\n")
        self.assertIn("reasons: none", self.stderr)

    def test_default_mode_puts_reasons_on_stderr(self):
        self.write("rust/crates/demo/src/lib.rs", "pub fn run(x: u16) -> u8 { 0 }\n")
        self.commit("sig")
        self.assertEqual(self.classify(), "G\n")
        self.assertIn("1 public item(s) changed", self.stderr)

    def test_added_function_is_interface(self):
        self.write(
            "rust/crates/demo/src/lib.rs",
            "pub fn run(x: u8) -> u8 { x }\npub fn walk() {}\n",
        )
        self.commit("add")
        result = json.loads(self.classify("--json"))
        self.assertEqual(result["class"], "I")
        self.assertEqual(result["crates_compared"], ["demo"])

    def test_changed_signature_is_global(self):
        self.write("rust/crates/demo/src/lib.rs", "pub fn run(x: u16) -> u8 { 0 }\n")
        self.commit("sig")
        text = self.classify("--verbose")
        self.assertTrue(text.startswith("G\n"))
        self.assertIn("1 public item(s) changed", text)

    def test_dependency_and_ci_changes_are_global(self):
        self.write(
            "rust/crates/demo/Cargo.toml",
            '[package]\nname = "demo"\nversion = "0.1.1"\n\n[dependencies]\nserde = "1"\n',
        )
        self.write("ci/gitlab-ci.yml", "stages: [verify]\n")
        self.commit("deps")
        result = json.loads(self.classify("--json"))
        self.assertEqual(result["class"], "G")
        reasons = [r["reason"] for r in result["reasons"]]
        self.assertIn("manifest rust/crates/demo/Cargo.toml: [dependencies] changed", reasons)
        self.assertIn("CI configuration: ci/gitlab-ci.yml", reasons)

    def test_version_bump_alone_is_local(self):
        self.write(
            "rust/crates/demo/Cargo.toml",
            '[package]\nname = "demo"\nversion = "0.2.0"\n\n[dependencies]\n',
        )
        self.commit("bump")
        self.assertEqual(self.classify(), "L\n")


if __name__ == "__main__":
    unittest.main()
