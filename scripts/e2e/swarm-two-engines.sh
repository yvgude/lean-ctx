#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Two-engine private swarm e2e (SPEC-ENGINE V7.1 done-when, V4.8 replica
# side). ENGINE-1's harness provides the engines and the dogfood access;
# this file holds ENGINE-2's participant steps 1-7 (lead split 25.10.).
#
# Environment (from the harness):
#   LCX_OWNER  command running the swarm owner's engine (conductor), e.g.
#              "env LEAN_CTX_DATA_DIR=/x/a LEAN_CTX_CONFIG_DIR=/x/a-cfg lean-ctx"
#   LCX_A      engine A, a member of the owning workspace
#   LCX_B      engine B, an invited non-member (its own personal workspace)
#   LCX_C      engine C, neither member nor invited
#   DF         "ssh pounce-server ~/hive-dogfood/deploy/host/dogfood.sh"
#   OWNER_ROLE / B_HANDLE  dogfood roles/handles of the owner and of B
#   OWNER_WKR  optional: the owner engine's workspace worker (coordinator;
#              E28/E29 accept it, R138); registered here when unset
#   W50        command answering a consent on the web as a role:
#              "$W50 <role> <consent_id> <pairing_code> allow" (pairing code +
#              the action-bound security code of that account, R49)
# Engines run on a host without an agent ancestor (pounce over ssh): the
# terminal approval refuses agent-descendant processes by design.
set -euo pipefail
umask 077

# Dogfood defaults (OPS 25.10.); the harness may override every one.
DF=${DF:-/opt/hive-dogfood/tools/deploy/host/dogfood.sh}
OWNER_ROLE=${OWNER_ROLE:-engine-2}
B_HANDLE=${B_HANDLE:-e2e-invitee}
W50=${W50:-"$DF consent"}

fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo "== $*"; }
need() { [[ -n "${!1:-}" ]] || fail "missing \$$1 (from the harness)"; }
for v in LCX_OWNER LCX_A LCX_B LCX_C DF OWNER_ROLE B_HANDLE W50; do need "$v"; done
# The owner's workspace worker coordinates the swarm; registered now (E10).
OWNER_WKR=${OWNER_WKR:-$($LCX_OWNER hive worker register --cli codex --role conductor)}
[[ $OWNER_WKR == wkr_* ]] || fail "owner worker not registered: $OWNER_WKR"
json() { python3 -I -c "import json,sys; print(json.load(sys.stdin)$1)"; }

# Approve a pending swarm_join of an engine: on its terminal (typed name),
# which continues into the browser flow; W50 answers that with the code.
approve_join() {
  local lcx=$1 role=$2 id code log pid i
  id=$($lcx hive consent list | awk '/swarm_join/ {print $1; exit}')
  [[ -n "$id" ]] || fail "no pending swarm_join request"
  log=$(mktemp)
  $lcx hive consent --web "$id" >"$log" 2>&1 &
  pid=$!
  for i in $(seq 1 30); do
    code=$(sed -n 's/.*enter code \([0-9A-Z]\{4\}-[0-9A-Z]\{4\}\).*/\1/p' "$log" | head -1)
    [[ -n "$code" ]] && break
    sleep 1
  done
  [[ -n "$code" ]] || { kill "$pid" 2>/dev/null; fail "no pairing code printed: $(cat "$log")"; }
  $W50 "$role" "$id" "$code" allow
  wait "$pid" || fail "consent --web did not finish: $(cat "$log")"
  rm -f "$log"
}

step "1. owner creates a team swarm with one open task"
SWARM=$($DF api "$OWNER_ROLE" POST /api/v1/swarms \
  "{\"slug\":\"e2e-swarm-$$\",\"title\":\"E2E swarm\",\"goal\":\"Prove the participant path.\",\"coordinator\":\"$OWNER_WKR\",\"participant_task_cap\":5,\"review_sla_hours\":24,\"repos\":[],\"success_criteria\":[{\"text\":\"one result reviewed\"}]}" \
  | json '["swarm"]["id"]')
ROOT=$($DF api "$OWNER_ROLE" GET "/api/v1/swarms/$SWARM/tree" \
  | python3 -I -c "import json,sys; print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['kind'] in ('root','subgoal')))")
plan() { $LCX_OWNER hive swarm plan "$SWARM" --worker "$OWNER_WKR" "$@"; }
EPOCH=$(plan lease | sed -n 's/.*epoch \([0-9]*\).*/\1/p')
[[ -n "$EPOCH" ]] || fail "no conductor lease"
plan act "$EPOCH" --action "{\"action\":\"split\",\"parent\":\"$ROOT\",\"children\":[{\"title\":\"Prove it\",\"text\":\"Claim, work and submit one result.\",\"acceptance\":[\"a result is in review\"],\"kind\":\"task\"}]}" \
  | grep -q "Done:" || fail "split"
plan release "$EPOCH" >/dev/null
$DF api "$OWNER_ROLE" POST "/api/v1/swarms/$SWARM/invites" "{\"handle\":\"$B_HANDLE\"}" >/dev/null

step "2. engine A (member) joins with consent"
out=$($LCX_A hive swarm join "$SWARM" --cli codex)
grep -q "needs your approval" <<<"$out" || fail "join did not ask: $out"
approve_join "$LCX_A" "${A_ROLE:-engine-1}"
out=$($LCX_A hive swarm join "$SWARM" --cli codex)
grep -q "Joined swarm $SWARM as worker wkr_" <<<"$out" || fail "A not joined: $out"

step "3. engine B (invited non-member) joins"
$LCX_B hive swarm join "$SWARM" --cli codex >/dev/null || true
approve_join "$LCX_B" "${B_ROLE:-e2e-b}"
$LCX_B hive swarm join "$SWARM" --cli codex | grep -q "Joined swarm" || fail "B not joined"

step "4. engine C (no invite) is refused"
out=$($LCX_C hive swarm join "$SWARM" --cli codex 2>&1 || true)
grep -q "not_found" <<<"$out" || fail "C was not refused not_found: $out"

step "5. A reads the fenced goal tree and claims the task"
tree=$($LCX_A hive swarm goal "$SWARM")
grep -q "<<foreign-data kind=swarm" <<<"$tree" || fail "goal text not fenced"
GOAL=$(grep -o 'g_[a-z2-7]\{26\} task open' <<<"$tree" | head -1 | cut -d' ' -f1)
[[ -n "$GOAL" ]] || fail "no open task in the tree"
out=$($LCX_A hive swarm claim "$SWARM" "$GOAL")
CLAIM=$(grep -o 'cl_[a-z2-7]\{26\}' <<<"$out" | head -1)
[[ -n "$CLAIM" ]] || fail "no claim: $out"

step "6. A submits; the goal is in review"
$LCX_A hive swarm submit "$SWARM" "$CLAIM" --summary "The participant path works end to end." \
  | grep -q "waits for review" || fail "submit"
$LCX_A hive swarm goal "$SWARM" | grep -q "$GOAL task in_review" || fail "goal not in_review"

step "7. A leaves; its swarm data is gone"
$LCX_A hive swarm leave "$SWARM" | grep -q "Left swarm" || fail "leave"
$LCX_A hive swarm goal "$SWARM" 2>&1 | grep -q "not_participant" || fail "still a participant"

echo "PASS swarm-two-engines ($SWARM)"
