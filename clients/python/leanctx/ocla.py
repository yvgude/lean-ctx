"""Dependency-free OCLA v1 wire types and offline verification.

The module mirrors the public JSON schemas.  It does not import the LeanCTX
engine, perform transport I/O, or treat wire validity as delivery evidence.
"""

from __future__ import annotations

import json
import struct
from dataclasses import dataclass
from typing import Any, Dict, Iterable, Mapping, Optional, Sequence, Tuple


OCLA_API_VERSION = "ocla/v1"
CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION = 1
AGENT_ENVELOPE_SCHEMA_VERSION = 1
MAX_OCLA_WIRE_BYTES = 64 * 1024
_U64_MAX = (1 << 64) - 1


class OclaWireError(ValueError):
    """A malformed, unsupported, non-canonical, or invalid OCLA document."""


class OclaGatewayAdmissibilityError(ValueError):
    """A wire-valid agent envelope rejected by local gateway policy."""


@dataclass(frozen=True)
class OclaRequestContext:
    """Stable identifiers joining decisions across interception surfaces."""

    request_id: str
    session_id: str
    agent_id: str
    content_ref: str
    tenant_id: Optional[str]


@dataclass(frozen=True)
class TokenBalanceV1:
    """Provider-neutral accounting at token lifecycle stages."""

    original_tokens: int
    materialized_tokens: int
    delivered_tokens: int
    provider_billed_tokens: int


@dataclass(frozen=True)
class CanonicalTokenEnvelopeV1:
    """Payload-free token decision at an OCLA boundary."""

    schema_version: int
    context: OclaRequestContext
    surface: str
    direction: str
    provider: str
    model: str
    token_balance: TokenBalanceV1
    route_ref: Optional[str]
    policy_ref: Optional[str]
    idempotency_key: str


@dataclass(frozen=True)
class AgentEnvelopeV1:
    """Payload-free agent-to-agent admission contract."""

    schema_version: int
    relay_id: str
    context: OclaRequestContext
    from_agent_id: str
    to_agent_id: str
    capsule_ref: str
    budget_tokens: int

    def canonical_relay_id(self) -> str:
        """Return the content-derived relay identity defined by OCLA v1."""

        body = _agent_mapping(self, relay_id="agent-relay:pending")
        return "agent-relay:" + _blake3(_canonical_json(body)).hex()


_IV = (
    0x6A09E667,
    0xBB67AE85,
    0x3C6EF372,
    0xA54FF53A,
    0x510E527F,
    0x9B05688C,
    0x1F83D9AB,
    0x5BE0CD19,
)
_MSG_PERMUTATION = (2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8)
_CHUNK_START = 1
_CHUNK_END = 2
_PARENT = 4
_ROOT = 8


def _rotate_right(value: int, count: int) -> int:
    return ((value >> count) | (value << (32 - count))) & 0xFFFFFFFF


def _mix(
    state: list[int],
    a: int,
    b: int,
    c: int,
    d: int,
    first: int,
    second: int,
) -> None:
    state[a] = (state[a] + state[b] + first) & 0xFFFFFFFF
    state[d] = _rotate_right(state[d] ^ state[a], 16)
    state[c] = (state[c] + state[d]) & 0xFFFFFFFF
    state[b] = _rotate_right(state[b] ^ state[c], 12)
    state[a] = (state[a] + state[b] + second) & 0xFFFFFFFF
    state[d] = _rotate_right(state[d] ^ state[a], 8)
    state[c] = (state[c] + state[d]) & 0xFFFFFFFF
    state[b] = _rotate_right(state[b] ^ state[c], 7)


def _compress(
    chaining_value: Sequence[int],
    block_words: Sequence[int],
    counter: int,
    block_length: int,
    flags: int,
) -> Tuple[int, ...]:
    state = list(chaining_value) + list(_IV[:4]) + [
        counter & 0xFFFFFFFF,
        (counter >> 32) & 0xFFFFFFFF,
        block_length,
        flags,
    ]
    message = list(block_words)
    for round_index in range(7):
        _mix(state, 0, 4, 8, 12, message[0], message[1])
        _mix(state, 1, 5, 9, 13, message[2], message[3])
        _mix(state, 2, 6, 10, 14, message[4], message[5])
        _mix(state, 3, 7, 11, 15, message[6], message[7])
        _mix(state, 0, 5, 10, 15, message[8], message[9])
        _mix(state, 1, 6, 11, 12, message[10], message[11])
        _mix(state, 2, 7, 8, 13, message[12], message[13])
        _mix(state, 3, 4, 9, 14, message[14], message[15])
        if round_index != 6:
            message = [message[index] for index in _MSG_PERMUTATION]
    return tuple(
        [(state[index] ^ state[index + 8]) & 0xFFFFFFFF for index in range(8)]
        + [
            (state[index + 8] ^ chaining_value[index]) & 0xFFFFFFFF
            for index in range(8)
        ]
    )


def _block_words(block: bytes) -> Tuple[int, ...]:
    return struct.unpack("<16I", block.ljust(64, b"\0"))


@dataclass(frozen=True)
class _Blake3Output:
    input_chaining_value: Tuple[int, ...]
    block_words: Tuple[int, ...]
    counter: int
    block_length: int
    flags: int

    def chaining_value(self) -> Tuple[int, ...]:
        return _compress(
            self.input_chaining_value,
            self.block_words,
            self.counter,
            self.block_length,
            self.flags,
        )[:8]

    def root_hash(self) -> bytes:
        words = _compress(
            self.input_chaining_value,
            self.block_words,
            0,
            self.block_length,
            self.flags | _ROOT,
        )
        return struct.pack("<8I", *words[:8])


def _chunk_output(chunk: bytes, chunk_counter: int) -> _Blake3Output:
    blocks = [chunk[index : index + 64] for index in range(0, len(chunk), 64)]
    if not blocks:
        blocks = [b""]
    chaining_value = _IV
    for index, block in enumerate(blocks[:-1]):
        flags = _CHUNK_START if index == 0 else 0
        chaining_value = _compress(
            chaining_value,
            _block_words(block),
            chunk_counter,
            len(block),
            flags,
        )[:8]
    final_flags = _CHUNK_END
    if len(blocks) == 1:
        final_flags |= _CHUNK_START
    final_block = blocks[-1]
    return _Blake3Output(
        tuple(chaining_value),
        _block_words(final_block),
        chunk_counter,
        len(final_block),
        final_flags,
    )


def _parent_output(
    left: Sequence[int], right: Sequence[int]
) -> _Blake3Output:
    return _Blake3Output(
        _IV,
        tuple(left) + tuple(right),
        0,
        64,
        _PARENT,
    )


def _blake3(data: bytes) -> bytes:
    """Return the standard 32-byte unkeyed BLAKE3 digest."""

    chunks = [
        data[index : index + 1024] for index in range(0, len(data), 1024)
    ]
    if not chunks:
        chunks = [b""]
    stack: list[Tuple[int, ...]] = []
    for chunk_index, chunk in enumerate(chunks[:-1]):
        chaining_value = _chunk_output(chunk, chunk_index).chaining_value()
        total_chunks = chunk_index + 1
        while total_chunks & 1 == 0:
            chaining_value = _parent_output(
                stack.pop(), chaining_value
            ).chaining_value()
            total_chunks >>= 1
        stack.append(chaining_value)
    output = _chunk_output(chunks[-1], len(chunks) - 1)
    while stack:
        output = _parent_output(stack.pop(), output.chaining_value())
    return output.root_hash()


def _reject_constant(_value: str) -> None:
    raise OclaWireError("non-finite JSON number")


def _strict_object(pairs: Iterable[Tuple[str, Any]]) -> Dict[str, Any]:
    result: Dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise OclaWireError("duplicate JSON field")
        result[key] = value
    return result


def _parse_integer(value: str) -> int:
    digits = value[1:] if value.startswith("-") else value
    if len(digits) > 20:
        raise OclaWireError("JSON integer exceeds the bounded wire range")
    return int(value)


def _decode_json(wire: bytes) -> Any:
    if len(wire) > MAX_OCLA_WIRE_BYTES:
        raise OclaWireError("OCLA wire document exceeds 64 KiB")
    try:
        text = wire.decode("utf-8")
        return json.loads(
            text,
            object_pairs_hook=_strict_object,
            parse_constant=_reject_constant,
            parse_int=_parse_integer,
        )
    except OclaWireError:
        raise
    except (
        UnicodeDecodeError,
        json.JSONDecodeError,
        ValueError,
        RecursionError,
    ) as error:
        raise OclaWireError("malformed OCLA wire document") from error


def _require_object(
    value: Any, required: Sequence[str], optional: Sequence[str] = ()
) -> Mapping[str, Any]:
    if type(value) is not dict:
        raise OclaWireError("expected JSON object")
    allowed = set(required) | set(optional)
    if set(value) - allowed or set(required) - set(value):
        raise OclaWireError("unexpected or missing JSON field")
    return value


def _require_text(value: Any, allow_empty: bool = False) -> str:
    if not isinstance(value, str):
        raise OclaWireError("expected string")
    if any(0xD800 <= ord(char) <= 0xDFFF for char in value):
        raise OclaWireError("string contains an invalid Unicode surrogate")
    if not allow_empty and not value.strip():
        raise OclaWireError("required string is empty")
    return value


def _require_optional_text(value: Any) -> Optional[str]:
    if value is None:
        return None
    return _require_text(value, allow_empty=True)


def _require_u64(value: Any, minimum: int = 0) -> int:
    if type(value) is not int or value < minimum or value > _U64_MAX:
        raise OclaWireError("integer is outside the supported range")
    return value


def _require_version(value: Any, expected: int) -> int:
    version = _require_u64(value)
    if version != expected:
        raise OclaWireError("unsupported OCLA schema version")
    return version


def _require_agent_id(value: Any) -> str:
    identifier = _require_text(value)
    if len(identifier) > 256 or not all(0x21 <= ord(char) <= 0x7E for char in identifier):
        raise OclaWireError("invalid agent identifier")
    return identifier


def _require_digest_ref(value: Any, prefix: str) -> str:
    reference = _require_text(value)
    digest = reference[len(prefix) :] if reference.startswith(prefix) else ""
    if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
        raise OclaWireError("invalid content digest reference")
    return reference


def _decode_context(value: Any, agent_contract: bool) -> OclaRequestContext:
    item = _require_object(
        value,
        ("request_id", "session_id", "agent_id", "content_ref", "tenant_id"),
    )
    agent_id = (
        _require_agent_id(item["agent_id"])
        if agent_contract
        else _require_text(item["agent_id"])
    )
    return OclaRequestContext(
        request_id=_require_text(item["request_id"]),
        session_id=_require_text(item["session_id"]),
        agent_id=agent_id,
        content_ref=_require_text(item["content_ref"]),
        tenant_id=_require_optional_text(item["tenant_id"]),
    )


def _context_mapping(context: OclaRequestContext) -> Dict[str, Any]:
    return {
        "request_id": context.request_id,
        "session_id": context.session_id,
        "agent_id": context.agent_id,
        "content_ref": context.content_ref,
        "tenant_id": context.tenant_id,
    }


def _token_mapping(envelope: CanonicalTokenEnvelopeV1) -> Dict[str, Any]:
    return {
        "schema_version": envelope.schema_version,
        "context": _context_mapping(envelope.context),
        "surface": envelope.surface,
        "direction": envelope.direction,
        "provider": envelope.provider,
        "model": envelope.model,
        "token_balance": {
            "original_tokens": envelope.token_balance.original_tokens,
            "materialized_tokens": envelope.token_balance.materialized_tokens,
            "delivered_tokens": envelope.token_balance.delivered_tokens,
            "provider_billed_tokens": envelope.token_balance.provider_billed_tokens,
        },
        "route_ref": envelope.route_ref,
        "policy_ref": envelope.policy_ref,
        "idempotency_key": envelope.idempotency_key,
    }


def _agent_mapping(
    envelope: AgentEnvelopeV1, relay_id: Optional[str] = None
) -> Dict[str, Any]:
    return {
        "schema_version": envelope.schema_version,
        "relay_id": envelope.relay_id if relay_id is None else relay_id,
        "context": _context_mapping(envelope.context),
        "from_agent_id": envelope.from_agent_id,
        "to_agent_id": envelope.to_agent_id,
        "capsule_ref": envelope.capsule_ref,
        "budget_tokens": envelope.budget_tokens,
    }


def _canonical_json(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
    except UnicodeEncodeError as error:
        raise OclaWireError("wire text is not valid Unicode") from error


def decode_canonical_token_envelope(wire: bytes) -> CanonicalTokenEnvelopeV1:
    """Decode and verify one canonical-token-envelope v1 document."""

    value = _require_object(
        _decode_json(wire),
        (
            "schema_version",
            "context",
            "surface",
            "direction",
            "provider",
            "model",
            "token_balance",
            "idempotency_key",
        ),
        ("route_ref", "policy_ref"),
    )
    balance_value = _require_object(
        value["token_balance"],
        (
            "original_tokens",
            "materialized_tokens",
            "delivered_tokens",
            "provider_billed_tokens",
        ),
    )
    balance = TokenBalanceV1(
        original_tokens=_require_u64(balance_value["original_tokens"]),
        materialized_tokens=_require_u64(balance_value["materialized_tokens"]),
        delivered_tokens=_require_u64(balance_value["delivered_tokens"]),
        provider_billed_tokens=_require_u64(
            balance_value["provider_billed_tokens"]
        ),
    )
    if balance.materialized_tokens > balance.original_tokens:
        raise OclaWireError("materialized tokens exceed original tokens")
    if balance.delivered_tokens > balance.materialized_tokens:
        raise OclaWireError("delivered tokens exceed materialized tokens")
    surface = _require_text(value["surface"])
    direction = _require_text(value["direction"])
    if surface not in ("mcp", "proxy", "shell", "agent"):
        raise OclaWireError("unsupported interception surface")
    if direction not in ("input", "output"):
        raise OclaWireError("unsupported token direction")
    envelope = CanonicalTokenEnvelopeV1(
        schema_version=_require_version(
            value["schema_version"], CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION
        ),
        context=_decode_context(value["context"], agent_contract=False),
        surface=surface,
        direction=direction,
        provider=_require_text(value["provider"]),
        model=_require_text(value["model"]),
        token_balance=balance,
        route_ref=_require_optional_text(value.get("route_ref")),
        policy_ref=_require_optional_text(value.get("policy_ref")),
        idempotency_key=_require_text(value["idempotency_key"]),
    )
    if _canonical_json(_token_mapping(envelope)) != wire:
        raise OclaWireError("OCLA wire document is not canonical JSON")
    return envelope


def decode_agent_envelope(wire: bytes) -> AgentEnvelopeV1:
    """Decode and verify one canonical agent-envelope v1 document."""

    value = _require_object(
        _decode_json(wire),
        (
            "schema_version",
            "relay_id",
            "context",
            "from_agent_id",
            "to_agent_id",
            "capsule_ref",
            "budget_tokens",
        ),
    )
    envelope = AgentEnvelopeV1(
        schema_version=_require_version(
            value["schema_version"], AGENT_ENVELOPE_SCHEMA_VERSION
        ),
        relay_id=_require_digest_ref(value["relay_id"], "agent-relay:"),
        context=_decode_context(value["context"], agent_contract=True),
        from_agent_id=_require_agent_id(value["from_agent_id"]),
        to_agent_id=_require_agent_id(value["to_agent_id"]),
        capsule_ref=_require_digest_ref(value["capsule_ref"], "capsule:"),
        budget_tokens=_require_u64(value["budget_tokens"], minimum=1),
    )
    if envelope.context.agent_id != envelope.from_agent_id:
        raise OclaWireError("context agent does not own the relay")
    if envelope.relay_id != envelope.canonical_relay_id():
        raise OclaWireError("relay identity does not match canonical content")
    if _canonical_json(_agent_mapping(envelope)) != wire:
        raise OclaWireError("OCLA wire document is not canonical JSON")
    return envelope


def verify_agent_gateway_admissibility(envelope: AgentEnvelopeV1) -> None:
    """Apply the local self-relay policy to a wire-valid agent envelope."""

    if envelope.from_agent_id == envelope.to_agent_id:
        raise OclaGatewayAdmissibilityError("agent gateway rejects self-relay")
