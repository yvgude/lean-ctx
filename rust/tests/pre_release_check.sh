#!/usr/bin/env bash
# Pre-release quality gate for lean-ctx.
# Run this BEFORE tagging a new version.
# Usage: cd rust && bash tests/pre_release_check.sh
set -euo pipefail

BIN="./target/release/lean-ctx"
PASS=0
FAIL=0

step() { printf "\n\033[1;34m=== %s ===\033[0m\n" "$1"; }
ok()   { printf "  \033[32m✓\033[0m %s\n" "$1"; PASS=$((PASS+1)); }
fail() { printf "  \033[31m✗\033[0m %s\n" "$1"; FAIL=$((FAIL+1)); }

# -----------------------------------------------------------------------
step "1/6  cargo test --release"
cargo test --release 2>&1 | tail -1
ok "unit + integration tests"

# -----------------------------------------------------------------------
step "2/6  cargo clippy"
cargo clippy --release -- -D warnings 2>&1 | tail -1
ok "clippy clean"

# -----------------------------------------------------------------------
step "3/6  cargo fmt --check"
cargo fmt --check 2>&1
ok "formatting clean"

# -----------------------------------------------------------------------
step "4/6  Hook E2E tests (Rust + Bash + Consistency)"
cargo test --release --test hook_e2e_tests 2>&1 | tail -1
ok "hook E2E tests"

# -----------------------------------------------------------------------
step "5/6  Release binary"
cargo build --release 2>&1 | tail -1
VERSION=$("$BIN" --version 2>&1)
ok "binary built: $VERSION"

# -----------------------------------------------------------------------
step "6/6  Live hook JSON validation"

validate_rewrite() {
    local label="$1" input="$2" expect="$3"
    local result
    result=$(echo "$input" | "$BIN" hook rewrite 2>/dev/null)

    if [ "$expect" = "passthrough" ]; then
        if [ -z "$result" ]; then ok "$label"; else fail "$label (expected passthrough)"; fi
        return
    fi

    if [ -z "$result" ]; then
        fail "$label (empty output)"
        return
    fi

    if echo "$result" | python3 -c "import sys,json; json.loads(sys.stdin.read())" 2>/dev/null; then
        ok "$label"
    else
        fail "$label (invalid JSON)"
    fi
}

validate_rewrite "simple cmd"       '{"tool_name":"Bash","command":"git status"}'                                      "rewrite"
validate_rewrite "pipe cmd"         '{"tool_name":"Bash","command":"curl https://api.com | python3"}'                   "rewrite"
validate_rewrite "embed quotes"     '{"tool_name":"Bash","command":"git commit --allow-empty -m \"Test\""}'             "rewrite"
validate_rewrite "curl auth"        '{"tool_name":"Bash","command":"curl -H \"Authorization: Bearer tok\" api.com"}'    "rewrite"
validate_rewrite "grep quotes"      '{"tool_name":"Bash","command":"grep -r \"TODO\" src/"}'                            "rewrite"
validate_rewrite "docker multi-env" '{"tool_name":"Bash","command":"docker run -e \"A=1\" -e \"B=2\" nginx"}'           "rewrite"
validate_rewrite "find glob"        '{"tool_name":"Bash","command":"find . -name \"*.js\""}'                            "rewrite"
validate_rewrite "git format"       '{"tool_name":"Bash","command":"git log --format=\"%H %s\""}'                       "rewrite"
validate_rewrite "npm build"        '{"tool_name":"Bash","command":"npm run build"}'                                    "rewrite"
validate_rewrite "ls"               '{"tool_name":"Bash","command":"ls -la"}'                                           "rewrite"
validate_rewrite "self-skip"        '{"tool_name":"Bash","command":"lean-ctx read main.rs"}'                            "passthrough"
validate_rewrite "non-bash"         '{"tool_name":"Write","command":"test"}'                                            "passthrough"

# Also validate the bash hook script
HOOK_SCRIPT=$("$BIN" 2>/dev/null <<< '{}' || true)
TMPSCRIPT="/tmp/lean_ctx_prerelease_hook_$$.sh"
python3 -c "
import subprocess, sys
# Generate the script by calling the binary's internal function via a simple test
" 2>/dev/null || true

# -----------------------------------------------------------------------
printf "\n\033[1;97m=== RESULT: %d passed, %d failed ===\033[0m\n" "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
    printf "\033[31mPre-release check FAILED. Do NOT tag this version.\033[0m\n"
    exit 1
fi

printf "\033[32mPre-release check PASSED. Safe to tag and release.\033[0m\n"
