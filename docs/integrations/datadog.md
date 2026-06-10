# LeanCTX + Datadog — Agentic FinOps in 30 Minutes

See your agents' token economy next to the rest of your AI spend: what they
*would* have consumed without lean-ctx, what they actually consumed, and the
verified (hash-chained ledger) savings — tagged by project, agent role and
model for FinOps showback.

Everything here builds on the stable metrics contract
(`docs/reference/metrics-contract.json`). Renaming a metric breaks CI in this
repo (`cargo test --test metrics_contract`) — your dashboards are treated as
API consumers.

## What you get

| Datadog metric | Source metric | Meaning |
|---|---|---|
| `leanctx.tokens.in` / `.out` | `lean_ctx_tokens_{input,output}_total` | Tokens processed through lean-ctx tools |
| `leanctx.tokens.saved` | `lean_ctx_tokens_saved_total` | Estimated savings (counts cache re-reads at full size) |
| `leanctx.tokens.saved_verified` | `lean_ctx_ledger_tokens_saved_total` | **Verified** savings — measured baselines from the hash-chained ledger, bounce-adjusted |
| `leanctx.cost.saved_usd` | `lean_ctx_cost_saved_usd_total` | Verified savings priced at the recorded per-model input rate |
| `leanctx.cache.hit_ratio` | `lean_ctx_cache_hit_rate` | Session cache effectiveness (0–1) |
| `leanctx.compression.ratio` | `lean_ctx_compression_ratio` | Share of input removed before sending (0–1) |
| `leanctx.slo.violations` | `lean_ctx_slo_violations_total` | Active SLO violations (see `lean-ctx slo`) |
| `leanctx.tools.calls` / `.errors` | `lean_ctx_tool_calls{,_error}_total` | Tool call volume and failures |
| `leanctx.info` | `lean_ctx_info` | Constant `1` carrying tags: `project`, `profile`, `agent_role`, `model`, `version` |

Tags ride on the single `leanctx.info` series (kube-state-metrics `_info`
idiom) instead of every metric — drill-downs stay possible while custom-metric
cardinality (and your Datadog bill) stays flat: one series per running
lean-ctx process.

## Setup path A — Datadog Agent (OpenMetrics check)

1. Create a read-only scrape token on the machine running lean-ctx:

   ```bash
   export LEAN_CTX_SCRAPE_TOKEN="$(openssl rand -hex 24)"
   lean-ctx dashboard --port 3333   # or your existing dashboard/daemon setup
   ```

   The scrape token is accepted **only** for `GET /metrics`. It never grants
   dashboard or API access — give it to monitoring, not to humans.

2. Copy [`integrations/datadog/conf.yaml`](../../integrations/datadog/conf.yaml)
   to the Agent:

   ```bash
   sudo cp integrations/datadog/conf.yaml /etc/datadog-agent/conf.d/openmetrics.d/leanctx.yaml
   # edit: endpoint host/port + the Bearer token
   sudo datadog-agent restart
   ```

3. Verify: `sudo datadog-agent check openmetrics` should list `leanctx.*`
   samples; metrics appear in the Metrics Explorer within one scrape interval.

## Setup path B — agentless (OTLP push)

Planned as an opt-in `otlp` build feature (direct push to
`https://api.datadoghq.com` with `DD-API-KEY` header, no local Agent). Not
shipped yet — tracked in GL #401. Until then, path A (any Prometheus-capable
collector, including Grafana Alloy or the OTel Collector's `prometheus`
receiver) is the supported route; the metric names above are already
OTel-semconv-friendly.

## Dashboard

Import [`integrations/datadog/dashboard.json`](../../integrations/datadog/dashboard.json):
Datadog → Dashboards → New Dashboard → ⚙ → Import dashboard JSON.

Widgets: savings overview (estimated vs. verified vs. USD), token flow
(in/out/saved), cache hit ratio, SLO status, cost trend per day, compression
ratio by project. Template variables `$project`, `$agent_role`, `$model` give
the FinOps showback drill-down.

## Monitors

Import both templates via Monitors → New Monitor → Import:

- [`monitors/savings-drop.json`](../../integrations/datadog/monitors/savings-drop.json)
  — savings dropped >50 % week-over-week (warning at 30 %): catches agents
  silently bypassing lean-ctx after an editor/config change.
- [`monitors/slo-violation.json`](../../integrations/datadog/monitors/slo-violation.json)
  — any active SLO violation, with triage pointers into
  `docs/runbooks/hosted-index-slo.md`.

Replace `@ops-team` with your notification handle after import.

## Estimated vs. verified — read this before showback

`leanctx.tokens.saved` is the *estimated* counter (it values every cache
re-read at full file size — an upper bound, same figure as the dashboard Home
hero). `leanctx.tokens.saved_verified` and `leanctx.cost.saved_usd` come from
the append-only, hash-chained savings ledger: measured baselines only, bounce
re-reads netted out, verifiable with `lean-ctx ledger verify`. Use the
verified pair for anything money-adjacent; use the estimate for trend shape.

## Cardinality guarantees

- All value metrics are **unlabeled** (one series per process).
- `leanctx.info` is one series with five bounded tag values — `project` is
  the working-directory basename (never a path), `model`/`profile`/`role`
  come from bounded registries.
- The contract test fails CI if a labeled metric is added without updating
  the committed contract — cardinality changes are reviewable, never silent.
