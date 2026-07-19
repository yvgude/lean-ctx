from __future__ import annotations

import json
from pathlib import Path

import pytest

from leanctx.ocla import (
    OclaGatewayAdmissibilityError,
    OclaWireError,
    _blake3,
    decode_agent_envelope,
    decode_canonical_token_envelope,
    verify_agent_gateway_admissibility,
)


ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "clients" / "rust" / "lean-ctx-client" / "tests" / "fixtures"


def fixture(name: str) -> bytes:
    return (FIXTURES / name).read_bytes()


def canonical(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def test_blake3_matches_standard_vectors() -> None:
    assert _blake3(b"").hex() == (
        "af1349b9f5f9a1a6a0404dea36dcc949"
        "9bcb25c9adc112b7cc9a93cae41f3262"
    )
    assert _blake3(b"abc").hex() == (
        "6437b3ac38465133ffb63b75273a8db5"
        "48c558465d79db03fd359c6cd5bd9d85"
    )


@pytest.mark.parametrize(
    ("size", "expected"),
    # Generated independently with the Rust client's pinned blake3 1.5.0:
    # input bytes are index % 251 for each declared size.
    [
        (
            1024,
            "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
        ),
        (
            1025,
            "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444",
        ),
        (
            2048,
            "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a",
        ),
        (
            4097,
            "9b4052b38f1c5fc8b1f9ff7ac7b27cd242487b3d890d15c96a1c25b8aa0fb995",
        ),
    ],
)
def test_blake3_chunk_tree_matches_rust_vectors(
    size: int, expected: str
) -> None:
    body = bytes(index % 251 for index in range(size))
    assert _blake3(body).hex() == expected


def test_public_token_and_agent_fixtures_verify() -> None:
    token = decode_canonical_token_envelope(
        fixture("canonical-token-envelope-v1.json")
    )
    assert token.idempotency_key == "request-1:input"
    agent = decode_agent_envelope(fixture("agent-envelope-v1.json"))
    assert agent.relay_id == agent.canonical_relay_id()
    verify_agent_gateway_admissibility(agent)


def test_gateway_policy_is_separate_from_wire_integrity() -> None:
    self_relay = decode_agent_envelope(
        fixture("self-relay-agent-envelope-v1.json")
    )
    with pytest.raises(OclaGatewayAdmissibilityError):
        verify_agent_gateway_admissibility(self_relay)


@pytest.mark.parametrize(
    "wire",
    [
        b'{"schema_version":1,' + fixture("canonical-token-envelope-v1.json")[1:],
        fixture("canonical-token-envelope-v1.json") + b"\n",
        fixture("invalid-token-envelope-v1.json"),
        fixture("canonical-token-envelope-v1.json").replace(
            b'"provider":"openai"', b'"provider":"\\ud800"'
        ),
        fixture("canonical-token-envelope-v1.json").replace(
            b'"original_tokens":100',
            b'"original_tokens":' + b"9" * 5000,
        ),
        b"[" * 2000 + b"0" + b"]" * 2000,
        b"{",
        b" " * (64 * 1024 + 1),
    ],
)
def test_token_decoder_fails_closed(wire: bytes) -> None:
    with pytest.raises(OclaWireError):
        decode_canonical_token_envelope(wire)


def test_numeric_bool_and_u64_boundaries_fail_closed() -> None:
    value = json.loads(fixture("canonical-token-envelope-v1.json"))
    for invalid in (-1, True, 1 << 64):
        changed = json.loads(json.dumps(value))
        changed["token_balance"]["original_tokens"] = invalid
        with pytest.raises(OclaWireError):
            decode_canonical_token_envelope(canonical(changed))


def test_agent_lineage_identity_budget_and_unknown_fields_fail_closed() -> None:
    original = json.loads(fixture("agent-envelope-v1.json"))
    mutations = []
    lineage = json.loads(json.dumps(original))
    lineage["context"]["agent_id"] = "other-agent"
    mutations.append(lineage)
    relay = json.loads(json.dumps(original))
    relay["relay_id"] = "agent-relay:" + "0" * 64
    mutations.append(relay)
    budget = json.loads(json.dumps(original))
    budget["budget_tokens"] = 0
    mutations.append(budget)
    unknown = json.loads(json.dumps(original))
    unknown["payload"] = "must-not-enter-the-wire"
    mutations.append(unknown)
    for value in mutations:
        with pytest.raises(OclaWireError):
            decode_agent_envelope(canonical(value))


def test_wrong_wire_kind_is_rejected() -> None:
    with pytest.raises(OclaWireError):
        decode_agent_envelope(fixture("canonical-token-envelope-v1.json"))
    with pytest.raises(OclaWireError):
        decode_canonical_token_envelope(fixture("agent-envelope-v1.json"))
