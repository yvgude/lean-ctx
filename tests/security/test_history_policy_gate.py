import hashlib
import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("history_gate", ROOT / "scripts/history-policy-gate.py")
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)


class HistoryPolicyGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp.name)
        self.git("init", "-q")
        self.git("config", "user.email", "test@leanctx.invalid")
        self.git("config", "user.name", "LeanCTX Test")
        (self.repo / "README.md").write_text("public\n")
        (self.repo / "scanner.py").write_text("# bounded scanner\n")
        self.commit("baseline")
        self.base = self.git("rev-parse", "HEAD").strip()
        self.report_path = self.repo / "baseline.json"
        report = {
            "schema_version": "leanctx.full-history-evidence/v1",
            "audited_commit": self.base,
            "policy_sha256": "0" * 64,
            "scanner_versions": GATE.BASELINE_SCANNER_VERSIONS,
            "counts": {"commits": 1, "objects": 3, "findings": 0},
            "audit_status": "rotation-and-rewrite-decision-pending",
            "current_tree_finding_ids": [],
            "finding_set_sha256": hashlib.sha256(GATE.canonical([])).hexdigest(),
            "findings": [],
        }
        self.report_path.write_bytes(GATE.canonical(report))
        self.policy_path = self.repo / "policy.json"
        self.write_policy(self.base, "0" * 64)
        report["policy_sha256"] = GATE.policy_fingerprint(self.policy())
        self.report_path.write_bytes(GATE.canonical(report))
        self.write_policy(self.base, hashlib.sha256(self.report_path.read_bytes()).hexdigest())

    def tearDown(self):
        self.temp.cleanup()

    def git(self, *args):
        result = subprocess.run(["git", "-C", str(self.repo), *args], capture_output=True, text=True, check=True)
        return result.stdout

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def write_policy(self, commit, digest, report="baseline.json"):
        policy = {
            "schema_version": "leanctx.history-policy/v1",
            "limits": {"max_commits": 50, "max_objects": 500, "max_findings": 20, "command_timeout_seconds": 10},
            "forbidden_paths": [{"id": "IP001", "prefix": "private/"}],
            "secret_rule": {"id": "SEC001", "pickaxe_regex": "sk_" + "live_|PRIVATE " + "KEY-----"},
            "policy_source_path": "policy.json",
            "scanner_source_paths": ["policy.json", "scanner.py"],
            "scanner_source_sha256": {"scanner.py": hashlib.sha256((self.repo / "scanner.py").read_bytes()).hexdigest()},
            "baseline": {"commit": commit, "report": report, "report_sha256": digest},
        }
        self.policy_path.write_bytes(GATE.canonical(policy))

    def policy(self):
        return GATE.load_policy(self.policy_path)

    def test_full_audit_is_bounded_and_redacted(self):
        secret = "sk_" + "live_DO_NOT_PRINT_THIS_VALUE"
        (self.repo / "leak.txt").write_text(secret + "\n")
        (self.repo / "private").mkdir()
        (self.repo / "private/data.txt").write_text("internal\n")
        self.commit("introduce fixtures")
        report = GATE.full_audit(self.repo, self.policy())
        encoded = GATE.canonical(report)
        self.assertGreaterEqual(len(report["findings"]), 2)
        self.assertNotIn(secret.encode(), encoded)
        self.assertTrue(all(set(item) == {"id", "scanner", "rule", "path", "object"} for item in report["findings"]))

    def test_clean_current_and_delta_pass(self):
        report = GATE.gate(self.repo, self.policy())
        self.assertEqual(report["findings"], [])

    def test_current_and_delta_secret_fail_closed(self):
        (self.repo / "leak.txt").write_text("sk_" + "live_DO_NOT_PRINT_THIS_VALUE\n")
        self.commit("secret delta")
        report = GATE.gate(self.repo, self.policy())
        scanners = {item["scanner"] for item in report["findings"]}
        self.assertEqual(scanners, {"current-secret", "delta-secret"})
        self.assertNotIn(b"DO_NOT_PRINT_THIS_VALUE", GATE.canonical(report))

    def test_current_and_delta_private_path_fail_closed(self):
        (self.repo / "private").mkdir()
        (self.repo / "private/data.txt").write_text("internal\n")
        self.commit("private delta")
        scanners = {item["scanner"] for item in GATE.gate(self.repo, self.policy())["findings"]}
        self.assertEqual(scanners, {"current-path", "delta-path"})

    def test_missing_or_modified_baseline_fails(self):
        self.report_path.unlink()
        with self.assertRaises(GATE.GateError):
            GATE.gate(self.repo, self.policy())
        self.report_path.write_text("{}\n")
        with self.assertRaises(GATE.GateError):
            GATE.gate(self.repo, self.policy())

    def test_unreachable_baseline_fails(self):
        self.write_policy("0" * 40, hashlib.sha256(self.report_path.read_bytes()).hexdigest())
        with self.assertRaises(GATE.GateError):
            GATE.gate(self.repo, self.policy())

    def test_policy_rule_drift_fails(self):
        value = json.loads(self.policy_path.read_text())
        value["forbidden_paths"].append({"id": "IP002", "prefix": "new-private/"})
        self.policy_path.write_bytes(GATE.canonical(value))
        with self.assertRaises(GATE.GateError):
            GATE.gate(self.repo, self.policy())

    def test_scanner_version_drift_fails(self):
        report = json.loads(self.report_path.read_text())
        report["scanner_versions"] = ["obsolete/v1"]
        self.report_path.write_bytes(GATE.canonical(report))
        self.write_policy(self.base, hashlib.sha256(self.report_path.read_bytes()).hexdigest())
        with self.assertRaises(GATE.GateError):
            GATE.gate(self.repo, self.policy())

    def test_scanner_source_drift_fails(self):
        (self.repo / "scanner.py").write_text("# modified scanner\n")
        with self.assertRaises(GATE.GateError):
            GATE.gate(self.repo, self.policy())

    def test_scanner_source_path_hash_set_drift_fails(self):
        value = json.loads(self.policy_path.read_text())
        value["scanner_source_paths"].append("missing.py")
        self.policy_path.write_bytes(GATE.canonical(value))
        with self.assertRaises(GATE.GateError):
            GATE.load_policy(self.policy_path)

        value = json.loads(self.policy_path.read_text())
        value["scanner_source_paths"].remove("missing.py")
        value["scanner_source_sha256"]["missing.py"] = "0" * 64
        self.policy_path.write_bytes(GATE.canonical(value))
        with self.assertRaises(GATE.GateError):
            GATE.load_policy(self.policy_path)

    def test_baseline_report_symlink_fails(self):
        target = self.repo / "baseline-target.json"
        self.report_path.rename(target)
        self.report_path.symlink_to(target.name)
        self.write_policy(self.base, hashlib.sha256(target.read_bytes()).hexdigest())
        with self.assertRaises(GATE.GateError):
            GATE.gate(self.repo, self.policy())

    def test_policy_fingerprint_mismatch_fails(self):
        report = json.loads(self.report_path.read_text())
        report["policy_sha256"] = "f" * 64
        self.report_path.write_bytes(GATE.canonical(report))
        self.write_policy(self.base, hashlib.sha256(self.report_path.read_bytes()).hexdigest())
        with self.assertRaises(GATE.GateError):
            GATE.gate(self.repo, self.policy())

    def test_bounds_fail_closed(self):
        policy = self.policy()
        policy["limits"]["max_commits"] = 0
        with self.assertRaises(GATE.GateError):
            GATE.full_audit(self.repo, policy)


if __name__ == "__main__":
    unittest.main()
