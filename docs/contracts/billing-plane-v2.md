# Contract: Billing Plane v2 — Metered Add-ons (`billing-plane-v2`)

Status: stable · Plane: commercial (Team/Cloud) · Base: [`billing-plane-v1`](billing-plane-v1.md)
Source: engine `rust/src/core/billing/metering.rs` · control plane `lean-ctx-cloud/src/metering.rs`

An **additive** extension of [`billing-plane-v1`](billing-plane-v1.md): it adds a
usage-metered **hosted-index storage-overage** add-on on top of the flat plans,
without changing any plan, entitlement, or the local experience. Everything in
v1 still holds.

> Local-Free Invariant (RFC §4/§6): the Personal (local) plane is free, ungated,
> best-in-class — forever. Metering only **describes** hosted usage; it never
> gates, throttles, or bills a local capability.

## What v2 adds (over v1)

1. A second metering **dimension** alongside the v1 savings-ledger `Usage`:
   **hosted-index storage overage** — bytes stored in the hosted retrieval index
   above the plan's included `hosted_index_mb` quota. It is **server-measured**
   (the team server's `/v1/storage` report), so it needs no client signature.
2. A `metering` block on the control-plane team payloads
   (`/api/account/team/storage` and `/api/account/team/usage`), computed from the
   already-measured storage figures + a configurable rate.
3. A **Stripe Billing Meter** (`event_name = leanctx_hosted_index_storage_gb`,
   aggregation `last`) and a linked metered price, provisioned by
   `stripe-setup.py --storage-metering`.

## Rollout: display-first (opt-in, no surprise bills)

Metering ships **visibility-first**. The rate lives in the control plane env
`LEANCTX_BILLING_STORAGE_OVERAGE_CENTS_PER_GB` (cents per GB / month):

- **Unset / `0`** ⇒ `billing_active = false`. Usage, quota headroom, and the
  threshold state are surfaced; **no projected cost is shown and nothing is
  billed**. `stripe-setup.py --storage-metering` refuses to invent a price.
- **Positive rate** ⇒ `billing_active = true`. A *projected* monthly cost is
  shown for any overage, clearly labelled "estimated · not yet billed" until the
  metered usage-record push is enabled (a deliberate follow-up).

## The `metering` block (`camelCase`)

Carries only numbers — no paths, prompts, or content — so it is safe to surface
and to reconcile against billing.

```json
{
  "usedBytes": 6000000000,
  "quotaBytes": 5000000000,
  "overageBytes": 1000000000,
  "percent": 120.0,
  "unlimited": false,
  "state": "over",
  "rateCentsPerGb": 50,
  "billingActive": true,
  "projectedCostCents": 50
}
```

- `state` ∈ `none | ok | warn (≥50%) | critical (≥80%) | over (≥100%) | unlimited`.
- `quotaBytes` is `null` and `overageBytes`/`projectedCostCents` are `0` for an
  **unlimited** (Enterprise) quota.
- Billing convention: 1 GB = 1e9 bytes (decimal), matching Stripe metered units.

## Invariants (test-enforced)

All of `billing-plane-v1`'s invariants, plus
(`lean-ctx-cloud/src/metering.rs` tests):

1. **`0` (none) is never conflated with `UNBOUNDED` (unlimited)** — a `0` quota
   yields `state = "none"` with no cost; an unlimited quota yields
   `state = "unlimited"` with no overage.
2. `overageBytes = max(0, used − quota)`; unlimited ⇒ `0`.
3. `projectedCostCents = 0` (and is suppressed) whenever `billingActive = false`
   — display-first never bills.
4. The `metering` block is privacy-preserving (numbers only).
5. Only `signed && chain_valid` savings-derived usage passes the frozen v1
   `Usage::is_billable` source-integrity predicate; this alone never
   authorizes settlement or an invoice (see
   [`settlement-evidence-v2`](settlement-evidence-v2.md)). The storage dimension
   is server-measured and additive.
6. Nothing in the metering path gates a local feature (Local-Free preserved).

## Team-server report endpoints (GL #463)

The team server serves both reports itself (`rust/src/http_server/team_billing.rs`);
the control plane proxies them. Both are gated by the `audit` scope — the same
sensitivity class as `/v1/metrics`, and the scope of the audit-only control token.
Sizing is allocated-blocks-based (`st_blocks * 512` on Unix), symlinks are not
followed, hard links count once, and reports are cached for 60 s per process.

### `GET /v1/storage` (camelCase)

```json
{
  "schemaVersion": 1,
  "measuredAt": "2026-06-10T08:00:00Z",
  "usedBytes": 123456789,
  "quotaBytes": 5000000000,
  "components": [
    { "id": "server-data", "bytes": 120000000 },
    { "id": "workspace:acme-api", "bytes": 3456789 }
  ],
  "cacheAgeSeconds": 0
}
```

- `usedBytes` (required) is what the metering job samples and bills against.
- `quotaBytes` (always present) resolves as: `LEANCTX_TEAM_STORAGE_QUOTA_BYTES`
  env override → `storageQuotaBytes` from `team.json` (rendered per plan by
  provisioning, #282: Team 5 GiB, Enterprise unbounded per catalog) → Team-tier
  5 GiB default. (Note: "50 GiB" was the initial team-server provisioning
  default; the Enterprise entitlement is `UNBOUNDED` per
  `billing-plane-v1-catalog.json`.)
  A concrete quota keeps the control plane's metering out of the degenerate
  `quota = 0 ⇒ state "none"` path.
- `components`: the server data root (audit log, savings store, hosted indices)
  plus each workspace's `.lean-ctx` state dir; workspace dirs nested inside the
  data root are skipped so nothing is counted twice.

### `GET /v1/usage` (savings roll-up + `snake_case` storage block)

```json
{
  "schemaVersion": 1,
  "generatedAt": "2026-06-10T08:00:00Z",
  "savings": {
    "memberCount": 4,
    "savedTokens": 81000000,
    "netSavedTokens": 78000000,
    "savedUsd": 196.42
  },
  "toolCalls": 36001,
  "storage": { "used_bytes": 123456789, "quota_bytes": 5000000000 }
}
```

- `savings` aggregates each signer's **latest** signed batch (same
  no-double-count rule as `/v1/savings/summary`).
- `toolCalls` is the sum of measured ledger events — every entry is one
  measured agent action, so this is the honest call figure.
- The `storage` block is deliberately `snake_case`: that is the spelling
  `metering.rs::from_usage` parses (the dedicated report above stays
  `camelCase`); both carry the same measured numbers.

### `GET /v1/savings/member/{signer}` (per-member drilldown, GL #389)

`signer` is the truncated public key from `by_member[].signer` in
`/v1/savings/summary`. Audit-scoped like the summary (same sensitivity class).

```json
{
  "schema_version": 1,
  "generated_at": "2026-06-10T08:00:00Z",
  "signer": "aaaaaaaaaaaaaaaa",
  "agent_id": "dev-laptop",
  "last_reported": "2026-06-08T00:00:00Z",
  "totals": { "saved_tokens": 4200, "net_saved_tokens": 4200, "saved_usd": 0.042, "total_events": 7 },
  "by_model": [{ "model": "claude-opus", "saved_tokens": 4200, "saved_usd": 0.042 }],
  "by_tool": [{ "tool": "ctx_read", "saved_tokens": 4200 }],
  "series": [{ "date": "2026-06-08", "net_saved_tokens": 4200, "saved_usd": 0.042, "total_events": 7 }],
  "window_days": 90
}
```

- `totals`/`by_model`/`by_tool` come from the member's **latest** signed batch;
  the `series` replays the member's full snapshot history (carry-forward, same
  geometry as the team series — member-only, so the last point equals `totals`).
- `400 invalid_signer` for ids outside `[A-Za-z0-9_-]{1,64}` (the id derives a
  store filename — validated before any filesystem access); `404 unknown_member`
  when the signer never reported a batch.
- Control plane: `GET /api/billing/team/{user_id}/savings/member/{signer}` →
  edge: `GET /api/account/team/savings/member/{signer}` (dashboard drilldown).

## Meter Events (Stripe Billing Meters API)

Usage is pushed via the Stripe Billing Meters API (`POST /v1/billing/meter_events`),
not the legacy subscription-item usage records (removed in Stripe 2025-03-31.basil).
The billing service runs an hourly background job (`metering_job`) that, for each
active team account with a provisioned server and control token:

1. Fetches `/v1/storage` from the team server.
2. Persists a `billing_storage_samples` row (usage trend + audit).
3. Checks threshold crossings (50/80/100%) and sends an idempotent email alert
   (one per threshold per billing period, via SMTP/ZeptoMail).
4. Pushes a meter event with the **current** overage in GB (including `0` when
   cleared — required by Stripe `last` aggregation to avoid stale overbilling),
   rounded up to 0.01 GB.

## Data Durability

Hosted team servers store workspaces, audit logs, and retrieval indices in `/data`.
Coolify v4 (beta.455) silently drops `-v` mounts from `custom_docker_run_options`,
so the provisioning code registers a durable named Docker volume by writing to
Coolify's `local_persistent_volumes` table (the same row the UI creates). This is
a contained, idempotent, additive coupling — it can be swapped for the REST API
once Coolify ships application-storage endpoints. Without `COOLIFY_DB_URL`, new
instances deploy with ephemeral `/data` (logged, non-fatal, recoverable).

## Versioning

Named `v2` because it introduces a new **metered add-on surface** (a billable
dimension + the `metering` block + a metered price + meter events), even though
it is additive and changes no v1 plan/entitlement or local-free semantics.
Adding further metered dimensions (connector sync volume, retrieval queries)
under the same display-first, signed/server-measured, Local-Free rules stays
`v2`.
