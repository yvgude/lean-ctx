# Spec: run Claude Code's OS-sandbox launcher verbatim (refs #1834)

## Problem / Why
With Claude Code `sandbox.enabled` (macOS Seatbelt), every Bash tool call is spawned as
`env SANDBOX_RUNTIME=1 … /usr/bin/sandbox-exec -p '<profile>' /bin/zsh -c '<Path B>'` through
`zsh -c`. The `.zshenv` redirect forwards that launcher to `lean-ctx -c`, `unwrap_agent_wrapper`
does not recognise it (only a string starting with `/bin/zsh -c` is stripped), and the allowlist
hard-blocks on the inner `eval` — exit 126 for every command.

## Goal
Bash tool calls work with the sandbox on, with lean-ctx gating and compressing the real command
inside the sandbox, and without ever executing the real command outside it.

## Acceptance Criteria (EARS)
- WHEN `lean-ctx -c` receives a single simple command of the shape `[env NAME=value…] {sandbox-exec|bwrap} … {zsh|bash|sh} -c <script>`, THE CLI SHALL spawn that argv directly, without gating it and without a shell hop.
- WHEN spawning the launcher, THE CLI SHALL clear `LEAN_CTX_ACTIVE`/`LEAN_CTX_WRAPPED` and stamp `LEAN_CTX_EXEC_DEPTH`, so the inner shell re-enters the hook and runaway nesting stays bounded.
- WHEN the launcher carries `env` options, an assignment to `LEAN_CTX_*`, `ZDOTDIR`, `BASH_ENV`, `ENV`, `HOME`, `PATH`, `SHELL` or an agent marker variable, an unquoted operator or newline, or does not end in `<shell> -c <script>`, THE CLI SHALL treat it as an ordinary command (existing allowlist path).
- WHEN `unwrap_agent_wrapper` receives the launcher, IT SHALL return `None` (the sandbox is never unwrapped through).
- WHEN the inner Path A/B script reaches the hook inside the sandbox, IT SHALL unwrap exactly as before.

## Out of Scope
- Changing the `.zshenv`/`.bashenv` redirect template (a substring skip marker would let any command that mentions `sandbox-exec` bypass the gate entirely).
- Refreshing already-installed `.zshenv` hooks on binary update.
- Linux `bwrap` end-to-end verification (shape accepted, not exercised against Claude Code on Linux).

## Verification
- `cargo test --lib shell::agent_wrapper`
- `cargo test --lib shell::exec`
- `scripts/preflight.sh fast`
- Manual: feed the launcher string to `lean-ctx -c` — 3.10.2 exits 126 with the `[BLOCKED …]` message; the patched binary execs `sandbox-exec`.

## Links
- Tracking issue: #1834
- Plan: ./plan.md · Tasks: ./tasks.md
