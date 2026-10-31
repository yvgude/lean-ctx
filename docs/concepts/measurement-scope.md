# Measurement scope — what lean-ctx can see, and what its numbers mean

lean-ctx reports token savings, reach and quality evidence. Each number is only
as strong as what lean-ctx observed. This page states, per data path, what is
visible, which evidence level a figure can reach, and what stays unknown.

## Data paths

| Path | How it is wired | What lean-ctx sees | What it cannot see |
|---|---|---|---|
| **Tool path** (Hybrid / MCP mode) | MCP tools (`ctx_read`, `ctx_search`, `ctx_shell`, …) and shell hooks | The tool output it transformed, before and after; native shell calls a hook let pass (counted, not tokenized) | Provider requests, turns, prompt caching, the model's answers, native tool calls no hook intercepts |
| **Request path** (proxy) | `lean-ctx proxy enable` points the agent's API base URL at the local proxy (API-key mode) | Every provider request and the provider's usage report (input, cache reads/writes, output) | Turns of clients that bypass the proxy, e.g. subscription logins |
| **Embedded** | The library or SDK inside another program | What the host passes in | Everything the host does not pass in |

The two paths combine: with the proxy enabled, the tool path still transforms
tool output, and the proxy additionally sees what reaches the provider.

## Economic evidence levels

`lean-ctx gain` and `ctx_gain` carry an `economic_evidence` field. Levels, from
weakest to strongest:

| Level | Where it comes from | Supports |
|---|---|---|
| `local_estimate` | Nothing observed yet | Token counts of local transformations |
| `observed_tool_traffic` | Tool path only | Tool-output reduction; **provider bill impact and ROI are `unknown`** |
| `provider_counted_input` | Proxy with `proxy.counterfactual_metering` (Anthropic); shown in `lean-ctx proxy status` | Input saving of each rewritten request, counted by the provider |
| `provider_measured_usage` | Proxy carried provider requests | A net figure (savings minus the injected per-turn context), still against an estimated baseline |
| `paired_control` | Paired with/without provider cost for whole conversations | An end-to-end bill claim |

`lean-ctx gain` reports one of `local_estimate`, `observed_tool_traffic` or
`provider_measured_usage`; the provider-counted pair stays in
`lean-ctx proxy status`. No current surface reaches `paired_control` for whole
conversations.
The proxy's opt-in compression holdout (`[proxy] compression_holdout`) forwards
a deterministic share of conversations uncompressed and reports the per-turn
prompt-size difference with a confidence interval (`lean-ctx output-savings`);
answer quality is reported as `unknown`.

A figure the path cannot observe is shown as `unknown` — never as the gross
saving and never as zero.

## Recorded savings

The local savings ledger is an append-only SHA-256 hash chain.
`lean-ctx savings verify` checks its chain integrity; the chain alone does not
authenticate its author or prevent someone with write access from rebuilding it.
`lean-ctx savings sign` explicitly exports a portable Ed25519-signed batch;
`lean-ctx savings verify-batch <file>` checks that batch offline. Unsigned batches
are possible. The savings and ROI figures are based on local token counts
(before vs. after LeanCTX), not provider-billed usage.

## Reach

Reach is the share of *observed* tool calls routed through lean-ctx:

```
observed = routed through lean-ctx + native shell calls a hook let pass
reach    = routed / observed
```

Native tool calls that bypass every hook are not observable and are reported as
`unknown`, so reach is never a share of all agent activity. It is exposed as
`compression_session.reach` in `/api/session`.

## Quality

Token savings say nothing about answer quality. Quality results carry an
evidence tier — A mechanism, B deterministic, C recorded replay, D live run,
E production — and a verdict (`improved`, `non-inferior`, `regressed`,
`underpowered`). Only a powered run of tier C or higher supports a task-quality
claim. Details: [context-quality-v1](../contracts/context-quality-v1.md).

`lean-ctx benchmark dual-arm` is a synthetic upper bound: its baseline never uses
the provider's prompt cache. The on/off comparison the Engine offers is the
proxy's compression holdout. Each method reports its own measurement scope.
