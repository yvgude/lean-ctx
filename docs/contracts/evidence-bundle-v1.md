# Evidence Bundle v1 (`evidence-bundle-v1`)

GitLab: `#425` (H3 Epic A) · Status: **stable** (additive evolution only)

A LeanCTX evidence bundle is a ZIP archive an auditor can verify **without
LeanCTX, without network access and without our involvement**, using the
standalone `leanctx-verify` tool (`packages/leanctx-verify/`). It composes
the engine's existing evidence surfaces — audit chain (OCP Part 4), policy
packs, the CGB coverage report — into one cryptographically linked,
deterministic artifact.

## Goals

1. **Offline-verifiable**: every integrity claim checkable from the bundle
   alone (plus, optionally, an out-of-band public key).
2. **Deterministic**: identical inputs produce a byte-identical bundle —
   same SHA-256 — so two parties can independently regenerate and compare.
3. **Honest**: the bundle states what it proves and what it cannot (see
   Threat model).

## ZIP layout

```
manifest.json                 the root of trust (see below)
audit/trail.jsonl             audit-chain segment for the period
policies/<name>.resolved.json resolved policy pack(s) in effect
coverage/cgb.json             CGB automated partial assessment
coverage/<framework>.json     framework coverage report — LeanCTX Enterprise only
```

Determinism rules (normative):

* Entries are written in **lexicographic path order**, compression method
  **Stored** (no compressor-version drift), all ZIP timestamps fixed to
  the ZIP epoch (1980-01-01).
* All JSON files are serialized with **sorted object keys** and no
  insignificant whitespace differences between runs.
* `manifest.json` contains **no wall-clock timestamps**; the period bounds
  are inputs, not observations.

`audit evidence --framework <id>` adds `coverage/<id>.json` and sets
`manifest.framework` to `<id>`; the assessment comes from the licensed LeanCTX
Enterprise runtime and is checked by the engine before it is written. Without
it the engine writes no such entry, keeps `manifest.framework` `null` and
refuses `--framework` with a pointer to the tier. Verifiers must accept both forms.

## `manifest.json`

```json
{
  "bundle": "evidence-bundle",
  "version": 1,
  "period": { "from": "2026-05-01T00:00:00Z", "to": "2026-06-01T00:00:00Z" },
  "subject": { "agent_id": "lean-ctx", "project": "<root dir name>" },
  "framework": null,
  "files": [ { "path": "audit/trail.jsonl", "sha256": "<hex>" } ],
  "chain": {
    "entries": 412,
    "anchor_prev_hash": "genesis | <hex>",
    "head_hash": "<hex>"
  },
  "signing": {
    "algorithm": "ed25519",
    "public_key": "<hex 64>",
    "signed_digest": "sha256(canonical manifest without 'signing.signature')",
    "signature": "<hex 128>"
  }
}
```

Signature construction (normative):

1. Build the manifest with `signing.signature = ""`.
2. `digest = sha256_hex(canonical_json(manifest))` — canonical = sorted
   keys, compact separators.
3. `signature = ed25519_sign(digest_utf8_bytes)` — note: the *hex string's
   UTF-8 bytes* are signed, matching the audit-trail convention
   (OCP Part 4 §4.1).
4. Write `digest` into `signing.signed_digest` and the hex signature into
   `signing.signature`.

## Verification procedure (what `leanctx-verify` runs)

| Step | Check | Failure meaning |
|---|---|---|
| 1 | ZIP opens; `manifest.json` parses; `version == 1` | malformed bundle |
| 2 | every `files[]` entry present; SHA-256 matches; no extra payload files | content swapped/added/removed |
| 3 | audit segment replays: first `prev_hash == chain.anchor_prev_hash`, every `entry_hash == sha256(prev ‖ canonical entry data)`, last `== chain.head_hash`, count matches | chain tampered or truncated |
| 4 | manifest digest recomputes; Ed25519 signature verifies against `signing.public_key` (or an out-of-band `--pubkey`) | manifest forged or signed by another key |
| 5 | per-entry signatures (where present) verify against the same key | entry provenance broken |

A bundle PASSES only if every step passes. One flipped byte anywhere
fails step 2, 3 or 4 (covered by mutation tests in CI).

## Threat model (honest limits)

* The chain proves **order and integrity after recording** — it cannot
  prove events were never omitted *before* being written. Mitigation:
  short flush intervals; future counter-anchoring (v2 candidate).
* A segment is verified **relative to its anchor**. `anchor_prev_hash`
  proves continuity with the preceding history only if the verifier also
  holds that history (or a previously accepted bundle ending at the
  anchor).
* The manifest key is **self-attested** unless the auditor receives the
  public key out-of-band (`--pubkey`); `leanctx-verify` reports which mode
  was used.
* Coverage reports are **statements by the engine**, reproducible by
  re-running `lean-ctx policy coverage` against the bundled pack — they
  are not third-party attestations.

## Sections reserved for future versions

`slo/` (SLO attainment reports, GL #391) and `registry/` (extension
provenance) are reserved paths: v1 verifiers MUST ignore unknown
*reserved* directories but MUST reject unknown files elsewhere (step 2).

## Compatibility

Additive evolution within v1 (new optional manifest fields, new reserved
sections). Any change to canonicalization, hashing or signature
construction requires `evidence-bundle-v2`.
