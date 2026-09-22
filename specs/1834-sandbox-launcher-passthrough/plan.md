# Plan: run Claude Code's OS-sandbox launcher verbatim

1. Capture the exact launcher Claude Code 2.1.275 spawns (multi-line quoted Seatbelt profile, `'"'"'` quoting of the inner script) as a test fixture.
2. In `agent_wrapper.rs`, add `os_sandbox_launcher_argv`: split the string into decoded words (reusing `decode_shell_word`, extended to report consumed bytes), accept only `[env NAME=value…] {sandbox-exec|bwrap} … {zsh|bash|sh} -c <script>`, reject `env` options and assignments that steer the hook.
3. In `exec()`, dispatch the recognised launcher to `exec_sandbox_launcher`, which spawns the argv directly with `clear_shell_default_markers` + `stamp_exec_depth`, before any unwrapping or gating.
4. Tests: recognition and verbatim decoding of the real launcher, never-unwrapped guarantee, rejection matrix for gate-dodging shapes, no false positives on ordinary commands and the #595 wrapper; `sandbox_launcher_command` builder asserted for exact argv, cleared `LEAN_CTX_ACTIVE`/`LEAN_CTX_WRAPPED` and stamped `LEAN_CTX_EXEC_DEPTH`.
5. Run the affected tests, `cargo fmt --check`, the CI clippy invocation and `scripts/preflight.sh fast`; open a PR closing #1834.
