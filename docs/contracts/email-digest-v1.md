# Email Digest v1 (GL #386)

Pillar: Money-Hooks / Stickiness
Scope: monthly Pro digest, weekly Team digest, opt-out

## Behaviour

The cloud server (`api.leanctx.com`) runs an hourly background job
(`cloud_server/digest.rs`) that sends each eligible account at most one
digest per period:

| Plan | Cadence | Period key | Data source |
|------|---------|------------|-------------|
| Pro | monthly | previous calendar month, `YYYY-MM` | synced CEP snapshots (`cep_scores`) — same aggregation as `/api/account/cloud` |
| Team / Enterprise | weekly | previous ISO week, `YYYY-Www` | hosted team server savings summary, proxied via the billing plane (audit-only control token) |

Rules:

- **Real numbers only.** A period with no synced activity (Pro) or no
  reporting members (Team) sends nothing; the period is claimed silently so
  it is never re-evaluated.
- **Idempotent.** `digest_log (user_id, kind, period_key)` is the send gate
  (`INSERT … ON CONFLICT DO NOTHING`). A failed SMTP send releases the claim,
  so the next hourly tick retries. Catch-up after downtime is automatic
  because periods reference the *previous* month/week.
- **Eligibility.** Verified email + Pro/Team plan (resolved live from the
  billing plane). No SMTP configured ⇒ the job is a no-op and claims nothing.
- **Billing-plane outages** are errors (no claim), not empty digests.

## Opt-out

- Every digest footer carries `GET /api/digest/opt-out?token=<64-hex>` — a
  one-click, login-free unsubscribe. Tokens are stored as SHA-256 and rotated
  on every send (the newest email always works; older links go stale).
  Unknown tokens get the same neutral confirmation page (no account probing).
- `GET /api/account/digest` → `{ "optOut": bool }` (authenticated).
- `PUT /api/account/digest` `{ "optOut": bool }` — dashboard toggle,
  re-enable after an email opt-out.
- Preference store: `email_prefs (user_id, digest_opt_out,
  opt_out_token_sha256, updated_at)`.

## Email shape (plain text)

Pro (subject `Your LeanCTX month — 4.2M tokens saved`):

```
May 2026 in numbers:

- Tokens saved: 4.2M (all-time: 78.0M)
- Agent actions measured: 3.4k
- Sessions synced: 18
- Mean CEP score: 87

Full picture: https://leanctx.com/account/cloud/

—
You receive this monthly digest because cloud sync is enabled on your LeanCTX Pro account.
Unsubscribe (one click): https://api.leanctx.com/api/digest/opt-out?token=…
```

Team (subject `Your team saved 78.0M tokens (~$196.42) — 2026-W23`):

```
Team ROI for 2026-W23:

- Net tokens saved: 78.0M (~$196.42)
- Measured agent actions: 36.0k
- Reporting members: 4
- Top model: claude-opus
- Top tool: ctx_read

Full dashboard: https://leanctx.com/account/team/
```

## Operational notes

- The job resolves plans via one billing-plane call per candidate per tick;
  candidates are pre-filtered by the ledger and opt-out flag, so steady-state
  cost is one `users` scan per hour.
- CORS on the cloud server allows `PUT`/`PATCH` (needed by the digest toggle
  and the team settings endpoint, GL #388).
