# Monitoring and Observability Guide

> **Status: historical operations guide.** This is not a dashboard, Cloud, or
> organization-monitoring product promise. Confirm behavior against the local
> Runtime; canonical scope is in
> `docs/internal/README.md` (internal, not in this repository).

## Overview

This guide helps operators establish health checks, baselines, alerting, and
capacity signals for lean-ctx. Monitor both availability (proxy, daemon, and
MCP connectivity) and effectiveness (token savings, cache behavior, latency,
and budgets).

Observability is local-first. The detailed reporting model, including the
auditable savings ledger, is documented in
[Analytics and Insights](../reference/11-analytics-and-insights.md). Use this
guide to turn those signals into operating practice.

## Prerequisites

- Complete installation and policy verification from the
  [Administrator Guide](admin-guide.md).
- Confirm which hosts run a proxy and whether it is loopback-only or a managed
  gateway.
- Define an owner for alerts and a retention policy for logs and ledger exports.
- Record a baseline after normal traffic has warmed the cache; cold-start values
  are not a production baseline.

Begin every new monitoring integration with:

```bash
lean-ctx status
lean-ctx doctor
lean-ctx proxy status
lean-ctx gain --json
```

## Steps

### 1. Use built-in monitoring commands

Use these commands at the terminal before building an external dashboard:

| Command | Operational use |
| --- | --- |
| `lean-ctx status` | Quick health check for the local installation and active components. |
| `lean-ctx doctor` | Comprehensive diagnostics for configuration, paths, services, and common integration faults. |
| `lean-ctx dashboard` | Open the terminal or browser dashboard when available in the installed build. |
| `lean-ctx stats` | Compatibility name for raw savings statistics; current releases fold it into `lean-ctx gain --raw`. |
| `lean-ctx gain` | Headline savings report; use `--daily`, `--graph`, `--deep`, or `--json` for analysis. |
| `lean-ctx savings` | Auditable savings-ledger summary. |

Examples:

```bash
lean-ctx status
lean-ctx doctor
lean-ctx dashboard
lean-ctx gain --daily
lean-ctx gain --deep --json
lean-ctx savings verify
```

Run `lean-ctx doctor` after an upgrade, configuration change, editor update, or
any repeated health-check failure. `status` is suitable for a frequent manual
or scripted check; use the exit code rather than parsing decorative output.

### 2. Measure the right signals

Track metrics at a session, daily, and deployment level.

| Signal | Why it matters | Collection path |
| --- | --- | --- |
| Input/output tokens saved | Shows realized savings by session and day. | `lean-ctx gain --daily --json` and ledger export. |
| Cache hit rate | Detects cold caches, invalidation churn, or poor locality across BM25, semantic, and provider caches. | Dashboard, diagnostics, and configured cache reports. |
| Compression ratio | Detects regressions by tool and file type without treating high compression alone as success. | `lean-ctx gain --deep` and tool-level reporting. |
| Proxy overhead | Identifies slow forwarding, upstream latency, or compression cost. | Proxy logs and deployment monitoring. |
| Cache lookup time | Identifies disk, memory, or index pressure before sessions become slow. | Diagnostics and local performance logs. |
| Budget consumption | Prevents a single turn or session from exhausting its configured context budget. | Dashboard and agent/session policy reports. |

Interpret savings honestly. A zero-savings report can mean the proxy is off or
the editor is not routed through it, not that compression failed. Verify proxy
engagement before declaring a regression.

For the metric definitions and ledger integrity model, see
[Analytics and Insights](../reference/11-analytics-and-insights.md) and the
[signed savings ledger reference](../reference/16-signed-savings-ledger.md).

### 3. Establish alerting patterns

Use layers of alerts rather than a single aggregate threshold.

| Alert | Condition | First action |
| --- | --- | --- |
| Proxy unavailable | Health check to `GET /health` fails or the proxy is not listening. | Run `lean-ctx proxy status`, then `lean-ctx doctor`. |
| Restart loop | Repeated proxy or daemon start events in logs. | Inspect configuration and service-manager status before restarting again. |
| Savings collapse | Daily savings materially below the warmed baseline while request volume is normal. | Confirm the editor still routes through the proxy and inspect cache behavior. |
| Cache degradation | Hit rate stays below baseline after the warm-up window. | Check cache limits, storage availability, and project locality. |
| Budget exceeded | Turn or session budget reaches the configured warning threshold. | End or compact the workload and review the relevant policy. |
| Ledger integrity failure | `lean-ctx savings verify` returns non-zero. | Preserve the ledger, stop automated export writes, and investigate immediately. |

The unauthenticated health endpoint is intended for liveness checks. A `200`
response proves the listener is responsive, not that an upstream provider,
editor integration, or policy is healthy. Pair it with `lean-ctx status` and
`lean-ctx doctor` in a readiness workflow.

Export the savings ledger for offline analysis or a controlled reporting job:

```bash
lean-ctx savings export
lean-ctx savings verify
```

`lean-ctx ledger export` may be available as a deployment-specific compatibility
command; prefer `lean-ctx savings export` on current releases and verify with
`lean-ctx --help` before automating it.

### 4. Integrate external monitoring carefully

For Prometheus and Grafana, scrape a proxy metrics endpoint only when the
installed deployment explicitly exposes a Prometheus-compatible endpoint.
Current local proxy deployments guarantee `GET /health`; do not assume that
`/health` is a Prometheus metrics stream. If the deployment template exposes
`/metrics`, configure Prometheus to scrape that endpoint and use Grafana for:

- proxy availability and request latency;
- cache hit rate and cache lookup latency;
- compression ratio and saved-token rate; and
- active sessions, budget warnings, and resource use.

For a deployment without a scrape endpoint, use `lean-ctx gain --json`,
`lean-ctx savings export`, and structured logs as the supported collection
inputs. Do not fabricate a `/metrics` target in a local configuration.

For log aggregation, forward structured JSON logs from the lean-ctx state
directory to ELK, Loki, or the organization's approved collector. Preserve
component, severity, process, and correlation fields; apply access controls and
redaction before logs leave the workstation or cluster.

For Kubernetes monitoring, use the observability guidance supplied with the
`lean-ctx-deploy-template` release that is deployed in your environment. Align
its ServiceMonitor, probes, log labels, and retention settings with the actual
container ports and endpoints; do not copy local macOS LaunchAgent paths into a
cluster manifest.

### 5. Set performance baselines

Capture a baseline after a representative workload has warmed the cache. Record
the workload type, tool profile, compression policy, model route, cache state,
and concurrency with each baseline; otherwise comparisons are misleading.

Use these directional expectations instead of universal percentages:

| Workload | Expected compression pattern | Expected cache pattern after warm-up |
| --- | --- | --- |
| Large repetitive text, logs, or structured output | Highest compression ratio. | High reuse when repeated inputs and keys remain stable. |
| Source files with repeated reads | Moderate-to-high savings. | Hit rate should rise after the first identical or nearby read. |
| Small unique files | Lower absolute savings; low ratio is normal when fidelity dominates. | Low reuse is normal when files are not revisited. |
| Search and provider context | Varies with result redundancy and policy. | Hit rate rises only after index warm-up and repeated queries. |

Do not publish universal percentage targets for compression ratios or cache hit
rates: file mix, tool profile, policy, and workload locality determine them.
Set numeric alerts from the recorded warm baseline for each workload class.

ETPAO (Effective Tokens Per Agent Operation) is a useful composite benchmark:

```text
ETPAO = tokens delivered to the agent / completed agent operations
```

Trend ETPAO alongside outcome quality and latency. Lower ETPAO is desirable only
when agents still complete the same work without increased retries or full
rereads. The performance-tuning guide explains the relevant cache and
compression trade-offs: [Performance Tuning](../reference/14-performance-tuning.md).

### 6. Plan capacity

Capacity requirements depend more on active sessions and retained data than on
the installed binary.

| Resource | Plan for | Operator action |
| --- | --- | --- |
| Disk | Cache growth, semantic/BM25 indexes, savings ledger, and rotated logs. | Set retention, monitor free space, and keep exports separate from live state. |
| Memory | Proxy process, in-memory caches, active sessions, and concurrent MCP clients. | Observe resident memory under peak concurrency; size cache conservatively. |
| CPU | Compression, index work, cache maintenance, and concurrent proxy requests. | Baseline CPU during representative bursts and reserve headroom for editor use. |

Avoid clearing caches simply to reduce disk use without first measuring the
impact on hit rate and ETPAO. Prefer a documented retention and rotation policy.

## Verification

Validate monitoring after installation, each dashboard change, and each upgrade:

```bash
lean-ctx status
lean-ctx doctor
lean-ctx proxy status
lean-ctx gain --daily --json
lean-ctx savings verify
```

Verify that:

- `/health` is reachable from the intended probe location.
- Alerts identify a component and provide a CLI first action.
- The external collector receives structured logs without credentials or raw
  sensitive content.
- Baselines are recorded after warm-up and distinguish a disconnected proxy
  from actual zero savings.
- Disk, memory, and CPU headroom are tested at expected concurrent-session load.

## Troubleshooting

Use [Troubleshooting](../reference/12-troubleshooting.md) for detailed fixes.

| Observation | Investigation |
| --- | --- |
| Health check fails | Run `lean-ctx proxy status` and `lean-ctx doctor`; inspect listener and service logs. |
| Dashboard has no data | Verify the relevant feature is present, then confirm proxy engagement and session activity. |
| Savings suddenly read zero | Confirm the proxy is running and the editor is routed through it before changing compression policy. |
| Cache rate stays low | Check cache retention, available disk, index warm-up, and whether workloads are actually repeated. |
| Prometheus scrape fails | Confirm that the deployed template exposes a metrics endpoint; `/health` alone is not one. |

For security-related log, PathJail, or shell-policy incidents, follow
[Security and Governance](../reference/13-security-and-governance.md) before
changing controls to restore service.
