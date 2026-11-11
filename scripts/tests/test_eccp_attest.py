#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""scripts/eccp-attest: in-toto Statement, DSSE signing, verification and risk budget."""

import base64
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "scripts" / "eccp-attest"

_loader = importlib.machinery.SourceFileLoader("eccp_attest", str(TOOL))
_spec = importlib.util.spec_from_loader("eccp_attest", _loader)
attest = importlib.util.module_from_spec(_spec)
_loader.exec_module(attest)

try:
    import cryptography  # noqa: F401

    HAVE_CRYPTO = True
except ImportError:  # pragma: no cover - depends on the host
    HAVE_CRYPTO = False

HEAD = "a" * 40


def envelope(checks, change_class="L"):
    return {
        "schema": "leanctx.change-envelope/v1",
        "head": HEAD,
        "intent": {"task": "157", "plan_url": None, "acceptance": ["scripts/tests/test_x.py"]},
        "base": {"ref": "origin/main", "commit": "b" * 40},
        "class": change_class,
        "read_set": {"crates": [], "toolchain": {}},
        "derivations": [],
        "evidence": {"checks": [{"name": n, "result": r, "commit": HEAD} for n, r in checks]},
    }


class DecisionTests(unittest.TestCase):
    def statement(self, checks, rates, change_class="L", budgets=None):
        return attest.build_statement(envelope(checks, change_class), rates, budgets or dict(attest.DEFAULT_BUDGETS))

    def test_accept_within_budget(self):
        self.assertEqual(attest.parse_escape_rates(None), {})
        rates = {"unit": {"rate": 0.02, "escapes": 2, "runs": 100}}
        st = self.statement([("unit", "pass"), ("clippy", "pass")], rates)
        acc = st["predicate"]["acceptance"]
        self.assertEqual(acc["decision"], "accept")
        self.assertEqual(acc["residual_risk"], 0.02)
        checks = {c["name"]: c for c in st["predicate"]["checks"]}
        self.assertEqual(checks["unit"]["escape_rate"], 0.02)
        self.assertEqual(checks["unit"]["escape_sample"], {"escapes": 2, "runs": 100})
        self.assertIsNone(checks["clippy"]["escape_rate"])
        self.assertEqual(st["subject"][0]["digest"]["gitCommit"], HEAD)
        self.assertEqual(st["predicate"]["change_envelope"]["sha256"], attest.digest(envelope([("unit", "pass"), ("clippy", "pass")])))

    def test_escalate_over_budget_and_unknown_rate(self):
        st = self.statement([("unit", "pass")], {"unit": {"rate": 0.02, "escapes": None, "runs": None}}, "G")
        self.assertEqual(st["predicate"]["acceptance"]["decision"], "escalate")
        st = self.statement([("unit", "pass")], {})
        self.assertEqual(st["predicate"]["acceptance"]["residual_risk"], 1.0)
        self.assertEqual(st["predicate"]["acceptance"]["decision"], "escalate")

    def test_null_class_uses_strictest_budget(self):
        st = self.statement([("unit", "pass")], {"unit": {"rate": 0.005, "escapes": None, "runs": None}}, None)
        acc = st["predicate"]["acceptance"]
        self.assertEqual(acc["effective_class"], "G")
        self.assertEqual(acc["decision"], "escalate")

    def test_failing_check_rejects(self):
        st = self.statement([("unit", "pass"), ("clippy", "fail")], {"unit": {"rate": 0.0, "escapes": 0, "runs": 5}})
        self.assertEqual(st["predicate"]["acceptance"]["decision"], "reject")

    def test_budget_and_rate_parsing(self):
        self.assertEqual(attest.parse_budgets(["L=0.2"])["L"], 0.2)
        with self.assertRaises(attest.UsageError):
            attest.parse_budgets(["X=0.1"])
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
            json.dump({"unit": {"escapes": 3, "runs": 4}, "e2e": 0.1}, fh)
        self.addCleanup(os.unlink, fh.name)
        rates = attest.parse_escape_rates(fh.name)
        self.assertEqual(rates["unit"]["rate"], 0.75)
        self.assertEqual(rates["e2e"]["rate"], 0.1)

    def test_pae_matches_dsse_spec(self):
        self.assertEqual(attest.pae("http://example.com/HelloWorld", b"hello world"),
                         b"DSSEv1 29 http://example.com/HelloWorld 11 hello world")


@unittest.skipUnless(HAVE_CRYPTO, "python cryptography not installed")
class SignVerifyTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="eccp-attest-")
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.key = self.dir / "key.pem"
        self.run_tool("keygen", "--out", str(self.key))
        self.assertEqual(self.key.stat().st_mode & 0o777, 0o600)
        self.env_file = self.dir / "env.json"
        self.env_file.write_text(json.dumps(envelope([("unit", "pass")])))
        self.rates = self.dir / "rates.json"
        self.rates.write_text(json.dumps({"unit": {"escapes": 1, "runs": 100}}))

    def run_tool(self, *args, code=0, env=None):
        proc = subprocess.run([sys.executable, str(TOOL), *args], capture_output=True, text=True,
                              env={**os.environ, **(env or {})})
        self.assertEqual(proc.returncode, code, proc.stderr)
        return proc

    def sign(self, *extra):
        out = self.dir / "att.json"
        self.run_tool("sign", "--envelope", str(self.env_file), "--escape-rates", str(self.rates),
                      "--key", str(self.key), "--out", str(out), *extra)
        return out

    def test_round_trip(self):
        out = self.sign()
        dsse = json.loads(out.read_text())
        self.assertEqual(dsse["payloadType"], "application/vnd.in-toto+json")
        report = json.loads(self.run_tool("verify", str(out), "--pubkey", f"{self.key}.pub",
                                          "--envelope", str(self.env_file), "--require-accept").stdout)
        self.assertTrue(report["valid"])
        self.assertEqual(report["decision"], "accept")
        self.assertTrue(report["envelope_checked"])

    def test_key_from_env(self):
        out = self.dir / "att.json"
        self.run_tool("sign", "--envelope", str(self.env_file), "--out", str(out), env={"ECCP_ATTEST_KEY_FILE": str(self.key)})
        self.run_tool("verify", str(out), env={"ECCP_ATTEST_PUBKEY_FILE": f"{self.key}.pub"})
        self.run_tool("verify", str(out), "--require-accept", code=1,
                      env={"ECCP_ATTEST_PUBKEY_FILE": f"{self.key}.pub"})  # unknown rate -> escalate

    def test_missing_key_is_usage_error(self):
        env = {k: v for k, v in os.environ.items() if k != "ECCP_ATTEST_KEY_FILE"}
        proc = subprocess.run([sys.executable, str(TOOL), "sign", "--envelope", str(self.env_file)],
                              capture_output=True, text=True, env=env)
        self.assertEqual(proc.returncode, 2)

    def test_tampered_payload_fails(self):
        out = self.sign()
        dsse = json.loads(out.read_text())
        st = json.loads(base64.b64decode(dsse["payload"]))
        st["predicate"]["acceptance"]["decision"] = "accept"
        st["predicate"]["checks"][0]["result"] = "fail"
        dsse["payload"] = base64.b64encode(json.dumps(st).encode()).decode()
        out.write_text(json.dumps(dsse))
        self.run_tool("verify", str(out), "--pubkey", f"{self.key}.pub", code=1)

    def test_wrong_key_fails(self):
        out = self.sign()
        other = self.dir / "other.pem"
        self.run_tool("keygen", "--out", str(other))
        self.run_tool("verify", str(out), "--pubkey", f"{other}.pub", code=1)

    def test_envelope_binding(self):
        out = self.sign()
        changed = envelope([("unit", "pass"), ("extra", "pass")])
        other = self.dir / "other-env.json"
        other.write_text(json.dumps(changed))
        self.run_tool("verify", str(out), "--pubkey", f"{self.key}.pub", "--envelope", str(other), code=1)

    def test_git_notes(self):
        repo = self.dir / "repo"
        repo.mkdir()
        genv = {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "t",
                "GIT_AUTHOR_EMAIL": "t@example.invalid", "GIT_COMMITTER_NAME": "t",
                "GIT_COMMITTER_EMAIL": "t@example.invalid", "HOME": str(self.dir)}
        env = {**os.environ, **genv}

        def g(*a, **kw):
            return subprocess.run(["git", *a], cwd=repo, env=env, check=True, capture_output=True, text=True, **kw)

        g("init", "-q", "-b", "main")
        (repo / "f").write_text("x\n")
        g("add", "f")
        g("commit", "-q", "-m", "c")
        head = g("rev-parse", "HEAD").stdout.strip()
        data = envelope([("unit", "pass")])
        data["head"] = head
        g("notes", "--ref=eccp", "add", "-F", "-", head, input=json.dumps(data))
        proc = subprocess.run([sys.executable, str(TOOL), "sign", "--key", str(self.key), "--note",
                               "--escape-rates", str(self.rates)], cwd=repo, env=env, capture_output=True, text=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        proc = subprocess.run([sys.executable, str(TOOL), "verify", "--note", "HEAD", "--pubkey", f"{self.key}.pub"],
                              cwd=repo, env=env, capture_output=True, text=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        report = json.loads(proc.stdout)
        self.assertEqual(report["head"], head)
        self.assertTrue(report["envelope_checked"])


if __name__ == "__main__":
    unittest.main()
