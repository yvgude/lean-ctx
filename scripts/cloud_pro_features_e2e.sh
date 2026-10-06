#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

# Live Pro E2E harness. The test is intentionally fail-closed: it never
# fabricates a backend or billing result. Operators must provision the backend
# and billing stub explicitly, then point LEAN_CTX_API_URL at it.
: "${LEAN_CTX_API_URL:?Set LEAN_CTX_API_URL to the provisioned backend URL}"
: "${LEAN_CTX_BILLING_STUB_URL:?Set LEAN_CTX_BILLING_STUB_URL to the provisioned billing stub URL}"
: "${LEANCTX_E2E_PHASE:?Set LEANCTX_E2E_PHASE to device-a|device-b|free for the provisioned live fixture}"

command -v cargo >/dev/null || { echo "cargo is required" >&2; exit 2; }
command -v curl >/dev/null || { echo "curl is required" >&2; exit 2; }

for url in "$LEAN_CTX_API_URL" "$LEAN_CTX_BILLING_STUB_URL"; do
  curl --fail --silent --show-error --max-time "${LEAN_CTX_HEALTH_TIMEOUT_SECS:-5}" \
    "${url%/}/health" >/dev/null || {
      echo "health check failed: $url" >&2
      exit 3
    }
done

export LEAN_CTX_LIVE_E2E=1
cargo test --locked --test main cloud_pro_features_e2e -- --ignored --nocapture
