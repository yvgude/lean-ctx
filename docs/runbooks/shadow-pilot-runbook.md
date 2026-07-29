# Shadow Pilot Runbook — OBSERVE → MEASURE

## Overview

A Shadow Pilot runs lean-ctx alongside the customer's existing AI workflow
without changing their primary pipeline. It collects baseline metrics,
measures compression savings, and validates quality preservation.

## Prerequisites

- [ ] lean-ctx v3.9.14+ deployed (via Helm chart v0.4.0)
- [ ] Gateway configured with customer's LLM providers
- [ ] SLO targets agreed with customer
- [ ] SDK conformance verified (`scripts/sdk-conformance.sh`)
- [ ] Proxy Bearer token available (`lean-ctx proxy token`)

## Phase 1: OBSERVE (Week 1-2)

### Setup

```bash
# Start the local proxy used for observation
lean-ctx proxy start --port=4444

# Verify health
curl http://127.0.0.1:4444/health
lean-ctx conformance --json
```

Authenticated status snapshots use the proxy Bearer token:

```bash
TOKEN="$(lean-ctx proxy token)"
curl -H "Authorization: Bearer ${TOKEN}" http://127.0.0.1:4444/status
```

### Baseline Collection

- Record uncompressed token usage per request.
- Measure provider latency (p50, p95, p99).
- Catalog coverage classes (languages, file types, request patterns).
- Log quality signals without enforcing.

### Daily Check

```bash
scripts/pilot-baseline.sh ./pilot-baseline
lean-ctx gain --json
```

## Phase 2: MEASURE (Week 3-4)

### Enable Compression

```bash
# Keep the proxy running and capture an end snapshot for comparison
PILOT_DURATION=1209600 scripts/pilot-baseline.sh --collect-end ./pilot-baseline
```

### SLO Validation

```bash
# Run the supported task benchmark profiles
lean-ctx benchmark tasks --config standard --json

# Compare conservative and aggressive profiles when tuning
lean-ctx benchmark tasks --config stock --json
lean-ctx benchmark tasks --config aggressive --json
```

### Evidence Collection

```bash
# Collect final pilot artifacts
scripts/pilot-baseline.sh --collect-end ./pilot-baseline
lean-ctx conformance --json > ./pilot-baseline/conformance-end.json
```

## Decision Points

| Metric | SLO Target | Action if Violated |
|---|---|---|
| Savings | ≥60% | Tune compression profiles |
| Quality | ≤5% degradation | Switch to safer mode |
| Latency p99 | ≤500ms | Check provider config |
| Coverage | ≥2 classes passing | Add fixtures |

## Escalation

If any SLO is violated for >24h:

1. Stop compression traffic by routing clients away from the proxy.
2. Collect a diagnostic bundle: `lean-ctx report-issue --include-evidence`.
3. Escalate to engineering.

## Rollback

```bash
lean-ctx proxy stop --port=4444  # stop local proxy
lean-ctx stop                    # full stop if needed
```
