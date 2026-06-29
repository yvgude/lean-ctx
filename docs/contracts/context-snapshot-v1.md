# Context Snapshot v1 (`CONTEXT_SNAPSHOT_V1`)

GitLab: `#1022` (epic) · `#1023` (this contract) · Status: **experimental**

A Context Snapshot is a **git-anchored, signed, point-in-time record of the
context-layer state** — the foundational artifact of the *Context Time Machine*.
It captures *what* the model saw (lineage, from Context IR), *why* it saw it
(ledger Φ-scores + item states), *at what token ROI*, and *which session*
produced it. Snapshots chain over time into an append-only timeline you can
rewind, reproduce, resume, or share.

A snapshot is a **distilled, typed, signed projection** of the live stores — it
never embeds raw transcripts. Each slice is bounded so a snapshot stays small
and its id stays stable.

## Goals

- **Git-anchored**: every snapshot pins to a commit / branch / dirty state.
- **Content-addressed**: `snapshot_id` is the BLAKE3 of the canonical body, so
  the same layer state yields the same id (modulo `created_at`).
- **Signed**: an ed25519 signature over the id proves authenticity and
  integrity (the body must still hash to the id).
- **Deterministic**: structs/`Vec`s/`Option`s only — no maps — so `serde_json`
  field order makes the encoding byte-stable (#498).
- **Bounded**: fixed `MAX_SNAPSHOT_*` caps on every embedded list.
- **Distilled-by-default**: projections of stores, never raw content.

## Format (JSON)

Top-level fields of `ContextSnapshotV1`:

- `schema_version`: `1`
- `snapshot_id`: BLAKE3 hex of the canonical body (empty in the body that is
  hashed; see *Identity & signing*)
- `parent_id`: id of the previous snapshot in this project's timeline, or `null`
- `created_at`: RFC3339 timestamp
- `lean_ctx_version`: producing CLI version
- `git`: `{ commit, branch, dirty }` — the git anchor
- `project`: `{ root_hash, identity_hash }` — hashes only, never raw paths
- `roi`: `{ input_tokens, output_tokens, tokens_saved, compression_rate }`
- `lineage`: `{ items_recorded, items[] }` — distilled Context IR slice
- `ledger`: `{ window_size, total_tokens_sent, total_tokens_saved, items[] }`
- `session`: optional `{ session_id, task, decisions[], files_touched[], progress_pct }`
- `signature`: optional `{ algorithm, public_key, value }` (ed25519, `null` until signed)

### `lineage.items[]`

`{ seq, kind, tool, path, input_tokens, output_tokens, compression_ratio, content_hash }`
where `kind ∈ { read, shell, search, provider, other }` and `content_hash` is a
BLAKE3 verification handle of the recorded excerpt.

### `ledger.items[]`

`{ path, state, phi, sent_tokens, original_tokens }` where
`state ∈ { candidate, included, excluded, pinned, stale, shadowed }`.

## Identity & signing

The `snapshot_id` is the BLAKE3 hex of the **canonical body**: the snapshot
serialized with `snapshot_id` blanked and `signature` cleared. The signature is
then computed over the id:

```
message = sha256-hex("ctxsnapshot-sign-v1:{snapshot_id}")
signature = ed25519_sign(signing_key, message)
```

This mirrors the `ctxpkg-sign-v1` scheme and **reuses the same publisher
keypair** (`<data_dir>/keys/ctxpkg-ed25519.key`) so a project has one stable
signing identity across packages and snapshots.

Verification is two-fold:

1. **Integrity** — recompute the id from the body; it must equal `snapshot_id`.
2. **Authenticity** — the ed25519 signature must validate over the id.

An unsigned snapshot, a tampered body, or a wrong key all verify as `false`;
malformed signature material (bad hex / wrong length / unknown algorithm) errors.

## Boundedness

- `MAX_SNAPSHOT_LINEAGE_ITEMS = 128`
- `MAX_SNAPSHOT_LEDGER_ITEMS = 256`
- `MAX_SNAPSHOT_SESSION_LIST = 64` (decisions and files each)

## Timeline chaining

Snapshots form an append-only chain via `parent_id`. This contract (#1023)
freezes the on-disk shape, the deterministic id, and the signing semantics; the
builder, timeline index, restore, and share/import verbs shipped on top of it
(#1024–#1027).

## Lifecycle verbs

The full CLI surface over a snapshot (each builds on this contract):

- `snapshot create [--sign]` — build + store from the live stores (#1024)
- `snapshot list|show|verify` — browse + prove the timeline (#1024)
- `snapshot restore <id> [--git]` — resume the session slice; optionally check
  out the commit anchor, guarded against a dirty tree (#1026)
- `snapshot publish <id> [--out]` — write a signed, shareable
  `*.ctxsnapshot.json`; `snapshot import <file>` proves it (integrity +
  signature) and appends it to the local timeline, idempotently (#1027)

## Relevant code

- Types: `rust/src/core/context_snapshot/types.rs`
- Canonical id: `rust/src/core/context_snapshot/digest.rs`
- Signing: `rust/src/core/context_snapshot/signing.rs`
- Builder (live stores → snapshot): `rust/src/core/context_snapshot/builder.rs`
- Append-only timeline: `rust/src/core/context_snapshot/timeline.rs`
- Restore / resume: `rust/src/core/context_snapshot/restore.rs`
- Publish / import: `rust/src/core/context_snapshot/publish.rs`
- CLI surface: `rust/src/cli/snapshot_cmd.rs`
- Schema version: `rust/src/core/contracts.rs` (`CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION`)
- Reused keypair: `rust/src/core/context_package/keys.rs`
