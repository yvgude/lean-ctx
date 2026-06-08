You've been letting LeanCTX compress your AI's reads, searches and shell output for a
sprint. Now someone — a lead, a client, a finance team — wants to see the payoff. This
journey turns your local savings into a **tamper-evident, cryptographically signed
receipt** that anyone can verify offline, without ever seeing a line of your code.

## 0. The principle

> Savings are recorded locally and continuously, in an append-only SHA-256 hash chain.
> Nothing leaves your machine unless you explicitly sign and share an artifact — and even
> then, only **aggregate numbers** travel, never code, paths or prompts.

So proof is a pull model: the ledger fills itself as you work, and you produce a signed
attestation only when you want one.

## 1. The ledger — an append-only hash chain

Every compression event LeanCTX performs is appended to the savings ledger at
`~/.lean-ctx/savings/`. Each entry commits the hash of the previous one, so editing,
reordering, inserting or deleting any past event breaks the chain. The latest hash — the
**chain head** — is a fingerprint of your entire savings history.

```bash
lean-ctx savings verify          # is the local chain intact?
```

```text
Savings ledger chain: VALID (1,240 events, head 9f2c4b…e1a7)
```

## 2. See what you've saved

```bash
lean-ctx savings summary         # the default; same data behind `gain`
```

```text
Verified Savings Ledger (local, auditable)
  Net saved:  12.8M tokens  (~$32.41)  over 1,240 events
  By model:   claude-opus 8.1M · gpt-4o 3.0M · …
  By tool:    ctx_read 6.4M · ctx_search 3.1M · ctx_shell 2.0M · …
```

Need the raw events for your own analysis? `lean-ctx savings export` prints every event as
JSON — still entirely local.

## 3. Sign it into a portable receipt

`lean-ctx savings sign` snapshots the **aggregate totals plus the chain head** and signs
the whole thing with your machine's persistent **Ed25519 key**. The output is a small,
self-verifying JSON file (it embeds its own public key and signature):

```bash
lean-ctx savings sign --out ./sprint-savings.json
```

```text
Signed savings batch written to ./sprint-savings.json
  Net saved:  12.8M tokens (~$32.41) over 1,240 event(s)
  Chain head: 9f2c4b…e1a7   (SHA-256 tip of your whole history)
  Chain:      intact (SHA-256)
  Signer key: 7b1e90…c4d2   (your Ed25519 public key)

Verify anywhere (no ledger needed):  lean-ctx savings verify-batch ./sprint-savings.json
```

Without `--out`, the artifact is written to `~/.lean-ctx/savings/signed-batch-v1_<utc>.json`.

## 4. Verify it anywhere — offline

Send the JSON file. The recipient runs `verify-batch` on their own machine. No LeanCTX
history, no network, and no access to your ledger or code are needed — the signature alone
proves both **integrity** (not a byte altered since signing) and **origin** (produced by
the holder of that key):

```bash
lean-ctx savings verify-batch ./sprint-savings.json
```

```text
Signed savings batch: VALID
  Signed by:  7b1e90…c4d2
  Agent:      local
  Created:    2026-06-02T18:45:00Z
  lean-ctx:   3.7.5
  Net saved:  12.8M tokens (~$32.41) over 1,240 event(s)
  Chain head: 9f2c4b…e1a7
```

Tamper with anything — inflate the tokens, swap the public key, rewrite the chain head —
and verification fails:

```text
Signed savings batch: INVALID — signature does not match payload (tampered or wrong key)
```

## 5. What's shared — and what never is

The artifact (`kind: lean-ctx.savings-batch`, schema v1) carries **only** what an auditor
needs:

| Included | Never included |
|----------|----------------|
| Net tokens, $ saved, event count | Raw events |
| Top by-model / by-tool rows | File names or paths |
| Chain head (`last_entry_hash`) | Source code or prompts |
| `created_at`, `lean_ctx_version` | Command contents |
| Ed25519 public key + signature | Per-event timestamps |

Signing is aggregate-only **by construction**: the payload is a dedicated struct, so a
private field cannot accidentally be serialized into a shared file.

## 6. When to reach for it

- **Justify the tool** to a lead or finance — a signed dollar figure beats an unverifiable claim.
- **Bill or report savings** to a client — attach the attestation; they verify it themselves.
- **Procurement & compliance** — a tamper-evident, version-stamped artifact fits an evidence trail.
- **Personal record** — snapshot each quarter and keep a verifiable savings history.

The deep-dive reference for every command and field lives in the
[Savings Ledger concept doc](/docs/concepts/savings-ledger).
