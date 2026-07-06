#!/usr/bin/env bash
#
# Local CI-parity gate — mirrors .github/workflows/ci.yml.
#
# A green run here means the *deterministic* CI jobs (Format, Clippy,
# Documentation, and the cross-platform compile) will pass. It exists because
# those failures otherwise only surface after a full ~50-min CI matrix:
# e.g. a private intra-doc link (Documentation job) or test-only code that is
# dead on Windows (Test job) — both invisible to `cargo test` / plain clippy.
#
# Change-aware (#850): the Rust gates can only be broken by Rust/Cargo changes,
# so a docs-only push (README, CHANGELOG, *.md, website, …) skips them and the
# pre-push gate finishes in a second. CI still runs every job unconditionally —
# it stays the source of truth — but a docs-only diff cannot turn a Rust gate
# red, so skipping it locally can never produce a local-green / CI-red split.
# `gen_docs --check` additionally guards the committed generated reference, so it
# also runs whenever a file under docs/reference/generated/** changed by hand.
# `full` (release parity) ignores classification and runs everything + tests.
#
# No-test policy (#849): a change to contract code (proxy / tools / config
# schema — anything feeding deterministic output, #498) that carries no test
# signal is flagged. Advisory by default; LEAN_CTX_PREFLIGHT_STRICT_TESTS=1
# makes it blocking.
#
# Usage:
#   scripts/preflight.sh [fast|full]      (default: fast)
#     fast   change-aware: fmt + clippy + doc + gen_docs drift + Windows
#            cross-compile, each skipped when no Rust/generated-doc file changed
#     full   force everything regardless of the diff, plus `cargo test --lib`
#
# Bypass: not from here — use `SKIP_PREFLIGHT=1 git push` / `git push --no-verify`.
#
# CI parity (.github/workflows/ci.yml):
#   - global env: RUSTFLAGS=-Dwarnings, LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD=0
#   - Documentation job: RUSTDOCFLAGS=-Dwarnings cargo doc --no-deps --all-features
#                        cargo run --example gen_docs --features dev-tools -- --check
#   - Clippy job:        cargo clippy --all-features -- -D warnings
#   - Format job:        cargo fmt --check
#   - Test job (Windows) compiles for x86_64-pc-windows-gnu

set -o pipefail

LEVEL="${1:-fast}"
case "$LEVEL" in
  fast|full) ;;
  *) echo "usage: $0 [fast|full]" >&2; exit 2 ;;
esac

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Base ref for changed-file detection. Overridable for forks/CI that track a
# different upstream; defaults to the branch CI gates against.
BASE_REF="${PREFLIGHT_BASE:-origin/main}"

# ── Change classification (#850/#849) ─────────────────────────────────
# Sets: RUST_CHANGED, GENERATED_DOCS_CHANGED, CONTRACT_CHANGED, TEST_SIGNAL,
# CHANGED_COUNT, CLASSIFY_OK. When the base ref is unreachable (fresh clone,
# detached history) CLASSIFY_OK=0 → callers fall back to running everything.
classify_changes() {
  RUST_CHANGED=0
  GENERATED_DOCS_CHANGED=0
  CONTRACT_CHANGED=0
  CCR_CHANGED=0
  TEST_SIGNAL=0
  CHANGED_COUNT=0
  CLASSIFY_OK=1
  BASE_SHA=""

  local base
  base="$(git -C "$REPO_ROOT" merge-base "$BASE_REF" HEAD 2>/dev/null || true)"
  if [[ -z "$base" ]]; then
    CLASSIFY_OK=0
    return
  fi
  BASE_SHA="$base"

  # base..working-tree (committed + staged + unstaged) ∪ untracked.
  local files
  files="$( {
      git -C "$REPO_ROOT" diff --name-only "$base"
      git -C "$REPO_ROOT" ls-files --others --exclude-standard
    } 2>/dev/null | sed 's#\\#/#g' | sort -u | sed '/^$/d' )"

  CHANGED_COUNT="$(printf '%s\n' "$files" | sed '/^$/d' | grep -c '' || true)"

  local f
  while IFS= read -r f; do
    [[ -z "$f" ]] && continue
    case "$f" in
      rust/*) RUST_CHANGED=1 ;;
    esac
    case "$f" in
      docs/reference/generated/*) GENERATED_DOCS_CHANGED=1 ;;
    esac
    case "$f" in
      rust/src/proxy/*|rust/src/tools/*|rust/src/core/config/schema/*) CONTRACT_CHANGED=1 ;;
    esac
    # CCR recovery surface (#983): a change here must keep the robustness suite
    # green, so flag it for the change-aware `cargo test --lib ccr_robustness` gate.
    case "$f" in
      rust/src/proxy/ccr.rs|rust/src/proxy/ccr_robustness_tests.rs|rust/src/tools/ctx_expand.rs|rust/src/tools/registered/ctx_retrieve.rs|rust/src/core/read_stub_index.rs|rust/src/core/tabular_crush.rs) CCR_CHANGED=1 ;;
    esac
    case "$f" in
      rust/tests/*|*/tests/*|*test*.rs|*tests.rs) TEST_SIGNAL=1 ;;
    esac
  done <<< "$files"

  # Inline tests live in the same .rs file as the code (#[cfg(test)] mod tests),
  # so a filename check misses them. If no test *file* changed, look for test
  # signal inside the Rust diff itself.
  if [[ "$TEST_SIGNAL" -eq 0 ]]; then
    if git -C "$REPO_ROOT" diff "$base" -- rust 2>/dev/null \
        | grep -Eq '^[+-].*(#\[test\]|#\[cfg\(test\)\]|assert(_eq|_ne)?!|proptest!)'; then
      TEST_SIGNAL=1
    fi
  fi
}

cd "$REPO_ROOT/rust"

# Match CI's environment. RUSTFLAGS=-Dwarnings is applied *per step* (only where
# it must change the build fingerprint — the Windows cross-check and the full
# test build) so the fast host checks keep sharing the normal dev target cache
# instead of recompiling the whole dependency tree.
export RUSTDOCFLAGS="-Dwarnings"
export LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD=0
# Keep proptest snappy like CI (local default is 256); override if you want more.
export PROPTEST_CASES="${PROPTEST_CASES:-64}"

# Windows test compiles the GNU target. jemalloc needs MinGW (not available on
# a plain dev box), so we cross-*check* with the default feature set minus
# jemalloc — enough to exercise the same cfg/dead-code analysis that bit us.
WIN_TARGET="x86_64-pc-windows-gnu"
WIN_FEATURES="tree-sitter,embeddings,http-server,team-server,secure-update"

BOLD="\033[1m"; CYAN="\033[1;36m"; GREEN="\033[1;32m"; RED="\033[1;31m"
YELLOW="\033[1;33m"; RESET="\033[0m"

PASSED=()
FAILED=()
SKIPPED=()
WARNED=()

step() { # step "Label" cmd...
  local label="$1"; shift
  printf "\n${CYAN}▶ %s${RESET}\n" "$label"
  printf "${BOLD}  \$ %s${RESET}\n" "$*"
  if "$@"; then
    PASSED+=("$label")
  else
    FAILED+=("$label")
  fi
}

skip() { # skip "Label" "reason"
  printf "\n${YELLOW}⊘ %s — skipped: %s${RESET}\n" "$1" "$2"
  SKIPPED+=("$1: $2")
}

# ── Classify + decide what to run ─────────────────────────────────────
classify_changes

FORCE_ALL=0
[[ "$LEVEL" == "full" ]] && FORCE_ALL=1
[[ "$CLASSIFY_OK" -eq 0 ]] && FORCE_ALL=1

if [[ "$FORCE_ALL" -eq 1 ]]; then
  RUN_RUST=1
  RUN_GEN_DOCS=1
else
  RUN_RUST="$RUST_CHANGED"
  RUN_GEN_DOCS=0
  { [[ "$RUST_CHANGED" -eq 1 ]] || [[ "$GENERATED_DOCS_CHANGED" -eq 1 ]]; } && RUN_GEN_DOCS=1
fi

printf "${BOLD}preflight (%s) — CI-parity gate${RESET}\n" "$LEVEL"
if [[ "$CLASSIFY_OK" -eq 1 ]]; then
  printf "  changed files vs %s: %s  (rust=%s, generated-docs=%s, contract=%s)\n" \
    "$BASE_REF" "$CHANGED_COUNT" "$RUST_CHANGED" "$GENERATED_DOCS_CHANGED" "$CONTRACT_CHANGED"
else
  printf "  ${YELLOW}change classification unavailable (no %s) — running full gate${RESET}\n" "$BASE_REF"
fi

# Always-on cheap gate: whitespace errors + leftover conflict markers. Checks
# the pushed range (base..HEAD) when known, else the working tree.
if [[ -n "$BASE_SHA" ]]; then
  step "Whitespace / conflict markers (git diff --check)" \
    git -C "$REPO_ROOT" diff --check "$BASE_SHA" HEAD
else
  step "Whitespace / conflict markers (git diff --check)" \
    git -C "$REPO_ROOT" diff --check
fi

if [[ "$RUN_RUST" -eq 1 ]]; then
  step "Format (cargo fmt --check)" \
    cargo fmt --check

  step "Clippy (--all-features -D warnings)" \
    cargo clippy --all-features -- -D warnings

  step "Docs (rustdoc -D warnings)" \
    cargo doc --no-deps --all-features
else
  skip "Format / Clippy / Docs" "no Rust/Cargo files changed (docs-only)"
fi

if [[ "$RUN_GEN_DOCS" -eq 1 ]]; then
  step "Generated-docs drift (gen_docs --check)" \
    cargo run --quiet --example gen_docs --features dev-tools -- --check
else
  skip "Generated-docs drift (gen_docs --check)" "no Rust or generated-doc files changed"
fi

if [[ "$RUN_RUST" -eq 1 ]]; then
  step "Registry-snapshot drift (gen_registry --check)" \
    cargo run --quiet --example gen_registry --features dev-tools -- --check
else
  skip "Registry-snapshot drift (gen_registry --check)" "no Rust files changed"
fi

if [[ "$RUN_RUST" -eq 1 ]]; then
  if rustup target list --installed 2>/dev/null | grep -qx "$WIN_TARGET"; then
    step "Windows cross-compile ($WIN_TARGET)" \
      env RUSTFLAGS=-Dwarnings cargo check --target "$WIN_TARGET" --lib --tests \
        --no-default-features --features "$WIN_FEATURES"
  else
    skip "Windows cross-compile ($WIN_TARGET)" \
      "target not installed — run: rustup target add $WIN_TARGET"
  fi
else
  skip "Windows cross-compile ($WIN_TARGET)" "no Rust/Cargo files changed (docs-only)"
fi

# CCR robustness gate (#983) — change-aware: `full` already runs every lib test,
# so only `fast` needs this focused build, and only when the recovery surface
# changed. A test build is otherwise kept out of `fast` by design.
if [[ "$LEVEL" != "full" && "$RUN_RUST" -eq 1 && "$CCR_CHANGED" -eq 1 ]]; then
  step "CCR robustness regression (cargo test --lib ccr_robustness)" \
    env RUSTFLAGS=-Dwarnings cargo test --lib --all-features ccr_robustness
elif [[ "$LEVEL" != "full" ]]; then
  skip "CCR robustness regression" "no CCR recovery files changed"
fi

if [[ "$LEVEL" = "full" ]]; then
  step "Unit tests (cargo test --lib)" \
    env RUSTFLAGS=-Dwarnings cargo test --lib --all-features

  # Entrypoint + rules-drift smoke gates (#902/#903). Integration tests, so they
  # build the bin — kept out of `fast` (which deliberately avoids test builds);
  # CI runs them unconditionally via `cargo test --all-features`.
  step "Entrypoint + rules drift (#902/#903)" \
    env RUSTFLAGS=-Dwarnings cargo test --all-features --test entrypoints_wired --test rules_drift
fi

# ── No-test policy (#849) ─────────────────────────────────────────────
# Contract code changed but the diff carries no test signal → flag it. Advisory
# unless LEAN_CTX_PREFLIGHT_STRICT_TESTS=1. Never triggers for docs/metadata-only
# changes (CONTRACT_CHANGED stays 0).
if [[ "$CLASSIFY_OK" -eq 1 && "$CONTRACT_CHANGED" -eq 1 && "$TEST_SIGNAL" -eq 0 ]]; then
  MSG="contract code changed (proxy/tools/config-schema) but the diff adds no test signal — add/adjust tests or justify"
  if [[ "${LEAN_CTX_PREFLIGHT_STRICT_TESTS:-0}" == "1" ]]; then
    printf "\n${RED}▶ No-test policy (#849)${RESET}\n  ${RED}%s${RESET}\n" "$MSG"
    FAILED+=("No-test policy: $MSG")
  else
    printf "\n${YELLOW}▶ No-test policy (#849) — advisory${RESET}\n  ${YELLOW}%s${RESET}\n" "$MSG"
    printf "  ${YELLOW}(set LEAN_CTX_PREFLIGHT_STRICT_TESTS=1 to make this blocking)${RESET}\n"
    WARNED+=("No-test policy: $MSG")
  fi
fi

# ── Summary ───────────────────────────────────────────────────────────
printf "\n${BOLD}── preflight summary ──${RESET}\n"
printf "${GREEN}  ok passed:  %d${RESET}\n" "${#PASSED[@]}"
if [ "${#SKIPPED[@]}" -gt 0 ]; then
  printf "${YELLOW}  -- skipped: %d${RESET}\n" "${#SKIPPED[@]}"
  for s in "${SKIPPED[@]}"; do printf "${YELLOW}      - %s${RESET}\n" "$s"; done
fi
if [ "${#WARNED[@]}" -gt 0 ]; then
  printf "${YELLOW}  !! warned:  %d${RESET}\n" "${#WARNED[@]}"
  for w in "${WARNED[@]}"; do printf "${YELLOW}      - %s${RESET}\n" "$w"; done
fi
if [ "${#FAILED[@]}" -gt 0 ]; then
  printf "${RED}  XX failed:  %d${RESET}\n" "${#FAILED[@]}"
  for f in "${FAILED[@]}"; do printf "${RED}      - %s${RESET}\n" "$f"; done
  printf "\n${RED}preflight FAILED — fix the above before pushing.${RESET}\n"
  exit 1
fi

printf "\n${GREEN}preflight PASSED — safe to push.${RESET}\n"
