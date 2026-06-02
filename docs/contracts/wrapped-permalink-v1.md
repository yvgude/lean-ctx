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
- **honest**: the `pricing_estimated` marker is preserved end-to-end; estimates stay labelled.

## Version (SSOT)

- Runtime: `rust/src/cloud_server/wrapped.rs`
- Schema: `rust/src/cloud_server/db.rs` (`init_schema`, table `wrapped_cards`)
- Routing + CORS: `rust/src/cloud_server/mod.rs`
- Client (publish/unpublish/leaderboard): `rust/src/cli/wrapped_publish.rs` (`gain --publish [--leaderboard]`)
- Permalink + leaderboard pages: server-rendered by the cloud API; `leanctx.com` proxies `/w/` and
  `/leaderboard` via `website/nginx.conf` (deploy branch)

---

## Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/api/wrapped` | none (rate-limited per `ip_hash`) | Publish a whitelisted card → `{ id, edit_token, url }` |
| GET | `/api/wrapped/:id` | none | Fetch the public card; increments `view_count` |
| DELETE | `/api/wrapped/:id` | `X-Edit-Token` | Delete the card (wrong/absent token → 403) |
| POST | `/api/wrapped/:id/claim` | account bearer + `X-Edit-Token` | Bind the anonymous card to the account |
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
- **Claim** — an authenticated user (`Authorization: Bearer <api_key|oauth>`) who also presents the
  matching `X-Edit-Token` binds the card to their `user_id`. This is the bridge to future cloud
  sync; claiming is idempotent and never required.

---

## Publish payload (the ONLY accepted fields)

`POST /api/wrapped` body — validated into a strict struct with `#[serde(deny_unknown_fields)]`.
Any unknown field → `400 invalid_payload`.

| Field | Type | Bound / validation | Source |
|-------|------|--------------------|--------|
| `period` | string | one of `day` \| `week` \| `month` \| `all` | time bucket |
| `tokens_saved` | integer | `>= 0` | headline (net of bounce) |
| `cost_avoided_usd` | number | `>= 0` | headline |
| `pricing_estimated` | bool | — | honesty marker |
| `compression_rate_pct` | number | `0..=100` | aggregate |
| `total_commands` | integer | `>= 0` | aggregate count |
| `sessions_count` | integer | `>= 0` | aggregate count |
| `files_touched` | integer | `>= 0` | aggregate count |
| `top_commands` | array | `<= 12` items | tool/prefix names + pct |
| `top_commands[].name` | string | `1..=40` chars, no markup | tool name, not user data |
| `top_commands[].pct` | number | `0..=100` | share of activity |
| `model_key` | string? | optional, `<= 60` chars | public model id (opt-out via `--no-model`) |
| `display_name` | string? | optional, `1..=60` chars, no `<`/`>`/control chars | user-chosen label |
| `leaderboard_opt_in` | bool | optional, default `false` | list this card on the public leaderboard (`--leaderboard`) |

**Server-rejected / never stored:** repo names, file paths, code, env vars, machine id, raw shell
history, client IP (only a salted `ip_hash` is kept, abuse-only), and any field not listed above.

Request body is capped at **8 KB**; larger bodies → `413 payload_too_large`.

---

## Responses

**`POST /api/wrapped` → `201`**
```json
{ "id": "9f86d081884c7d65...", "edit_token": "<256-bit hex, shown once>", "url": "https://leanctx.com/w/9f86d081884c7d65..." }
```

**`GET /api/wrapped/:id` → `200`**
```json
{
  "id": "9f86d081884c7d65...",
  "created_at": "2026-06-02T07:00:00Z",
  "view_count": 42,
  "card": { "period": "week", "tokens_saved": 480600000, "cost_avoided_usd": 1441.79, "pricing_estimated": true, "compression_rate_pct": 91.2, "total_commands": 1234, "sessions_count": 56, "files_touched": 789, "top_commands": [ { "name": "ctx_search", "pct": 60.0 } ], "model_key": "claude-opus", "display_name": "yvesg" }
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
  tokens_saved    BIGINT NOT NULL DEFAULT 0     -- denormalized for leaderboard ORDER BY
);
CREATE INDEX IF NOT EXISTS wrapped_cards_ip_created ON wrapped_cards (ip_hash, created_at);
CREATE INDEX IF NOT EXISTS wrapped_cards_leaderboard ON wrapped_cards (leaderboard_opt_in, tokens_saved DESC);
```

### Leaderboard

`leaderboard_opt_in` defaults to **off**: a published card is private-by-link unless the user passes
`--leaderboard`. The leaderboard query returns the top **50** opt-in cards by `tokens_saved`
(`tokens_saved` is denormalized at publish so the listing never parses every payload). The only
person-facing field surfaced is the user-chosen `display_name`; everything else is an aggregate.

---

## Abuse & safety

- **Rate limit**: at most 20 publishes per rolling hour per `ip_hash`; over the cap → `429`.
- **`ip_hash`**: `sha256(salt + client_ip)`, where `client_ip` is read from `X-Forwarded-For` /
  `X-Real-IP` (set by the Traefik front proxy) and `salt` from `LEANCTX_CLOUD_IP_SALT`. The raw IP
  is never stored; the hash exists solely to bound abuse and is not used for tracking.
- **Body cap** 8 KB; **`display_name`** length-capped and rejected if it contains markup/control
  characters (defence against stored XSS); the frontend additionally HTML-escapes on render.
- **Ids** are ≥128-bit from a CSPRNG → not enumerable; `GET` never reveals the `edit_token`.
