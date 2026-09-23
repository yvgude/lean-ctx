# Tasks: gate and run Claude Code's OS-sandbox launcher

- [x] Fixture: real launcher string from Claude Code 2.1.280 (`LAUNCHER` in `agent_wrapper.rs` tests).
- [x] `decode_shell_word_at` + `split_simple_command` (operators/newlines/unterminated quotes → `None`).
- [x] `os_sandbox_launcher_argv`: exact `sandbox-exec -p … <trusted shell> -c` shape, env assignments restricted to what Claude Code's launcher sets.
- [x] `exec()`: gate the (unwrapped) script in the outer process, then `exec_sandbox_launcher` (direct spawn, markers cleared, depth stamped).
- [x] Tests: recognition, never-unwrapped, allowlist/argv rejection matrices, no false positives, gated-command assertions.
- [x] `cargo test --lib -- agent_wrapper exec_tests`, `cargo clippy --all-targets -- -D warnings`.
- [x] Manual e2e under `LEAN_CTX_SHELL_SECURITY=enforce`: real launcher runs, `eval`/`SHELLOPTS` variants exit 126.
