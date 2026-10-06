#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Deterministically compare seat/value JSON root fields with Rust wire fields."""

import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
RUST = ROOT / "rust/crates/lean-ctx-protocol/src/seat_value.rs"
PACK = ROOT / "docs/contracts/team-seat-value-v1"

RECORDS = {
    "SeatValueSignatureEnvelopeV1": "seat-value-signature-envelope.schema.json",
    "SeatEntitlementV1": "seat-entitlement.schema.json",
    "SeatLimitDecisionV1": "seat-limit-decision.schema.json",
    "SeatAllocationV1": "seat-allocation.schema.json",
    "SeatUsageV1": "seat-usage.schema.json",
    "TeamValueAggregateV1": "team-value-aggregate.schema.json",
}


def reject_duplicates(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON member: {key}")
        result[key] = value
    return result


def load_schema(path):
    return json.loads(path.read_text(), object_pairs_hook=reject_duplicates)


def rust_fields(source, record):
    match = re.search(
        rf"strict_record!\(\s*{record}\s*,.*?\{{(?P<body>.*?)\}}\s*,\s*\|",
        source,
        re.DOTALL,
    )
    if match is None:
        raise ValueError(f"missing strict_record declaration: {record}")
    body = re.sub(r"#\s*\[[^]]*\]", "", match.group("body"))
    return set(re.findall(r"\b([a-z][a-z0-9_]*)\s*:", body))


def rust_required_fields(source, record):
    match = re.search(
        rf"strict_record!\(\s*{record}\s*,.*?\{{(?P<body>.*?)\}}\s*,\s*\|",
        source, re.DOTALL,
    )
    if match is None:
        raise ValueError(f"missing strict_record declaration: {record}")
    body = match.group("body")
    fields = {}
    for item in re.finditer(
            r"(?P<attrs>(?:#\s*\[[^]]*\]\s*)*)(?P<name>[a-z][a-z0-9_]*)\s*:\s*(?P<type>[^,]+)",
            body):
        optional = "Option<" in item.group("type") or "default" in item.group("attrs")
        fields[item.group("name")] = optional
    return {name for name, optional in fields.items() if not optional}


def rust_field_types(source, record, macro=True):
    if macro:
        pattern = rf"strict_record!\(\s*{record}\s*,.*?\{{(?P<body>.*?)\}}\s*,\s*\|"
    else:
        pattern = rf"struct\s+{record}\s*\{{(?P<body>.*?)\}}"
    match = re.search(pattern, source, re.DOTALL)
    if match is None:
        raise ValueError(f"missing Rust declaration: {record}")
    body = re.sub(r"#\s*\[[^]]*\]", "", match.group("body"))
    return {
        item.group(1): re.sub(r"\s+", "", item.group(2))
        for item in re.finditer(r"\b([a-z][a-z0-9_]*)\s*:\s*([^,}]+)", body)
    }


REF_TYPES = {
    "TeamId": "id", "MemberId": "memberId", "RecordDigest": "digest",
    "BillingDigest": "billingDigest", "CatalogDigest": "catalogDigest",
    "EvidenceDigest": "evidenceDigest", "MethodologyDigest": "methodologyDigest",
    "PriceDigest": "priceDigest", "AcceptedOutcomeDigest": "acceptedOutcomeDigest",
    "IssuerId": "issuerId", "KeyId": "keyId", "TeamTimestampV1": "timestamp",
    "ReasonCode": "reasonCode", "IdempotencyKey": "idempotencyKey",
}


def type_matches_schema(rust_type, schema):
    optional = rust_type.startswith("Option<")
    if optional:
        rust_type = rust_type[7:-1]
    if "null" in schema.get("type", []) if isinstance(schema.get("type"), list) else False:
        return False
    if rust_type in REF_TYPES:
        return schema.get("$ref") == f"common.schema.json#/$defs/{REF_TYPES[rust_type]}"
    if rust_type in {"u32", "u64", "u8"}:
        return schema.get("type") == "integer" or "const" in schema or schema.get("$ref", "").split("/")[-1] in {"quantity", "sequence", "version"}
    if rust_type == "String":
        return schema.get("type") == "string" or isinstance(schema.get("const"), str)
    vector = re.fullmatch(r"Vec<(.+)>", rust_type)
    if vector:
        return schema.get("type") == "array" and type_matches_schema(vector.group(1), schema.get("items", {}))
    named = {
        "SeatValueScopeV1": ("$ref", "common.schema.json#/$defs/scope"),
        "SeatValueActionV1": ("const", "seat.allocate"),
        "SignedSeatValueKindV1": ("$ref", "common.schema.json#/$defs/signedKind"),
        "SeatValueKeyPurposeV1": ("enum", ["billing-entitlement", "seat-authority", "value-attestation"]),
        "ValueFactV1": ("$ref", "#/$defs/valueFact"),
        "Ed25519PublicKey": ("pattern", "^[A-Za-z0-9_-]{43}$"),
        "Ed25519Signature": ("pattern", "^[A-Za-z0-9_-]{86}$"),
    }
    if rust_type in named:
        key, expected = named[rust_type]
        return schema.get(key) == expected
    if rust_type in {"SeatPlanV1", "EntitlementSourceV1", "SeatLimitV1",
                     "AllocationRequestV1", "SeatDecisionOutcomeV1",
                     "SeatAllocationStateV1", "SeatUsageSourceV1",
                     "SeatUsageWatermarkV1", "ValueFactV1"}:
        return any(key in schema for key in ("enum", "oneOf", "properties")) or schema.get("type") == "object"
    return False


ENUM_LITERALS = {
    "action": {"entitlement.issue", "entitlement.renew", "entitlement.revoke",
                        "seat.allocate", "seat.release", "seat.revoke", "seat.reconcile",
                        "value.aggregate.generate", "value.aggregate.correct",
                        "value.dashboard.read"},
    "signedKind": set(kind for kind in (
        "seat-entitlement", "seat-limit-decision", "seat-allocation", "seat-usage",
        "team-value-aggregate")),
}


def main():
    source = RUST.read_text()
    failures = []
    for record, filename in RECORDS.items():
        document = load_schema(PACK / filename)
        schema = set(document["properties"])
        rust = rust_fields(source, record)
        if schema != rust:
            failures.append(
                f"{record}: schema-only={sorted(schema-rust)} rust-only={sorted(rust-schema)}"
            )
        required = set(document.get("required", []))
        rust_required = rust_required_fields(source, record)
        if required != rust_required:
            failures.append(
                f"{record}: required schema-only={sorted(required-rust_required)} "
                f"rust-only={sorted(rust_required-required)}"
            )
        if document.get("additionalProperties") is not False:
            failures.append(f"{record}: schema root is not closed")
        for field, rust_type in rust_field_types(source, record).items():
            if field in document["properties"] and not type_matches_schema(
                    rust_type, document["properties"][field]):
                failures.append(f"{record}.{field}: Rust type {rust_type} disagrees with schema")
    nested = {
        "AllocationRequestV1": load_schema(PACK / "seat-limit-decision.schema.json")
        ["properties"]["allocation_request"],
        "SeatUsageWatermarkV1": load_schema(PACK / "seat-usage.schema.json")
        ["properties"]["watermark"],
    }
    for record, schema in nested.items():
        fields = rust_field_types(source, record, macro=False)
        if set(fields) != set(schema.get("properties", {})):
            failures.append(f"{record}: nested field drift")
        if set(fields) != set(schema.get("required", [])) or schema.get("additionalProperties") is not False:
            failures.append(f"{record}: nested required/closure drift")
        for field, rust_type in fields.items():
            if not type_matches_schema(rust_type, schema["properties"][field]):
                failures.append(f"{record}.{field}: nested type drift")
    common = load_schema(PACK / "common.schema.json").get("$defs", {})
    for name, expected in ENUM_LITERALS.items():
        actual = set(common.get(name, {}).get("enum", []))
        if actual != expected:
            failures.append(f"common.{name}: enum drift")
    purposes = set(load_schema(PACK / "seat-value-signature-envelope.schema.json")
                   ["properties"]["key_purpose"]["enum"])
    if purposes != {"billing-entitlement", "seat-authority", "value-attestation"}:
        failures.append("signature key_purpose enum drift")
    for enum_type in ("SeatValueActionV1", "SignedSeatValueKindV1",
                      "SeatValueKeyPurposeV1"):
        if f"enum {enum_type}" not in source:
            failures.append(f"missing Rust enum: {enum_type}")
    if failures:
        print("team-seat-value-v1 Rust/schema parity: FAIL")
        print("\n".join(failures))
        return 1
    print(f"team-seat-value-v1 Rust/schema parity: PASS ({len(RECORDS)} records)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
