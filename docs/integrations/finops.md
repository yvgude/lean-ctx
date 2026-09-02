# Historical FinOps integration proposal

> **Status: historical research — not an available LeanCTX Cloud/FinOps product
> or savings claim.** LeanCTX is **The Context SDK for AI Agents**; current scope
> and status are governed by `docs/internal/README.md` (internal, not in this repository).

`lean-ctx finops export` turns the tamper-evident savings ledger into daily
cost rows your FinOps platform ingests for showback/chargeback. Unlike
self-reported savings claims, every exported number is backed by a
hash-chained event (`lean-ctx ledger verify`) with the model price pinned at
recording time — an auditor can replay the chain.

```text
savings ledger (hash-chained JSONL)
        │  aggregate: day × project × agent × model × tool
        ▼
DailyCostRow { date, project, agent_role, model, tool,
               tokens_actual, tokens_saved, cost_usd, savings_usd }
        │
        ├── FOCUS 1.2 CSV ──────────► any FOCUS consumer / data warehouse
        ├── CBF CSV / Stream JSON ──► CloudZero (AnyCost)
        └── Vantage CSV ────────────► Vantage (Custom Provider)
                                          │
                                          ▼
                                   CFO cost report
```

## Quick reference

```bash
lean-ctx finops export --target=focus   --out=leanctx_focus.csv
lean-ctx finops export --target=cbf     --from=2026-06-01 --to=2026-06-30 --upload
lean-ctx finops export --target=vantage --out=leanctx_vantage.csv --upload
```

## Data model decisions (read before importing)

- **Costs are real reads**: `cost_usd` = tokens actually sent through
  lean-ctx × the per-event pinned model price. No counterfactuals.
- **Savings are separate rows, never mixed into spend**: FOCUS/Vantage get
  `ChargeCategory=Credit` rows with negative `BilledCost` (FOCUS's category
  for granted reductions); CloudZero gets `lineitem/type=Discount` rows
  (CBF's documented mechanism, included in CloudZero "Real Cost"). Budgets
  built on Usage stay clean; savings stay drillable.
- **No pricing table to maintain**: each ledger event stores
  `unit_price_per_m_usd` at recording time. Provider price changes never
  rewrite history — the export is reproducible forever.
- **Privacy**: `project` is the truncated repo hash from the ledger (paths
  never leave the machine). For readable dashboards, add an opt-in showback
  mapping (see below) — applied at export time only, so the ledger and signed
  batch stay privacy-preserving.

## Showback project names (`--aliases`, #668)

The ledger stores only a truncated repo hash, never a path. To show readable
team/project names in chargeback dashboards, drop a mapping file and it is
applied **at export time only** — the ledger, the signed batch and the hash
chain are never touched.

```toml
# <config_dir>/finops-aliases.toml   (or point --aliases=FILE / $LEAN_CTX_FINOPS_ALIASES)
[projects]
# <repo_hash> = "<display name>"
a1b2c3d4e5 = "Payments"
deadbeef00 = "Platform / SRE"
```

```bash
lean-ctx finops export --target=focus --out=leanctx_focus.csv          # uses the default file
lean-ctx finops export --target=focus --aliases=team-map.toml          # explicit mapping
```

Unmapped hashes fall back to the hash, so an incomplete map never drops rows.
Find the hashes to map in the `project` column of a plain (unmapped) export.

## CloudZero (AnyCost)

Spec pinned: [CBF — Common Bill Format](https://docs.cloudzero.com/docs/anycost-common-bill-format-cbf),
required columns `time/usage_start` + `cost/cost`.

1. In CloudZero, create an **AnyCost Stream** connection and note the
   connection ID + an API key.
2. Export and upload:

   ```bash
   export CLOUDZERO_API_KEY="..."
   export CLOUDZERO_CONNECTION_ID="..."
   lean-ctx finops export --target=cbf --from=2026-06-01 --to=2026-06-30 --upload
   ```

3. **Idempotency**: uploads carry `"operation": "replace_drop"` per month —
   re-running an export replaces that month's drop instead of duplicating
   (CloudZero-side guarantee). One drop is posted per calendar month in the
   range.

Dimensions arrive as `resource/tag:project|agent_role|model|tool` tags and
are filterable in CloudZero Explorer.

## Vantage (Custom Provider)

Spec pinned: Vantage Custom Providers ingest a FOCUS-aligned CSV — required
columns `BilledCost`, `ChargeCategory`, `ChargePeriodStart`, `ServiceName`;
negative costs documented as accepted.

1. Create the provider once: `POST /v2/integrations/custom_provider` (or
   console → Integrations → Custom Provider) and note the integration token.
2. Export and upload:

   ```bash
   export VANTAGE_API_TOKEN="..."
   export VANTAGE_INTEGRATION_TOKEN="accss_crdntl_..."
   lean-ctx finops export --target=vantage --from=2026-06-01 --to=2026-06-30 --upload
   ```

3. **Idempotency warning**: Vantage treats each CSV upload as an additive
   dataset — there is no replace operation. Before re-sending a period,
   delete the previous upload in Vantage (Settings → Integrations → your
   provider). The CLI prints the dataset window after every upload as a
   reminder.

Dimensions arrive in the `Tags` JSON column (`project`, `agent_role`,
`model`, `tool`).

## FOCUS CSV (generic)

Spec pinned: [FOCUS v1.2](https://focus.finops.org/focus-specification/v1-2/)
(June 2024 — first version with SaaS/token-denominated pricing columns). The
file emits all 21 v1.2 Mandatory columns **plus** the FOCUS 1.0 required set
(`Provider`, `InvoiceIssuer`, `ResourceID`, `ChargeType`, `Tags`, …) so both
generations of consumers accept it; additive columns are explicitly allowed.
lean-ctx dimensions ride in `x_project`, `x_agent_role`, `x_model`,
`x_tool`, `x_tokens_saved`.

Validated against the FinOps Foundation's official validator
([`focus-validator` 1.0.0](https://pypi.org/project/focus-validator/)):

```bash
pip install focus-validator 'multimethod<2.0'
lean-ctx finops export --target=focus --out=leanctx_focus.csv
# Run from the site-packages dir — the 1.0.0 validator resolves its
# currency-code list via a relative path (upstream packaging bug):
cd "$(python3 -c 'import focus_validator, os; print(os.path.dirname(os.path.dirname(focus_validator.__file__)))')"
focus-validator --data-file /path/to/leanctx_focus.csv --column-namespace x
# → Validation succeeded.
```

## Showback queries that now work

- *"What did agent context cost per project last month, and what did
  lean-ctx save us?"* — group by `project` (tag/column), compare Usage vs.
  Credit/Discount.
- *"Which agent role burns the most tokens?"* — group by `agent_role`.
- *"Is the savings rate degrading after the editor update?"* — Credit ÷
  Usage trend per day.

## Verifying the numbers

Every row aggregates hash-chained ledger events recorded on the producing
machine. To audit: `lean-ctx ledger verify` (chain integrity) and
`lean-ctx gain` (the same totals the export uses). The ledger design is
documented in `docs/business/03-verified-savings-ledger.md`.
