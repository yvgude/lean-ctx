#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Benchmark runner.
# usage: BENCH_REPO=/path/to/checkout ./run.sh ["<variants>"] ["<tasks>"] [reps] [model]
# Records one line per run in runs/index.txt; summarize.py reads it.
set -euo pipefail
umask 077
BENCH_DIR=$(cd "$(dirname "$0")" && pwd)
export BENCH_DIR
: "${BENCH_REPO:?set BENCH_REPO to the repository checkout the sessions work in}"
export BENCH_REPO
VARIANTS=${1:-"native code_mode code_mode_leanctx"}
TASKS=${2:-"t1 t2 t3 t4 t5"}
REPS=${3:-3}
MODEL=${4:-claude-sonnet-5-5}
# When started from inside a Claude Code session, drop the env it leaks to children.
for v in $(env | grep -oE '^(CLAUDE[A-Z_]*|CLAUDECODE)=' | tr -d '='); do unset "$v"; done
mkdir -p "$BENCH_DIR/runs"
uuid() { uuidgen | tr 'A-Z' 'a-z'; }
# Warm-up: one unrecorded run per variant, so no recorded run pays the cold
# prompt-cache write of its system prompt and tool list.
if [ "${WARMUP:-1}" = 1 ]; then
  for v in $VARIANTS; do
    expect "$BENCH_DIR/drive.exp" "$v" "$(uuid)" "$BENCH_DIR/tasks/t2.txt" "$BENCH_DIR/runs/warmup-$v.log" "$MODEL" > /dev/null 2>&1 || true
    echo "warmup $v"
  done
fi
for rep in $(seq 1 "$REPS"); do
  for t in $TASKS; do
    # Alternate forward and reverse variant order across reps and tasks.
    order=$VARIANTS
    if [ $(( (rep + ${t#t}) % 2 )) -eq 1 ]; then
      order=$(echo "$VARIANTS" | awk '{for(i=NF;i>0;i--) printf "%s ", $i}')
    fi
    for v in $order; do
      sid=$(uuid)
      expect "$BENCH_DIR/drive.exp" "$v" "$sid" "$BENCH_DIR/tasks/$t.txt" "$BENCH_DIR/runs/$v-$t-$rep.log" "$MODEL" > /dev/null 2>&1 || true
      echo "$v $t $rep $sid" >> "$BENCH_DIR/runs/index.txt"
      echo "done $v $t rep$rep"
    done
  done
done
echo BENCH_ALL_DONE
