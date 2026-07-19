import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "release_tag_gate", ROOT / "scripts/check-release-tag.py"
)
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)


class ReleaseTagGateTests(unittest.TestCase):
    def fixture_root(self, version="3.9.11"):
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        root = Path(temporary.name)
        (root / "rust").mkdir()
        (root / "packages/pi-lean-ctx").mkdir(parents=True)
        (root / "packages/lean-ctx-bin").mkdir(parents=True)
        (root / "rust/Cargo.toml").write_text(
            f'[package]\nname = "lean-ctx"\nversion = "{version}"\n\n[dependencies]\n',
            encoding="utf-8",
        )
        for relative in GATE.COUPLED_PACKAGES:
            (root / relative).write_text(
                json.dumps({"name": Path(relative).parent.name, "version": version}),
                encoding="utf-8",
            )
        return temporary, root

    def assert_tag_rejected(self, tag):
        temporary, root = self.fixture_root()
        with temporary:
            with self.assertRaises(GATE.InvalidReleaseTag):
                GATE.verify_tag(tag, root)

    def test_current_repository_tag_matches_all_release_versions(self):
        self.assertEqual(GATE.verify_tag("v3.9.12", ROOT), "3.9.12")

    def test_accepts_strict_prerelease_when_every_manifest_matches(self):
        temporary, root = self.fixture_root("3.9.11-rc.1")
        with temporary:
            self.assertEqual(GATE.verify_tag("v3.9.11-rc.1", root), "3.9.11-rc.1")

    def test_rejects_glob_matching_partial_tag(self):
        self.assert_tag_rejected("v3")

    def test_rejects_leading_zero(self):
        self.assert_tag_rejected("v03.9.11")

    def test_rejects_trailing_garbage(self):
        self.assert_tag_rejected("v3.9.11junk")

    def test_rejects_missing_v_prefix(self):
        self.assert_tag_rejected("3.9.11")

    def test_rejects_engine_tag_drift(self):
        self.assert_tag_rejected("v3.9.12")

    def test_rejects_coupled_package_drift(self):
        temporary, root = self.fixture_root()
        with temporary:
            path = root / GATE.COUPLED_PACKAGES[0]
            value = json.loads(path.read_text(encoding="utf-8"))
            value["version"] = "3.9.10"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaises(GATE.InvalidReleaseTag):
                GATE.verify_tag("v3.9.11", root)

    def test_rejects_non_semver_package_version(self):
        temporary, root = self.fixture_root()
        with temporary:
            path = root / GATE.COUPLED_PACKAGES[1]
            value = json.loads(path.read_text(encoding="utf-8"))
            value["version"] = "latest"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaises(GATE.InvalidReleaseTag):
                GATE.verify_tag("v3.9.11", root)


if __name__ == "__main__":
    unittest.main()
