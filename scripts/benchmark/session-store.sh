#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${LEAN_CTX_BIN:-$ROOT/rust/target/release/lean-ctx}"
if [[ ! -x "$BIN" ]]; then
  BIN="$(command -v lean-ctx)"
fi
[[ -x "$BIN" ]] || { echo "Build rust/target/release/lean-ctx or set LEAN_CTX_BIN." >&2; exit 1; }

COUNT="${1:-10000}"
RUNS="${RUNS:-15}"
WARMUP="${WARMUP:-5}"
DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/lean-ctx-session-store.XXXXXX")"
PROJECT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/lean-ctx-session-project.XXXXXX")"
cleanup() { rm -rf "$DATA_DIR" "$PROJECT_DIR"; }
trap cleanup EXIT

git -C "$PROJECT_DIR" init -q
export LEAN_CTX_DATA_DIR="$DATA_DIR"
"$BIN" call ctx_session --project-root "$PROJECT_DIR" --json '{"action":"save"}' >/dev/null
SEED="$(find "$DATA_DIR/sessions" -maxdepth 1 -type f -name '*.json' ! -name latest.json -print -quit)"
[[ -n "$SEED" ]] || { echo "Could not create a seed session." >&2; exit 1; }

python3 - "$SEED" "$DATA_DIR/sessions" "$COUNT" "$BIN" "$PROJECT_DIR" "$RUNS" "$WARMUP" <<'PY'
import copy
import json
import os
import statistics
import subprocess
import sys
import time

seed_path, sessions_dir, count, binary, project_dir, runs, warmup = sys.argv[1:]
count, runs, warmup = map(int, (count, runs, warmup))
with open(seed_path, encoding="utf-8") as handle:
    seed = json.load(handle)

for number in range(count):
    session = copy.deepcopy(seed)
    session["id"] = f"nonrelevant-{number:05d}"
    session["project_root"] = f"/tmp/lean-ctx-nonrelevant-project-{number:05d}"
    session["shell_cwd"] = session["project_root"]
    with open(
        os.path.join(sessions_dir, f"{session['id']}.json"),
        "w",
        encoding="utf-8",
    ) as handle:
        json.dump(session, handle)

command = [binary, "-c", "/usr/bin/true"]
for _ in range(warmup):
    subprocess.run(command, cwd=project_dir, check=True, stdout=subprocess.DEVNULL)

samples = []
for _ in range(runs):
    started = time.perf_counter_ns()
    subprocess.run(command, cwd=project_dir, check=True, stdout=subprocess.DEVNULL)
    samples.append((time.perf_counter_ns() - started) / 1_000_000)

print(f"sessions={count} runs={runs} warmup={warmup}")
print(f"p50_ms={statistics.median(samples):.1f}")
print("samples_ms=" + ",".join(f"{sample:.1f}" for sample in samples))
PY
