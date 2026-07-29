#!/usr/bin/env bash
# SDK conformance matrix runner (GL #395).
#
# Builds the engine (unless LEAN_CTX_BIN points at one), starts a real
# `lean-ctx serve` on an ephemeral port, then runs the conformance kit of all
# four first-party SDKs against it:
#
#   * Python   clients/python            (pytest  tests/test_conformance_live.py)
#   * TypeScript cookbook/sdk            (vitest  src/conformance.e2e.test.ts)
#   * Go       go-sdk                    (go test TestConformanceLive)
#   * Rust     clients/rust/lean-ctx-client (cargo test --test conformance_live)
#
# Each suite writes its scorecard into $LEANCTX_MATRIX_DIR; afterwards the
# matrix generator renders docs/reference/sdk-conformance-matrix.md.
#
# Usage:  scripts/sdk-conformance.sh [--keep-server]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="${LEAN_CTX_BIN:-$ROOT/rust/target/debug/lean-ctx}"
if [[ ! -x "$BIN" ]]; then
  echo "==> building engine (debug, all features)"
  (cd rust && cargo build --all-features)
fi
[[ -x "$BIN" ]] || { echo "engine binary missing: $BIN"; exit 1; }

PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
URL="http://127.0.0.1:$PORT"
TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(24))')"
MATRIX_DIR="$(mktemp -d)"
export LEANCTX_CONFORMANCE_URL="$URL"
export LEANCTX_CONFORMANCE_TOKEN="$TOKEN"
export LEANCTX_MATRIX_DIR="$MATRIX_DIR"

echo "==> starting lean-ctx serve on $URL"
"$BIN" serve --host 127.0.0.1 --port "$PORT" --project-root "$ROOT" --auth-token "$TOKEN" &
SERVER_PID=$!
cleanup() {
  if [[ "${1:-}" != "--keep-server" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

for _ in $(seq 1 100); do
  if curl -fsS "$URL/health" >/dev/null 2>&1; then break; fi
  sleep 0.1
done
curl -fsS "$URL/health" >/dev/null || { echo "server did not become healthy"; exit 1; }

FAIL=0

echo "==> Python SDK conformance"
(cd clients/python && python3 -m pytest tests/test_conformance_live.py -q) || FAIL=1

echo "==> Python adapter smoke (OpenAI/LangChain/LlamaIndex/CrewAI; frameworks optional)"
(cd clients/python && python3 -m pytest tests/test_adapters_live.py -q) || FAIL=1

echo "==> SDK release gate (versions + contract coupling)"
python3 scripts/check-sdk-versions.py || FAIL=1

echo "==> TypeScript SDK conformance"
(cd cookbook && npm install --no-fund --no-audit >/dev/null \
  && cd sdk && npx vitest run src/conformance.e2e.test.ts) || FAIL=1

echo "==> Go SDK conformance"
(
  cd go-sdk
  set +e
  set -o pipefail
  LEANCTX_CONFORMANCE_URL="$URL" go test -run TestConformanceLive -v ./... 2>&1 | tee "$MATRIX_DIR/go.log"
  GO_EXIT=$?
  echo "$GO_EXIT" > "$MATRIX_DIR/go.exit"
  exit "$GO_EXIT"
) || FAIL=1
echo "==> Rust SDK conformance"
(cd clients/rust/lean-ctx-client && cargo test --test conformance_live) || FAIL=1

echo "==> generating matrix"
python3 scripts/gen-sdk-matrix.py "$MATRIX_DIR" \
  --engine-version "$("$BIN" --version | head -1)" \
  --out docs/reference/sdk-conformance-matrix.md

exit "$FAIL"
