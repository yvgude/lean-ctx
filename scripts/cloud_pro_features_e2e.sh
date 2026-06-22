#!/usr/bin/env bash
# Live, isolated E2E for the **whole advertised Pro surface** (GL #797), with the
# headline guarantee: *your context follows you to another machine*.
#
# It spins up an ephemeral Postgres + the real lean-ctx-cloud-api (example
# target) + a tiny billing stub (`billing_entitlements_stub.py`) that resolves
# the caller's plan, then drives the real engine client across three phases:
#
#   device-a : a Pro machine pushes every bucket (knowledge, gotchas, the five
#              telemetry streams, the hosted index bundle) and reads its dashboard.
#   device-b : a *different* Pro machine (separate data dir, same account, same
#              repo identity) restores — pull_knowledge returns the entry and
#              pull_index_bundle reconstructs the index artifacts. Cross-device.
#   free     : a Free account is refused (402) on all eight gated buckets.
#
# Finally it proves ciphertext-at-rest: a secret needle pushed by device A must
# appear in none of the three encrypted tables. Everything lives under a temp dir
# and is torn down on exit — it never touches your real lean-ctx state or prod DB.
#
#   ./scripts/cloud_pro_features_e2e.sh
#
# Requires a local PostgreSQL toolchain (and python3). Point at PG explicitly if
# it is not on PATH / Homebrew:
#
#   LEANCTX_E2E_PGBIN=/path/to/pg/bin ./scripts/cloud_pro_features_e2e.sh
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
RUSTDIR="$SCRIPT_DIR/../rust"
# Honour a shared CARGO_TARGET_DIR (e.g. a warm cache from another checkout) so
# the built example binary is looked up where cargo actually wrote it.
TARGET_DIR="${CARGO_TARGET_DIR:-$RUSTDIR/target}"

# Resolve the PostgreSQL bin dir: explicit override → pg_config → Homebrew 15.
PGBIN="${LEANCTX_E2E_PGBIN:-}"
[ -z "$PGBIN" ] && PGBIN="$(pg_config --bindir 2>/dev/null || true)"
[ -z "$PGBIN" ] && [ -d /opt/homebrew/opt/postgresql@15/bin ] && PGBIN=/opt/homebrew/opt/postgresql@15/bin
if [ -z "$PGBIN" ] || [ ! -x "$PGBIN/initdb" ]; then
  echo "PostgreSQL tools not found. Set LEANCTX_E2E_PGBIN to the bin dir."; exit 1
fi

BASE=$(mktemp -d "${TMPDIR:-/tmp}/leanctx_pro_e2e.XXXXXX")
PGDATA="$BASE/pg"
PGSOCK="$BASE/sock"
DEV_A="$BASE/device-a"          # Pro, machine #1 (pushes)
DEV_B="$BASE/device-b"          # Pro, machine #2 (restores) — same account
FREE="$BASE/free"              # Free account (must be gated)
PROJ_A="$BASE/proj-a"          # repo checkout on machine #1
PROJ_B="$BASE/proj-b"          # same repo on machine #2 (identical git remote)
PGPORT=55440
APIPORT=18093
STUBPORT=18092
NEEDLE="PRO-E2E-NEEDLE-9b1e4d77"        # keep in sync with the ignored test
INTERNAL_KEY="e2e-internal-key"
PRO_UID_FILE="$BASE/pro_uid.txt"
GIT_REMOTE="git@github.com:leanctx/pro-e2e-demo.git"
DBURL="postgres://postgres@127.0.0.1:$PGPORT/leanctx"
API_PID=""
STUB_PID=""

log(){ printf '\n=== %s ===\n' "$*"; }
cleanup(){
  set +e
  [ -n "$API_PID" ] && kill "$API_PID" 2>/dev/null
  [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null
  "$PGBIN/pg_ctl" -D "$PGDATA" -m immediate stop >/dev/null 2>&1
  rm -rf "$BASE"
  echo "[cleanup] stopped api+stub+postgres, removed $BASE"
}
trap cleanup EXIT

mkdir -p "$PGSOCK" "$DEV_A/cloud" "$DEV_B/cloud" "$FREE/cloud" "$PROJ_A/.git" "$PROJ_B/.git"

# Same repo identity on both machines → identical index namespace hash, so
# device B pulls exactly the bundle device A pushed (mirrors "same repo, two
# laptops"). project_hash::project_identity reads .git/config's origin url.
printf '[remote "origin"]\n\turl = %s\n' "$GIT_REMOTE" > "$PROJ_A/.git/config"
cp "$PROJ_A/.git/config" "$PROJ_B/.git/config"

log "initdb"
"$PGBIN/initdb" -D "$PGDATA" -U postgres --auth=trust --encoding=UTF8 >/dev/null \
  || { echo "initdb FAILED"; exit 1; }

log "start postgres :$PGPORT"
"$PGBIN/pg_ctl" -D "$PGDATA" -l "$BASE/pg.log" \
  -o "-p $PGPORT -k $PGSOCK -c listen_addresses=127.0.0.1" -w start \
  || { echo "pg start FAILED"; cat "$BASE/pg.log" 2>/dev/null; exit 1; }

log "createdb leanctx"
"$PGBIN/createdb" -h 127.0.0.1 -p "$PGPORT" -U postgres leanctx \
  || { echo "createdb FAILED"; exit 1; }

log "build cloud-api (debug, example target)"
( cd "$RUSTDIR" && cargo build --features cloud-server --example lean-ctx-cloud-api ) \
  || { echo "build FAILED"; exit 1; }

log "start billing stub :$STUBPORT"
PORT="$STUBPORT" INTERNAL_KEY="$INTERNAL_KEY" PRO_UID_FILE="$PRO_UID_FILE" \
  python3 "$SCRIPT_DIR/billing_entitlements_stub.py" >"$BASE/stub.log" 2>&1 &
STUB_PID=$!
ok=0
for _ in $(seq 1 40); do
  if curl -fsS -H "X-Internal-Key: $INTERNAL_KEY" \
      "http://127.0.0.1:$STUBPORT/api/billing/entitlements/healthcheck" >/dev/null 2>&1; then ok=1; break; fi
  if ! kill -0 "$STUB_PID" 2>/dev/null; then echo "stub died early"; cat "$BASE/stub.log"; exit 1; fi
  sleep 0.2
done
[ "$ok" = 1 ] || { echo "billing stub not responding"; cat "$BASE/stub.log"; exit 1; }

log "start cloud-api :$APIPORT (billing wired → gate LIVE, no sync_open)"
LEANCTX_CLOUD_DATABASE_URL="$DBURL" \
LEANCTX_CLOUD_BILLING_URL="http://127.0.0.1:$STUBPORT" \
LEANCTX_CLOUD_BILLING_INTERNAL_KEY="$INTERNAL_KEY" \
LEANCTX_CLOUD_BIND_HOST=127.0.0.1 \
LEANCTX_CLOUD_BIND_PORT="$APIPORT" \
RUST_LOG=warn \
"$TARGET_DIR/debug/examples/lean-ctx-cloud-api" >"$BASE/api.log" 2>&1 &
API_PID=$!

log "wait for /health"
ok=0
for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:$APIPORT/health" >/dev/null 2>&1; then ok=1; break; fi
  if ! kill -0 "$API_PID" 2>/dev/null; then echo "api process died early"; cat "$BASE/api.log"; exit 1; fi
  sleep 0.3
done
[ "$ok" = 1 ] || { echo "server not healthy"; cat "$BASE/api.log"; exit 1; }

# Register an account and emit "api_key user_id".
register(){
  local email="$1"
  curl -fsS -XPOST "http://127.0.0.1:$APIPORT/api/auth/register" \
    -H 'content-type: application/json' \
    -d "{\"email\":\"$email\",\"password\":\"E2eTestPassw0rd!\"}" \
  | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d["api_key"],d["user_id"])'
}

write_creds(){
  local dir="$1" email="$2" api_key="$3" user_id="$4"
  printf '{"api_key":"%s","user_id":"%s","email":"%s","oauth_client_id":null,"oauth_client_secret":null}\n' \
    "$api_key" "$user_id" "$email" > "$dir/cloud/credentials.json"
  chmod 600 "$dir/cloud/credentials.json"
}

log "register Pro + Free accounts"
read -r PRO_KEY PRO_UID < <(register "pro@example.com") || { echo "pro register FAILED"; cat "$BASE/api.log"; exit 1; }
read -r FREE_KEY FREE_UID < <(register "free@example.com") || { echo "free register FAILED"; cat "$BASE/api.log"; exit 1; }
[ -n "${PRO_KEY:-}" ] && [ -n "${FREE_KEY:-}" ] || { echo "no api_key parsed"; exit 1; }
printf '%s' "$PRO_UID" > "$PRO_UID_FILE"   # the stub now resolves this uid to Pro
echo "pro=$PRO_UID (${PRO_KEY:0:8}…)  free=$FREE_UID (${FREE_KEY:0:8}…)"

# Both Pro machines carry the same account credentials; Free is its own account.
write_creds "$DEV_A" "pro@example.com"  "$PRO_KEY"  "$PRO_UID"
write_creds "$DEV_B" "pro@example.com"  "$PRO_KEY"  "$PRO_UID"
write_creds "$FREE"  "free@example.com" "$FREE_KEY" "$FREE_UID"

# Run one ignored phase of the comprehensive test with an isolated data dir.
run_phase(){
  local phase="$1" data="$2" proj="${3:-$PROJ_A}"
  log "phase: $phase"
  ( cd "$RUSTDIR" \
    && LEAN_CTX_DATA_DIR="$data" \
       LEAN_CTX_API_URL="http://127.0.0.1:$APIPORT" \
       LEANCTX_E2E_PHASE="$phase" \
       LEANCTX_E2E_PROJECT="$proj" \
       cargo test --test cloud_pro_features_e2e -- --ignored --nocapture )
}

run_phase device-a "$DEV_A" "$PROJ_A"; RC_A=$?
run_phase device-b "$DEV_B" "$PROJ_B"; RC_B=$?
run_phase free      "$FREE"  "$PROJ_A"; RC_FREE=$?

# Ciphertext-at-rest: the needle device A sealed must not appear in any encrypted
# table (knowledge_blobs.blob, gotcha_blobs.blob, index_bundles.bytes).
leak_count(){
  "$PGBIN/psql" "$DBURL" -tAc "SELECT encode($2,'escape') FROM $1;" 2>/dev/null | grep -c "$NEEDLE"
}
log "ciphertext-at-rest check (3 encrypted tables)"
LEAK_K=$(leak_count knowledge_blobs blob)
LEAK_G=$(leak_count gotcha_blobs blob)
LEAK_I=$(leak_count index_bundles bytes)
echo "plaintext-needle occurrences — knowledge:$LEAK_K gotchas:$LEAK_G index:$LEAK_I"

log "RESULT"
if [ "$RC_A" = 0 ] && [ "$RC_B" = 0 ] && [ "$RC_FREE" = 0 ] \
   && [ "$LEAK_K" = 0 ] && [ "$LEAK_G" = 0 ] && [ "$LEAK_I" = 0 ]; then
  echo "PRO-E2E RESULT: PASS — device A→B restore works, Free fully gated, zero plaintext at rest"
  exit 0
fi
echo "PRO-E2E RESULT: FAIL — device-a=$RC_A device-b=$RC_B free=$RC_FREE leaks(k/g/i)=$LEAK_K/$LEAK_G/$LEAK_I"
exit 1
