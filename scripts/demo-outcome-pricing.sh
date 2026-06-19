#!/usr/bin/env bash
#
# demo-outcome-pricing.sh — live, end-to-end walkthrough of the Outcome-Based
# Pricing golden path (Epic #671), using the REAL lean-ctx binary on REAL
# command output. No mock data: every saving is measured by the engine
# compressor, every signature is real Ed25519.
#
# What it proves, in order:
#   1. real shell compression records measured savings into the signed ledger
#   2. the ledger is an intact SHA-256 hash chain
#   3. a signed batch verifies OFFLINE (the artifact a customer/auditor checks)
#   4. usage is BILLABLE only because it is signed && chain-valid
#   5. a FOCUS FinOps CSV carries the savings as a Credit row (reconcilable
#      against the customer's real provider bill)
#
# The private control-plane (lean-ctx-cloud) turns step 4's verified total into
# the Stripe success-fee invoice — see lean-ctx-cloud/docs/outcome-pricing-e2e.md.
#
# Usage:
#   scripts/demo-outcome-pricing.sh            # build (if needed) + run
#   LEANCTX_BIN=/path/to/lean-ctx scripts/demo-outcome-pricing.sh
#   KEEP=1 scripts/demo-outcome-pricing.sh     # keep the temp data dir + artifacts

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# ── Resolve a real lean-ctx binary (never touches the installed one) ──────────
if [[ -n "${LEANCTX_BIN:-}" ]]; then
    BIN="$LEANCTX_BIN"
elif [[ -x "rust/target/release/lean-ctx" ]]; then
    BIN="rust/target/release/lean-ctx"
else
    echo "→ building lean-ctx (release)…"
    (cd rust && cargo build --release --quiet)
    BIN="rust/target/release/lean-ctx"
fi
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
echo "→ using binary: $BIN"

# ── Isolated, throwaway data dir so the demo never pollutes real savings ──────
DEMO_DIR="$(mktemp -d "${TMPDIR:-/tmp}/leanctx-outcome-demo.XXXXXX")"
export LEAN_CTX_DATA_DIR="$DEMO_DIR/data"
export LEAN_CTX_AGENT_ID="outcome-pilot-demo"
export LEAN_CTX_MODEL="gpt-4o"
export LEAN_CTX_SAVINGS_LEDGER="on"
mkdir -p "$LEAN_CTX_DATA_DIR"

cleanup() { [[ "${KEEP:-0}" == "1" ]] || rm -rf "$DEMO_DIR"; }
trap cleanup EXIT

hr() { printf '\n\033[1;36m── %s ─────────────────────────────────────────\033[0m\n' "$1"; }

hr "1. Generate REAL savings (engine compresses real command output)"
# Each call runs the command for real and records the measured raw-vs-compressed
# token delta into the ledger. git status/log on this repo are reliably verbose.
"$BIN" -c "git status" >/dev/null
"$BIN" -c "git log --oneline -80" >/dev/null
"$BIN" -c "git diff --stat HEAD~30 2>/dev/null || git log --stat -5" >/dev/null
echo "recorded measured shell-compression events"

hr "2. Local ledger + SHA-256 chain status"
"$BIN" savings summary || true
"$BIN" savings verify

hr "3. Sign a portable batch and verify it OFFLINE"
BATCH="$DEMO_DIR/signed-batch.json"
"$BIN" savings sign --out "$BATCH"
echo "→ signed artifact: $BATCH"
"$BIN" savings verify-batch "$BATCH"

hr "4. Billable usage meter (signed && chain_valid)"
"$BIN" billing usage --json | tee "$DEMO_DIR/usage.json"
if command -v python3 >/dev/null 2>&1; then
    python3 - "$DEMO_DIR/usage.json" <<'PY'
import json, sys
u = json.load(open(sys.argv[1]))
billable = u.get("signed") and u.get("chain_valid")
print(f"\n→ signed={u.get('signed')} chain_valid={u.get('chain_valid')} "
      f"→ BILLABLE={billable}; net_saved_tokens={u.get('net_saved_tokens')} "
      f"saved_usd=${u.get('saved_usd'):.4f}")
PY
fi

hr "5. FOCUS FinOps export (savings as a Credit row)"
FOCUS="$DEMO_DIR/focus.csv"
"$BIN" finops export --target=focus --out="$FOCUS"
echo "→ FOCUS CSV: $FOCUS"
echo "Credit (savings) rows — negative BilledCost is the verified reduction:"
{ head -1 "$FOCUS"; grep ',Credit,' "$FOCUS" || grep 'Credit' "$FOCUS" || true; } | head -5

hr "Done"
cat <<EOF
The open engine produced a SIGNED, verifiable savings figure and a FOCUS export
the customer can reconcile against their provider invoice. The private
control-plane (lean-ctx-cloud) reads exactly this verified total to raise the
capped success-fee invoice in Stripe.

Artifacts (${KEEP:+kept }in $DEMO_DIR):
  - signed batch : $BATCH
  - usage meter  : $DEMO_DIR/usage.json
  - FOCUS export : $FOCUS
EOF
[[ "${KEEP:-0}" == "1" ]] && echo "(KEEP=1 — temp dir preserved)" || echo "(temp dir cleaned up; re-run with KEEP=1 to inspect)"
