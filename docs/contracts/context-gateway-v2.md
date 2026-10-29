# Context Gateway v2 — keyed receipt objects

Status: **experimental** — additive to [`context-gateway-v1.md`](context-gateway-v1.md),
which stays valid unchanged. License: Apache-2.0 (Community). Rust definition:
`lean_ctx_protocol::context_gateway` (`ContextDecisionReceiptV1` carries both
receipt schema versions; `RECEIPT_V2_SCHEMA_VERSION`, `ReceiptObjectDigestV2`).

## Why

A v1 receipt names every decision's object by the plain SHA-256 of the object.
For an object that is one short string — a message, a tool argument, a
credential the gateway masked — that digest confirms a guess: anyone holding
the receipt can hash a candidate text and compare (AP10-C4). A v2 receipt
names the same objects by a keyed digest that only the installation that
issued the receipt can compute.

## What changes in a receipt with `schema_version: 2`

| Field | v1 meaning | v2 meaning |
|---|---|---|
| `schema_version` | `1` | `2` |
| `object_digest` | absent (must be) | **required**: `{"algorithm": "hmac-sha256", "key_id": "rk-<32 hex>"}` |
| `decisions[].object` | `sha256:<hex>` = SHA-256 of the object | `sha256:<hex>` = `HMAC-SHA256(key, "leanctx.receipt.object.v2" ‖ 0x00 ‖ v1_object)`, where `v1_object` is the ASCII string v1 would have recorded |

Nothing else changes meaning. In particular `final_context` stays the plain
SHA-256 of the exact delivered bytes (hosts bind it to what they send), and
`policy.digest`, the receipt's own canonical digest (SHA-256 over canonical
JSON) and `receipt_id` keep their v1 definitions. The lexical form of
`object` is unchanged, so v1 field validators still apply; a verifier tells
the two meanings apart only by `schema_version` / `object_digest`.

- **Key.** The installation's 32-byte receipt key `<data>/gateway/receipt.key`
  (the same key as the keyed `principal_ref`/`request_head_ref`, AP10-C6).
  It never appears in a receipt, trace sidecar, log or SDK response.
- **Key id.** `rk-` and the first 32 hex digits of
  `HMAC-SHA256(key, "leanctx.receipt.key-id.v2")`. It tells keys apart and
  reveals nothing of the key.
- **Determinism.** Same key and same object, same digest; no time or random
  input. Receipts of one installation stay comparable to each other.
- **Fail closed.** Without a usable key the Engine issues no receipt; where a
  receipt is owed, the delivery is refused (`required context gateway
  receipt unavailable`). It never falls back to v1 objects.
- **Trace sidecar.** The objects a receipt's trace sidecar itemizes are named
  as the receipt names them (keyed for a v2 receipt).

## Verification

- **Structure** (anyone): a v2 receipt validates exactly like v1, plus
  `object_digest` present with a known algorithm and a well-formed `key_id`.
  A v1 receipt must not carry `object_digest`.
- **Objects** (key holder only): compute the v1 object, key it with
  `keyed_object_digest(key, v1_object)` and compare; check `key_id` against
  `receipt_key_id(key)` first. Third-party confirmation of an object from its
  plaintext is deliberately not possible in v2.

The SDKs (Python, TypeScript, Go, Rust, JVM, .NET) parse both versions and
expose `receipt_key_id` / `keyed_object_digest` (language-cased).

## Compatibility

The Engine reads and verifies v1 and v2 receipts; it emits v2 from 3.11.5.
Readers that accept only `schema_version: 1` reject v2 receipts rather than
misread them (unknown fields and versions are rejected). Any further change
to an invariant requires `context-gateway-v3.md`.
