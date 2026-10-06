#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Focused receiving journey; reuses the signed export fixture, no model calls."""
import copy
import datetime as dt
import hashlib
import os
import subprocess
import sys
import json


def receiving_journey(binary, a, b, call, checkpoint, receipt, packaged,
                      settings_a, settings_b, canonical, stamp, checks, private_driver=None):
    def request(package):
        return {"schema_version": 1, "package_digest": package["package_digest"], "package": package["package"]}

    def settings(package, revision):
        result = copy.deepcopy(settings_b)
        result["checkpoint_continue"] = {"schema_version": 1,
            "package_digest": package["package_digest"],
            "object_id": "dddddddd-bbbb-4ccc-8ddd-eeeeeeeeeeee", "revision": revision,
            "expires_at": stamp(dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=60))}
        return result

    def continued(package, revision, **kwargs):
        return call(b, "context-checkpoint-continue", request(package), settings(package, revision), **kwargs)

    def state():
        return {str(path.relative_to(b)): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in (b / "data").rglob("*.json") if "sessions" in path.parts}

    # No copy admission, no session. A current independently supplied grant can
    # select copied notes, without adopting source execution/source-access rights.
    call(b, "context-checkpoint-continue", request(packaged), settings_b, rejected=True)
    first = continued(packaged, 1)
    assert first["continued"] and first["canonical_session_adopted"]
    assert not first["source_ledger_adopted"] and not first["source_rights_transferred"]
    assert "COPIED CONTEXT" in first["context"] and "resume on host b" in first["context"]
    assert state(), "no canonical receiving session persisted"
    before = state()
    repeated = continued(packaged, 1)
    assert repeated["reused"] and repeated["session_id"] == first["session_id"]
    assert state() == before, "idempotent copy changed persisted state"
    checks.append("copied_notes_continue_in_canonical_session_and_retry_is_idempotent")

    expired = settings(packaged, 1)
    expired["checkpoint_continue"]["expires_at"] = stamp(dt.datetime.now(dt.timezone.utc) - dt.timedelta(seconds=1))
    call(b, "context-checkpoint-continue", request(packaged), expired, rejected=True)
    policy = b / "project/.lean-ctx/policy.toml"
    policy.parent.mkdir(exist_ok=True)
    policy.write_text("name='copy'\nversion='1.0.0'\ndescription='fixture'\n[redaction]\ncustomer='K-[0-9]{6}'\n")
    continued(packaged, 1, rejected=True)
    policy.unlink()
    assert state() == before
    checks.append("expired_admission_and_current_masking_withhold_copy_without_session_mutation")

    updated = copy.deepcopy(checkpoint)
    updated["identity"]["checkpoint_id"] = "22222222-2222-4333-8444-555555555555"
    # This fixture signs another independent root checkpoint; the managed object
    # revision advances separately from per-checkpoint device lineage.
    updated["identity"]["device_sequence"] = 1
    updated["live_state"]["next_steps"] = ["receiver second revision"]
    signed = call(a, "context-checkpoint", {"schema_version": 1,
        "checkpoint_json": canonical(updated).decode(), "receipt_digests": [receipt]}, settings_a)
    second_package = call(a, "context-checkpoint-package", {"schema_version": 1,
        "artifact_digest": signed["artifact_digest"], "include_source_evidence": True}, settings_a)
    second = continued(second_package, 2)
    assert second["continued"] and not second["reused"] and second["session_id"] != first["session_id"]
    assert "receiver second revision" in second["context"]
    checks.append("unchanged_receiving_head_advances_to_new_revision")

    # Evidence is persisted alongside the canonical checkpoint but intentionally
    # excluded from its digest. An evidence-only edit must still stop replacement.
    session_file = next((b / "data").rglob(second["session_id"] + ".json"))
    saved = session_file.read_bytes()
    stored = json.loads(saved)
    extra = copy.deepcopy(stored["view"]["evidence"][0])
    extra["key"], extra["value"] = "local_review", "preserve evidence-only note"
    stored["view"]["evidence"].append(extra)
    session_file.write_bytes(canonical(stored))
    edited_bytes = session_file.read_bytes()
    evidence_conflict = continued(packaged, 3)
    assert evidence_conflict["conflict"] and session_file.read_bytes() == edited_bytes
    session_file.write_bytes(saved)
    checks.append("evidence_only_local_change_prevents_fast_forward")

    env = {**os.environ, "LEAN_CTX_DATA_DIR": str(b / "data"),
           "XDG_CONFIG_HOME": str(b / "config"), "XDG_STATE_HOME": str(b / "state"),
           "XDG_CACHE_HOME": str(b / "cache"), "DO_NOT_TRACK": "1",
           "__LEAN_CTX_NO_DAEMON": "1"}
    edited = subprocess.run([str(binary), "session", "task", "preserve my local task"],
        cwd=b / "project", env=env, capture_output=True, timeout=30)
    assert edited.returncode == 0, "local session edit failed"
    before_conflict = state()
    assert before_conflict != before
    conflict = continued(packaged, 3)
    assert conflict["conflict"] and not conflict["active_context_selected"]
    assert state() == before_conflict, "conflicting remote revision replaced local work"
    checks.append("local_edit_retained_and_remote_revision_reported_as_conflict")
    if private_driver:
        c = b.parent / "host-c"
        c.mkdir(mode=0o700)
        (c / "project").mkdir()
        host = copy.deepcopy(settings_b)
        host["ledger_path"] = str(c / "ledger.jsonl")
        host["checkpoint_import"]["receiving"]["project_root"] = str(c / "project")
        host_file = c / "host.json"
        host_file.write_bytes(canonical(host))
        host_file.chmod(0o600)
        payload = c / "package.json"
        payload.write_bytes(canonical(packaged["package"]))
        payload.chmod(0o600)
        config = c / "continue.json"
        config.write_text(json.dumps({"schema_version": 1, "engine_path": str(binary),
            "engine_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "project_root": str(c / "project"), "host_settings_path": str(host_file)}))
        config.chmod(0o600)
        subprocess.run([sys.executable, str(private_driver), str(payload), str(config), str(c)],
                       check=True, timeout=180)
        checks.append("private_paid_fetch_over_https_keychain_to_engine_copy_and_revocation_expiry")
