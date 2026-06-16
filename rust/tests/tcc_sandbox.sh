#!/usr/bin/env bash
#
# tcc_sandbox.sh — #356 regression guard (macOS only).
#
# Boots the lean-ctx foreground daemon as a *TCC-standalone* process (the
# end-user LaunchAgent condition, forced via LEAN_CTX_TCC_STANDALONE=1) in three
# runs:
#   1. control    — no sandbox, proves the throwaway HOME is sane.
#   2. detection  — deny ~/Documents + ~/Desktop + ~/Downloads with SIGKILL, so
#                   any stray access kills the daemon and fails the test.
#   3. production — the exact `sandbox-exec -p` wrapper lean-ctx bakes into the
#                   LaunchAgent (silent EPERM deny), proving it boots + survives.
# The daemon is told about a project living under ~/Documents (via
# LEAN_CTX_PROJECT_ROOT and a stored .lean-ctx.toml there) — exactly the setup
# that made #356 recur. If any boot path stats / reads / canonicalizes those
# dirs, the kernel kills the daemon and this test fails.
#
# This codifies the empirical method used to root-cause #356. It is the only
# check that reproduces the real end-user condition: running `lean-ctx update`
# (or tests) from a terminal masks the bug, because the terminal already holds
# the Documents TCC grant.
#
# A control run (same conditions, no sandbox) first proves the daemon boots in
# this throwaway HOME, so a sandbox-run death is unambiguously a ~/Documents
# access rather than an unrelated environment problem.
#
# Gated: only runs when LEAN_CTX_TCC_SANDBOX_TEST=1. Needs macOS + sandbox-exec.
# Binary: LEAN_CTX_BIN, else `lean-ctx` on PATH.
# Tunables: LEAN_CTX_TCC_SOAK_SECS (default 10).

set -uo pipefail

if [[ "${LEAN_CTX_TCC_SANDBOX_TEST:-0}" != "1" ]]; then
  echo "SKIP: set LEAN_CTX_TCC_SANDBOX_TEST=1 to run this regression (macOS only)"
  exit 0
fi
if [[ "$(uname)" != "Darwin" ]]; then
  echo "SKIP: macOS only (TCC is a macOS feature)"
  exit 0
fi
if ! command -v sandbox-exec >/dev/null 2>&1; then
  echo "SKIP: sandbox-exec not available"
  exit 0
fi

BIN="${LEAN_CTX_BIN:-}"
[[ -z "$BIN" ]] && BIN="$(command -v lean-ctx 2>/dev/null || true)"
if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "FAIL: no lean-ctx binary — set LEAN_CTX_BIN or put lean-ctx on PATH"
  exit 1
fi

SOAK="${LEAN_CTX_TCC_SOAK_SECS:-10}"
# Keep this base path SHORT: the daemon binds a Unix domain socket under
# "$HOME/Library/Application Support/lean-ctx/daemon.sock", and sun_path caps at
# ~104 bytes on macOS. TMPDIR (/var/folders/...) is already long enough to blow
# that budget, so anchor under /tmp.
ROOT_TMP="$(mktemp -d /tmp/lc.XXXXXX)"
# Resolve symlinks (/tmp -> /private/tmp, /var -> /private/var): the kernel
# matches sandbox subpath filters against the *canonical* path, and so must the
# daemon's HOME, or the deny rule silently misses.
ROOT_TMP="$(cd "$ROOT_TMP" && pwd -P)"

PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do
    [[ -n "$p" ]] && kill -9 "$p" 2>/dev/null || true
  done
  rm -rf "$ROOT_TMP" 2>/dev/null || true
}
trap cleanup EXIT

# Seed a throwaway HOME with a project under ~/Documents (markers + local config).
seed_home() {
  local home="$1"
  local proj="$home/Documents/proj"
  mkdir -p "$proj/.git"
  printf 'fn main() {}\n' >"$proj/main.rs"
  printf '[context]\nmax_tokens = 1234\n' >"$proj/.lean-ctx.toml"
}

# Boot the foreground daemon and report whether it survives the soak.
# Args: <home> <soak> <mode: off|kill|prod>. Returns 0 if alive, 1 if it died.
boot_and_soak() {
  local home="$1" soak="$2" mode="$3"
  local proj="$home/Documents/proj"
  local log="$home/daemon.log"

  # Production-equivalent deny set: read+write under all three TCC-protected
  # home dirs — mirrors src/core/tcc_guard_sandbox.rs. `file-read*` covers
  # stat / open-read / read_dir / realpath; `file-write*` covers writes — both
  # trip the TCC prompt (#356). `(allow default)` then a later `(deny ...)`:
  # last match wins in SBPL.
  local denies='(subpath "'"$home"'/Documents") (subpath "'"$home"'/Desktop") (subpath "'"$home"'/Downloads")'

  local -a cmd=()
  case "$mode" in
    kill)
      # SIGKILL on any access makes a stray ~/Documents touch *detectable*.
      local profile="$home/deny-tcc.sb"
      cat >"$profile" <<SB
(version 1)
(allow default)
(deny file-read* file-write* $denies (with send-signal SIGKILL))
SB
      cmd+=(sandbox-exec -f "$profile")
      ;;
    prod)
      # The exact form lean-ctx bakes into the LaunchAgent ProgramArguments:
      # a silent deny (EPERM, no SIGKILL) passed inline via `-p`.
      local prof="(version 1) (allow default) (deny file-read* file-write* $denies)"
      cmd+=(sandbox-exec -p "$prof")
      ;;
    off) ;;
  esac
  cmd+=("$BIN" serve --_foreground-daemon)

  # cwd=/ mimics a real LaunchAgent; env -i isolates from the caller's XDG/LEAN_CTX.
  (
    cd / || exit 1
    exec env -i \
      HOME="$home" \
      PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
      TMPDIR="${TMPDIR:-/tmp}" \
      LEAN_CTX_TCC_STANDALONE=1 \
      LEAN_CTX_PROJECT_ROOT="$proj" \
      "${cmd[@]}"
  ) >"$log" 2>&1 &
  local pid=$!
  PIDS+=("$pid")

  local s
  for ((s = 0; s < soak; s++)); do
    if ! kill -0 "$pid" 2>/dev/null; then
      return 1
    fi
    sleep 1
  done
  kill -0 "$pid" 2>/dev/null && return 0 || return 1
}

# --- Control run: prove the daemon boots in this throwaway HOME (no sandbox). ---
CTRL_HOME="$ROOT_TMP/ctrl"
seed_home "$CTRL_HOME"
if ! boot_and_soak "$CTRL_HOME" 4 off; then
  echo "FAIL(inconclusive): daemon did not stay up even WITHOUT the sandbox."
  echo "  The environment, not #356, is the problem. Daemon log:"
  echo "----- control daemon log -----"
  cat "$CTRL_HOME/daemon.log" 2>/dev/null || true
  exit 1
fi
echo "ok: control daemon booted and survived (env is sane)"

# --- Detection run: SIGKILL on any access under the three TCC-protected dirs. -
SB_HOME="$ROOT_TMP/sandbox"
seed_home "$SB_HOME"
if ! boot_and_soak "$SB_HOME" "$SOAK" kill; then
  echo "FAIL: daemon was killed under the deny-TCC sandbox."
  echo "      A TCC-standalone boot path accessed ~/Documents — #356 has regressed."
  echo "----- sandbox daemon log -----"
  cat "$SB_HOME/daemon.log" 2>/dev/null || true
  exit 1
fi
echo "ok: no boot path stats/reads/canonicalizes ~/Documents/~Desktop/~Downloads"

# --- Production run: the exact LaunchAgent wrapper (silent deny via `-p`). -----
# Proves the real production invocation form boots and survives (EPERM, no kill),
# i.e. the seatbelt wrapper does not break normal daemon operation.
PROD_HOME="$ROOT_TMP/prod"
seed_home "$PROD_HOME"
if ! boot_and_soak "$PROD_HOME" 4 prod; then
  echo "FAIL: daemon did not survive under the production 'sandbox-exec -p' wrapper."
  echo "----- production daemon log -----"
  cat "$PROD_HOME/daemon.log" 2>/dev/null || true
  exit 1
fi

echo "PASS: production seatbelt wrapper boots clean; no ~/Documents access (#356 fixed)"
exit 0
