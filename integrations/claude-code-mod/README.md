# lean-ctx for Claude Code

A Claude Code mod (Community tier) that makes lean-ctx part of Claude Code itself
instead of something the model has to be talked into using. Requires Claude Code
**2.1.287+**; tested with **2.1.287**.

## Install

```sh
lean-ctx claude-mod install     # or answer "y" in `lean-ctx setup`
lean-ctx claude-mod status
lean-ctx claude-mod uninstall
```

The lean-ctx binary carries the mod and installs it from a local marketplace in
its data dir — nothing is downloaded, and the mod's version is the engine's
version, so every lean-ctx update rolls it forward (`lean-ctx setup`/`update`
refresh an existing install; they never install it on their own). It becomes
active in new Claude Code sessions, or after `/reload-plugins`.

## Why

Measured on 569 Claude Code sessions (30 days): **~19 % of all model requests
were waiting** — 7,551 status polls for 1,030 background jobs plus 1,770
`sleep` calls — and every extra request re-reads the whole context (median
149k tokens). The mod removes those requests instead of compressing around them.

## What it does

- **Wake, don't poll.** Watches `ctx_shell(run_in_background=true)` jobs in
  Claude Code's process (state from ctx_shell's `structuredContent`, then its
  JSON text, then the `[background:…]` header) and starts one new turn when they
  finish — including failed jobs, which MCP reports as error results — with the
  job id, exit status and recovery handle. Bare `sleep N` waits are answered
  while a job is watched. Verified live: the model started a job, ended its
  turn, and was woken by the mod with `Job … finished · exit 0` (no polls).
- **Shape, don't redirect.** Large native `Bash` stdout (≥ 2,000 chars) is
  compressed by lean-ctx's command-aware engine via the internal `ctx_shape`
  tool — the same patterns, secret redaction and output filters as `ctx_shell`,
  ending lossy results with a recovery handle. stderr, small output, images,
  background launches and commands with `LEAN_CTX_RAW=1` / `lean-ctx raw` stay
  byte-for-byte; any failure keeps the native result. Turn it off with the
  `shape_native_output` setting.
- **Focused tool surface.** Front-loads `ctx_read`, `ctx_search`, `ctx_shell`,
  `ctx_compose`, `ctx_callgraph`, `ctx_session` (setting `front_loaded_tools`)
  and defers every other lean-ctx tool — including the `shell` alias — behind
  ToolSearch, with descriptions byte-identical. Verified by capturing the real
  `/v1/messages` request: exactly the configured six keep their schema.
- **Live skill.** Prefixes the `lean-ctx` skill with what is true in this
  session (wake, front-loaded tools, shaping), so it never contradicts the mod.
- **`/leanctx`.** This session's requests, input/output/cache tokens,
  ToolSearch-only requests, lean-ctx calls, answered sleeps, wakes and shaped
  Bash outputs — from Claude Code's own `turn.step` usage, not estimates.

It keeps nothing after the session, sends no telemetry, sets no gateway policy
and stays inert when no lean-ctx MCP server is connected.

## Security model

A mod runs inside Claude Code with your permissions. This one only calls the
mods API methods the validator lists below; it reaches lean-ctx through Claude
Code's own MCP connection (`$.mcp.call`), never through a shell or the network.
`ctx_shape` is an internal host hook: callable, but never advertised to agents.

## Develop

The sources are canonical in `rust/src/templates/claude_mod/` (embedded in the
binary); this directory is the development workspace, regenerated with
`cargo run --example gen_rules --features dev-tools` and drift-checked in CI.
Edit the templates, regenerate, then:

```sh
claude --plugin-dir ./integrations/claude-code-mod      # one session, hot reload
claude plugin validate --strict integrations/claude-code-mod
claude plugin validate --strict integrations/claude-code-mod/.claude-plugin/plugin.json
claude plugin test integrations/claude-code-mod
tsc -p integrations/claude-code-mod/tsconfig.json
```

`tsc` needs the version-matched declarations in `.claude-plugin/types/`
(git-ignored), which Claude Code writes the first time it loads the mod with
`--plugin-dir`. The validator reports:

```text
  ❯ ./register.ts hooks: tool.describe{tool=/"^mcp__lean[-_]ctx__[A-Za-z0-9_-]+$"/}, skill.prompt{skill=lean-ctx}, session.start, tool.call, turn.step, command.run{command=leanctx}
  ❯ ./register.ts calls: $.clock.every (via startWatcher), $.command.register (via ensureMeterCommand), $.mcp.call (via pollWatchedJobs, shapeBash), $.prompt.submit (via submitWake)
```
