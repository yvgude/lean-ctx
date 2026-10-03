# lean-ctx for Claude Code

A Community tier Claude Code mod for lean-ctx. It shapes the lean-ctx tool surface, wakes the model when watched shell jobs finish, and shows session-only request usage.

Claude Code **2.1.287+** is required; this MVP was tested with **2.1.287**.

## Why

The measured corpus recorded **7,551 status polls** for **1,030 background jobs**. The mod checks watched jobs inside Claude Code and submits one wake prompt when jobs finish.

## What it does

- Keeps descriptions byte-identical and front-loads `ctx_read`, `ctx_search`, `ctx_shell`, `ctx_compose`, `ctx_callgraph`, and `ctx_session` by default; other lean-ctx tools stay deferred. Configure the list with the plugin's `front_loaded_tools` setting. This refines the server-wide `alwaysLoad: true` that `lean-ctx setup` writes for Claude Code: without the mod every lean-ctx tool is front-loaded, with it only the configured ones.
- Watches `ctx_shell(run_in_background=true)` jobs every two seconds, reading the job state from ctx_shell's `structuredContent` (falling back to its JSON text, then to the `[background:…]` header). A finished job — including a failed one, which MCP reports as an error result — produces one deterministic wake with its ID, exit status, and the recovery handle, summary, or up to 20 output lines.
- Answers bare `sleep N` waits, and the supported `sleep N && …status/tail…` form, only while a job is watched. Other Bash calls pass through unchanged.
- Adds a stable reminder to watched job results so the model can continue other work instead of polling.
- Registers `/leanctx` when lean-ctx MCP tools are present. It reports session request and token totals, ToolSearch-only requests, lean-ctx calls, answered sleeps, and delivered wakes.
- Keeps no usage data after the session and sends no telemetry. It does not configure gateway policy, upload data, or create long-term proof.
- Stays inert when no lean-ctx MCP tools are available. A status check that cannot be read keeps the watch; after five consecutive misses the job is handed back to the model's own polling. Normal tool calls always proceed.

## Try it for one session

From the repository root:

```sh
claude --plugin-dir ./integrations/claude-code-mod
```

## Install from the local marketplace

From the repository root:

```sh
claude plugin marketplace add ./integrations/claude-code-mod
claude plugin install lean-ctx@lean-ctx-local
```

Disable or uninstall the mod from the Installed tab in `/plugin`.

## Validate and test

Run the strict marketplace check, plugin validation, tests, and TypeScript check from the repository root. `tsc` needs the version-matched declarations in `.claude-plugin/types/` (git-ignored), which Claude Code writes the first time it loads the mod with `--plugin-dir`:

```sh
claude plugin validate --strict integrations/claude-code-mod
claude plugin validate --strict integrations/claude-code-mod/.claude-plugin/plugin.json
claude plugin test integrations/claude-code-mod
tsc -p integrations/claude-code-mod/tsconfig.json
```

The plugin validator reports these hooks and calls:

```text
  ❯ ./register.ts hooks: tool.describe{tool=/"^mcp__lean[-_]ctx__ctx_[A-Za-z0-9_-]+$"/}, session.start, tool.call, turn.step, command.run{command=leanctx}
  ❯ ./register.ts calls: $.clock.every (via startWatcher), $.command.register (via ensureMeterCommand), $.mcp.call (via pollWatchedJobs), $.prompt.submit (via submitWake)
```
