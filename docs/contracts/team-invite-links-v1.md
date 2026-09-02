# Team Invite Links v1 (GL #385)

> **Status: historical research contract — unavailable.** This describes a
> proposed hosted team, dashboard, and account flow. It is not a current LeanCTX
> product surface. LeanCTX is **The Context SDK for AI Agents**; current scope
> and status are governed by `docs/internal/README.md` (internal, not in this repository).

One-time links replace manual token copy-paste when onboarding teammates onto
a hosted team server. The owner mints a link on the dashboard; the teammate
opens it on `leanctx.com/join/?code=…` and receives their own member token —
shown exactly once, with prefilled setup snippets.

## Lifecycle

```
owner dashboard ── POST /api/account/team/invites ──► edge ──► control plane
                                                              mints 64-hex code
                                                              stores SHA-256 only
        ◄── { code, expires_at } (code shown exactly once) ──┘

teammate ── opens /join/?code=… ── clicks "Join the team"
         ── POST /api/team/join { code } ──► edge (rate-limited, no account)
                                             └─► control plane redeem:
                                                 1. atomic claim (consumed_at)
                                                 2. seat check (auto-grow like
                                                    direct member add)
                                                 3. mint member token
                                                 4. team.json re-render + deploy
         ◄── { token, role, team_url } (token shown exactly once)
```

- **TTL**: 7 days from mint.
- **Single use**: the `consumed_at IS NULL` claim admits exactly one winner
  under concurrency. If the post-claim mint fails (seat ceiling, Stripe,
  redeploy), the claim is released and the invite stays redeemable.
- **Roles**: invites carry `viewer` or `member` only — a link can never grant
  admin or owner.
- **Pending cap**: 25 open invites per team (hygiene bound; seats are only
  consumed at redeem time).
- **Revocation**: pending invites are revocable from the dashboard, exactly
  like member tokens. Used/expired invites stay listed for audit.

## Security

- Codes are 256-bit (64 hex chars, `provisioning::mint`); only the SHA-256 is
  stored. A leaked database cannot resurrect codes.
- The public join endpoint is rate-limited per salted IP hash (10 attempts/h)
  and validates the code shape before any upstream call.
- Unknown / expired / revoked / used codes all yield one neutral 404 — the
  endpoint cannot be used to probe which codes exist.
- The join page redeems only on an explicit click, so link-preview bots and
  mail scanners never burn the one-time code.

## API surface

### Edge (api.leanctx.com, open backend)

| Route | Auth | Purpose |
|---|---|---|
| `POST /api/account/team/invites` | account bearer | mint; body `{label?, role?}`; returns `code` once |
| `GET /api/account/team/invites` | account bearer | audit list (`status`: pending/used/revoked/expired) |
| `DELETE /api/account/team/invites/{invite_id}` | account bearer | revoke a pending invite |
| `POST /api/team/join` | none (code is the credential) | redeem; returns `{token, token_id, role, team_url}` once |

### Control plane (private)

| Route | Purpose |
|---|---|
| `POST /api/billing/team/{user_id}/invites` | mint + persist hash |
| `GET /api/billing/team/{user_id}/invites` | list rows |
| `DELETE /api/billing/team/{user_id}/invites/{invite_id}` | revoke pending |
| `POST /api/billing/invites/redeem` | atomic claim → seat check → token mint → config sync |

Storage: `billing_team_invites` (id, user_id, code_sha256 unique, label, role,
created_at, expires_at, consumed_at, consumed_token_id, revoked_at).

## Failure modes the teammate can see

| Case | Status | Message |
|---|---|---|
| malformed code | 400 | "that does not look like an invite code" |
| dead code (any reason) | 404 | "invalid, expired, or already used" |
| seat limit (no auto-grow) | 400 | control-plane text ("seat limit reached …") — invite stays redeemable after the owner frees a seat |
| rate limit | 429 | "too many attempts" |
| billing plane down | 502/503 | generic retry text; the invite is not consumed |
