# Journey 16 — Proof & Audit (Signed Savings Ledger)

> You've been saving tokens for weeks; now a lead, client, or finance team wants
> proof. This journey covers the local savings ledger and how to turn it into a
> portable, Ed25519-signed receipt that anyone can verify **offline** — integrity
> and origin — without ever seeing your code, paths, or prompts.

Source files referenced here:
- `rust/src/cli/dispatch/analytics.rs` — `cmd_savings`, `cmd_savings_sign`, `cmd_savings_verify_batch`
- `rust/src/core/savings_ledger/store.rs` — append-only SHA-256 hash chain (`verify`)
- `rust/src/core/savings_ledger/signed_batch.rs` — `SignedSavingsBatchV1`, `BatchTotals`, `BatchVerifyResult`
- `rust/src/core/savings_ledger/mod.rs` — `summary()`, `verify()`, `all_events()`
- `rust/src/core/agent_identity.rs` — persistent per-machine Ed25519 keypair

---

## 0. The principle

> The ledger fills itself as lean-ctx compresses your reads, searches and shell
> output. Nothing leaves your machine unless you explicitly `sign` and share an
> artifact — and even then, only **aggregate numbers** travel, never code, file
> paths, prompts, or per-event timestamps.

---

## 1. The ledger — an append-only hash chain

Every compression event is appended to `~/.lean-ctx/savings/`. Each entry commits the
SHA-256 hash of the previous one (`store.rs`), forming a tamper-evident chain: editing,
reordering, inserting, or deleting any past event breaks `verify()`. The **chain head**
(latest `entry_hash`) is a fingerprint of the entire history.

```bash
lean-ctx savings verify        # core::savings_ledger::verify()
```

---

## 2. The `savings` command surface

`cmd_savings` (`analytics.rs`) dispatches on the first argument; default is `summary`.

| Command | Code path | Leaves machine? |
|---------|-----------|-----------------|
| `savings summary` | `format_savings_summary()` → `savings_ledger::summary()` | No |
| `savings verify` | `savings_ledger::verify()` | No |
| `savings export` | `savings_ledger::all_events()` → pretty JSON | No |
| `savings sign [--out FILE]` | `cmd_savings_sign` → `SignedSavingsBatchV1::build_all` + `sign` | Only the file you share |
| `savings verify-batch <file>` | `cmd_savings_verify_batch` → `signed_batch::load_artifact` + `verify` | No (any machine) |
| `savings roi [--json]` | `cmd_savings_roi` → `savings_ledger::roi_report` → `RoiReport::from_signed_batch` | No (read-only aggregate) |

### ROI / metering surface (EPIC 12.20)

`savings roi` derives a [`RoiReport`](../../rust/src/core/savings_ledger/roi.rs)
**strictly from the signed batch** — `BatchTotals` + the committed
`last_entry_hash` + the Ed25519 signature. It adds derived metering metrics
(net tokens, USD, averages per event, top models/tools) plus provenance
(`chain_valid`, `signed`, signer public key). This is the minimal,
privacy-preserving aggregate the **Cloud plane** meters on: it carries no raw
events, paths, prompts, or code — only numbers and hashes — and is read-only
with respect to the local ledger.

---

## 3. `savings sign` — build + sign the artifact

`cmd_savings_sign` calls `SignedSavingsBatchV1::build_all(agent_id)`, which reads the
ledger (`all_events`, `summary`, `verify`), copies the aggregate totals into `BatchTotals`,
records the first/last `entry_hash`, and signs the canonical bytes with the machine's
Ed25519 key from `agent_identity::get_or_create_keypair`.

```bash
lean-ctx savings sign --out ./sprint-savings.json
```

```text
Signed savings batch written to ./sprint-savings.json
  Net saved:  12.8M tokens (~$32.41) over 1,240 event(s)
  Chain head: 9f2c4b…e1a7
  Chain:      intact (SHA-256)
  Signer key: 7b1e90…c4d2

Verify anywhere (no ledger needed):  lean-ctx savings verify-batch ./sprint-savings.json
```

Default path (no `--out`): `<data_dir>/savings/signed-batch-v1_<utc-stamp>.json`
(`signed_batch::default_artifact_path`). An empty ledger exits non-zero with a hint.

### Artifact shape (`SignedSavingsBatchV1`, schema v1)

`kind = "lean-ctx.savings-batch"`. The two signature fields are excluded from the signed
payload (`canonical_bytes` clears them), so the file is self-verifying.

| Field | Meaning |
|-------|---------|
| `totals` | `BatchTotals`: net tokens, $ saved, event count, top `by_model` / `by_tool` rows (capped at 8) |
| `first_entry_hash` / `last_entry_hash` | chain endpoints — bind totals to a concrete history |
| `chain_valid` | whether the SHA-256 chain verified intact at signing time |
| `created_at`, `lean_ctx_version`, `agent_id`, `period` | provenance (`period = "all"`) |
| `signer_public_key`, `signature` | Ed25519 hex — make the artifact self-verifying |

**Never serialized:** raw events, file paths, code, prompts, per-event timestamps. The
payload is a dedicated struct, so a private field cannot leak by construction.

---

## 4. `savings verify-batch` — offline verification

`cmd_savings_verify_batch` loads the file (`load_artifact`, which rejects foreign JSON by
`kind`) and calls `SignedSavingsBatchV1::verify()`. Verification recomputes the canonical
bytes and checks the embedded Ed25519 signature against the embedded public key — no
network, no ledger, no source access required.

```bash
lean-ctx savings verify-batch ./sprint-savings.json
```

```text
Signed savings batch: VALID
  Signed by:  7b1e90…c4d2
  Agent:      local
  Created:    2026-06-02T18:45:00Z
  lean-ctx:   3.7.0
  Net saved:  12.8M tokens (~$32.41) over 1,240 event(s)
  Chain head: 9f2c4b…e1a7
```

Any post-signing edit (totals, public key, chain head) fails:

```text
Signed savings batch: INVALID — signature does not match payload (tampered or wrong key)
```

A valid result proves two things at once:
- **Integrity** — not a byte altered since signing (the signature covers the whole payload).
- **Origin** — produced by the holder of that keypair. Pair the public key with your name
  once and every future artifact from that key is attributable to you.

---

## 5. When to use it

- Justify the tool to a lead or finance with a signed dollar figure.
- Bill or report savings to a client; they verify the attestation themselves.
- Procurement / compliance evidence trails (tamper-evident, version-stamped).
- A personal, verifiable record snapshotted each quarter.

On-site deep dive: `/docs/concepts/savings-ledger` · journey page: `/docs/journeys/signed-savings-ledger`.
