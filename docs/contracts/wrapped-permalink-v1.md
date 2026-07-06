# Wrapped Permalink Contract v1

## Goal

A **versioned HTTP API contract** for the opt-in, hosted **Wrapped permalink** — the public side
of the lean-ctx viral loop (`docs/business/10-wrapped-viral-loop-spec.md`, VL-3). A user may
**anonymously publish** a curated, privacy-safe slice of their local Wrapped report and get back a
shareable URL (`https://leanctx.com/w/<id>`). No login is required to publish; an account may later
**claim** the card.

- **opt-in only**: nothing is uploaded unless the user runs `lean-ctx gain --publish`.
- **whitelist-only**: the server accepts a closed set of aggregate fields (`deny_unknown_fields`);
  repo names, paths, code, env vars, machine ids, raw history and IPs are rejected or never sent.
- **anonymous-first**: publish returns a public `id` and a one-time secret `edit_token`; the token
  authorizes update/delete and the optional account claim.
- **stable without login** (v1.1): an optional Ed25519 signature binds a card to a login-less
  `publisher_id` derived from the machine's public key. Re-publishing then **upserts** a single card
  per `(publisher_id, period)` — one stable URL, no duplicates, no account (VL-3c).
- **minimal by default** (v1.2): the client now publishes only the four numbers anything public
  uses — `tokens_saved`, `cost_avoided_usd`, `compression_rate_pct` (energy is *derived* from
  tokens, never sent) — plus `period`, the optional `display_name` and `leaderboard_opt_in`.
  Command/session/file counts, top command names and the model id are **no longer collected**.
- **honest**: the `pricing_estimated` marker is preserved end-to-end; estimates stay labelled.

## Version (SSOT)

- Runtime: `rust/src/cloud_server/wrapped.rs`
- Schema: `rust/src/cloud_server/db.rs` (`init_schema`, table `wrapped_cards`)
- Routing + CORS: `rust/src/cloud_server/mod.rs`
- Login-less identity (Ed25519): `rust/src/core/agent_identity.rs` (sign/verify, shared with the signed savings ledger)
- Client (publish/unpublish/leaderboard): `rust/src/cli/wrapped_publish.rs` (`gain --publish [--leaderboard] [--name=…]`, `[gain] auto_publish`)
- Permalink + leaderboard pages: server-rendered by the cloud API; `leanctx.com` proxies `/w/` and
  `/leaderboard` via `website/nginx.conf` (deploy branch)

---

## Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/api/wrapped` | none (rate-limited per `ip_hash`) | Publish/refresh a card. Bare payload → anonymous insert (`201`); signed envelope → upsert by `(publisher_id, period)` (`201` insert / `200` update) → `{ id, url, edit_token? }` |
| GET | `/api/wrapped/:id` | none | Fetch the public card; increments `view_count` |
| DELETE | `/api/wrapped/:id` | `X-Edit-Token` | Delete the card (wrong/absent token → 403) |
| POST | `/api/wrapped/:id/claim` | account bearer + `X-Edit-Token` | Bind the anonymous card to the account |
| POST | `/api/wrapped/:id/link/start` | `X-Edit-Token` | Mint a short-lived pairing code for login-less machine linking → `{ code, expires_in_secs }` |
| POST | `/api/wrapped/:id/link/complete` | `X-Edit-Token` | Join this card into the code's `link_group` (body: `{ "code": "XXXX-XXXX" }`) |
| GET | `/api/wrapped/:id/card.svg` | none | Server-rendered share card (SVG) |
| GET | `/api/wrapped/:id/card.png` | none | Rasterized Open Graph image (PNG, 1200×630, `resvg`) |
| GET | `/w/:id` | none | Crawler-friendly permalink page (per-card OG/Twitter meta); counts as a view |
| GET | `/leaderboard` | none | Server-rendered public leaderboard (opt-in cards, top by tokens saved) |
| GET | `/api/leaderboard` | none | Leaderboard as JSON (`{ "entries": [ … ] }`) |

The canonical share host is `leanctx.com`; the static-site nginx proxies `/w/` and `/leaderboard`
to the cloud API (`website/nginx.conf`). `og:image` points at `api.leanctx.com/api/wrapped/:id/card.png`
directly, so no asset route needs proxying on the canonical host.

---

## Identity model (`anon_claim`)

- **`id`** — public, unguessable 128-bit identifier, hex-encoded (32 chars). It is the URL slug.
- **`edit_token`** — 256-bit secret returned **once** at publish, stored client-side in
  `~/.lean-ctx/wrapped/published.json`. The server persists only `sha256(edit_token)`.
- **Claim** — an authenticated user (identified by a standard bearer credential — API key or OAuth —
  in the `Authorization` request header) who also presents the matching `X-Edit-Token` binds the card
  to their `user_id`. This is the bridge to future cloud sync; claiming is idempotent and never required.
- **Link (login-less, v1.2, GH #736)** — two machines merge into one leaderboard entry without any
  account. Machine A mints a pairing code (`POST /:id/link/start`, authorized by its own
  `X-Edit-Token`); machine B presents the code plus *its own* `X-Edit-Token`
  (`POST /:id/link/complete`). Both cards then share a `link_group` and the leaderboard stacks them
  (tokens summed, token-weighted rate, highest-saving machine as representative). Grouping is
  transitive across `link_group` and `user_id`. Codes: 8 chars from an unambiguous alphabet
  (`XXXX-XXXX`), single-use, 10-minute TTL, at most 3 outstanding per card, stored hashed
  (`sha256`). A leaked expired code is useless; no PII is involved at any point.

---

## Signed publisher identity (v1.1 — login-less, idempotent)

To make re-publishing idempotent **without any login**, the client may wrap the payload in a signed
envelope. The envelope is the body of `POST /api/wrapped`:

```json
{
  "payload_json": "<the exact PublishPayload JSON string the client signed>",
  "public_key":   "<hex Ed25519 public key, 64 chars>",
  "signature":    "<hex Ed25519 signature over payload_json bytes, 128 chars>"
}
```

- **Key = identity.** The key is the machine's persistent Ed25519 keypair (`agent_identity.rs`,
  `~/.lean-ctx/keys/`), the same identity that signs the savings ledger. No account, no email, no login.
- **`publisher_id` = `sha256(public_key_hex)[..32]`**, derived **server-side** — a stable, non-reversible
  pseudonym. The client never asserts its own id, so one cannot publish under another machine's identity
  without holding its private key.
- **Verification.** The server verifies the signature over the **exact** `payload_json` bytes before
  parsing/validating it, then stores `payload_json` verbatim (so a stored card stays re-verifiable). A
  missing/invalid signature → `401 invalid_signature`.
- **Upsert.** Insert with `ON CONFLICT (publisher_id, period) DO UPDATE` — a re-publish from the same
  machine refreshes its existing card **in place** (same `id`/URL). `201` (with `edit_token`) on the
  first publish, `200` (no token; the client keeps the one it stored) on every refresh.
- **Backward compatible.** A bare payload object (old clients) still takes the legacy anonymous-insert
  path (`publisher_id` NULL), which may create duplicates — those are de-duplicated on the leaderboard.

---

## Publish payload (the ONLY accepted fields)

`POST /api/wrapped` body — validated into a strict struct with `#[serde(deny_unknown_fields)]`.
Any unknown field → `400 invalid_payload`.

**Current fields (v1.2)** — everything a current client sends:

| Field | Type | Bound / validation | Source |
|-------|------|--------------------|--------|
| `period` | string | one of `day` \| `week` \| `month` \| `all` | time bucket / upsert key |
| `tokens_saved` | integer | `>= 0` | headline (net of bounce); energy is derived from this |
| `cost_avoided_usd` | number | `>= 0` | headline |
| `pricing_estimated` | bool | — | honesty marker |
| `compression_rate_pct` | number | `0..=100` | aggregate, shown on the leaderboard |
| `display_name` | string? | optional, `1..=60` chars, no `<`/`>`/control chars | user-chosen label |
| `leaderboard_opt_in` | bool | optional, default `false` | list this card on the public leaderboard (`--leaderboard`) |

**Legacy fields (accepted, ignored)** — still parsed (optional, defaulted) so cards from clients
older than v1.2 keep deserializing, but **no longer collected by current clients and never
rendered publicly**: `total_commands`, `sessions_count`, `files_touched`, `top_commands[]`
(`name` ≤ 40 chars / `pct`), `model_key`. The hosted card omits any of these that are zero/empty.

**Server-rejected / never stored:** repo names, file paths, code, env vars, machine id, raw shell
history, client IP (only a salted `ip_hash` is kept, abuse-only), and any field not listed above.

Request body is capped at **8 KB**; larger bodies → `413 payload_too_large`.

---

## Responses

**`POST /api/wrapped` → `201`** (fresh insert — anonymous, or first signed publish)
```json
{ "id": "9f86d081884c7d65...", "edit_token": "<256-bit hex, shown once>", "url": "https://leanctx.com/w/9f86d081884c7d65..." }
```

**`POST /api/wrapped` → `200`** (signed re-publish — existing card updated in place; no new `edit_token`)
```json
{ "id": "9f86d081884c7d65...", "url": "https://leanctx.com/w/9f86d081884c7d65..." }
```

**`GET /api/wrapped/:id` → `200`**
```json
{
  "id": "9f86d081884c7d65...",
  "created_at": "2026-06-02T07:00:00Z",
  "view_count": 42,
  "card": { "period": "week", "tokens_saved": 480600000, "cost_avoided_usd": 1441.79, "pricing_estimated": true, "compression_rate_pct": 91.2, "display_name": "yvesg", "leaderboard_opt_in": true }
}
```

**`DELETE /api/wrapped/:id` → `200`** `{ "deleted": true }`
**`POST /api/wrapped/:id/claim` → `200`** `{ "claimed": true }`

---

## Error responses

Errors use the cloud server's JSON convention (`{"error":"<code>"}`), `Content-Type: application/json`:

| Status | `error` code | Cause |
|--------|--------------|-------|
| 400 | `invalid_payload` | unknown field, wrong type, or failed bound/shape validation |
| 403 | `forbidden` | missing/incorrect `X-Edit-Token` (delete/claim) |
| 401 | `unauthorized` | claim without a valid account bearer token |
| 401 | `invalid_signature` | signed envelope with a missing/invalid Ed25519 signature |
| 404 | `not_found` | unknown `id` |
| 413 | `payload_too_large` | body over the 8 KB cap |
| 429 | `rate_limited` | too many publishes from the same `ip_hash` within the window |
| 500 | `internal_error` | unexpected server/database error |

---

## Storage

Added to `init_schema` (JSON stored as `TEXT`, matching the existing `models_snapshot`/`buddy_state`
convention rather than JSONB):

```sql
CREATE TABLE IF NOT EXISTS wrapped_cards (
  id              TEXT PRIMARY KEY,            -- 128-bit unguessable, hex
  edit_token_hash TEXT NOT NULL,               -- sha256 of the one-time secret
  user_id         UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  payload_json    TEXT NOT NULL,               -- validated whitelist, re-serialized
  created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  ip_hash         TEXT NULL,                   -- salted, abuse-only (never an IP)
  view_count      BIGINT NOT NULL DEFAULT 0,
  leaderboard_opt_in BOOLEAN NOT NULL DEFAULT FALSE, -- public leaderboard opt-in
  tokens_saved    BIGINT NOT NULL DEFAULT 0,    -- denormalized for leaderboard ORDER BY
  publisher_id    TEXT NULL,                    -- v1.1: sha256(public_key)[..32], login-less identity
  period          TEXT NULL                     -- v1.1: upsert key alongside publisher_id
);
CREATE INDEX IF NOT EXISTS wrapped_cards_ip_created ON wrapped_cards (ip_hash, created_at);
CREATE INDEX IF NOT EXISTS wrapped_cards_leaderboard ON wrapped_cards (leaderboard_opt_in, tokens_saved DESC);
-- v1.1: one card per (publisher, period). Partial → legacy anonymous rows (NULL) never collide.
CREATE UNIQUE INDEX IF NOT EXISTS wrapped_cards_publisher_period
  ON wrapped_cards (publisher_id, period) WHERE publisher_id IS NOT NULL;
```

### Leaderboard

`leaderboard_opt_in` defaults to **off**: a published card is private-by-link unless the user passes
`--leaderboard`. The query returns the top **50** opt-in cards by `tokens_saved` (denormalized at
publish so the listing never parses every payload), **de-duplicated to one row per publisher** via
`DISTINCT ON (COALESCE(publisher_id, id))` — a signed publisher appears once (their highest-saving
period); legacy anonymous rows (`publisher_id` NULL) each stay distinct. Each row surfaces
`tokens_saved`, `compression_rate_pct`, `cost_avoided_usd` and a derived energy figure; the only
person-facing field is the user-chosen `display_name`. Everything else is an aggregate.

---

## Abuse & safety

- **Rate limit**: at most 20 publishes per rolling hour per `ip_hash`; over the cap → `429`.
- **`ip_hash`**: `sha256(salt + client_ip)`, where `client_ip` is read from `X-Forwarded-For` /
  `X-Real-IP` (set by the Traefik front proxy) and `salt` from `LEANCTX_CLOUD_IP_SALT`. The raw IP
  is never stored; the hash exists solely to bound abuse and is not used for tracking.
- **Body cap** 8 KB; **`display_name`** length-capped and rejected if it contains markup/control
  characters (defence against stored XSS); the frontend additionally HTML-escapes on render.
- **Ids** are ≥128-bit from a CSPRNG → not enumerable; `GET` never reveals the `edit_token`.

