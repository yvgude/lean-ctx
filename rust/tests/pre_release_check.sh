#!/usr/bin/env bash
# Pre-release quality gate for lean-ctx.
# Run this BEFORE tagging a new version.
# Usage: cd rust && bash tests/pre_release_check.sh
set -euo pipefail

# Honour CARGO_TARGET_DIR / config.toml target-dir overrides (#671); fall back
# to ./target when `cargo metadata` is unavailable. `|| true` keeps the
# pipeline from tripping `set -euo pipefail` before the fallback applies.
TARGET_DIR=$(cargo metadata --no-deps --format-version=1 2>/dev/null \
    | grep -o '"target_directory":"[^"]*"' \
    | head -1 \
    | sed -E 's/^"target_directory":"(.*)"$/\1/' \
    | sed 's/\\\\/\//g' || true)
BIN="${TARGET_DIR:-./target}/release/lean-ctx"
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
step "4/6  Shell & Agent tests"
cargo test --release --test shell_and_agent_tests 2>&1 | tail -1
ok "shell & agent tests"

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

    # The PreToolUse hook contract always emits a non-empty JSON "allow" decision.
    #   passthrough = "allow" WITHOUT updatedInput  (command left untouched: self-calls,
    #                 non-Bash tools, non-rewritable commands)
    #   rewrite     = "allow" WITH updatedInput     (command routed through `lean-ctx -c`)
    if [ -z "$result" ]; then
        fail "$label (empty output)"
        return
    fi

    local kind
    kind=$(echo "$result" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
except Exception:
    print('invalid'); sys.exit()
hs = d.get('hookSpecificOutput', {}) or {}
print('rewrite' if ('updatedInput' in hs or 'updated_input' in d) else 'passthrough')
" 2>/dev/null)

    case "$kind" in
        invalid)   fail "$label (invalid JSON)" ;;
        "$expect") ok "$label" ;;
        *)         fail "$label (expected $expect, got $kind)" ;;
    esac
}

validate_rewrite "simple cmd"       '{"tool_name":"Bash","command":"git status"}'                                      "rewrite"
# Gate-clean pipe → wrapped WHOLE in one `lean-ctx -c` (fixes #589: Windows _lc +
# left-of-pipe compression). Robust: git/head are default-allowlisted, no
# interpreter, so the decision is independent of strict-mode/allowlist tweaks.
validate_rewrite "clean pipe"       '{"tool_name":"Bash","command":"git log --oneline | head -5"}'                     "rewrite"
validate_rewrite "embed quotes"     '{"tool_name":"Bash","command":"git commit --allow-empty -m \"Test\""}'             "rewrite"
validate_rewrite "curl auth"        '{"tool_name":"Bash","command":"curl -H \"Authorization: Bearer tok\" api.com"}'    "rewrite"
validate_rewrite "rg quotes"        '{"tool_name":"Bash","command":"rg \"TODO\" src/"}'                                 "rewrite"
validate_rewrite "grep passthrough" '{"tool_name":"Bash","command":"grep -r \"TODO\" src/"}'                            "passthrough"
# Compat-first (#589): a compound whose sink is NOT gate-clean (kubectl is never
# in the defaults) is left RAW for the agent shell — wrapping it would newly
# subject the sink to lean-ctx's gate and block a previously-working command.
validate_rewrite "tricky sink pipe" '{"tool_name":"Bash","command":"git log | kubectl apply -f -"}'                    "passthrough"
validate_rewrite "docker multi-env" '{"tool_name":"Bash","command":"docker run -e \"A=1\" -e \"B=2\" nginx"}'           "rewrite"
validate_rewrite "find glob"        '{"tool_name":"Bash","command":"find . -name \"*.js\""}'                            "rewrite"
validate_rewrite "git format"       '{"tool_name":"Bash","command":"git log --format=\"%H %s\""}'                       "rewrite"
validate_rewrite "npm build"        '{"tool_name":"Bash","command":"npm run build"}'                                    "rewrite"
validate_rewrite "ls"               '{"tool_name":"Bash","command":"ls -la"}'                                           "rewrite"
validate_rewrite "self-skip"        '{"tool_name":"Bash","command":"lean-ctx read main.rs"}'                            "passthrough"
validate_rewrite "non-bash"         '{"tool_name":"Write","command":"test"}'                                            "passthrough"

# -----------------------------------------------------------------------
printf "\n\033[1;97m=== RESULT: %d passed, %d failed ===\033[0m\n" "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
    printf "\033[31mPre-release check FAILED. Do NOT tag this version.\033[0m\n"
    exit 1
fi

printf "\033[32mPre-release check PASSED. Safe to tag and release.\033[0m\n"
