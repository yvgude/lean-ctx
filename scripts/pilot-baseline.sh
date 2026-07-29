#!/usr/bin/env bash
set -euo pipefail

# Pilot Baseline Collection Script
# Captures start/end snapshots for a Shadow Pilot comparison.

PROXY_PORT="${LEANCTX_PROXY_PORT:-4444}"
GATEWAY_URL="${LEANCTX_GATEWAY_URL:-http://127.0.0.1:${PROXY_PORT}}"
DURATION="${PILOT_DURATION:-86400}" # 24h default

COLLECT_END=false
if [[ "${1:-}" == "--collect-end" ]]; then
    COLLECT_END=true
    OUTPUT_DIR="${2:-./pilot-baseline}"
else
    OUTPUT_DIR="${1:-./pilot-baseline}"
fi

mkdir -p "$OUTPUT_DIR"

echo "==> Collecting baseline from $GATEWAY_URL"
echo "    Duration: ${DURATION}s"
echo "    Output: $OUTPUT_DIR"

TOKEN="${LEAN_CTX_PROXY_TOKEN:-}"
if [[ -z "$TOKEN" ]]; then
    TOKEN="$(lean-ctx proxy token)"
fi

auth_curl() {
    curl -sf -H "Authorization: Bearer ${TOKEN}" "$@"
}

# Health check
curl -sf "$GATEWAY_URL/health" > "$OUTPUT_DIR/health.json"
echo "    Health: OK"

# Snapshot current conformance
lean-ctx conformance --json > "$OUTPUT_DIR/conformance.json"

# Collect live proxy status snapshot
auth_curl "$GATEWAY_URL/status" > "$OUTPUT_DIR/status-start.json"

# Collect current savings
lean-ctx gain --json > "$OUTPUT_DIR/savings-start.json"

echo "==> Baseline collection started at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "    Will complete after ${DURATION}s"
echo "    Re-run with 'pilot-baseline.sh --collect-end' to capture end snapshot"

if [[ "$COLLECT_END" == true ]]; then
    auth_curl "$GATEWAY_URL/status" > "$OUTPUT_DIR/status-end.json"
    lean-ctx gain --json > "$OUTPUT_DIR/savings-end.json"
    echo "==> End snapshot collected"
fi
