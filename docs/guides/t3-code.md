# T3 Code + LeanCTX | OSS

> **Status: local Attach reference.** T3 Code is not a LeanCTX setup target of
> its own. It runs the coding agents you already have (Claude Code, Codex,
> OpenCode and others), and LeanCTX | OSS attaches to those agents. Checked
> against T3 Code 0.1.8 (upstream `pingdotgg/t3code` at `d756fed7aa`,
> 2026-11-10).

[T3 Code](https://github.com/pingdotgg/t3code) is a desktop, web and mobile
front end for coding agents. It does not ship its own harness: each thread runs
on a provider such as Claude Code (through the Claude Agent SDK) or Codex
(through the Codex App Server). Those harnesses load MCP servers from their
normal configuration, and T3 Code adds only its own `t3-code` MCP server.

That makes LeanCTX | OSS the context layer underneath without any change to
T3 Code: register the `lean-ctx` MCP server for the provider, and every T3
thread on that provider gets `ctx_read`, `ctx_search`, `ctx_tree` and the other
LeanCTX tools, with the same local controls and records as in the agent's own
terminal.

```text
T3 Code (UI, threads, remote access)
  └─ provider harness: Claude Code / Codex / OpenCode
       ├─ t3-code MCP server   (T3 Code's own tools)
       └─ lean-ctx MCP server  (LeanCTX | OSS: select, control, prove)
```

## Quick setup

On the machine that runs the T3 Code environment (the `t3` server, not the
phone or browser you control it from):

```bash
curl -fsSL https://leanctx.com/install.sh | sh
lean-ctx init --agent claude    # and/or: lean-ctx init --agent codex
lean-ctx doctor
```

`init` registers the MCP server in the agent's user configuration. T3 Code
threads on that provider pick it up the next time a session starts; use
**Refresh provider status** in T3 Code's provider settings if a thread was
already open.

## Sample configuration

The files in [`examples/t3-code/`](../../examples/t3-code/) are a project-level
setup you can copy into a repository, so everyone who opens the project in
T3 Code gets LeanCTX | OSS without per-user setup. `lean-ctx` must be on the
`PATH` of the T3 Code server.

### Claude provider

`.mcp.json` in the project root registers the server:

```json
{
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx",
      "args": []
    }
  }
}
```

`.claude/settings.json` approves that project server (headless sessions cannot
show Claude Code's trust prompt) and pre-approves the read-only LeanCTX tools:

```json
{
  "enabledMcpjsonServers": ["lean-ctx"],
  "permissions": {
    "allow": [
      "mcp__lean-ctx__ctx_read",
      "mcp__lean-ctx__ctx_search",
      "mcp__lean-ctx__ctx_tree",
      "mcp__lean-ctx__ctx_glob",
      "mcp__lean-ctx__ctx_compose"
    ]
  }
}
```

The allow list deliberately leaves out `ctx_shell`, `ctx_patch` and other tools
that run commands or change files: those keep following the thread's T3 Code
permission mode (Supervised asks, Full access does not).

If you already registered `lean-ctx` at user scope with `lean-ctx init`, the
project entry of the same name takes precedence and you do not get the tools
twice.

### Codex provider

Codex reads MCP servers from `config.toml` in its `CODEX_HOME`. T3 Code
overrides only its own `t3-code` entries, so a `lean-ctx` entry stays active:

```toml
[mcp_servers.lean-ctx]
command = "lean-ctx"
args = []
```

`lean-ctx init --agent codex` writes this to `~/.codex/config.toml`. A T3 Code
Codex instance with a separate **CODEX_HOME path** needs the section in that
home's `config.toml` too. Instances with a shadow home share the main home's
configuration and need nothing extra.

### Other providers

OpenCode, Cursor, Pi and the other providers also start their own harness with
its own configuration. Follow the matching guide ([OpenCode](opencode.md),
[Cursor](cursor.md), [Pi](pi.md)); T3 Code's provider notes say which
providers keep the user's MCP configuration. Google Antigravity runs on a
T3-managed profile that leaves the MCP servers in `~/.gemini` out, so a
user-level LeanCTX registration does not reach Antigravity threads.

## T3 Code settings that matter

- **Custom Claude config directory.** A Claude instance with its own config
  directory (`CLAUDE_CONFIG_DIR`) does not see user-scope servers from your
  normal `~/.claude.json`. Use the project `.mcp.json`, or run
  `CLAUDE_CONFIG_DIR=<that dir> lean-ctx init --agent claude`.
- **Read-only threads.** In a read-only sandbox T3 Code pre-approves only
  Claude Code's `Read`, `Glob` and `Grep`. The allow list above adds the
  LeanCTX read tools. If you use LeanCTX in *replace* mode (native reads
  denied), keep that allow list, or set `hook_mode = "hybrid"` for such
  projects, otherwise a read-only thread cannot read files.
- **Remote environments.** With `t3 up` or T3 Connect the agents run on the
  host, so install and configure LeanCTX | OSS there; the phone or browser
  needs nothing.

## Check that it works

From the project folder on the T3 Code host:

```bash
id=$(t3 thread start . "Call the ctx_tree tool of the lean-ctx MCP server on this folder and print its output.")
t3 thread send "$id" "Which MCP servers did you use?" --wait
```

The reply shows the `ctx_tree` map and names the `lean-ctx` server. In the
T3 Code app the call appears in the thread's activity as
`mcp__lean-ctx__ctx_tree`, and `lean-ctx gain --terminal` on the host counts
it.

## Troubleshooting

- **No `lean-ctx` tools in a Claude thread:** check `lean-ctx` is on the `PATH`
  of the T3 Code server process (a background service started with
  `t3 service install` may have a shorter `PATH`; use the absolute path from
  `command -v lean-ctx` in `.mcp.json`).
- **Project server ignored:** `enabledMcpjsonServers` is missing, or a
  `.claude/settings.local.json` disables it.
- **Codex thread without the tools:** compare the instance's **CODEX_HOME
  path** with the `config.toml` you edited.

## Further reading

- [Claude Code guide](claude-code.md) · [Codex CLI guide](codex-cli.md)
- T3 Code: [providers](https://github.com/pingdotgg/t3code/blob/main/docs/internals/providers.md),
  [Codex provider](https://github.com/pingdotgg/t3code/blob/main/docs/user/providers-codex.md),
  [Claude provider](https://github.com/pingdotgg/t3code/blob/main/docs/user/providers-claude.md)
