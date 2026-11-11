#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""CI configuration is never part of a frozen contract hash set (task 595).

A frozen hash of ci/gitlab-ci.yml or a workflow file turns every CI-only change
red in v4-provenance. Contract packs freeze normative content; CI config is
validated by GitLab CI lint and scripts/tests/test_gitlab_runtime_gate.py instead.
"""

import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
CI_PREFIXES = ("ci/", ".github/workflows/", "ee/ci/")
CI_FILES = (".gitlab-ci.yml",)


def frozen_paths(manifest):
    if "sha256" in manifest:
        return set(manifest["sha256"])
    if "files" in manifest:
        return set(manifest["files"])
    return {key for key, value in manifest.items() if isinstance(value, str)}


def is_ci_config(path):
    return path.startswith(CI_PREFIXES) or path in CI_FILES


class FrozenHashesExcludeCiTests(unittest.TestCase):
    def test_no_frozen_manifest_pins_ci_config(self):
        manifests = sorted((ROOT / "docs/contracts").rglob("frozen-hashes.json"))
        self.assertTrue(manifests)
        for manifest_path in manifests:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            pinned = sorted(p for p in frozen_paths(manifest) if is_ci_config(p))
            self.assertEqual(pinned, [], "%s freezes CI config" % manifest_path.relative_to(ROOT))

    def test_ci_only_change_keeps_seat_value_frozen_gate_green(self):
        spec = importlib.util.spec_from_file_location(
            "verify_team_seat_value_v1", ROOT / "scripts/verify-team-seat-value-v1.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        pack_rel = Path("docs/contracts/team-seat-value-v1")
        manifest = json.loads((ROOT / pack_rel / "frozen-hashes.json").read_text(encoding="utf-8"))
        with tempfile.TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            for rel in (list(manifest["sha256"]) + list(manifest["registry_fragment_sha256"])
                        + [str(pack_rel / "frozen-hashes.json")]):
                target = tmp_root / rel
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / rel, target)
            for rel in ("ci/gitlab-ci.yml", ".github/workflows/ci.yml"):
                target = tmp_root / rel
                target.parent.mkdir(parents=True, exist_ok=True)
                # A CI-only change outside the pinned verifier lines (task 595 plus the
                # ci_fragment_sha256 pin from ci/gate-stability) keeps the gate green.
                target.write_text((ROOT / rel).read_text(encoding="utf-8") + "# CI-only change\n",
                                  encoding="utf-8")
            module.ROOT = tmp_root
            module.PACK = tmp_root / pack_rel
            module.verify_frozen()
            # The gate still bites on a normative file.
            schema = tmp_root / pack_rel / "common.schema.json"
            schema.write_bytes(schema.read_bytes() + b"\n")
            with self.assertRaises(Exception):
                module.verify_frozen()


if __name__ == "__main__":
    unittest.main()
