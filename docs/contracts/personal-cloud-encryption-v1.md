# personal-cloud-encryption-v1 — Zero-Knowledge Vaults (Knowledge + Gotchas)

> **Status: Historical / Research.** This retained record is not current
> onboarding or a release claim. LeanCTX is the **Context Gateway for AI Systems**.
> See [current positioning](../POSITIONING_CANONICAL.md) and the relevant edition's
> released contract; older product definitions and availability below are historical.

Status: historical research · Engine: `core/knowledge_vault.rs` ·
Server: `lean-ctx-enterprise: cloud_server/knowledge.rs` (`knowledge_blobs`),
`lean-ctx-enterprise: cloud_server/gotchas.rs` (`gotcha_blobs`) (ADR-023)

## Historical design claim

For E2E surfaces, the Personal Cloud backend stores **only ciphertext**. We
cannot read, search, sell or leak knowledge content — provably: the
decryption key is derived from the account API key, of which the server only
ever stores a SHA-256 hash.

## Construction

| Property | Value |
|---|---|
| Cipher | XChaCha20-Poly1305 (AEAD), 24-byte random nonce per seal |
| Key derivation | HKDF-SHA256(salt=`leanctx`, ikm=API key, info=`knowledge-vault-v1` \| `gotcha-vault-v1`) |
| Domain separation | distinct HKDF `info` per surface — the index-bundle key (`index-bundle-v1`), the knowledge-vault key and the gotcha-vault key can never open each other's blobs |
| Envelope | `{"v":1,"entries":[{category,key,value},…]}`, serialized then sealed; wire format `nonce ‖ ciphertext` |
| Consistency | whole-account snapshot, last-writer-wins (same model as `hosted-personal-index-v1`) |

The key is identical on every logged-in device (stable API key, not the
rotating OAuth token) — that is what makes cross-device pull work. Key
rotation = new API key + one re-push from any device that has the local store.

> **Corrected in 3.11.5.** The claim above was wrong: every device logs in
> with its own API key, so each device derived a different key and could not
> open another device's vault. The API-key derivation is now **legacy, read
> only** (migration). Vaults are sealed under the account vault key below.

## Wire protocol (`/api/sync/knowledge`, `/api/sync/gotchas`)

Both routes speak the same dual wire format; each has its own blob table
(`knowledge_blobs` / `gotcha_blobs`) and purges its own legacy table
(`knowledge_entries` / `gotchas`).

| Request | Behaviour |
|---|---|
| `POST` `Content-Type: application/octet-stream` + `X-Entry-Count: N` | store vault blob (≤ 8 MB), then **delete the account's plaintext rows** — the built-in re-encryption migration |
| `POST` `Content-Type: application/json` | legacy plaintext upserts (deprecated; removed two releases after vault clients ship) |
| `GET` `Accept: application/octet-stream` | encrypted vault blob; `404` when the account has none yet |
| `GET` (anything else) | legacy plaintext listing |

`X-Entry-Count` is a client-declared display metadatum (dashboards show
counts and sizes); the server cannot verify it — by design.

Clients pull vault-first and fall back to the legacy listing on `404` *or*
when an older server ignores the `Accept` header (detected via the response
`Content-Type`).

## What is deliberately NOT E2E

| Surface | Why it stays aggregate/plaintext |
|---|---|
| Commands / CEP / Gain stats | numbers only (counts, token totals) — no content; they feed the savings dashboard and the opt-in leaderboard, which require server-side aggregation |
| Supporter / billing metadata | Stripe-owned, never includes code or knowledge |
| Index bundles | already E2E under `hosted-personal-index-v1` |

## Server obligations

- Zero-content logging: sizes, hashes, counts — never payloads.
- `knowledge_blobs` carries `sha256` over the ciphertext for drift detection.
- The legacy table stays queryable until the deprecation window closes, but
  any vault push purges that account's plaintext rows immediately.

## Account vault key (3.11.5)

Engine: `core/vault_account_key.rs`, `cloud_client/vault_key.rs`.

### Key model

| Item | Value |
|---|---|
| Account vault key | 32 random bytes from the OS RNG, created once per account |
| `key_id` | first 32 lowercase hex characters of `SHA-256("leanctx-vault-key-id-v1\0" ‖ key)` |
| Vault keys | `HKDF-SHA256(salt="leanctx", ikm=account key, info="knowledge-vault-v1" \| "gotcha-vault-v1")`; the two vaults stay domain-separated |
| Local custody | `<data dir>/cloud/vault-key.json`, mode `0600`, directory `0700` |
| Recovery code | 128 random bits + 22-bit checksum, Crockford base32, `XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX`; shown once, never stored, never accepted on the command line |
| Wrapping | XChaCha20-Poly1305, key `HKDF-SHA256(salt="leanctx", ikm=code secret, info="vault-key-wrap-v1")`, AAD `"leanctx-vault-key-wrap\0" ‖ version (1) ‖ key_id`; wire `nonce (24B) ‖ ciphertext` |

No slow KDF is used for the code: it carries 128 bits from the OS RNG, not a
human-chosen password. The service stores only the wrapped key and `key_id`
and can open neither the key nor any vault.

### Endpoints

All require the account Bearer token; unknown request fields are refused;
`wrapped` is base64 that decodes to 1..4096 bytes; there is no `DELETE`.

| Request | Answers |
|---|---|
| `GET /api/account/vault-key` | `200 {"wrapped","key_id","created_at":"RFC3339"}` · `404 {"error":"not_found"}` |
| `PUT /api/account/vault-key` `{"wrapped","key_id"}` | create-if-absent: `201 {"key_id"}` · `409 {"error":"exists","key_id":"<existing>"}` |
| `PUT /api/account/vault-key/rewrap` `{"wrapped","key_id"}` | replaces only `wrapped` when `key_id` matches: `200 {"key_id"}` · `409 {"error":"key_mismatch","key_id":"<existing>"}` · `404` |

Every endpoint answers `402 {"error":"pro_required"}` without Pro.

### Engine rules

- **Creation (first writer wins).** A device creates a key only when it has
  none locally, `GET` answers `404`, and every cloud vault blob it is about to
  replace is absent or opens with its own (legacy) key. The key is kept only
  after `201`; on `409` the candidate is discarded and the device reports
  `Personal Cloud vault locked: … run lean-ctx cloud vault unlock`.
- **Unlock.** `lean-ctx cloud vault unlock` reads the code (hidden prompt or
  `--code-file` with an owner-private file), checks the checksum before any
  network call, `GET`s the wrapped key, unwraps it, verifies `key_id`, and
  stores it locally.
- **New code.** `lean-ctx cloud vault new-code` re-wraps the local key under a
  fresh code via `/rewrap`.
- **Reads** try the account key, then the device's legacy API-key key.
  **Pushes** seal with the account key (legacy key only when the service has
  no vault-key endpoint yet), so the next push re-seals a legacy vault.
- **No overwrite.** Every vault push reads the cloud blob first and refuses,
  even with `sync --force`, when this device cannot open it. The blob is kept;
  `lean-ctx cloud status` shows the refusal and the fix.
- A key created by background auto-sync is never printed there; `cloud
  status` asks for `lean-ctx cloud vault new-code` until a code was shown.
