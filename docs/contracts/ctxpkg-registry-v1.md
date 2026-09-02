# Contract: ctxpkg Registry v1 (GL #406)

> **Status: historical research contract — unavailable.** This proposed hosted
> registry, account, storefront, and control-plane flow is not a current LeanCTX
> product or public endpoint. LeanCTX is **The Context SDK for AI Agents**; the
> available `.ctxpkg` substrate is local and signed. Current scope and status are
> governed by `docs/internal/README.md` (internal, not in this repository).

Status: historical research
Consumers: `lean-ctx pack publish/install` (CLI), ctxpkg.com storefront,
leanctx.com account pages (token management), control plane (server)

The hosted registry for `.ctxpkg` context packages, served via
**ctxpkg.com**. v1 is deliberately minimal: publish, index, download, yank —
no payouts, no search API, no WASM plugins (see Out of scope).

## Identity & trust model

| Concern | Who enforces it | How |
|---|---|---|
| Authenticity | Registry (publish gate) | mandatory ed25519 signature, verified server-side against the engine's exact signing message |
| Name binding | Registry (publish gate) | `manifest.name` must equal `@{namespace}/{name}` from the URL; version likewise |
| Transport integrity | Client (install gate) | artifact SHA-256 from the index compared against downloaded bytes |
| Content integrity | Client (install gate) | engine `verify_integrity` (content_hash, composite sha256, byte_size) + local ed25519 re-verification |
| Immutability | Registry (DB constraint) | `(package, version)` unique; re-publish → 409; yank flips a flag, never deletes |

The signing message is the engine's `ctxpkg-sign-v1` format
(`rust/src/core/context_package/signing.rs`):

```
message = hex(sha256("ctxpkg-sign-v1:{name}:{version}:{integrity.sha256}"))
signature = ed25519_sign(signing_key, message_bytes)
```

## Namespaces

- One namespace per account, claimed once, **permanent in v1**.
- **Org-owned namespaces (GL #524)**: claim with `org_id` — the claimer
  stays the *anchor* (FK target for tokens/domains); org owners/admins
  manage (mint publish tokens, domains), members mint read tokens and
  install private packages. Requires owner/admin role in the org to claim.
- Format: `[a-z0-9-]`, 2–32 chars, no leading/trailing dash.
- Reserved (never self-serve): `leanctx`, `lean-ctx`, `ctxpkg`, `official`,
  `verified`, `registry`, `api`, `www`, `admin`, `internal`, `system`,
  `root`, `staff`, `support`.
- Package names: `[a-z0-9-]`, 1–64 chars. Versions: SemVer
  `MAJOR.MINOR.PATCH` with optional pre-release/build suffix.

## Visibility (GL #524)

- `manifest.visibility`: `public` (default) or `private`.
- Private packages never appear in catalog/search/badge; index, manifest
  and download answer **404** unless the request carries a bearer token of
  the owning namespace (any scope). Authenticated private responses are
  `Cache-Control: private, no-store`.
- Set at export: `lean-ctx pack export <name> --sign --private`.

## Paid packs (GL #529, v0 first-party)

- Price is **registry metadata** (`registry_packages.price_cents/currency`),
  *not* part of the signed manifest — repricing never forces a release.
  Set via `PUT /api/account/registry/price {name, price_cents}` (owner/admin;
  `0`/`null` clears; v0 cap 50 000 cents).
- Catalog/index expose `price: {amount_cents, currency} | null`. Discovery
  of paid packs stays public — only the **download** is gated.
- Download gate: no purchase and no owning-namespace token → **402** with an
  actionable `{"error": …}` (where to buy). Purchased or owner → 200 with
  `Cache-Control: private, no-store`.
- Buy flow: `POST /api/account/registry/buy {namespace, name}` (session auth
  on leanctx.com) → Stripe Checkout (`mode=payment`, metadata
  `kind=ctxpkg_purchase`, `user_id`, `package_id`) → webhook
  `checkout.session.completed` records the purchase (idempotent on the
  session id). Purchases bind to the account; buyers mint **account install
  tokens** (`ctxr_…`, no namespace required) to install.
- Refunds: manual in v0 (Stripe dashboard; delete the purchase row to
  revoke access).

## Public surface (ctxpkg.com → control plane `/registry/v1/*`)

ctxpkg.com proxies `/api/*` → control-plane `/registry/*`. All bodies JSON
unless noted.

| Method & path (public form) | Auth | Returns |
|---|---|---|
| `GET /api/v1/index.json` | — | global catalog (public packages only): `{schema:"ctxpkg-registry-index-v1", packages:[{namespace,name,scoped_name,description,latest_version,versions,downloads,updated_at,tags,verified,visibility,quality}]}` (Cache-Control 300 s) |
| `GET /api/v1/packages/{ns}/{name}/index.json` | token if private | `{schema:"ctxpkg-registry-package-v1", latest, visibility, versions:[{version,artifact_sha256,size_bytes,signer_public_key,yanked,downloads,published_at}]}` (Cache-Control 60 s public / no-store private) |
| `GET /api/v1/packages/{ns}/{name}/{version}/download` | token if private | artifact bytes (`application/octet-stream`), `x-ctxpkg-sha256` header, `x-ctxpkg-yanked: true` when yanked. Yanked stays downloadable (reproducibility) but is excluded from `latest`. |
| `PUT /api/v1/packages/{ns}/{name}/{version}` | Bearer `ctxp_…` (publish scope) | 201 `{published, artifact_sha256, size_bytes, trust_report}`; 400 invalid/unsigned/read-token; 401 bad token; 409 version exists; 413/400 too large |
| `DELETE /api/v1/packages/{ns}/{name}/{version}` | Bearer `ctxp_…` (publish scope) | 200 `{yanked}` — yank, not delete; owner namespace only |

Publish-time checks (`trust_report`, persisted per release):
`schema:"ok"`, `signature:"verified"`, `name_version_match`,
`size_within_limit` (default cap 8 MiB), and an explicit `skipped` list
(`wasm_capability_audit`, `malware_heuristics`) — the report never claims
more than was checked.

## Publisher self-service (leanctx.com account → edge → control plane)

Edge routes (session auth, `lean-ctx-enterprise` `cloud_server`, ADR-023):

| Edge route | Forwards to (internal-key auth) |
|---|---|
| `GET /api/account/registry` | `GET /api/billing/registry/{user_id}` |
| `PUT /api/account/registry/namespace` | `PUT /api/billing/registry/{user_id}/namespace` |
| `POST /api/account/registry/tokens` | `POST /api/billing/registry/{user_id}/tokens` |
| `DELETE /api/account/registry/tokens/{id}` | `DELETE /api/billing/registry/{user_id}/tokens/{id}` |
| `PUT /api/account/registry/price` | `PUT /api/billing/registry/{user_id}/price` |
| `POST /api/account/registry/buy` | `POST /api/billing/registry/{user_id}/buy` |

Tokens (GL #524 scopes): 256-bit, plaintext shown exactly once at mint; only
the SHA-256 is stored. Max 10 active per publisher; revocation is immediate.

| Scope | Prefix | May | Minted by |
|---|---|---|---|
| `publish` (default) | `ctxp_` | publish, yank, install (incl. private) | anchor / org owner / org admin |
| `read` | `ctxr_` | install only (incl. private + purchased) — CI-safe | any org member; **any account** (buyer install tokens, GL #529) |

`POST …/tokens` body: `{label?, scope?: "publish"\|"read"}`. The claim body
accepts `{namespace, org_id?}`; org claims need owner/admin in that org.
Accounts without a namespace may mint `read` tokens only (`GET
/api/account/registry` then returns `namespace: null` plus their tokens and
purchases).

## CLI surface

```bash
lean-ctx pack export <name>[@version] --sign     # ed25519-signed bundle
                                                 # key: ~/.lean-ctx/keys/ctxpkg-ed25519.key
                                                 # (auto-generated, 0600 — back it up)
lean-ctx pack export <name> --sign --private     # private on the hosted registry
lean-ctx pack publish <file.ctxpkg> [--registry <url>] [--token <ctxp_…>]
                                                 # token also via CTXPKG_TOKEN
                                                 # ctxr_ tokens are rejected locally
lean-ctx pack install <ns>/<name>[@version] [--registry <url>]
                                                 # registry also via CTXPKG_REGISTRY
                                                 # CTXPKG_TOKEN (ctxp_/ctxr_) unlocks
                                                 # private packages
```

Install resolves `latest` (newest non-yanked) unless pinned; a pinned yanked
version installs with a loud warning. Every remote install is recorded in
the project's `.lean-ctx/ctxpkg.lock`:

```toml
[[package]]
name = "@acme/auth-context"
version = "1.2.0"
artifact_sha256 = "…"
registry = "https://ctxpkg.com/api"
```

## Out of scope for v1 (deliberate)

- **WASM plugins / policy packs as registry artifacts** — blocked on the
  capability-audit pipeline (#403 signing story); v1 hosts signed `.ctxpkg`
  context packages only.
- Publisher payouts (Stripe Connect rev-share, GL #532), namespace
  transfer, multiple keys per publisher (key rotation = new publisher
  identity in v1).
- ~~Server-side search~~ → shipped (GL #514). ~~Org-owned namespaces,
  private packages, read tokens~~ → shipped (GL #524, P2).
  ~~First-party paid packs~~ → shipped (GL #529, P3 v0).

## Module map

| Concern | Where |
|---|---|
| Server: routes + validation | control plane `src/routes/registry.rs`, `src/routes/registry_tokens.rs`, `src/routes/registry_purchases.rs` |
| Server: queries / blobs / verify | control plane `src/registry_db.rs`, `src/registry_store.rs`, `src/registry_verify.rs` |
| Client: remote calls | `rust/src/core/context_package/remote.rs` |
| Client: signing keys | `rust/src/core/context_package/keys.rs` |
| Client: lockfile | `rust/src/core/context_package/lockfile.rs` |
| Edge proxies | `lean-ctx-enterprise: cloud_server/billing_edge.rs` |
| CLI | `rust/src/cli/pack_cmd.rs` |
