# SPDX-License-Identifier: Apache-2.0
"""The macOS Homebrew bottles (Air test F57) and the formula that names them.

Without a bottle for the running macOS, Homebrew takes the build-from-source
path and refuses outdated Command Line Tools before the formula's install step
runs. These tests pin what scripts/homebrew-bottles.py builds and that the
formula release.yml writes carries its bottle block.
"""

import gzip
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "homebrew-bottles.py"
WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"
COCKPIT_FILES = ("archive.tar.gz", "channel.json", "channel.sig", "manifest.json", "manifest.sig")


def load_script():
    spec = importlib.util.spec_from_file_location("homebrew_bottles", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def tarball(path, members, mtime=1_700_000_000):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w:gz") as archive:
        for name, data, mode in members:
            if data is None:
                info = tarfile.TarInfo(name)
                info.type, info.mode, info.mtime = tarfile.DIRTYPE, 0o755, mtime
                archive.addfile(info)
                continue
            info = tarfile.TarInfo(name)
            info.size, info.mode, info.mtime = len(data), mode, mtime
            archive.addfile(info, io.BytesIO(data))
    path.write_bytes(raw.getvalue())


def release_assets(dist, mtime=1_700_000_000):
    for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
        tarball(dist / f"lean-ctx-{target}.tar.gz", [("lean-ctx", f"engine {target}".encode(), 0o755)], mtime)
        members = [("lean-ctx-cockpit/", None, 0)]
        members += [(f"lean-ctx-cockpit/{name}", f"{target} {name}".encode(), 0o644) for name in COCKPIT_FILES]
        tarball(dist / f"lean-ctx-cockpit-{target}.tar.gz", members, mtime)


class HomebrewBottleTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.dist = self.tmp / "dist"
        self.dist.mkdir()
        release_assets(self.dist)
        self.hb = load_script()

    def test_bottles_hold_exactly_the_keg_under_the_download_name(self):
        bottles = self.hb.build("3.12.8", self.dist, self.tmp / "out")
        self.assertEqual([tag for tag, _, _ in bottles], ["arm64_big_sur", "big_sur"])
        for (tag, filename, sha), target in zip(bottles, ("aarch64-apple-darwin", "x86_64-apple-darwin")):
            # Homebrew downloads <name>-<version>.<tag>.bottle.tar.gz from a root_url.
            self.assertEqual(filename, f"lean-ctx-3.12.8.{tag}.bottle.tar.gz")
            data = (self.tmp / "out" / filename).read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), sha)
            with tarfile.open(fileobj=io.BytesIO(data)) as archive:
                files = {m.name: m for m in archive.getmembers() if m.isfile()}
                self.assertEqual(
                    sorted(files),
                    ["lean-ctx/3.12.8/bin/lean-ctx"]
                    + [f"lean-ctx/3.12.8/share/lean-ctx/cockpit/{n}" for n in sorted(COCKPIT_FILES)],
                )
                self.assertEqual(files["lean-ctx/3.12.8/bin/lean-ctx"].mode, 0o755)
                self.assertEqual(
                    archive.extractfile(files["lean-ctx/3.12.8/bin/lean-ctx"]).read(),
                    f"engine {target}".encode(),
                )
                for member in archive.getmembers():
                    self.assertTrue(member.name.startswith("lean-ctx"), member.name)
                    self.assertEqual(member.mtime, 0)

    def test_same_release_archives_give_the_same_digests(self):
        first = self.hb.build("3.12.8", self.dist, self.tmp / "a")
        other = self.tmp / "dist2"
        other.mkdir()
        release_assets(other, mtime=1_800_000_000)
        self.assertEqual(first, self.hb.build("3.12.8", other, self.tmp / "b"))
        gz = (self.tmp / "a" / first[0][1]).read_bytes()
        self.assertEqual(gz[4:8], b"\0\0\0\0")  # gzip header mtime

    def test_refuses_an_engine_archive_with_more_than_the_binary(self):
        tarball(self.dist / "lean-ctx-x86_64-apple-darwin.tar.gz",
                [("lean-ctx", b"x", 0o755), ("extra", b"y", 0o644)])
        with self.assertRaisesRegex(ValueError, "exactly the lean-ctx binary"):
            self.hb.build("3.12.8", self.dist, self.tmp / "out")

    def test_refuses_a_cockpit_archive_missing_a_signed_file(self):
        tarball(self.dist / "lean-ctx-cockpit-aarch64-apple-darwin.tar.gz",
                [(f"lean-ctx-cockpit/{n}", b"x", 0o644) for n in COCKPIT_FILES[:-1]])
        with self.assertRaisesRegex(ValueError, "must hold exactly"):
            self.hb.build("3.12.8", self.dist, self.tmp / "out")

    def test_refuses_a_version_that_is_not_a_release(self):
        with self.assertRaisesRegex(ValueError, "not a release version"):
            self.hb.build("3.12.8/../x", self.dist, self.tmp / "out")

    def test_cli_writes_the_bottle_block(self):
        block = self.tmp / "block.rb"
        result = subprocess.run(
            ["python3", str(SCRIPT), "--version", "3.12.8", "--dist", str(self.dist),
             "--out", str(self.tmp / "out"), "--block", str(block)],
            capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        text = block.read_text()
        self.assertIn('  bottle do\n    root_url "https://github.com/yvgude/lean-ctx/releases/download/v3.12.8"\n', text)
        self.assertRegex(text, r'    sha256 cellar: :any_skip_relocation, arm64_big_sur: "[0-9a-f]{64}"\n')
        self.assertRegex(text, r'    sha256 cellar: :any_skip_relocation, big_sur: "[0-9a-f]{64}"\n  end\n$')


class ReleaseFormulaTests(unittest.TestCase):
    def render_formula(self, block):
        """Run the formula heredoc of release.yml as the job does, with fixed inputs."""
        text = WORKFLOW.read_text()
        start = text.index("          cat > Formula/lean-ctx.rb <<EOF\n")
        end = text.index("          sed -i 's/^          //' Formula/lean-ctx.rb\n", start)
        heredoc = "\n".join(line[10:] if line.startswith(" " * 10) else line
                            for line in text[start:end].splitlines())
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, tmp)
        (tmp / "Formula").mkdir()
        (tmp / "block.rb").write_text(block)
        env = {"PATH": os.environ["PATH"], "VERSION": "3.12.8", "RELEASE_URL": "https://example.invalid"}
        for name in ("AARCH64_DARWIN_SHA", "X86_64_DARWIN_SHA", "AARCH64_LINUX_SHA", "X86_64_LINUX_SHA",
                     "COCKPIT_AARCH64_DARWIN_SHA", "COCKPIT_X86_64_DARWIN_SHA",
                     "COCKPIT_AARCH64_LINUX_SHA", "COCKPIT_X86_64_LINUX_SHA"):
            env[name] = "0" * 64
        script = 'BOTTLE_BLOCK="$(cat block.rb)"\n' + heredoc + "\n" \
                 "sed 's/^          //' Formula/lean-ctx.rb > Formula/out.rb\n"
        subprocess.run(["bash", "-c", script], cwd=tmp, env=env, check=True)
        return (tmp / "Formula" / "out.rb").read_text()

    def test_formula_carries_the_bottle_block_at_class_level(self):
        block = ('  bottle do\n    root_url "https://github.com/yvgude/lean-ctx/releases/download/v3.12.8"\n'
                 f'    sha256 cellar: :any_skip_relocation, arm64_big_sur: "{"a" * 64}"\n'
                 f'    sha256 cellar: :any_skip_relocation, big_sur: "{"b" * 64}"\n  end\n')
        formula = self.render_formula(block)
        self.assertIn("\n" + block.rstrip("\n") + "\n", formula)
        self.assertLess(formula.index("  bottle do\n"), formula.index("  on_macos do\n"))
        if shutil.which("ruby"):
            check = subprocess.run(["ruby", "-c"], input=formula, capture_output=True, text=True)
            self.assertEqual(check.returncode, 0, check.stderr)

    def test_job_attaches_and_reads_back_bottles_before_the_formula(self):
        text = WORKFLOW.read_text()
        job = text[text.index("  update-homebrew:\n"):text.index("  announce-twitter:\n")]
        self.assertIn("      contents: write", job)
        self.assertIn("sha256sum --check --strict engine.sums cockpit.sums", job)
        self.assertLess(job.index("gh release upload"), job.index("git clone"))
        self.assertLess(job.index("cmp \"$bottle\""), job.index("git push"))
        self.assertIn('echo "::error::the macOS bottle block is missing"; exit 1', job)


if __name__ == "__main__":
    unittest.main()
