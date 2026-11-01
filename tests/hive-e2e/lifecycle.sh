#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# MANUAL WALK, not run in CI: it needs a LeanCTX | Hive checkout with
# scripts/dev-run.sh, a local PostgreSQL (initdb/pg_ctl/psql) and Node.
#
# The knowledge-lifecycle walk: two real lean-ctx engines against a private Hive
# stack (own ports and state, never the developer ports, never hive.leanctx.com).
# Engine one learns, re-pins, corrects and withdraws an anchored fact; the vault
# must show each step and engine two must receive it. Everything is deleted at the
# end (E2E_KEEP=1 keeps it).
#
#   LEAN_CTX_HIVE_PREVIEW=1 tests/hive-e2e/lifecycle.sh HIVE_REPO LEAN_CTX_BINARY HIVE_SERVER_BINARY
#
# HIVE_SERVER_BINARY: `cargo build -p hive-server --features dev-seed` in HIVE_REPO.
# Ports: LC_API_PORT (8591) and LC_PG_PORT (55891); PG_BIN / HIVE_PSQL locate PostgreSQL.
set -uo pipefail
umask 077

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="${1:-}"; B="${2:-}"; BIN="${3:-}"
API_PORT="${LC_API_PORT:-8591}"
PG_PORT="${LC_PG_PORT:-55891}"
[ -x "$B" ] && [ -x "$BIN" ] && [ -x "$ROOT/scripts/dev-run.sh" ] || {
  echo "usage: $0 HIVE_REPO LEAN_CTX_BINARY HIVE_SERVER_BINARY" >&2; exit 2;
}
for p in "$API_PORT" "$PG_PORT"; do
  case "$p" in 8110|55430|8100|55420|4321|8191|55491|4391|8391|55691|8491|55791) echo "refusing shared port $p" >&2; exit 2 ;; esac
  if lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then echo "port $p is in use" >&2; exit 2; fi
done

T="$(mktemp -d "${TMPDIR:-/tmp}/hive-lifecycle.XXXXXX")"
SERVER_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null && wait "$SERVER_PID" 2>/dev/null
  [ -d "$T/dev/pg" ] && "${PG_BIN:-/opt/homebrew/opt/postgresql@15/bin}/pg_ctl" -D "$T/dev/pg" stop -m fast >/dev/null 2>&1
  if [ "${E2E_KEEP:-}" = 1 ]; then echo "kept $T"; else rm -rf "$T"; fi
}
trap cleanup EXIT

export HIVE_DEV_DIR="$T/dev" HIVE_DEV_PORT="$API_PORT" HIVE_DEV_PG_PORT="$PG_PORT"
export HIVE_DEV_PUBLIC_URL="http://localhost:$API_PORT"
export RUST_LOG="${RUST_LOG:-warn}"
export HIVE_DEV_SERVER_BIN="$BIN"

"$ROOT/scripts/dev-run.sh" > "$T/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 240); do
  [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$API_PORT/ready")" = 200 ] && break
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -20 "$T/server.log"; exit 1; }
  sleep 0.5
done

export HIVE_URL="http://127.0.0.1:$API_PORT" HIVE_DEVRUN="$ROOT/scripts/dev-run.sh"
export LC_WORK="$T" LEAN_CTX_BIN="$B" LC_PG_PORT="$PG_PORT"
export HIVE_PSQL="${HIVE_PSQL:-${PG_BIN:-/opt/homebrew/opt/postgresql@15/bin}/psql}"
echo "=== stack :$API_PORT (pg :$PG_PORT, state $T)"
node "$HERE/lifecycle.cjs"
