# Plan: gate and run Claude Code's OS-sandbox launcher

1. Capture the exact launcher that Claude Code 2.1.280 spawns as a test fixture. It includes the proxy/CA env, `GIT_SSH_COMMAND`, `GIT_CONFIG_KEY_n`/`VALUE_n` and the multi-line quoted Seatbelt profile.
2. In `agent_wrapper.rs`, add `os_sandbox_launcher_argv`. It splits the string into decoded words and accepts only `[env [-u NAME…] [NAME=value…]] /usr/bin/sandbox-exec -p <profile> <trusted shell> -c <script>`. Assignments must match the list of names and values Claude Code's launcher builder sets; everything else falls through.
3. In `exec()`, a recognised launcher first goes through `allowlist_gate` on its script, which is unwrapped when it is a Path A/B wrapper. It is then spawned by `exec_sandbox_launcher` with `clear_shell_default_markers` + `stamp_exec_depth`.
4. Tests:
   - recognition and verbatim decoding of the real launcher
   - the never-unwrapped guarantee
   - user `-u` and the login shell
   - rejection of env options, of assignments outside the allowlist, and of argv shapes
   - no false positives on ordinary commands
   - the gated command, with the wrapper unwrapped, a raw script gated whole, and `eval` blocked
5. Run the affected tests and the CI clippy invocation. Check end to end with the debug binary under `LEAN_CTX_SHELL_SECURITY=enforce`.
