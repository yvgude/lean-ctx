# Model-routing retirement

This integration branch removes automatic model selection. The caller continues
to choose the model and provider; explicit destinations and their locality,
security and failover policy remain separate Gateway responsibilities.

## Engine and runtime

- Intent Route v2 recommends a context view only. Token/cost accounting and the
  legacy profile model cap cannot change the chosen view or select a model.
- `pro.runtime.adaptive_routing` and `pro.ocla.model_router` are absent from the
  active product registry. No plan or legacy signed grant authorizes them.
- `trust.runtime.routing` and its `routing` alias remain: these represent
  explicit operator targets.
- The host still validates exact supported legacy package descriptions for
  compatibility. Invocation separately permits only the exact context
  capabilities advertised by that verified package. Routing invocations fail.
- Routing-free packages advertise adaptive context, memory curation, personal
  protection, code security and the supported context-policy versions.
- The private runtime's signed enum wire value `ModelRouting` remains a legacy
  base-entitlement name. It is not an executable routing capability. Renaming
  signed bytes or reissuing customer licenses is not part of this migration.

## Signed entitlement compatibility

Historical canonical envelopes and their conformance vector stay byte-identical
and verifiable with their existing trusted key. Unknown/retired IDs remain in
those stored signed bytes but do not grant an active capability. Unrelated
signed grants, seat counts, deployment bindings and expiry rules remain intact.
Tests check the real historical issuer vector and explicitly deny both retired
IDs. Local free Gateway and manual target capabilities stay accountless.

The Cloud issuer must stop advertising retired IDs in the coordinated next
catalogue. An Engine-only source change is not evidence of that issuer cutover.
Live issuance, catalog rotation and signed package publication require the
combined Engine/Cloud/runtime acceptance evidence.

## Release conditions

This is a breaking migration, including removal of stable Intent Route v1
fields. It must not ship as a 3.x patch/minor. Promotion requires the major
release and deprecation-window evidence specified by `CONTRACTS.md`, explicit
consumer migration notes, and tests against the final artifacts. This document
does not assert that an announcement or the two-minor window has already
happened. Existing contract artifacts and published license terms are retained.
