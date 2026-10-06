#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Actual two-host checkpoint CLI gate; synthetic inputs, no model calls."""
import argparse
import copy
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import subprocess
import tempfile


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--openssl", required=True)
    parser.add_argument("--result", required=True, type=Path)
    parser.add_argument("--source-batch", action="store_true",
                        help="Exercise signed source-plan evidence instead of a native read")
    parser.add_argument("--continue-copy", action="store_true",
                        help="Run only the changed receiving continuation journey")
    parser.add_argument("--roundtrip-driver", type=Path)
    parser.add_argument("--roundtrip-driver-sha256")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == args.sha256
    assert bool(args.roundtrip_driver) == bool(args.roundtrip_driver_sha256)
    if args.roundtrip_driver:
        assert hashlib.sha256(args.roundtrip_driver.read_bytes()).hexdigest() == args.roundtrip_driver_sha256
    checks = []
    now = dt.datetime.now(dt.timezone.utc)
    stamp = lambda time: time.strftime("%Y-%m-%dT%H:%M:%SZ")

    def key(seed, name):
        private = bytes([seed]) * 32
        # Standard Ed25519 PKCS#8 / SPKI encodings; OpenSSL derives the public key.
        public_der = subprocess.run(
            [args.openssl, "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
            input=bytes.fromhex("302e020100300506032b657004220420") + private,
            capture_output=True, check=True, timeout=10,
        ).stdout
        assert public_der[:12] == bytes.fromhex("302a300506032b6570032100")
        public = public_der[12:]
        assert len(public) == 32
        return private.hex(), public.hex(), {
            "key_id": name, "public_key_digest": digest(public),
            "admitted_at": stamp(now - dt.timedelta(days=1)),
            "expires_at": stamp(now + dt.timedelta(days=1)), "revoked_at": None,
        }

    with tempfile.TemporaryDirectory(prefix="leanctx-transfer-cli-") as tmp:
        root = Path(tmp).resolve()
        a, b = root / "host-a", root / "host-b"
        for path in (a, b):
            path.mkdir(mode=0o700)
            (path / "project").mkdir()
        scope = {"project_id": "transfer-project-1", "tenant_id": "tenant-1",
                 "workspace_id": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"}
        private_a, public_a, signer_a = key(41, "host-transfer-key-a")
        private_b, _, signer_b = key(42, "host-transfer-key-b")
        settings_a = {"schema_version": 1, "signing_key_hex": private_a,
                      "signer": signer_a, "ledger_path": str(a / "ledger.jsonl"),
                      "allow_checkpoint_signing": True, "allow_checkpoint_transfer_export": True}
        settings_b = {"schema_version": 1, "signing_key_hex": private_b,
                      "signer": signer_b, "ledger_path": str(b / "ledger.jsonl"),
                      "checkpoint_import": {"schema_version": 1, "source_public_key_hex": public_a,
                          "source_signer": signer_a, "admit_source_scope": scope,
                          "receiving": {"project_root": str(b / "project"), **scope},
                          "allow_cross_scope_adoption": False}}

        def call(host, command, request, settings, rejected=False, view=False, raw=None):
            request_path = host / "request.json"
            request_path.write_bytes(canonical(request) if raw is None else raw)
            env = dict(os.environ)
            env.update({"LEAN_CTX_DATA_DIR": str(host / "data"),
                        "XDG_CONFIG_HOME": str(host / "config"),
                        "XDG_STATE_HOME": str(host / "state"),
                        "XDG_CACHE_HOME": str(host / "cache"),
                        "DO_NOT_TRACK": "1"})
            argv = [str(binary), "engine", command]
            stdin = canonical(settings)
            if command == "context-checkpoint-continue":
                stdin += b"\n" + canonical(request)
            elif command == "context-sources-receipt-v2":
                argv += ["--project-root", str(host / "project"), "--host-stdin"]
                stdin += b"\n" + canonical(request)
            elif view:
                argv += ["--project-root", str(host / "project"), "--json-file", str(request_path)]
            else:
                argv += ["--json", str(request_path)]
            if command not in ("context-plan-sources", "context-sources-receipt-v2"):
                argv += ["--host-stdin"]
            result = subprocess.run(argv, input=stdin, cwd=host / "project",
                                    env=env, capture_output=True, timeout=30)
            if rejected:
                assert result.returncode != 0, f"{command} unexpectedly accepted"
                for canary in (b"K-482193", b"K-731842", b"CONFIDENTIAL"):
                    assert canary not in result.stdout + result.stderr, "rejection disclosed content"
                return None
            assert result.returncode == 0, f"{command} failed: {result.stderr.decode(errors='replace')[:500]}"
            return json.loads(result.stdout)

        task_id, plan_id = "transfer-task-1", "transfer-plan-1"
        capability = "capability://leanctx/context-optimization"
        source_text = "// K-731842\npub fn retained(v: i64) -> i64 { v.saturating_add(7) }\n"
        (a / "project/sample.rs").write_text(source_text)
        view_request = {
            "schema_version": 1, "transport_version": 1, "engine_interface_version": "1.0.0",
            "path": "sample.rs", "mode": "aggressive",
            "task": {"schema_version": 1, "task_id": task_id, "trace_id": "transfer-trace-1",
                     "project_id": scope["project_id"], "session_id": "transfer-session-1",
                     "agent_id": "transfer-agent-1", "complexity": "unknown",
                     "created_at": "2026-01-01T00:00:00Z", "tenant_id": scope["tenant_id"]},
            "plan": {"schema_version": 1, "plan_id": plan_id, "task_id": task_id,
                     "context_budget_tokens": 10000, "context_strategy": "minimal", "knowledge_refs": [],
                     "capability_ids": [capability], "model": "local-native", "provider": "local-native",
                     "reasoning_allocation_milli": 0, "max_retries": 0, "fallback_refs": [],
                     "stop_condition": "on_completion", "expected_cost_micros": 0,
                     "expected_quality_milli": 0, "expected_latency_ms": 30000,
                     "policy_decision_ref": "policy:engine-transport-v1:admitted",
                     "capability_bindings": [{"capability_id": capability, "version": "1.0.0"}]},
        }
        if args.source_batch:
            source_ref = "object:sample-source-1"
            source_plan = {"planning": {
                "schema_version": 1, "transport_version": 1, "engine_interface_version": "1.0.0",
                "task_id": task_id, "query": "retained saturating_add", "budget_tokens": 10000,
                "max_candidates": 8}, "sources": [{"descriptor": {
                    "object_ref": source_ref, "source_id": "fixture-files",
                    "source_type": "filesystem", "content_digest": digest(source_text.encode()),
                    "revision": None, "owner": None, "observed_at": stamp(now - dt.timedelta(minutes=1)),
                    "valid_until": None, "classification": "Public", "permission": "permitted"},
                    "content": source_text}]}
            planned = call(a, "context-plan-sources", source_plan, settings_a, view=True)
            assert [item["object_ref"] for item in planned["source_bindings"]] == [source_ref]
            settings_a["allow_context_decision_signing"] = True
            execution = call(a, "context-sources-receipt-v2", {
                "schema_version": 1, "transport_version": 1, "engine_interface_version": "1.0.0",
                "task": view_request["task"], "plan": view_request["plan"],
                "materialization": {"source_plan": source_plan,
                    "expected_binding_digest": planned["binding_digest"]}}, settings_a)
            receipt = execution["execution"]["canonical_receipt"]["receipt_digest"]
            checks.append("source_plan_execution_receipt_drives_receiver_dependencies")
        else:
            view = call(a, "context-view-receipt", view_request, settings_a, view=True)
            receipt = view["canonical_receipt"]["receipt_digest"]
            source_ref = view["engine"]["recovery"]["source_ref"]
        checkpoint = {
            "schema_version": 1,
            "identity": {"checkpoint_id": "11111111-2222-4333-8444-555555555555",
                         "parent_checkpoint_id": None, "branch_id": "main",
                         "device_id": "device-a", "device_sequence": 1},
            "lineage": {**scope, "task_id": task_id, "plan_id": plan_id, "receipt_ids": [],
                        "context_ir_digest": None, "hosted_index_digest": None,
                        "evidence_refs": [], "knowledge_refs": [], "gotcha_refs": [], "snapshot_refs": []},
            "live_state": {"schema_version": 1,
                "task": {"task_id": task_id, "title": "continue transfer K-482193 CONFIDENTIAL", "status": "in_progress", "plan_id": plan_id},
                "progress": {"completed_steps": 1, "total_steps": 2, "confidence_milliunits": 500, "summary": "halfway"},
                "decisions": [], "findings": [], "next_steps": ["resume on host b"],
                "handoff_summary": "CONFIDENTIAL", "files": [], "profile_id": None, "policy_pins": [], "package_pins": []},
            "carrier": None, "engine_version": "4.0.0", "created_at": "2026-08-23T12:00:00Z",
            "updated_at": "2026-08-23T12:00:00Z",
        }
        signed = call(a, "context-checkpoint", {"schema_version": 1,
            "checkpoint_json": canonical(checkpoint).decode(), "receipt_digests": [receipt]}, settings_a)
        packaged = call(a, "context-checkpoint-package", {"schema_version": 1,
            "artifact_digest": signed["artifact_digest"], "include_source_evidence": True}, settings_a)
        assert private_a not in canonical(packaged).decode()
        checks.append("source_receipt_checkpoint_package_without_private_key")
        if args.continue_copy:
            from pro_checkpoint_continue_cli_smoke import receiving_journey
            receiving_journey(binary, a, b, call, checkpoint, receipt, packaged,
                              settings_a, settings_b, canonical, stamp, checks,
                              args.roundtrip_driver)
            args.result.write_text(json.dumps({"passed": True, "binary_sha256": args.sha256,
                "checks": checks, "scope": "Actual Engine canonical receiving-copy continuation; no managed private fetch or background scheduler assertion."}, indent=2) + "\n")
            print(f"PASS: {len(checks)} continuation journey checks")
            return
        def policy(host, text):
            directory = host / "project/.lean-ctx"
            directory.mkdir(exist_ok=True)
            (directory / "policy.toml").write_text(
                "name = 'transfer'\nversion = '1.0.0'\ndescription = 'test'\n" + text)

        policies = [
            "[redaction]\ncustomer = 'K-[0-9]{6}'\n",
            "[filters]\nclassification = 'block'\n",
            "[context]\ndeny_tools = ['ctx_read']\n",
            "[redaction]\ninvalid = '[unterminated'\n",
        ]
        export_dir = a / "data/execution/checkpoint-packages"
        export_before = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in export_dir.iterdir()}
        for index, current in enumerate(policies):
            print(f"checking export policy case {index}", flush=True)
            policy(a, current)
            call(a, "context-checkpoint-package", {"schema_version": 1,
                 "artifact_digest": signed["artifact_digest"], "include_source_evidence": True}, settings_a, rejected=True)
            assert export_before == {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in export_dir.iterdir()}
        (a / "project/.lean-ctx/policy.toml").unlink()
        assert call(a, "context-checkpoint-package", {"schema_version": 1,
                    "artifact_digest": signed["artifact_digest"], "include_source_evidence": True}, settings_a) == packaged
        checks.append("export_rechecks_mask_block_tool_and_invalid_rules_without_rewriting_history")
        request = {"schema_version": 1, "package_digest": packaged["package_digest"], "package": packaged["package"]}
        for index, current in enumerate(policies):
            print(f"checking import policy case {index}", flush=True)
            policy(b, current)
            call(b, "context-checkpoint-import", request, settings_b, rejected=True)
            assert not list((b / "data/execution/checkpoint-packages-staged").glob("*.json"))
        (b / "project/.lean-ctx/policy.toml").unlink()
        checks.append("receiving_rules_reject_before_staging_and_recover_after_policy_repair")
        source_settings = copy.deepcopy(settings_b)
        source_settings["checkpoint_source_files"] = [{
            "source_ref": source_ref, "path": "replica.rs"}]
        replica = b / "project/replica.rs"
        replica.write_text(source_text)
        admitted = call(b, "context-checkpoint-sources", request, source_settings)
        assert admitted["current_source_reads_verified"] and admitted["source_count"] == 1
        assert admitted["automatic_continuation_admitted"] is False
        assert not list((b / "data/execution/checkpoint-packages-staged").glob("*.json"))
        assert not (b / "ledger.jsonl").exists()
        assert not list((b / "data/sessions").glob("**/*.json"))
        call(b, "context-checkpoint-sources", request, settings_b, rejected=True)
        for text in ("changed bytes", ""):
            replica.write_text(text)
            call(b, "context-checkpoint-sources", request, source_settings, rejected=True)
        replica.unlink()
        call(b, "context-checkpoint-sources", request, source_settings, rejected=True)
        replica.write_text(source_text)
        for current in ("[context]\ndeny_tools = ['ctx_read']\n",
                        "[redaction]\ncustomer = 'K-731842'\n"):
            policy(b, current)
            call(b, "context-checkpoint-sources", request, source_settings, rejected=True)
        (b / "project/.lean-ctx/policy.toml").unlink()
        wrong_mapping = copy.deepcopy(source_settings)
        wrong_mapping["checkpoint_source_files"][0]["source_ref"] = "source:unbound"
        call(b, "context-checkpoint-sources", request, wrong_mapping, rejected=True)
        escaped = copy.deepcopy(source_settings)
        escaped["checkpoint_source_files"][0]["path"] = "../request.json"
        call(b, "context-checkpoint-sources", request, escaped, rejected=True)
        assert call(b, "context-checkpoint-sources", request, source_settings) == admitted
        assert not list((b / "data/execution/checkpoint-packages-staged").glob("*.json"))
        checks.append("receiver_reacquires_exact_local_sources_without_promoting_foreign_narrative")
        checks.append("changed_missing_masked_denied_unmapped_and_escaping_sources_rejected")
        if args.roundtrip_driver:
            prepared, received = root / "prepared.json", root / "received.json"
            package_path = a / "data/execution/checkpoint-packages" / (
                packaged["package_digest"].removeprefix("sha256:") + ".json")
            package_bytes = package_path.read_bytes()
            assert digest(package_bytes) == packaged["package_digest"]
            assert json.loads(package_bytes) == packaged["package"]
            with prepared.open("xb") as stream:
                prepared.chmod(0o600)
                stream.write(package_bytes)
            relay = subprocess.run([sys.executable, str(args.roundtrip_driver.resolve()),
                                    str(prepared), str(received)], capture_output=True, timeout=150)
            assert relay.returncode == 0, "private checkpoint roundtrip failed; inspect the driver's retained log"
            assert received.read_bytes() == package_bytes, "checkpoint roundtrip changed canonical bytes"
            request["package"] = json.loads(received.read_bytes())
            checks.append("private_cli_encrypted_roundtrip_preserves_exact_canonical_package")
        legacy = call(a, "context-checkpoint-package", {"schema_version": 1,
            "artifact_digest": signed["artifact_digest"]}, settings_a)
        assert legacy["package"]["schema_version"] == "leanctx.checkpoint-transfer/v1"
        legacy_request = {"schema_version": 1, "package_digest": legacy["package_digest"],
                          "package": legacy["package"]}
        assert call(b, "context-checkpoint-import", legacy_request, settings_b)["staged"]
        call(b, "context-checkpoint-sources", legacy_request, source_settings, rejected=True)
        checks.append("default_v1_manual_transfer_remains_valid_without_source_inspection")
        shutil.rmtree(a)  # Proven fixture-owned source; B must rely solely on the package.
        staged = call(b, "context-checkpoint-import", request, settings_b)
        for name in ("canonical_session_adopted", "legacy_projection_created", "active_context_selected", "source_ledger_adopted"):
            assert staged[name] is False, name
        assert staged["staged"] and staged["source_rights_revalidation_required"]
        stage_dir = b / "data/execution/checkpoint-packages-staged"
        stored = stage_dir / (staged["staged_digest"].removeprefix("sha256:") + ".json")
        assert stored.is_file()
        assert json.loads(stored.read_bytes())["package"] == packaged["package"]
        assert not list((b / "data/sessions").glob("**/*.json"))
        assert not (b / "ledger.jsonl").exists()
        before = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in stage_dir.iterdir()}
        checks.append("independent_host_stages_without_session_after_source_removal")
        bad = copy.deepcopy(request)
        bad["package"]["lineage"]["task_id"] = "forged-task"
        bad["package_digest"] = digest(canonical(bad["package"]))
        call(b, "context-checkpoint-import", bad, settings_b, rejected=True)
        checks.append("declared_lineage_tampering_rejected")
        bad_schema = copy.deepcopy(request)
        bad_schema["package"]["envelope_schema"] = "leanctx.host-checkpoint/unknown"
        bad_schema["package_digest"] = digest(canonical(bad_schema["package"]))
        call(b, "context-checkpoint-import", bad_schema, settings_b, rejected=True)
        checks.append("unknown_envelope_schema_rejected_with_recomputed_checksum")
        bad_signature = copy.deepcopy(request)
        envelope = json.loads(bad_signature["package"]["envelope_json"])
        envelope["signature"] = "A" * 86 + "=="
        envelope_bytes = canonical(envelope)
        bad_signature["package"]["envelope_json"] = envelope_bytes.decode()
        bad_signature["package"]["artifact_digest"] = digest(envelope_bytes)
        bad_signature["package_digest"] = digest(canonical(bad_signature["package"]))
        call(b, "context-checkpoint-import", bad_signature, settings_b, rejected=True)
        checks.append("invalid_signature_rejected_with_recomputed_envelope_and_package_checksums")
        missing_evidence = copy.deepcopy(request)
        missing_evidence["package"]["evidence"].pop()
        missing_evidence["package_digest"] = digest(canonical(missing_evidence["package"]))
        call(b, "context-checkpoint-import", missing_evidence, settings_b, rejected=True)
        checks.append("missing_evidence_rejected_with_recomputed_checksum")
        forged = copy.deepcopy(request)
        altered = False
        for entry in forged["package"]["evidence"]:
            document = json.loads(entry["json"])
            if "source_refs" in document:
                document["source_refs"] = ["source:canonical-path-sha256:" + "a" * 64]
                entry["json"] = canonical(document).decode()
                entry["digest"] = digest(canonical(document))
                altered = True
                break
        assert altered, "fixture must contain signed invocation evidence"
        forged["package_digest"] = digest(canonical(forged["package"]))
        call(b, "context-checkpoint-sources", forged, source_settings, rejected=True)
        checks.append("source_evidence_tampering_rejected_after_recomputed_checksums")
        revoked = copy.deepcopy(settings_b)
        revoked["checkpoint_import"]["source_signer"]["revoked_at"] = stamp(now)
        call(b, "context-checkpoint-import", request, revoked, rejected=True)
        checks.append("revoked_source_pin_rejected")
        call(b, "context-checkpoint-import", {}, settings_b, rejected=True, raw=b"x" * (1024 * 1024 + 1))
        checks.append("oversized_cli_request_rejected")
        assert before == {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in stage_dir.iterdir()}
        assert not list((b / "data/sessions").glob("**/*.json"))
        assert not (b / "ledger.jsonl").exists()
        checks.append("rejections_leave_staged_state_unchanged_and_no_live_session")
    args.result.write_text(json.dumps({"passed": True, "binary_sha256": args.sha256,
        "checks": checks, "fixture_removed": True,
        "source_batch": args.source_batch,
        "scope": "Actual CLI export/import and receiver-owned file reacquisition with current policy. Recorded invocation sources only; no narrative completeness, automatic continuation or full sync acceptance."}, indent=2) + "\n")
    print(json.dumps({"passed": True, "checks": len(checks), "result": str(args.result)}))


if __name__ == "__main__":
    main()
