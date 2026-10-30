# SPDX-License-Identifier: Apache-2.0
"""The `cargo fast` alias is tracked in the exported crate config."""

from pathlib import Path
import subprocess
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
CRATE_CONFIG = ROOT / "rust/.cargo/config.toml"


class CargoAliasTests(unittest.TestCase):
    def test_crate_config_carries_only_the_fast_alias(self):
        config = tomllib.loads(CRATE_CONFIG.read_text(encoding="utf-8"))
        # No [build] section: a job budget here would override every
        # developer's own Cargo settings.
        self.assertEqual(set(config), {"alias"})
        self.assertIn("--profile dev-fast", config["alias"]["fast"])
        root_config = ROOT / ".cargo/config.toml"
        if root_config.is_file():  # private line only; never exported
            root = tomllib.loads(root_config.read_text(encoding="utf-8"))
            self.assertEqual(config["alias"]["fast"], root["alias"]["fast"])

    def test_crate_config_is_tracked_and_exported(self):
        ignored = subprocess.run(
            ["git", "check-ignore", "-q", "rust/.cargo/config.toml"],
            cwd=ROOT,
            check=False,
        )
        self.assertNotEqual(ignored.returncode, 0, "rust/.cargo/config.toml is gitignored")
        manifest = tomllib.loads((ROOT / "PUBLISH_MANIFEST.toml").read_text(encoding="utf-8"))
        self.assertIn("rust/.cargo/config.toml", manifest["new_paths"])
        self.assertIn("tests/build/test_cargo_alias.py", manifest["new_paths"])

    def test_cargo_fast_resolves_from_the_crate(self):
        result = subprocess.run(
            ["cargo", "fast", "--help"],
            cwd=ROOT / "rust",
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
