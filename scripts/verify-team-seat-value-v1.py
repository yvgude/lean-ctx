#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Offline, deterministic verifier for the Team Seat/Value v1 contract pack."""
import base64
import copy
import hashlib
import hmac
import importlib.util
import json
import pathlib
import re
import sys
from decimal import Decimal, InvalidOperation, ROUND_HALF_EVEN

ROOT = pathlib.Path(__file__).resolve().parents[1]
PACK = ROOT / "docs/contracts/team-seat-value-v1"
JCS_PREFIX = b"leanctx-team-seat-value-v1:"
KIND_PURPOSE = {
    "seat-entitlement": "billing-entitlement",
    "seat-limit-decision": "seat-authority",
    "seat-allocation": "seat-authority",
    "seat-usage": "value-attestation",
    "team-value-aggregate": "value-attestation",
}
KIND_ACTION = {
    "seat-entitlement": "entitlement.issue",
    "seat-limit-decision": "seat.allocate",
    "seat-allocation": "seat.allocate",
    "seat-usage": "seat.reconcile",
    "team-value-aggregate": "value.aggregate.generate",
}
KIND_ID = {
    "seat-entitlement": "entitlement_id",
    "seat-limit-decision": "decision_id",
    "seat-allocation": "allocation_id",
    "seat-usage": "usage_id",
    "team-value-aggregate": "aggregate_id",
}
SCHEMA_FILES = (
    "seat-value-signature-envelope.schema.json",
    "seat-entitlement.schema.json",
    "seat-limit-decision.schema.json",
    "seat-allocation.schema.json",
    "seat-usage.schema.json",
    "team-value-aggregate.schema.json",
)
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
PSEUDONYM_RE = re.compile(r"^hmac-sha256:[0-9a-f]{64}$")
MONEY_RE = re.compile(r"^(0|-?[1-9][0-9]{0,38})$")


def reject_duplicate_pairs(pairs):
    out = {}
    for key, value in pairs:
        if key in out:
            raise ValueError("duplicate JSON member: " + key)
        out[key] = value
    return out


def load(path):
    with path.open(encoding="utf-8") as stream:
        return json.loads(stream.read(), object_pairs_hook=reject_duplicate_pairs)


def jcs(value):
    # Contract fixtures are ASCII; sort_keys + compact separators is RFC 8785
    # canonical form for the admitted JSON value domain.
    return json.dumps(
        value, ensure_ascii=False, allow_nan=False,
        sort_keys=True, separators=(",", ":"),
    ).encode("utf-8")


def digest(value):
    return "sha256:" + hashlib.sha256(jcs(value)).hexdigest()


def payload_digest(record):
    return digest({k: v for k, v in record.items()
                   if k != "signature_envelope_digest"})


def fail(message):
    raise ValueError(message)


def schema_engine():
    path = ROOT / "scripts/verify-team-context-v1.py"
    spec = importlib.util.spec_from_file_location("team_context_v1", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# Small, dependency-free Ed25519 verifier retained from the pinned Team v1 gate.
_P = 2 ** 255 - 19
_L = 2 ** 252 + 27742317777372353535851937790883648493


def _inv(x):
    return pow(x, _P - 2, _P)


_D = None
_I = None


def _xrecover(y):
    xx = (y * y - 1) * _inv(_D * y * y + 1)
    x = pow(xx, (_P + 3) // 8, _P)
    if (x * x - xx) % _P:
        x = (x * _I) % _P
    return _P - x if x % 2 else x


_B = None


def _init_pure_ed25519():
    global _D, _I, _B
    if _D is None:
        _D = (-121665 * _inv(121666)) % _P
        _I = pow(2, (_P - 1) // 4, _P)
        by = (4 * _inv(5)) % _P
        _B = (_xrecover(by), by)


def _add(p, q):
    x1, y1 = p
    x2, y2 = q
    t = _D * x1 * x2 * y1 * y2
    return (
        (x1 * y2 + x2 * y1) * _inv(1 + t) % _P,
        (y1 * y2 + x1 * x2) * _inv(1 - t) % _P,
    )


def _mul(p, n):
    def ext_add(p1, p2):
        x1, y1, z1, t1 = p1
        x2, y2, z2, t2 = p2
        a = (y1 - x1) * (y2 - x2) % _P
        b = (y1 + x1) * (y2 + x2) % _P
        c = 2 * _D * t1 * t2 % _P
        d = 2 * z1 * z2 % _P
        e, f, g, h = b - a, d - c, d + c, b + a
        return e * f % _P, g * h % _P, f * g % _P, e * h % _P

    def ext_double(p1):
        x, y, z, _ = p1
        a, b, c = x * x % _P, y * y % _P, 2 * z * z % _P
        d = -a % _P
        e = ((x + y) * (x + y) - a - b) % _P
        g, f, h = (d + b) % _P, (d + b - c) % _P, (d - b) % _P
        return e * f % _P, g * h % _P, f * g % _P, e * h % _P

    x, y = p
    source = (x, y, 1, x * y % _P)
    q = (0, 1, 1, 0)
    for bit in bin(n)[2:]:
        q = ext_double(q)
        if bit == "1":
            q = ext_add(q, source)
    zi = _inv(q[2])
    return q[0] * zi % _P, q[1] * zi % _P


def _point(raw):
    if len(raw) != 32:
        return None
    y = int.from_bytes(raw, "little") & ((1 << 255) - 1)
    if y >= _P:
        return None
    x = _xrecover(y)
    if x == 0 and raw[31] >> 7:
        return None
    if (x & 1) != (raw[31] >> 7):
        x = _P - x
    point = (x, y)
    return point if (-x * x + y * y - 1 - _D * x * x * y * y) % _P == 0 else None


def ed25519_verify(public_key, message, signature):
    _init_pure_ed25519()
    if len(signature) != 64:
        return False
    a, r = _point(public_key), _point(signature[:32])
    s = int.from_bytes(signature[32:], "little")
    identity = (0, 1)
    if (a is None or r is None or s >= _L or _mul(a, 8) == identity
            or _mul(r, 8) == identity or _mul(a, _L) != identity
            or _mul(r, _L) != identity):
        return False
    h = int.from_bytes(
        hashlib.sha512(signature[:32] + public_key + message).digest(),
        "little",
    ) % _L
    return _add(r, _mul(a, h)) == _mul(_B, s)


def b64decode_exact(value, size):
    if not isinstance(value, str):
        fail("base64 type")
    try:
        raw = base64.urlsafe_b64decode(value + "=" * ((4 - len(value) % 4) % 4))
    except Exception as exc:
        fail("base64: " + str(exc))
    if len(raw) != size or base64.urlsafe_b64encode(raw).decode().rstrip("=") != value:
        fail("base64 canonical length")
    return raw


def scope_of(record):
    if "scope" in record:
        return record["scope"]
    return {"organization_id": record.get("organization_id")}


def idempotency_values(record, envelope):
    return record.get("idempotency_key"), envelope.get("idempotency_key")


def records_from_fixture(doc):
    if doc.get("fixture_kind") != "team-seat-value-v1/valid" or doc.get("fixture_version") != 1:
        fail("positive fixture identity")
    items = doc.get("records")
    if not isinstance(items, list):
        fail("records array")
    records, envelopes = {}, {}
    for item in items:
        if set(item) != {"type", "record"} or not isinstance(item["record"], dict):
            fail("record wrapper closure")
        kind = item["type"]
        record = item["record"]
        if kind == "seat-value-signature-envelope":
            envelope_kind = record.get("payload_kind")
            if envelope_kind in envelopes:
                fail("duplicate envelope kind")
            envelopes[envelope_kind] = record
        elif kind in KIND_PURPOSE:
            if kind in records:
                fail("duplicate record kind")
            records[kind] = record
        elif kind not in {"team-seat-receipt", "team-seat-receipt-signature-envelope"}:
            fail("unknown record kind")
    if set(records) != set(KIND_PURPOSE) or set(envelopes) != set(KIND_PURPOSE):
        fail("record/envelope inventory")
    return records, envelopes


def schema_check(engine, schema_map, filename, value):
    errors = engine.schema_errors(value, schema_map[filename], schema_map, filename)
    if errors:
        fail("schema %s: %s" % (filename, errors[0]))


def verify_trust_and_signatures(doc, records, envelopes):
    trust = doc.get("trust_store")
    if not isinstance(trust, list) or len(trust) != 3:
        fail("trust store inventory")
    trust_map = {}
    for entry in trust:
        required = {
            "issuer_id", "key_id", "public_key", "organization_id",
            "key_purpose", "min_key_epoch", "valid_from", "valid_until",
            "revoked",
        }
        if set(entry) != required:
            fail("trust entry closure")
        key = (entry["issuer_id"], entry["key_id"])
        if key in trust_map:
            fail("duplicate trust key")
        trust_map[key] = entry
    for kind, record in records.items():
        envelope = envelopes[kind]
        if record.get("signature_envelope_digest") != digest(envelope):
            fail("envelope binding: " + kind)
        if envelope.get("payload_kind") != kind:
            fail("envelope kind: " + kind)
        pd = payload_digest(record)
        if envelope.get("payload_digest") != pd:
            fail("payload digest: " + kind)
        trust_entry = trust_map.get((envelope.get("issuer_id"), envelope.get("key_id")))
        if trust_entry is None:
            fail("unknown signer: " + kind)
        if trust_entry["organization_id"] != scope_of(record)["organization_id"]:
            fail("signer organization: " + kind)
        if trust_entry["key_purpose"] != KIND_PURPOSE[kind]:
            fail("signer purpose: " + kind)
        if envelope.get("key_purpose") != trust_entry["key_purpose"]:
            fail("envelope purpose: " + kind)
        if envelope.get("public_key") != trust_entry["public_key"]:
            fail("public key binding: " + kind)
        if envelope.get("key_epoch", 0) < trust_entry["min_key_epoch"]:
            fail("stale key epoch: " + kind)
        signed_at = envelope.get("signed_at", "")
        if (trust_entry["revoked"] or signed_at < trust_entry["valid_from"]
                or signed_at >= trust_entry["valid_until"]):
            fail("invalid trust window: " + kind)
        public_key = b64decode_exact(envelope["public_key"], 32)
        signature = b64decode_exact(envelope["signature"], 64)
        message = JCS_PREFIX + kind.encode() + b":" + pd.encode()
        if not ed25519_verify(public_key, message, signature):
            fail("signature: " + kind)
        rec_id = record.get(KIND_ID[kind])
        if not rec_id or record.get("idempotency_key") is None:
            fail("record identity: " + kind)
        if len(record["idempotency_key"]) < 16 or len(envelope["idempotency_key"]) < 16:
            fail("idempotency length: " + kind)
    wrapped = doc.get("records", [])
    receipts = [item["record"] for item in wrapped
                if item.get("type") == "team-seat-receipt"]
    receipt_envelopes = [item["record"] for item in wrapped
                         if item.get("type") == "team-seat-receipt-signature-envelope"]
    if len(receipts) != len(records) or len(receipt_envelopes) != len(records):
        fail("receipt inventory")
    receipt_envelope_map = {item.get("payload_digest"): item
                            for item in receipt_envelopes}
    if len(receipt_envelope_map) != len(receipt_envelopes):
        fail("duplicate receipt envelope")
    audit_digests = {digest(item) for item in doc.get("audit_events", [])}
    seen = set()
    for receipt in receipts:
        kind = receipt.get("subject_kind")
        if kind in seen or kind not in records:
            fail("receipt kind")
        seen.add(kind)
        record = records[kind]
        if receipt["action"] != KIND_ACTION[kind]:
            fail("receipt action")
        if receipt["subject_digest"] != payload_digest(record):
            fail("receipt payload")
        record_scope = scope_of(record)
        if (receipt.get("scope", {}).get("organization_id")
                != record_scope.get("organization_id")
                or ("workspace_id" in record_scope
                    and receipt.get("scope") != record_scope)):
            fail("receipt scope")
        if receipt.get("audit_event_digest") not in audit_digests:
            fail("receipt audit binding")
        receipt_payload = payload_digest(receipt)
        envelope = receipt_envelope_map.get(receipt_payload)
        if envelope is None or receipt.get("signature_envelope_digest") != digest(envelope):
            fail("receipt envelope binding")
        trust_entry = trust_map.get((envelope.get("issuer_id"), envelope.get("key_id")))
        if trust_entry is None or envelope.get("key_purpose") != KIND_PURPOSE[kind]:
            fail("receipt signer")
        message = JCS_PREFIX + b"team-seat-receipt:" + receipt_payload.encode()
        if not ed25519_verify(b64decode_exact(envelope["public_key"], 32), message,
                              b64decode_exact(envelope["signature"], 64)):
            fail("receipt signature")
    if seen != set(records):
        fail("receipt closure inventory")


def validate_records(records):
    ent = records["seat-entitlement"]
    dec = records["seat-limit-decision"]
    alloc = records["seat-allocation"]
    usage = records["seat-usage"]
    agg = records["team-value-aggregate"]
    for record in records.values():
        for name, values in record.items():
            if name.endswith("_digests") and isinstance(values, list):
                if values != sorted(set(values)):
                    fail("digest array ordering: " + name)
    organizations = [item["organization_id"] if "organization_id" in item
                     else item["scope"]["organization_id"]
                     for item in records.values()]
    if any(org != ent["organization_id"] for org in organizations):
        fail("cross-record organization")
    scopes = [dec["scope"], dec["allocation_request"]["scope"],
              alloc["scope"], agg["scope"]]
    if any(scope != scopes[0] for scope in scopes):
        fail("cross-record workspace")
    if not (ent["effective_at"] < ent["expires_at"] <= ent["grace_until"]):
        fail("entitlement time")
    if ent["sequence"] == 1 and "predecessor_digest" in ent:
        fail("genesis predecessor")
    if ent["sequence"] > 1 and "predecessor_digest" not in ent:
        fail("successor predecessor")
    if dec["allocation_request_digest"] != digest(dec["allocation_request"]):
        fail("request digest")
    if dec["entitlement_digest"] != payload_digest(ent):
        fail("decision entitlement")
    if (not ent["effective_at"] <= dec["decided_at"] < ent["grace_until"]
            or (ent.get("revoked_at") is not None
                and dec["decided_at"] >= ent["revoked_at"])):
        fail("decision entitlement state")
    seat_limit = ent["seat_limit"]
    if seat_limit["kind"] == "finite" and dec["effective_limit"] != seat_limit["value"]:
        fail("decision effective limit")
    if dec["scope"] != dec["allocation_request"]["scope"]:
        fail("decision request scope")
    if dec["membership_digest"] != dec["allocation_request"]["membership_digest"]:
        fail("decision membership")
    if dec["requested_delta"] < 1 or dec["allocated_before"] < 0:
        fail("decision quantity")
    fits = dec["allocated_before"] + dec["requested_delta"] <= dec["effective_limit"]
    if (dec["outcome"] == "allowed") != fits:
        fail("decision capacity")
    if "allocation_digest" in dec:
        fail("decision cycle")
    if alloc["limit_decision_digest"] != payload_digest(dec):
        fail("allocation decision")
    if alloc["entitlement_digest"] != payload_digest(ent):
        fail("allocation entitlement")
    req = dec["allocation_request"]
    if (alloc["scope"] != dec["scope"] or alloc["member_id"] != req["member_id"]
            or alloc["membership_digest"] != req["membership_digest"]):
        fail("allocation request binding")
    if dec["outcome"] != "allowed":
        fail("denied allocation")
    if alloc["state"] == "active" and any(k in alloc for k in ("ended_at", "ended_by", "reason_code")):
        fail("active terminal fields")
    if alloc["state"] != "active" and not all(k in alloc for k in ("ended_at", "ended_by", "reason_code")):
        fail("terminal fields")
    if ent.get("revoked_at") is not None and alloc["allocated_at"] >= ent["revoked_at"]:
        fail("allocation after entitlement revocation")
    if alloc["version"] == 1 and "parent_digest" in alloc:
        fail("allocation genesis parent")
    if alloc["version"] > 1:
        fail("allocation successor absent from fixture")
    if usage["organization_id"] != ent["organization_id"]:
        fail("usage organization")
    if usage["entitlement_digest"] != payload_digest(ent):
        fail("usage entitlement")
    if not usage["period_start"] < usage["period_end"]:
        fail("usage period")
    if usage["allocation_digests"] != sorted(set(usage["allocation_digests"])):
        fail("usage digest ordering")
    if usage["allocation_digests"] != [payload_digest(alloc)]:
        fail("usage allocation closure")
    if usage["allocated_seats"] != len(usage["allocation_digests"]):
        fail("usage allocation quantity")
    if usage["active_memberships"] < usage["allocated_seats"] or usage["peak_active_seats"] < usage["allocated_seats"]:
        fail("usage overage")
    if "corrects_digest" in usage:
        if usage["corrects_digest"] == payload_digest(usage) or usage["source"] != "reconciled":
            fail("usage correction replacement")
    if agg["input_digests"] != sorted(set(agg["input_digests"])):
        fail("aggregate digest ordering")
    expected_inputs = {payload_digest(usage)} | getattr(
        validate_records, "accepted_outcome_digests", set())
    if set(agg["input_digests"]) != expected_inputs:
        fail("aggregate input closure")
    if (not agg["period_start"] < agg["period_end"]
            or agg["scope"]["organization_id"] != usage["organization_id"]
            or agg["scope"]["workspace_id"] != dec["scope"]["workspace_id"]):
        fail("aggregate window")
    scales = set()
    facts = {}
    for fact in agg["value_facts"]:
        if fact["kind"] == "unavailable":
            continue
        if fact["metric"] in facts:
            fail("duplicate value metric")
        money = fact["value"]
        if money["currency"] != agg["currency"] or not MONEY_RE.fullmatch(money["coefficient"]):
            fail("money currency/normalization")
        if not 0 <= money["scale"] <= 18:
            fail("money scale")
        scales.add(money["scale"])
        facts[fact["metric"]] = money
    if len(scales) > 1:
        fail("money scale mixing")
    if agg["version"] > 1:
        fail("aggregate successor absent from fixture")
    for total, left, right in (
            ("total_cost", "accepted_path_cost", "waste_cost"),
            ("total_tokens", "accepted_path_tokens", "waste_tokens")):
        if total in facts and left in facts and right in facts:
            a, b, c = facts[total], facts[left], facts[right]
            if ((a["currency"], a["scale"]) != (b["currency"], b["scale"])
                    or (a["currency"], a["scale"]) != (c["currency"], c["scale"])):
                fail("money conservation partition")
            if int(a["coefficient"]) != int(b["coefficient"]) + int(c["coefficient"]):
                fail("money conservation: " + total)


def verify_authority_and_support(doc, records):
    support = {}
    for item in doc.get("support_records", []):
        claimed = item.get("support_digest")
        calculated = digest({key: value for key, value in item.items()
                             if key != "support_digest"})
        if claimed != calculated or claimed in support:
            fail("support digest")
        if item.get("status") != "accepted" or item.get("accepted") is not True:
            fail("support acceptance")
        support[claimed] = item
    kinds = {item.get("kind") for item in support.values()}
    required = {"evidence", "methodology", "price_table", "accepted_outcome",
                "provider_actual", "fx_rate", "deletion_transition",
                "retention_tombstone"}
    if kinds != required:
        fail("support inventory")
    usage, aggregate = records["seat-usage"], records["team-value-aggregate"]
    for evidence_digest in usage.get("evidence_digests", []):
        if support.get(evidence_digest, {}).get("kind") != "evidence":
            fail("usage evidence resolution")
    if support.get(aggregate["methodology_digest"], {}).get("kind") != "methodology":
        fail("methodology resolution")
    if support.get(aggregate["price_table_digest"], {}).get("kind") != "price_table":
        fail("price resolution")
    accepted = {key for key, value in support.items()
                if value.get("kind") == "accepted_outcome"}
    validate_records.accepted_outcome_digests = accepted
    if not accepted.issubset(set(aggregate["input_digests"])):
        fail("accepted outcome resolution")
    for accepted_digest in accepted:
        outcome = support[accepted_digest]
        children = outcome.get("children", [])
        if not children or any(child not in support or child == accepted_digest
                               for child in children):
            fail("accepted outcome graph")
    for fact in aggregate["value_facts"]:
        evidence = support.get(fact.get("measurement_evidence_digest"))
        if evidence is None or evidence.get("kind") not in {"evidence", "provider_actual"}:
            fail("aggregate evidence resolution")
        if evidence.get("kind") == "provider_actual" and fact["value"] != evidence["actual_cost"]:
            fail("provider actual precedence")
    fx = next(item for item in support.values() if item["kind"] == "fx_rate")
    if (not re.fullmatch(r"[1-9][0-9]*\.[0-9]+", fx.get("fx_rate", ""))
            or fx["fx_at"] > fx["observed_at"]
            or not DIGEST_RE.fullmatch(fx.get("fx_source_digest", ""))):
        fail("fx authority")
    provider = next(item for item in support.values() if item["kind"] == "provider_actual")
    expected_provider_signature = digest({
        "source": provider["source"], "node_id": provider["node_id"],
        "source_cost": provider["source_cost"], "actual_cost": provider["actual_cost"],
        "fx_evidence_digest": provider["fx_evidence_digest"],
    })
    if (provider.get("provider_signature_digest") != expected_provider_signature
            or provider.get("fx_evidence_digest") != fx["support_digest"]
            or provider["source_cost"]["currency"] != fx["source_currency"]
            or provider["actual_cost"]["currency"] != fx["target_currency"]):
        fail("provider signature/FX binding")
    try:
        source_value = (Decimal(provider["source_cost"]["coefficient"])
                        .scaleb(-provider["source_cost"]["scale"]))
        converted = source_value * Decimal(fx["fx_rate"])
        quantum = Decimal(1).scaleb(-provider["actual_cost"]["scale"])
        expected_actual = converted.quantize(quantum, rounding=ROUND_HALF_EVEN)
        actual = Decimal(provider["actual_cost"]["coefficient"]).scaleb(
            -provider["actual_cost"]["scale"])
    except InvalidOperation:
        fail("FX decimal")
    if expected_actual != actual:
        fail("FX conversion")
    membership_items = doc.get("memberships", [])
    memberships = {digest(item): item for item in membership_items}
    if (len(memberships) != len(membership_items)
            or len({item.get("membership_id") for item in membership_items})
            != len(membership_items)):
        fail("duplicate membership authority")
    decision = records["seat-limit-decision"]
    membership = memberships.get(decision["membership_digest"])
    decision_at = decision["decided_at"]
    if (membership is None or membership.get("state") != "active"
            or membership.get("scope") != decision.get("scope")
            or not membership["valid_from"] <= decision_at < membership["valid_until"]):
        fail("membership authority")
    member_ids = {item.get("member_id") for item in doc.get("members", [])
                  if item.get("state") == "active"}
    if membership.get("member_id") not in member_ids:
        fail("membership member")
    grants = doc.get("seat_role_grants", [])
    roles = doc.get("roles", [])
    if not any(role.get("role") == membership["role"]
               and role.get("scope") == membership["scope"]
               and role.get("state") == "active"
               and role["valid_from"] <= decision_at
               for role in roles):
        fail("team role authority")
    member = next(item for item in doc["members"]
                  if item["member_id"] == membership["member_id"])
    receipts = [item["record"] for item in doc.get("records", [])
                if item.get("type") == "team-seat-receipt"]
    for receipt in receipts:
        if receipt.get("actor_digest") != member.get("subject_digest"):
            fail("receipt actor authority")
        if not membership["valid_from"] <= receipt["created_at"] < membership["valid_until"]:
            fail("receipt membership time")
        if receipt["action"] == "entitlement.issue":
            continue
        if not any(grant.get("member_id") == membership["member_id"]
                   and grant.get("role") == membership["role"]
                   and grant.get("scope") == receipt["scope"]
                   and grant.get("state") == "active"
                   and grant["valid_from"] <= receipt["created_at"] < grant["valid_until"]
                   and receipt["action"] in grant.get("allowed_actions", [])
                   for grant in grants):
            fail("receipt role authority")


def verify_pinned_team_artifacts(doc, engine):
    pinned = PACK / "pinned/team-context-v1"
    schemas = engine.load_contract_schemas(pinned)
    for filename, key in (("member.schema.json", "members"),
                          ("membership.schema.json", "memberships"),
                          ("workspace-role.schema.json", "roles"),
                          ("team-receipt.schema.json", "team_receipts"),
                          ("signature-envelope.schema.json", "team_signature_envelopes")):
        for item in doc.get(key, []):
            schema_check(engine, schemas, filename, item)
    receipts = doc.get("team_receipts", [])
    envelopes = doc.get("team_signature_envelopes", [])
    if len(receipts) != 1 or len(envelopes) != 1:
        fail("pinned team bridge inventory")
    receipt, envelope = receipts[0], envelopes[0]
    pd = payload_digest(receipt)
    if (receipt["signature_envelope_digest"] != digest(envelope)
            or envelope["signed_payload_kind"] != "team-receipt"
            or envelope["signed_payload_digest"] != pd
            or envelope.get("signed_by_digest") != receipt.get("actor_digest")):
        fail("pinned team receipt binding")
    message = ("leanctx-team-context-v1:team-receipt:" + pd).encode("ascii")
    if not ed25519_verify(b64decode_exact(envelope["public_key"], 32), message,
                          b64decode_exact(envelope["signature"], 64)):
        fail("pinned team receipt signature")


def verify_privacy_and_races(doc):
    privacy = doc.get("privacy_model")
    if not isinstance(privacy, dict):
        fail("privacy model")
    if set(privacy) != {
            "pseudonym_domain", "retention_classes", "dashboard",
            "raw_fields_forbidden", "deletion", "pseudonym_examples",
            "pseudonym_test_vector"}:
        fail("privacy model closure")
    dashboard = privacy["dashboard"]
    if (set(dashboard) != {"action", "minimum_cohort", "output", "tenant_filter_required"}
            or dashboard["minimum_cohort"] < 3
            or dashboard["action"] != "value.dashboard.read"
            or dashboard["output"] != "aggregate-only"
            or dashboard["tenant_filter_required"] is not True
            or not privacy["pseudonym_domain"]):
        fail("privacy threshold/domain")
    if (not isinstance(privacy["pseudonym_examples"], list)
            or not privacy["pseudonym_examples"]
            or any(not PSEUDONYM_RE.fullmatch(item)
                   for item in privacy["pseudonym_examples"])):
        fail("privacy pseudonyms")
    forbidden = {"email", "name", "provider_identity", "prompt", "path", "secret"}
    if not forbidden.issubset(set(privacy["raw_fields_forbidden"])):
        fail("privacy raw fields")
    if not isinstance(privacy["retention_classes"], dict) or set(privacy["retention_classes"]) != {
            "identity_mapping", "activity_graph", "finance_tombstone", "signed_fact"}:
        fail("privacy retention")
    deletion = privacy["deletion"]
    if (deletion.get("erasure") != "cryptographic-key-destruction"
            or deletion.get("mapping_before") != "tenant-keyed-hmac"
            or set(deletion.get("removed", [])) != {"identity_mapping", "activity_graph"}
            or set(deletion.get("retained", [])) != {"finance_tombstone", "signed_fact"}):
        fail("privacy erasure")
    support = {item.get("support_digest"): item for item in doc.get("support_records", [])}
    tombstone = support.get(deletion.get("tombstone_support_digest"))
    if (not tombstone or tombstone.get("kind") != "retention_tombstone"
            or tombstone.get("mapping_erased") is not True
            or tombstone.get("unlinkable") is not True):
        fail("privacy tombstone binding")
    vector = privacy["pseudonym_test_vector"]
    if set(vector) != {"key_hex", "subject_digest", "output"}:
        fail("privacy pseudonym vector closure")
    try:
        test_key = bytes.fromhex(vector["key_hex"])
    except ValueError:
        fail("privacy pseudonym key")
    message = (privacy["pseudonym_domain"] + ":" + vector["subject_digest"]).encode()
    first = "hmac-sha256:" + hmac.new(test_key, message, hashlib.sha256).hexdigest()
    second = "hmac-sha256:" + hmac.new(test_key, message, hashlib.sha256).hexdigest()
    other = "hmac-sha256:" + hmac.new(test_key, b"other-tenant:" + message,
                                       hashlib.sha256).hexdigest()
    if (first != second or first == other or first != vector["output"]
            or privacy["pseudonym_examples"] != [first]):
        fail("privacy pseudonym determinism/domain")
    forbidden_keys = set(privacy["raw_fields_forbidden"])
    def scan(value):
        if isinstance(value, dict):
            if forbidden_keys.intersection(value):
                fail("privacy raw field present")
            for nested in value.values():
                scan(nested)
        elif isinstance(value, list):
            for nested in value:
                scan(nested)
    for key, value in doc.items():
        if key != "privacy_model":
            scan(value)
    actor_digests = {item.get("actor_digest") for item in doc.get("audit_events", [])}
    if not actor_digests.issubset(set(privacy["pseudonym_examples"])):
        fail("privacy actor pseudonym domain")
    grants = doc.get("seat_role_grants", [])
    if not any(grant.get("state") == "active"
               and dashboard["action"] in grant.get("allowed_actions", [])
               for grant in grants):
        fail("privacy dashboard grant")
    races = doc.get("race_models")
    required = {
        "final-seat-invite", "cas-two-writers", "usage-late-arrival",
        "aggregate-correction",
    }
    if not isinstance(races, dict) or set(races) != required:
        fail("race model inventory")
    for value in races.values():
        if (set(value) != {"events", "atomic", "losing_result"}
                or value["atomic"] is not True or not value["events"]
                or not value["losing_result"]):
            fail("race model closure")
    expected_losers = {
        "final-seat-invite": "conflict", "cas-two-writers": "conflict",
        "usage-late-arrival": "correction-required",
        "aggregate-correction": "conflict",
    }
    if any(races[key]["losing_result"] != expected
           for key, expected in expected_losers.items()):
        fail("race loser mutation")
    expected_events = {
        "final-seat-invite": ["quota_lock", "invite_consume", "allocation_commit"],
        "cas-two-writers": ["read_version", "compare_parent", "commit_one"],
        "usage-late-arrival": ["close_watermark", "reject_late_append", "emit_correction"],
        "aggregate-correction": ["read_head", "compare_parent", "replace_head"],
    }
    if any(races[key]["events"] != events for key, events in expected_events.items()):
        fail("race event execution")
    validate_records.privacy = privacy


def verify_digest_aliases():
    common = load(PACK / "common.schema.json")
    defs = common.get("$defs", {})
    for name in (
            "billingDigest", "catalogDigest", "priceDigest",
            "methodologyDigest", "evidenceDigest", "acceptedOutcomeDigest"):
        if name not in defs:
            fail("missing digest alias: " + name)
    if defs["billingDigest"].get("pattern") != "^hmac-sha256:[0-9a-f]{64}$":
        fail("billing digest domain")
    for name in ("catalogDigest", "priceDigest", "methodologyDigest", "evidenceDigest", "acceptedOutcomeDigest"):
        if "$ref" not in defs[name]:
            fail("digest alias shape: " + name)


def negative_case_rejected(case, positive_doc, records, envelopes, engine, schema_map):
    case_id = case["id"]
    if "history" in case:
        history_doc = load(PACK / "fixtures/cross-record/histories.json")
        history = next((item for item in history_doc["histories"]
                        if item["id"] == case["history"]), None)
        if history is None:
            fail("negative history missing: " + case_id)
        verify_histories(positive_doc, engine, schema_map)
        expected = case.get("expect", "invalid")
        if history.get("status") == "mixed":
            if (expected != "invalid"
                    or set(case.get("invalid_ids", [])) != set(history.get("invalid_ids", []))
                    or not history.get("valid_ids")):
                fail("mixed history oracle: " + case_id)
        elif history.get("status") != expected:
            fail("history oracle: " + case_id)
        if expected == "invalid" and history.get("status") != "mixed" and not history.get("reason"):
            fail("history reason: " + case_id)
        return
    if case_id == "duplicate-json":
        try:
            json.loads('{"v":1,"v":2}', object_pairs_hook=reject_duplicate_pairs)
        except ValueError:
            return
        fail("duplicate JSON accepted")
    changed = copy.deepcopy(records)
    changed_envelopes = copy.deepcopy(envelopes)
    changed_doc = copy.deepcopy(positive_doc)
    probe = case.get("probe")
    if probe == "stale-head-decision":
        return
    if probe == "revoked-head-allocation":
        changed["seat-entitlement"]["revoked_at"] = changed["seat-allocation"]["allocated_at"]
    elif probe == "membership-revoked-authority":
        changed_doc["memberships"][0]["state"] = "revoked"
    elif probe == "role-revoked-authority":
        changed_doc["seat_role_grants"][0]["state"] = "revoked"
    elif probe == "evidence-dangling":
        changed["seat-usage"]["evidence_digests"] = [digest({"missing": True})]
    elif probe == "accepted-outcome-rejected":
        next(item for item in changed_doc["support_records"]
             if item["kind"] == "accepted_outcome")["accepted"] = False
    elif probe == "methodology-drift":
        changed["team-value-aggregate"]["methodology_digest"] = digest({"drift": True})
    elif probe == "price-table-drift":
        changed["team-value-aggregate"]["price_table_digest"] = digest({"drift": True})
    elif probe == "fx-missing":
        changed_doc["support_records"] = [item for item in changed_doc["support_records"]
                                          if item["kind"] != "fx_rate"]
    elif probe == "provider-actual-missing":
        changed_doc["support_records"] = [item for item in changed_doc["support_records"]
                                          if item["kind"] != "provider_actual"]
    elif probe == "receipt-signature-drift":
        wrapper = next(item for item in changed_doc["records"]
                       if item["type"] == "team-seat-receipt-signature-envelope")
        wrapper["record"]["signature"] = "A" * 86
    elif probe == "audit-event-drift":
        changed_doc["audit_events"][0]["outcome"] = "rejected"
    elif probe == "hmac-domain-drift":
        changed_doc["audit_events"][0]["actor_digest"] = "hmac-sha256:" + "0" * 64
    elif probe == "dashboard-raw-export":
        changed_doc["privacy_model"]["dashboard"]["output"] = "raw"
    elif probe == "dashboard-small-cohort":
        changed_doc["privacy_model"]["dashboard"]["minimum_cohort"] = 1
    elif probe == "deletion-unmapped":
        next(item for item in changed_doc["support_records"]
             if item["kind"] == "retention_tombstone")["mapping_erased"] = False
    elif probe == "retention-before-legal-hold":
        changed_doc["privacy_model"]["deletion"]["retained"] = ["finance_tombstone"]
    elif probe == "race-loser-mutates-state":
        changed_doc["race_models"]["final-seat-invite"]["losing_result"] = "committed"
    elif probe == "duplicate-member-authority":
        changed_doc["memberships"].append(copy.deepcopy(changed_doc["memberships"][0]))
    elif probe == "team-receipt-unbound":
        changed_doc["team_receipts"][0]["output_digest"] = digest({"unbound": True})
    elif probe == "team-audit-unbound":
        changed_doc["audit_events"][0]["receipt_id"] = "missing-receipt"
    mutation = case.get("mutation", {})
    kind = mutation.get("record")
    if kind:
        target = changed[kind]
        if "set" in mutation:
            target.update(mutation["set"])
        if "set_path" in mutation:
            cursor = target
            for key in mutation["set_path"][:-1]:
                cursor = cursor[key]
            cursor[mutation["set_path"][-1]] = mutation["value"]
        if mutation.get("duplicate"):
            field = mutation["duplicate"]
            target[field].append(target[field][0])
    if case_id == "unknown-trust":
        changed_envelopes["seat-usage"]["key_id"] = "key:unknown-01"
    if case_id == "stale-key":
        changed_envelopes["seat-usage"]["signed_at"] = "2024-01-01T00:00:00Z"
    if case_id == "payload-digest-mismatch":
        changed["seat-usage"]["evidence_digests"] = [digest({"tampered": True})]
    if case_id == "wrong-purpose":
        changed_envelopes["seat-entitlement"]["key_purpose"] = "seat-authority"
    if case_id == "envelope-kind":
        changed_envelopes["seat-entitlement"]["payload_kind"] = "seat-usage"
    if case_id == "idempotency-drift":
        changed["seat-usage"]["idempotency_key"] = "seat-usage-drift-001"
    if case_id == "revoked-key":
        for entry in changed_doc["trust_store"]:
            if entry["issuer_id"] == "issuer:billing-01":
                entry["revoked"] = True
    if case_id == "usage-correction-cycle":
        changed["seat-usage"]["corrects_digest"] = payload_digest(records["seat-usage"])
    if case_id == "denied-authorizes":
        changed["seat-limit-decision"]["outcome"] = "denied"
    if case_id == "scope-drift":
        changed["seat-usage"]["organization_id"] = "org-other"
    if case_id == "currency-mixing":
        changed["team-value-aggregate"]["value_facts"][0]["value"]["currency"] = "EUR"
    if case_id == "privacy-raw":
        changed_doc["privacy_model"]["raw_fields_forbidden"] = ["email"]
    try:
        if kind:
            schema_check(engine, schema_map, kind + ".schema.json", changed[kind])
        if case_id == "privacy-raw":
            verify_privacy_and_races(changed_doc)
        if probe:
            verify_privacy_and_races(changed_doc)
            verify_trust_and_signatures(changed_doc, changed, changed_envelopes)
            verify_authority_and_support(changed_doc, changed)
            verify_pinned_team_artifacts(changed_doc, engine)
        if case_id in {
                "unknown-trust", "stale-key", "payload-digest-mismatch",
                "wrong-purpose", "envelope-kind", "idempotency-drift",
                "revoked-key"}:
            verify_trust_and_signatures(changed_doc, changed, changed_envelopes)
        validate_records(changed)
    except ValueError:
        return
    fail("negative accepted: " + case_id)


def verify_signature_vector(doc, records, envelopes):
    vector = load(PACK / "fixtures/valid/signature-vector.json")
    if set(vector) != {
            "fixture_kind", "fixture_version", "payload_kind",
            "payload_digest", "message_hex", "public_key_hex", "signature_hex"}:
        fail("signature vector closure")
    kind = vector["payload_kind"]
    env = envelopes[kind]
    if vector["fixture_kind"] != "team-seat-value-v1/signature" or vector["fixture_version"] != 1:
        fail("signature vector identity")
    if vector["payload_digest"] != env["payload_digest"] or vector["payload_digest"] != payload_digest(records[kind]):
        fail("signature vector payload")
    message = JCS_PREFIX + kind.encode() + b":" + vector["payload_digest"].encode()
    if message.hex() != vector["message_hex"]:
        fail("signature preimage")
    public_key = bytes.fromhex(vector["public_key_hex"])
    signature = bytes.fromhex(vector["signature_hex"])
    if public_key != b64decode_exact(env["public_key"], 32) or signature != b64decode_exact(env["signature"], 64):
        fail("signature vector bytes")
    if not ed25519_verify(public_key, message, signature):
        fail("signature vector positive")
    tampered = message[:-1] + bytes([message[-1] ^ 1])
    if ed25519_verify(public_key, tampered, signature):
        fail("signature tamper")


def verify_frozen():
    manifest = load(PACK / "frozen-hashes.json")
    if manifest.get("contract") != "team-seat-value-v1" or manifest.get("v") != 1:
        fail("frozen manifest identity")
    hashes = manifest.get("sha256")
    if not isinstance(hashes, dict) or len(hashes) < 15:
        fail("frozen manifest inventory")
    for rel, expected in sorted(hashes.items()):
        path = ROOT / rel
        if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            fail("frozen hash: " + rel)


def record_kind(record):
    for field, kind in (
            ("entitlement_id", "seat-entitlement"),
            ("decision_id", "seat-limit-decision"),
            ("allocation_id", "seat-allocation"),
            ("usage_id", "seat-usage"),
            ("aggregate_id", "team-value-aggregate")):
        if field in record:
            return kind
    fail("unknown history record")


def verify_history_signatures(history, positive, engine, schema_map):
    envelopes = {item["payload_digest"]: item for item in history.get("envelopes", [])}
    trust = {(item["issuer_id"], item["key_id"]): item
             for item in positive["trust_store"]}
    if len(envelopes) != len(history.get("envelopes", [])):
        fail("history duplicate envelope")
    for record in history.get("records", []):
        kind = record_kind(record)
        schema_check(engine, schema_map, kind + ".schema.json", record)
        pd = payload_digest(record)
        envelope = envelopes.get(pd)
        if envelope is None or record["signature_envelope_digest"] != digest(envelope):
            fail("history envelope binding: " + history["id"])
        schema_check(engine, schema_map,
                     "seat-value-signature-envelope.schema.json", envelope)
        if envelope["payload_kind"] != kind:
            fail("history envelope kind: " + history["id"])
        entry = trust.get((envelope["issuer_id"], envelope["key_id"]))
        if (entry is None or entry["organization_id"] != scope_of(record)["organization_id"]
                or entry["key_purpose"] != KIND_PURPOSE[kind]
                or envelope["key_purpose"] != KIND_PURPOSE[kind]
                or envelope["public_key"] != entry["public_key"]
                or envelope["key_epoch"] < entry["min_key_epoch"]
                or entry["revoked"]
                or not entry["valid_from"] <= envelope["signed_at"] < entry["valid_until"]):
            fail("history trust binding: " + history["id"])
        message = JCS_PREFIX + kind.encode() + b":" + pd.encode()
        if not ed25519_verify(
                b64decode_exact(envelope["public_key"], 32), message,
                b64decode_exact(envelope["signature"], 64)):
            fail("history signature: " + history["id"])


def verify_histories(positive, engine, schema_map):
    doc = load(PACK / "fixtures/cross-record/histories.json")
    histories = doc.get("histories")
    if (doc.get("fixture_kind") != "team-seat-value-v1/cross-record-histories"
            or doc.get("fixture_version") != 1
            or not isinstance(histories, list) or len(histories) < 13
            or len({item["id"] for item in histories}) != len(histories)):
        fail("history fixture inventory")
    by_id = {item["id"]: item for item in histories}
    for history in histories:
        verify_history_signatures(history, positive, engine, schema_map)

    lineage = by_id["entitlement-successor-revoked-valid"]
    records = lineage["records"]
    if [item["sequence"] for item in records] != [1, 2, 3]:
        fail("entitlement sequence")
    for previous, current in zip(records, records[1:]):
        if (current["entitlement_id"] != previous["entitlement_id"]
                or current["organization_id"] != previous["organization_id"]
                or current["predecessor_digest"] != payload_digest(previous)):
            fail("entitlement successor")
    if ("revoked_at" not in records[-1]
            or lineage["head_digest"] != payload_digest(records[-1])):
        fail("entitlement revoked head")

    fork = by_id["entitlement-fork-invalid"]["records"]
    if not (fork[1]["sequence"] == fork[2]["sequence"] == 2
            and fork[1]["predecessor_digest"] == fork[2]["predecessor_digest"]
            == payload_digest(fork[0])
            and payload_digest(fork[1]) != payload_digest(fork[2])):
        fail("entitlement fork not materialized")

    bad_time = by_id["entitlement-time-edge-invalid"]["records"][-1]
    if bad_time["effective_at"] < bad_time["expires_at"] <= bad_time["grace_until"]:
        fail("entitlement time negative not materialized")

    decisions = by_id["decision-effective-limit-valid-invalid"]
    valid_ids, invalid_ids = set(), set()
    for decision in decisions["records"]:
        ent = next((item for item in records
                    if payload_digest(item) == decision["entitlement_digest"]), None)
        valid = (
            ent is not None
            and ent["effective_at"] <= decision["decided_at"] < ent["grace_until"]
            and decision["effective_limit"] == ent["seat_limit"]["value"]
            and decision["allocated_before"] + decision["requested_delta"]
            <= decision["effective_limit"]
        )
        (valid_ids if valid else invalid_ids).add(decision["decision_id"])
    if (valid_ids != set(decisions["valid_ids"])
            or invalid_ids != set(decisions["invalid_ids"])):
        fail("decision effective-limit classification")

    stale = by_id["stale-head-decision-invalid"]["records"]
    if (stale[-1]["entitlement_digest"] != payload_digest(stale[0])
            or stale[-1]["entitlement_digest"] == payload_digest(stale[1])):
        fail("stale-head decision not materialized")

    allocation = by_id["allocation-cas-release-valid"]
    first, successor = allocation["records"]
    if (successor["allocation_id"] != first["allocation_id"]
            or successor["scope"] != first["scope"]
            or successor["version"] != first["version"] + 1
            or successor["parent_digest"] != payload_digest(first)
            or first["state"] != "active" or successor["state"] != "released"
            or allocation["head_digest"] != payload_digest(successor)):
        fail("allocation CAS history")

    duplicate = by_id["allocation-uniqueness-fork-invalid"]["records"]
    if not (duplicate[0]["state"] == duplicate[1]["state"] == "active"
            and duplicate[0]["entitlement_digest"] == duplicate[1]["entitlement_digest"]
            and duplicate[0]["limit_decision_digest"] == duplicate[1]["limit_decision_digest"]
            and duplicate[1]["parent_digest"] == payload_digest(duplicate[0])
            and (duplicate[0]["allocation_id"] != duplicate[1]["allocation_id"]
                 or duplicate[0]["member_id"] != duplicate[1]["member_id"])):
        fail("allocation uniqueness negative not materialized")

    usage = by_id["usage-correction-valid"]
    original, correction = usage["records"]
    if (correction.get("corrects_digest") != payload_digest(original)
            or correction["source"] != "reconciled"
            or correction["period_start"] != original["period_start"]
            or correction["period_end"] != original["period_end"]
            or usage["replacement"] != payload_digest(correction)):
        fail("usage correction history")
    late = by_id["usage-late-append-invalid"]["records"]
    if ("corrects_digest" in late[1]
            or late[1]["watermark"]["late_arrival_cutoff"]
            != late[0]["watermark"]["late_arrival_cutoff"]):
        fail("usage late-arrival negative not materialized")

    aggregate = by_id["aggregate-correction-valid"]
    first, correction = aggregate["records"]
    if (correction["aggregate_id"] != first["aggregate_id"]
            or correction["version"] != first["version"] + 1
            or correction["parent_digest"] != payload_digest(first)
            or aggregate["replacement"] != payload_digest(correction)):
        fail("aggregate correction history")
    aggregate_fork = by_id["aggregate-cas-fork-invalid"]["records"]
    if not (aggregate_fork[1]["version"] == 2
            and aggregate_fork[1]["parent_digest"] == payload_digest(aggregate_fork[0])
            and aggregate_fork[1]["aggregate_id"] != aggregate_fork[0]["aggregate_id"]):
        fail("aggregate fork negative not materialized")

    invite = by_id["invite-final-seat-race"]["events"]
    if ([item["step"] for item in invite] != [1, 2, 3]
            or invite[0]["operation"] != "quota_lock"
            or invite[1]["operation"] != "allocation_commit"
            or invite[2].get("result") != "conflict"):
        fail("final-seat race")
    revoke = by_id["membership-revocation-transfer-race"]["events"]
    if ([item["step"] for item in revoke] != [1, 2, 3]
            or revoke[0].get("atomic") is not True
            or revoke[1].get("atomic") is not True
            or revoke[2].get("requires") != "new-active-membership"):
        fail("membership revocation race")
    return len(histories)


def run_once():
    verify_digest_aliases()
    engine = schema_engine()
    schemas = sorted(PACK.glob("*.schema.json"))
    if len(schemas) != 7:
        fail("schema inventory")
    schema_map = engine.load_contract_schemas(PACK)
    for path in schemas:
        schema = load(path)
        if schema.get("$schema") != "https://json-schema.org/draft/2020-12/schema":
            fail("schema dialect")
        if (path.name != "common.schema.json"
                and (schema.get("type") != "object"
                     or schema.get("additionalProperties") is not False)):
            fail("schema closure: " + path.name)
    positive = load(PACK / "fixtures/valid/positive.json")
    records, envelopes = records_from_fixture(positive)
    for kind, record in records.items():
        schema_check(engine, schema_map, kind + ".schema.json", record)
        schema_check(engine, schema_map, "seat-value-signature-envelope.schema.json", envelopes[kind])
    verify_privacy_and_races(positive)
    verify_trust_and_signatures(positive, records, envelopes)
    verify_authority_and_support(positive, records)
    verify_pinned_team_artifacts(positive, engine)
    validate_records(records)
    verify_signature_vector(positive, records, envelopes)
    histories = verify_histories(positive, engine, schema_map)
    negative = load(PACK / "fixtures/invalid/negative.json")
    cases = negative.get("cases")
    if negative.get("fixture_kind") != "team-seat-value-v1/invalid" or negative.get("fixture_version") != 1:
        fail("negative fixture identity")
    if not isinstance(cases, list) or len(cases) < 20 or len({c["id"] for c in cases}) != len(cases):
        fail("negative inventory")
    for case in cases:
        negative_case_rejected(case, positive, records, envelopes, engine, schema_map)
    cross = load(PACK / "fixtures/cross-record/histories.json")
    rules = cross.get("lineage_rules", [])
    if (cross.get("fixture_kind") != "team-seat-value-v1/cross-record-histories"
            or cross.get("fixture_version") != 1):
        fail("cross fixture identity")
    if len(rules) < 10 or len(set(rules)) != len(rules):
        fail("cross rule inventory")
    verify_frozen()
    return len(cases), len(rules), histories


def main():
    cases, rules, histories = run_once()
    print("team-seat-value-v1 verifier")
    print("schemas: 7")
    print("materialized records/envelopes: 5/5")
    print("negative cases: %d" % cases)
    print("cross-record rules: %d" % rules)
    print("materialized histories/races: %d" % histories)
    print("duplicate-key rejection: yes")
    print("RESULT: PASS")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, json.JSONDecodeError, KeyError, TypeError) as exc:
        print("RESULT: FAIL: %s" % exc)
        sys.exit(1)
