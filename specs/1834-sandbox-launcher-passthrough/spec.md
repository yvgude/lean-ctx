# Spec: gate and run Claude Code's OS-sandbox launcher (refs #1834)

## Problem / Why
With Claude Code `sandbox.enabled` (macOS Seatbelt), every Bash tool call is spawned as
`env SANDBOX_RUNTIME=1 … /usr/bin/sandbox-exec -p '<profile>' /bin/zsh -c '<Path B>'` through
`zsh -c`. The `.zshenv` redirect forwards that launcher to `lean-ctx -c`. `unwrap_agent_wrapper`
does not recognise it, because only a string starting with `/bin/zsh -c` is stripped. The
allowlist then hard-blocks on the inner `eval`, so every command exits 126.

## Goal
Bash tool calls work with the sandbox on. The real command is gated by lean-ctx before the
sandbox starts, and it is never executed outside the sandbox.

## Acceptance Criteria (EARS)
- WHEN `lean-ctx -c` receives a single simple command of exactly the shape Claude Code builds, THE CLI SHALL recognise it as the launcher. That shape is `[env [-u NAME…] [NAME=value…]] /usr/bin/sandbox-exec -p <profile> <shell> -c <script>`, where `<shell>` is `/bin/zsh`, `/bin/bash`, `/bin/sh` or an absolute `$SHELL` whose basename is one of those.
- WHEN the launcher is recognised, THE CLI SHALL run the allowlist gate on `<script>` in the outer process first. If `<script>` is a Path A/B agent wrapper, the gate runs on the unwrapped command. The CLI SHALL spawn the launcher argv only if the gate passes.
- WHEN spawning the launcher, THE CLI SHALL clear `LEAN_CTX_ACTIVE`/`LEAN_CTX_WRAPPED` and stamp `LEAN_CTX_EXEC_DEPTH`. Nesting then stays bounded, and a zsh inner shell may re-enter the hook to compress.
- WHEN an `env` assignment has a name outside the list of variables Claude Code sets, THE CLI SHALL treat the command as ordinary (existing allowlist path). That list is proxy, CA, `TMPDIR`, `SANDBOX_RUNTIME`, and the exact `GIT_SSH_COMMAND`, `GIT_CONFIG_PARAMETERS`, `GIT_CONFIG_KEY_n`/`VALUE_n`/`COUNT` and `JAVA_TOOL_OPTIONS` values. The same applies when a value for one of the exact-value names differs.
- WHEN the launcher carries an `env` option other than `-u`, `-u` of a hook-steering variable, anything between `sandbox-exec` and the shell, a relative or untrusted launcher/shell path, an unquoted operator or newline, extra arguments, or `bwrap`, THE CLI SHALL treat it as an ordinary command.
- WHEN `unwrap_agent_wrapper` receives the launcher, IT SHALL return `None`, so the sandbox is never unwrapped through.

## Out of Scope
- Changing the `.zshenv`/`.bashenv` redirect template. A substring skip marker would let any command that mentions `sandbox-exec` bypass the gate entirely.
- Refreshing already-installed `.zshenv` hooks on binary update.
- Linux `bwrap`. Not recognised; falls back to the gate.
- User-defined `sandbox.setEnvVars` with arbitrary names. These fall back to the gate, which is fail-closed and the same as before.

## Verification
- `cargo test --lib -- agent_wrapper exec_tests`
- `cargo clippy --all-targets -- -D warnings`
- Manual, with `LEAN_CTX_SHELL_SECURITY=enforce`: the real launcher runs. A launcher around `eval …`, or with a `SHELLOPTS=` assignment, exits 126.

## Links
- Tracking issue: #1834
- Plan: ./plan.md · Tasks: ./tasks.md
