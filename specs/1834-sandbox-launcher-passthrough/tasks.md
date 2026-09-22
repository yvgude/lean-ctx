# Tasks: run Claude Code's OS-sandbox launcher verbatim

- [x] Fixture: real launcher string from Claude Code 2.1.275 (`LAUNCHER` in `agent_wrapper.rs` tests).
- [x] `decode_shell_word_at` + `split_simple_command` (operators/newlines/unterminated quotes → `None`).
- [x] `os_sandbox_launcher_argv` with `env`/launcher/`<shell> -c <script>` shape checks and `steers_shell_hook` deny list.
- [x] `exec()` dispatch to `exec_sandbox_launcher` (direct spawn, markers cleared, depth stamped).
- [x] Tests: recognition, never-unwrapped, rejection matrix, no false positives; `sandbox_launcher_command` argv/env assertions for the re-entry criterion.
- [x] Manual repro: 3.10.2 exits 126 on the launcher; patched build execs `sandbox-exec`.
- [ ] `cargo fmt --check`, CI clippy invocation, `scripts/preflight.sh fast`.
- [ ] Push and open a PR closing #1834.
