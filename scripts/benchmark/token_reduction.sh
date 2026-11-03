#!/usr/bin/env bash
# Reproducible raw-vs-LeanCTX output-size benchmark.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RESULTS_DIR="$ROOT/scripts/benchmark/results"
RESULTS_FILE="$RESULTS_DIR/token_reduction.tsv"
RUN_DIR="$RESULTS_DIR/latest"

mkdir -p "$RUN_DIR"
rm -f "$RUN_DIR"/*.raw "$RUN_DIR"/*.compressed "$RESULTS_FILE"

task_name() {
  case "$1" in
    1) echo "read_core_mod";; 2) echo "read_registry";; 3) echo "test_triage";;
    4) echo "git_log";; 5) echo "git_diff_stat";; 6) echo "search_task_envelope";;
    7) echo "read_cargo_toml";; 8) echo "clippy_head";; 9) echo "tree_core";;
    10) echo "read_readme";;
  esac
}

run_raw() {
  case "$1" in
    1) LEAN_CTX_DISABLED=1 cat rust/src/core/mod.rs;;
    2) LEAN_CTX_DISABLED=1 cat rust/crates/lean-ctx-server/src/server/registry.rs;;
    3) (cd rust && LEAN_CTX_DISABLED=1 cargo test --lib -- triage 2>&1);;
    4) LEAN_CTX_DISABLED=1 git log --oneline -20;;
    5) LEAN_CTX_DISABLED=1 git diff --stat HEAD~5;;
    6) LEAN_CTX_DISABLED=1 rg -n --glob '!.git/**' --glob '!rust/target/**' TaskEnvelope .;;
    7) LEAN_CTX_DISABLED=1 cat rust/Cargo.toml;;
    8) (cd rust && LEAN_CTX_DISABLED=1 cargo clippy 2>&1 | head -50);;
    9) LEAN_CTX_DISABLED=1 find rust/src/core -maxdepth 2 -print | sort;;
    10) LEAN_CTX_DISABLED=1 cat README.md;;
  esac
}

run_compressed() {
  case "$1" in
    1) lean-ctx read rust/src/core/mod.rs;;
    2) lean-ctx read rust/crates/lean-ctx-server/src/server/registry.rs;;
    3) lean-ctx -c 'cd rust && cargo test --lib -- triage 2>&1';;
    4) lean-ctx -c 'git log --oneline -20';;
    5) lean-ctx -c 'git diff --stat HEAD~5';;
    6) lean-ctx grep TaskEnvelope .;;
    7) lean-ctx read rust/Cargo.toml;;
    8) lean-ctx -c 'cd rust && cargo clippy 2>&1 | head -50';;
    # `lean-ctx ls` is the CLI counterpart to the ctx_tree MCP operation.
    9) lean-ctx ls rust/src/core --depth 2;;
    10) lean-ctx read README.md;;
  esac
}

chars() { wc -c < "$1" | tr -d '[:space:]'; }
tokens() { echo $(( $1 / 4 )); }

printf 'task\traw_chars\tcompressed_chars\traw_tokens\tcompressed_tokens\n' > "$RESULTS_FILE"
cd "$ROOT"
for id in $(seq 1 10); do
  name="$(task_name "$id")"
  raw="$RUN_DIR/${id}_${name}.raw"
  compressed="$RUN_DIR/${id}_${name}.compressed"
  run_raw "$id" > "$raw" 2>&1 || true
  run_compressed "$id" > "$compressed" 2>&1 || true
  raw_chars="$(chars "$raw")"
  compressed_chars="$(chars "$compressed")"
  printf '%s\t%s\t%s\t%s\t%s\n' "$name" "$raw_chars" "$compressed_chars" \
    "$(tokens "$raw_chars")" "$(tokens "$compressed_chars")" >> "$RESULTS_FILE"
done

echo "Wrote $RESULTS_FILE"
"$ROOT/scripts/benchmark/report.sh" "$RESULTS_FILE" | tee "$RESULTS_DIR/report.md"
