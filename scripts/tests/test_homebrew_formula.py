# SPDX-License-Identifier: Apache-2.0
"""scripts/homebrew-formula.py: the tap formula ships a bottle for every
platform, so Homebrew pours the signed release binary instead of treating the
formula as a source build that demands Command Line Tools on macOS (F57)."""

import contextlib
import hashlib
import importlib.util
import io
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("homebrew_formula", ROOT / "scripts/homebrew-formula.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

VERSION = "3.12.8"
RELEASE_URL = f"https://github.com/yvgude/lean-ctx/releases/download/v{VERSION}"
TARGETS = tuple(MODULE.BOTTLE_TAGS)
WORKFLOW = ROOT / ".github/workflows/release.yml"


def _tar_gz(path, files, directories=()):
    with tarfile.open(path, "w:gz") as tar:
        for name in directories:
            info = tarfile.TarInfo(name)
            info.type = tarfile.DIRTYPE
            tar.addfile(info)
        for name, data in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o755
            tar.addfile(info, io.BytesIO(data))


def _sums(path, files):
    path.write_text(
        "".join(f"{hashlib.sha256(f.read_bytes()).hexdigest()}  {f.name}\n" for f in files),
        encoding="utf-8",
    )


class HomebrewFormulaTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.inputs = self.tmp / "in"
        self.inputs.mkdir()
        engines, cockpits = [], []
        for target in TARGETS:
            engine = self.inputs / f"lean-ctx-{target}.tar.gz"
            _tar_gz(engine, {"lean-ctx": f"engine {target}".encode()})
            engines.append(engine)
            cockpit = self.inputs / f"lean-ctx-cockpit-{target}.tar.gz"
            _tar_gz(
                cockpit,
                {f"lean-ctx-cockpit/{n}": f"{n} {target}".encode() for n in MODULE.COCKPIT_FILES},
                directories=("lean-ctx-cockpit",),
            )
            cockpits.append(cockpit)
        _sums(self.inputs / "SHA256SUMS", engines)
        _sums(self.inputs / "COCKPIT_SHA256SUMS", cockpits)

    def _bottles(self, out):
        return MODULE.main([
            "bottles", "--version", VERSION,
            "--engine-dir", str(self.inputs), "--cockpit-dir", str(self.inputs),
            "--sha256sums", str(self.inputs / "SHA256SUMS"),
            "--cockpit-sha256sums", str(self.inputs / "COCKPIT_SHA256SUMS"),
            "--out", str(out),
        ])

    def _formula(self, out):
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            MODULE.main([
                "formula", "--version", VERSION, "--release-url", RELEASE_URL,
                "--sha256sums", str(self.inputs / "SHA256SUMS"),
                "--cockpit-sha256sums", str(self.inputs / "COCKPIT_SHA256SUMS"),
                "--bottle-sha256sums", str(out / MODULE.BOTTLE_SUMS),
            ])
        return stdout.getvalue()

    def test_bottle_unpacks_to_the_keg_the_formula_installs(self):
        out = self.tmp / "out"
        self.assertEqual(self._bottles(out), 0)
        for target, tag in MODULE.BOTTLE_TAGS.items():
            bottle = out / f"lean-ctx-{VERSION}.{tag}.bottle.tar.gz"
            with tarfile.open(bottle, "r:gz") as tar:
                members = {m.name: m for m in tar.getmembers()}
                prefix = f"lean-ctx/{VERSION}"
                binary = members[f"{prefix}/bin/lean-ctx"]
                self.assertTrue(binary.isfile())
                self.assertEqual(binary.mode, 0o755)
                self.assertEqual(tar.extractfile(binary).read(), f"engine {target}".encode())
                for name in MODULE.COCKPIT_FILES:
                    member = members[f"{prefix}/share/lean-ctx/cockpit/{name}"]
                    self.assertEqual(tar.extractfile(member).read(), f"{name} {target}".encode())
                self.assertTrue(all(n == "lean-ctx" or n.startswith("lean-ctx/") for n in members))
                self.assertTrue(all(not m.issym() and not m.islnk() for m in members.values()))

    def test_bottles_are_reproducible(self):
        first, second = self.tmp / "a", self.tmp / "b"
        self._bottles(first)
        self._bottles(second)
        for path in first.iterdir():
            self.assertEqual(path.read_bytes(), (second / path.name).read_bytes(), path.name)

    def test_bottle_refuses_an_archive_that_does_not_match_its_checksum(self):
        _tar_gz(self.inputs / f"lean-ctx-{TARGETS[0]}.tar.gz", {"lean-ctx": b"swapped"})
        with self.assertRaises(SystemExit):
            self._bottles(self.tmp / "out")

    def test_bottle_refuses_a_malformed_cockpit_layout(self):
        cases = {
            "missing file": {f"lean-ctx-cockpit/{n}": b"x" for n in MODULE.COCKPIT_FILES[1:]},
            "extra file": {
                **{f"lean-ctx-cockpit/{n}": b"x" for n in MODULE.COCKPIT_FILES},
                "lean-ctx-cockpit/extra": b"x",
            },
            "wrong directory": {f"cockpit/{n}": b"x" for n in MODULE.COCKPIT_FILES},
        }
        cockpit = self.inputs / f"lean-ctx-cockpit-{TARGETS[0]}.tar.gz"
        cockpits = [self.inputs / f"lean-ctx-cockpit-{t}.tar.gz" for t in TARGETS]
        for label, files in cases.items():
            with self.subTest(label):
                _tar_gz(cockpit, files)
                _sums(self.inputs / "COCKPIT_SHA256SUMS", cockpits)
                with self.assertRaises(SystemExit) as raised:
                    self._bottles(self.tmp / label.replace(" ", "-"))
                self.assertIn("unexpected Cockpit bundle layout", str(raised.exception))

    def test_bottle_refuses_a_symlink_in_an_archive(self):
        engine = self.inputs / f"lean-ctx-{TARGETS[0]}.tar.gz"
        with tarfile.open(engine, "w:gz") as tar:
            info = tarfile.TarInfo("lean-ctx")
            info.type = tarfile.SYMTYPE
            info.linkname = "/bin/sh"
            tar.addfile(info)
        _sums(self.inputs / "SHA256SUMS", [self.inputs / f"lean-ctx-{t}.tar.gz" for t in TARGETS])
        with self.assertRaises(SystemExit) as raised:
            self._bottles(self.tmp / "out")
        self.assertIn("is not a regular file", str(raised.exception))

    def test_public_formula_texts_use_the_product_name(self):
        out = self.tmp / "out"
        self._bottles(out)
        formula = self._formula(out)
        # desc (brew info/search) and caveats are public texts.
        self.assertIn('desc "Local LeanCTX Context Gateway for AI systems"', formula)
        self.assertIn("lean-ctx is licensed under Apache-2.0.", formula)
        self.assertNotIn("engine", formula.lower())

    def test_formula_lists_a_bottle_for_every_platform(self):
        out = self.tmp / "out"
        self._bottles(out)
        formula = self._formula(out)
        bottle_sums = MODULE.read_sums(out / MODULE.BOTTLE_SUMS)
        block = re.search(r"\n  bottle do\n(.*?)\n  end\n", formula, re.S)
        self.assertIsNotNone(block)
        self.assertIn(f'root_url "{RELEASE_URL}"', block.group(1))
        for tag in MODULE.BOTTLE_TAGS.values():
            sha = bottle_sums[f"lean-ctx-{VERSION}.{tag}.bottle.tar.gz"]
            self.assertIn(f'sha256 cellar: :any_skip_relocation, {tag}: "{sha}"', block.group(1))
        # One bottle per architecture covers every newer macOS; Linux needs both.
        self.assertEqual(
            set(MODULE.BOTTLE_TAGS.values()),
            {"arm64_monterey", "monterey", "arm64_linux", "x86_64_linux"},
        )

    def test_formula_needs_no_build_tools(self):
        out = self.tmp / "out"
        self._bottles(out)
        formula = self._formula(out)
        self.assertNotIn("=> :build", formula)
        self.assertNotIn("system ", formula)
        self.assertNotIn('depends_on "rust"', formula)
        self.assertNotIn("xcode", formula.lower())
        engine_sums = MODULE.read_sums(self.inputs / "SHA256SUMS")
        cockpit_sums = MODULE.read_sums(self.inputs / "COCKPIT_SHA256SUMS")
        for target in TARGETS:
            self.assertIn(f'url "{RELEASE_URL}/lean-ctx-{target}.tar.gz"', formula)
            self.assertIn(f'sha256 "{engine_sums[f"lean-ctx-{target}.tar.gz"]}"', formula)
            self.assertIn(f'sha256 "{cockpit_sums[f"lean-ctx-cockpit-{target}.tar.gz"]}"', formula)
        self.assertIn(f'assert_match "lean-ctx {VERSION}", shell_output("#{{bin}}/lean-ctx --version")', formula)

    @unittest.skipUnless(shutil.which("ruby"), "ruby not installed")
    def test_formula_is_valid_ruby(self):
        out = self.tmp / "out"
        self._bottles(out)
        path = self.tmp / "lean-ctx.rb"
        path.write_text(self._formula(out), encoding="utf-8")
        subprocess.run(["ruby", "-c", str(path)], check=True, capture_output=True)

    @unittest.skipUnless(shutil.which("brew"), "Homebrew not installed")
    def test_homebrew_downloads_the_published_bottle_name(self):
        """Read-only: Homebrew resolves the formula to a published bottle on
        every macOS from Monterey on, without installing anything."""
        out = self.tmp / "out"
        self._bottles(out)
        formula = self.tmp / "lean-ctx.rb"
        formula.write_text(self._formula(out), encoding="utf-8")
        probe = self.tmp / "probe.rb"
        probe.write_text(
            f'p = Pathname("{formula}")\n'
            'f = Formulary.from_contents("lean-ctx", p, p.read)\n'
            "%i[arm64_tahoe arm64_sequoia arm64_sonoma arm64_monterey tahoe sequoia monterey].each do |t|\n"
            "  tag = Utils::Bottles::Tag.from_symbol(t)\n"
            "  spec = f.bottle_specification\n"
            '  abort "no bottle for #{t}" unless spec.tag?(tag)\n'
            "end\n"
            "puts f.bottle.url\n",
            encoding="utf-8",
        )
        env = {**os.environ, "HOMEBREW_NO_AUTO_UPDATE": "1", "HOMEBREW_NO_ENV_HINTS": "1"}
        result = subprocess.run(
            ["brew", "ruby", str(probe)], capture_output=True, text=True, env=env, timeout=120
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        url = result.stdout.strip().splitlines()[-1]
        self.assertTrue(url.startswith(f"{RELEASE_URL}/"), url)
        self.assertIn(url.rsplit("/", 1)[1], {p.name for p in out.glob("*.bottle.tar.gz")})

    def test_formula_refuses_a_missing_bottle(self):
        out = self.tmp / "out"
        self._bottles(out)
        sums = out / MODULE.BOTTLE_SUMS
        sums.write_text("".join(sums.read_text().splitlines(True)[1:]), encoding="utf-8")
        with self.assertRaises(SystemExit):
            self._formula(out)

    def test_release_workflow_publishes_bottles_before_the_release(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        bottles = workflow.split("\n  homebrew-bottles:\n", 1)[1].split("\n  publish-release:\n", 1)[0]
        self.assertIn("scripts/homebrew-formula.py bottles", bottles)
        self.assertIn("cosign verify-blob", bottles)
        self.assertIn("gh release upload", bottles)
        publish = workflow.split("\n  publish-release:\n", 1)[1].split("\n  jetbrains-plugin:\n", 1)[0]
        self.assertIn("needs: [release, cockpit-assets, homebrew-bottles]", publish)
        self.assertIn("needs.homebrew-bottles.result == 'success'", publish)
        update = workflow.split("\n  update-homebrew:\n", 1)[1].split("\n  announce-twitter:\n", 1)[0]
        self.assertIn("scripts/homebrew-formula.py formula", update)
        self.assertIn("sha256sum --check --strict HOMEBREW_BOTTLES_SHA256SUMS", update)
        self.assertNotIn("class LeanCtx", workflow)

    @staticmethod
    def _job(workflow, name, following):
        return workflow.split(f"\n  {name}:\n", 1)[1].split(f"\n  {following}:\n", 1)[0]

    def test_release_workflow_pins_exact_signer_identities(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        release_id = (
            '--certificate-identity "https://github.com/${GITHUB_REPOSITORY}'
            '/.github/workflows/release.yml@${GITHUB_REF}"'
        )
        cockpit_id = (
            '--certificate-identity "https://github.com/${COCKPIT_REPOSITORY}'
            '/.github/workflows/cockpit-release.yml@refs/heads/main"'
        )
        # A regexp identity would accept a signature of the workflow on any ref.
        self.assertNotIn("--certificate-identity-regexp", workflow)
        cockpit = self._job(workflow, "cockpit-assets", "homebrew-bottles")
        bottles = self._job(workflow, "homebrew-bottles", "publish-release")
        self.assertEqual(cockpit.count(cockpit_id), 1)
        self.assertEqual(bottles.count(release_id), 1)
        self.assertEqual(bottles.count(cockpit_id), 1)
        check = '[[ "$COCKPIT_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]'
        for job in (cockpit, bottles):
            # The repository variable is validated before cosign uses it.
            self.assertLess(job.index(check), job.index("cosign verify-blob"))

    def test_formula_digests_come_from_the_verifying_job(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        bottles = self._job(workflow, "homebrew-bottles", "publish-release")
        for output in ("engine_sha256sums", "cockpit_sha256sums", "bottle_sha256sums"):
            self.assertIn(f"{output}: ${{{{ steps.bottles.outputs.{output} }}}}", bottles)
        # Assets are replaced only while the release is a draft.
        self.assertLess(bottles.index("--json isDraft"), bottles.index("--clobber"))
        update = self._job(workflow, "update-homebrew", "announce-twitter")
        self.assertIn("needs: [publish-release, homebrew-bottles]", update)
        self.assertIn("needs.homebrew-bottles.outputs.bottle_sha256sums", update)
        self.assertIn("needs.homebrew-bottles.outputs.engine_sha256sums", update)
        self.assertIn("needs.homebrew-bottles.outputs.cockpit_sha256sums", update)
        # No checksum file is downloaded again from the release.
        download = [l for l in update.splitlines() if "gh release download" in l]
        self.assertEqual(len(download), 1)
        self.assertNotIn("SHA256SUMS", download[0])
        self.assertIn("'*.bottle.tar.gz'", download[0])


if __name__ == "__main__":
    unittest.main()
