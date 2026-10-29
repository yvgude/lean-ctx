# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]
TOOL = SCRIPTS / "export-secret-gate.py"
GATE = SCRIPTS / "history-policy-gate.py"
# Assembled at run time: this file must not itself match the secret rule.
TOKEN = "AK" + "IA" + "X" * 16
# The repository's own policy: its SEC001 rule is the full rule contract the
# gate validates; every other field is the fixture's own (write_policy).
REPOSITORY_POLICY = json.loads((SCRIPTS.parent / "security" / "history-policy-v1.json")
                               .read_text(encoding="utf-8"))


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def blob_id(content: bytes) -> str:
    return hashlib.sha1(b"blob %d\x00" % len(content) + content).hexdigest()


def finding_id(scanner: str, rule: str, path: str, obj: str) -> str:
    return hashlib.sha256("\0".join((scanner, rule, path, obj)).encode()).hexdigest()


class ExportSecretGateTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.tree = Path(self.directory.name) / "export"
        (self.tree / "scripts").mkdir(parents=True)
        (self.tree / "security").mkdir()
        shutil.copy(GATE, self.tree / "scripts" / "history-policy-gate.py")
        self.write("README.md", "public\n")
        self.known = "docs/known.md"
        self.write(self.known, f"rotated long ago {TOKEN}\n")
        self.write_policy([finding_id("current-secret", "SEC001", self.known,
                                      blob_id((self.tree / self.known).read_bytes()))])
        self.report = Path(self.directory.name) / "report.json"

    def write(self, name: str, text: str) -> None:
        path = self.tree / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def write_policy(self, inherited: list[str]) -> None:
        baseline = self.tree / "security" / "baseline.json"
        baseline.write_text(json.dumps({"current_tree_finding_ids": inherited}), encoding="utf-8")
        policy = {
            **REPOSITORY_POLICY,
            "schema_version": "leanctx.history-policy/v1",
            "limits": {"max_commits": 10, "max_objects": 100, "max_findings": 10,
                       "command_timeout_seconds": 30},
            "forbidden_paths": [{"id": "IP001", "prefix": "cloud/"}],
            "policy_source_path": "security/history-policy-v1.json",
            "scanner_source_paths": ["security/history-policy-v1.json",
                                     "scripts/history-policy-gate.py"],
            "scanner_source_sha256": {"scripts/history-policy-gate.py": sha256(GATE)},
            "baseline": {"commit": "0" * 40, "report": "security/baseline.json",
                         "report_sha256": sha256(baseline)},
        }
        (self.tree / "security" / "history-policy-v1.json").write_text(json.dumps(policy),
                                                                      encoding="utf-8")

    def run_gate(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run([sys.executable, str(TOOL), "--tree", str(self.tree),
                               "--output", str(self.report)],
                              check=False, capture_output=True, text=True)

    def test_passes_with_only_the_inherited_finding(self) -> None:
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(self.report.read_text())
        self.assertEqual(report["counts"]["inherited"], 1)
        self.assertEqual(report["findings"], [])

    def test_new_findings_fail_without_disclosing_content(self) -> None:
        cases = {
            "new secret": ("src/config.txt", f"key = {TOKEN}\n", "current-secret"),
            "forbidden path": ("cloud/main.rs", "fn main() {}\n", "current-path"),
            "changed inherited file": (self.known, f"edited {TOKEN}\n", "current-secret"),
        }
        for name, (path, text, scanner) in cases.items():
            with self.subTest(name):
                self.setUp()
                self.write(path, text)
                result = self.run_gate()
                self.assertEqual(result.returncode, 1, name)
                raw = self.report.read_text()
                self.assertNotIn(TOKEN, raw + result.stdout + result.stderr)
                self.assertEqual([(f["scanner"], f["path"]) for f in json.loads(raw)["findings"]],
                                 [(scanner, path)])

    def test_a_candidate_that_is_no_credential_is_not_a_finding(self) -> None:
        # Pickaxe candidates the precise stage rejects, exactly as the history gate does.
        self.write("docs/aws.md", "documented key " + "AK" + "IA" + "IOSFODNN7" + "EXAMPLE\n")
        self.write("docs/prefix.md", "keys start with " + "AK" + "IA" + "\n")
        self.write("docs/db.md", "postgres" + "://app:${DB_PASSWORD}@db:5432/app\n")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(self.report.read_text())["findings"], [])

    def test_a_new_token_in_a_known_path_is_never_a_pass(self) -> None:
        # The path was a finding at the baseline commit; the export carries other bytes.
        history = Path(self.directory.name) / "history"
        (history / "docs").mkdir(parents=True)
        (history / self.known).write_text(f"rotated long ago {TOKEN}\n", encoding="utf-8")
        env = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
               "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null",
               "PATH": __import__("os").environ["PATH"]}
        for args in (("init", "-q"), ("add", "-A"), ("commit", "-q", "-m", "baseline")):
            subprocess.run(["git", "-C", str(history), *args], check=True, env=env,
                           capture_output=True)
        commit = subprocess.run(["git", "-C", str(history), "rev-parse", "HEAD"], check=True,
                                capture_output=True, text=True, env=env).stdout.strip()
        policy_path = self.tree / "security" / "history-policy-v1.json"
        policy = json.loads(policy_path.read_text())
        policy["baseline"]["commit"] = commit
        policy_path.write_text(json.dumps(policy), encoding="utf-8")
        replaced = "AK" + "IA" + "Y" * 16
        self.write(self.known, f"a different credential {replaced}\n")
        result = subprocess.run(
            [sys.executable, str(TOOL), "--tree", str(self.tree), "--history-repo", str(history),
             "--output", str(self.report)], check=False, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        raw = self.report.read_text()
        report = json.loads(raw)
        self.assertEqual(report["result"], "REVIEW_REQUIRED")
        self.assertEqual([f["path"] for f in report["inherited_changed_content"]], [self.known])
        self.assertNotIn(replaced, raw + result.stdout + result.stderr)

    def test_a_content_review_is_consumed_only_when_pinned_and_exact(self) -> None:
        commit = "2" * 40
        first, second = f"edited {TOKEN}", f"and again {TOKEN}"
        changed = f"{first}\nplain\n{second}\n"
        lines = [{"line": 1, "line_sha256": hashlib.sha256(first.encode()).hexdigest()},
                 {"line": 3, "line_sha256": hashlib.sha256(second.encode()).hexdigest()}]

        def review(**changes) -> dict:
            self.write(self.known, changed)
            blob = blob_id(changed.encode())
            policy_file = self.tree / "security" / "history-policy-v1.json"
            spec = __import__("importlib.util").util.spec_from_file_location("g", GATE)
            gate = __import__("importlib.util").util.module_from_spec(spec)
            spec.loader.exec_module(gate)
            row = {"finding_id": finding_id("current-secret", "SEC001", self.known, blob),
                   "scanner": "current-secret", "rule": "SEC001", "path": self.known, "blob": blob,
                   "source_sha256": hashlib.sha256(changed.encode()).hexdigest(),
                   "matching_lines": changes.pop("matching_lines", lines),
                   "disposition": "intentional_detection_pattern_or_synthetic_fixture"}
            value = {"schema": "leanctx.root-secret-finding-content-review/v1",
                     "source_commit": commit, "release_approval": False,
                     "status": "technical_content_review_completed",
                     "policy_fingerprint": gate.policy_fingerprint(gate.load_policy(policy_file)),
                     "findings": [row]}
            value.update(changes)
            return value

        other = {"finding_id": "0" * 64, "scanner": "current-secret", "rule": "SEC001",
                 "path": "docs/other.md", "blob": "0" * 40,
                 "disposition": "intentional_detection_pattern_or_synthetic_fixture"}
        cases = {
            # The review's own release_approval is false and stays irrelevant: accepting it
            # qualifies this gate, the release is authorized elsewhere.
            "exact technical adjudication": (lambda: review(), None, 0),
            "review not completed": (lambda: review(status="draft"), None, 1),
            "one matching line not reviewed": (lambda: review(matching_lines=lines[:1]), None, 1),
            "a reviewed line has other bytes": (
                lambda: review(matching_lines=[lines[0], {"line": 3, "line_sha256": "0" * 64}]),
                None, 1),
            "another source commit": (lambda: review(source_commit="3" * 40), None, 1),
            "another policy": (lambda: review(policy_fingerprint="0" * 64), None, 1),
            "names one finding more": (lambda: review(findings=review()["findings"] + [other]),
                                       None, 1),
            "names no finding": (lambda: review(findings=[]), None, 1),
            "not the pinned file": (lambda: review(), "0" * 64, 1),
        }
        for name, (make, pinned, code) in cases.items():
            with self.subTest(name):
                self.setUp()
                path = Path(self.directory.name) / "review.json"
                path.write_text(json.dumps(make()), encoding="utf-8")
                result = subprocess.run(
                    [sys.executable, str(TOOL), "--tree", str(self.tree), "--output",
                     str(self.report), "--content-review", str(path), "--content-review-sha256",
                     pinned or sha256(path), "--source-commit", commit],
                    check=False, capture_output=True, text=True)
                self.assertEqual(result.returncode, code, name + result.stdout + result.stderr)
                expected = "PASS_REVIEWED" if code == 0 else "FAIL"
                self.assertEqual(json.loads(self.report.read_text())["result"], expected, name)

    def test_refuses_another_gate_than_the_pinned_one_and_symlinks(self) -> None:
        policy_path = self.tree / "security" / "history-policy-v1.json"
        policy = json.loads(policy_path.read_text())
        policy["scanner_source_sha256"]["scripts/history-policy-gate.py"] = "0" * 64
        policy_path.write_text(json.dumps(policy), encoding="utf-8")
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("not the one the policy pins", result.stderr)

        self.setUp()
        (self.tree / "link.md").symlink_to(self.tree / "README.md")
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("symlink", result.stderr)


if __name__ == "__main__":
    unittest.main()
