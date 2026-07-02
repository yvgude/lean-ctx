# pi-lean-ctx

[Pi Coding Agent](https://github.com/badlogic/pi-mono) extension that provides `ctx_`-prefixed tools backed by [lean-ctx](https://leanctx.com) for **60–90% token savings**.

- **Default**: embedded MCP bridge ON (persistent session cache → unchanged re-reads cost ~13 tokens), additive mode (Pi builtins preserved)
- **Opt out**: `LEAN_CTX_PI_ENABLE_MCP=0` (or `"enableMcp": false`) forces the one-shot CLI path, which cannot cache across calls
- **Optional**: replace mode (`LEAN_CTX_PI_MODE=replace`) disables Pi builtins

## Tool Mode

By default, pi-lean-ctx runs in **additive mode**: Pi's built-in tools (`read`, `bash`, `ls`, `find`, `grep`) remain available alongside the `ctx_*` tools. Agents can use either set.

To switch to **replace mode** (disables Pi builtins, only `ctx_*` tools available):

```bash
export LEAN_CTX_PI_MODE=replace
```

## Tool surface (lean / standard / power)

The embedded bridge advertises whatever tool surface it requests from lean-ctx —
by default the **lean core** (the essential tools) plus the `ctx_call` gateway,
exactly like a normal lean-ctx install. Every other tool, including the editors
`ctx_edit` / `ctx_patch`, stays reachable through `ctx_call`, and Pi's own native
`edit` / `write` builtins are available in every mode regardless of this setting.

To promote the **whole** lean-ctx registry (`ctx_edit`, `ctx_patch`, architecture
and quality tools, …) to first-class Pi tools, set `toolProfile`:

```bash
export LEAN_CTX_PI_TOOL_PROFILE=power   # or "standard" for the balanced 15-tool set
```

or in `config.json`: `"toolProfile": "power"`. Values: `lean` (default) ·
`standard` · `power` (`full`/`all` alias `power`). It maps to the engine's
`LEAN_CTX_TOOL_PROFILE`, so it mirrors `lean-ctx profile <name>` on a normal
install. `power` costs more prompt tokens (more tool schemas) — opt in when you
want the full surface in Pi. Check the active profile any time with `/lean-ctx`.

## Config file

If you only use lean-ctx through Pi, keep every setting in one file instead of
env vars — `~/.pi/agent/extensions/pi-lean-ctx/config.json`:

```json
{
  "mode": "replace",
  "enableMcp": true,
  "toolProfile": "power",
  "binary": "/opt/lean-ctx/bin/lean-ctx",
  "env": { "LEAN_CTX_COMPRESSION": "aggressive" }
}
```

`mode` → `LEAN_CTX_PI_MODE`, `enableMcp` → `LEAN_CTX_PI_ENABLE_MCP`,
`toolProfile` → `LEAN_CTX_PI_TOOL_PROFILE`, `binary` → `LEAN_CTX_BIN`,
`disableTools` → `LEAN_CTX_PI_DISABLE_TOOLS`,
`toolPrefix` → `LEAN_CTX_PI_TOOL_PREFIX` (see
[Coexisting with AFT and magic-context](#coexisting-with-aft-and-magic-context)).
The `env` map is forwarded to every `lean-ctx` subprocess, so it can override
`~/.lean-ctx/config.toml` engine settings. Explicit env vars still win over the
file; the file wins over defaults. The deny-list is the one exception — the env
and file lists are **merged**, since a deny-list is additive by intent.

## What it does

### ctx_ Tools (CLI-backed)

Adds `ctx_`-prefixed tools alongside Pi's builtins (or replaces them in `replace` mode):

| Tool | Replaces | Compression |
|------|----------|-------------|
| `ctx_read` | `read` | Smart mode selection (full/map/signatures) based on file type and size |
| `ctx_shell` | `bash` | All shell commands compressed via lean-ctx's 95+ patterns |
| `ctx_grep` | `grep` | Results grouped and compressed via ripgrep + lean-ctx |
| `ctx_find` | `find` | File listings compressed and .gitignore-aware |
| `ctx_ls` | `ls` | Directory output compressed |

Pi's `edit` and `write` builtins remain unchanged.

### Direct lean-ctx CLI tool

The `lean_ctx` tool runs `lean-ctx` directly (no nested compression).
Use it for commands like:

- `lean_ctx overview`
- `lean_ctx session …`
- `lean_ctx knowledge …`
- `lean_ctx gain` / `lean_ctx stats`
- `lean_ctx index …`

### Optional MCP Tools (Embedded Bridge)

By default, pi-lean-ctx does **not** start an MCP server. If enabled, it spawns `lean-ctx` as an MCP
server and registers advanced tools directly in Pi:

| Tool | Purpose |
|------|---------|
| `ctx_session` | Session state management and persistence |
| `ctx_knowledge` | Project knowledge graph with temporal validity |
| `ctx_semantic_search` | Find code by meaning, not exact text |
| `ctx_overview` | Codebase overview and architecture analysis |
| `ctx_compress` | Manual compression control |
| `ctx_metrics` | Token savings dashboard |
| `ctx_multi_read` | Batch file reads |
| `ctx_search` | MCP-native search |
| `ctx_tree` | File tree listing |

If you don't want MCP: keep it disabled and use the `ctx_` CLI tools + `lean_ctx` tool only.

## Install

```bash
# 1. Install lean-ctx (if not already installed)
cargo install lean-ctx
# or: brew tap yvgude/lean-ctx && brew install lean-ctx

# 2. Install the Pi package
pi install npm:pi-lean-ctx

# 3. Restart Pi
```

Or use the automated setup:

```bash
lean-ctx init --agent pi
```

The published package has **zero runtime npm dependencies**: the MCP SDK
(incl. zod) is shipped as a self-contained vendor bundle
(`extensions/vendor/mcp-sdk.cjs`). This makes the extension immune to pi's
shared npm prefix rewriting `node_modules` on every `pi install`/`pi remove`
(which previously corrupted zod's locale files — GH #670).

## How it works

### ctx_ tools (CLI-backed)

These tools invoke the `lean-ctx` binary via CLI with `LEAN_CTX_COMPRESS=1`.
The built-in tools they replace (`read`, `bash`, `ls`, `find`, `grep`) are disabled
via `pi.setActiveTools()` so only the `ctx_` versions are available to the LLM.

### Embedded MCP bridge (session cache + advanced tools)

On by default, pi-lean-ctx spawns the `lean-ctx` binary as an MCP server (JSON-RPC over stdio).
This persistent process holds the **session cache**: `ctx_read` (every mode, including line
ranges) is routed through the bridge, so an unchanged re-read costs ~13 tokens instead of the
full file and the read registers as a real CEP session (counted by `lean-ctx gain`). The bridge
also discovers the server's advertised tools (`ctx_overview`, `ctx_graph`, `ctx_session`, …),
filters out those already exposed as `ctx_` CLI tools, and registers the rest as native Pi tools.
By default that surface is the lean core + `ctx_call`; set `toolProfile: power` (see the
[Tool surface](#tool-surface-lean--standard--power) section) to also surface `ctx_edit` /
`ctx_patch` and the rest of the registry as first-class Pi tools.

The bridge wins over `~/.pi/agent/mcp.json`: a `lean-ctx` entry there (written by
`lean-ctx init --agent pi`) does **not** disable the embedded bridge, because Pi has no native
MCP support and that entry only does anything if you separately run
[pi-mcp-adapter](https://github.com/nicobailon/pi-mcp-adapter). `/lean-ctx` warns about possible
duplicates only when the adapter is genuinely running. If the bridge can't start, the CLI path
keeps working — only the cache and advanced tools are unavailable.

### Automatic reconnection

If the MCP server process crashes, the bridge automatically reconnects (up to 3 attempts with exponential backoff). If reconnection fails, CLI-based tools continue working normally — only the advanced MCP tools become unavailable.

## Disabling the bridge (optional)

The bridge is on by default. To force the one-shot CLI path (no cross-call cache),
set an environment variable and restart Pi:

```bash
export LEAN_CTX_PI_ENABLE_MCP=0
pi
```

…or set `"enableMcp": false` in `~/.pi/agent/extensions/pi-lean-ctx/config.json`.

## Verifying token savings

The session cache's headline claim — an **unchanged re-read costs ~13 tokens** —
is now a one-command, machine-checkable self-test (issue #361). No manual
transcript inspection required:

```bash
lean-ctx verify-cache
```

It reads a file twice through the real session cache and asserts the second read
collapses to a `[unchanged …]` stub:

```text
lean-ctx verify-cache

  Target:        src/main.rs
  Cache policy:  aggressive
  Read #1 (full):     3731 tokens
  Read #2 (re-read):  13 tokens  [unchanged stub]
  Re-read savings:    100%
  Cache hits (run):   1/2
  CEP sessions:       42 (88% cross-call hit ratio)

  PASS — session cache engaged: the unchanged re-read cost 13 tokens (≈13-token stub).
```

- Exit code `0` = cache proven, `1` = no stub (cache not engaging), `2` =
  stubbing disabled by config (e.g. `cache_policy = safe`). Add `--json` for CI.
- Pass an explicit path to probe a real file: `lean-ctx verify-cache src/app.ts`.
- `lean-ctx doctor` also prints a **Session cache** line (CEP sessions +
  cross-call hit ratio) so you can answer "is the cache engaging?" at a glance.

> On Pi specifically, the embedded MCP bridge (on by default) is what holds the
> cache across calls. If `verify-cache` fails, confirm the bridge is connected
> via `/lean-ctx`; the one-shot CLI path cannot cache across calls.

This check was added in response to the independent, pre-registered
[tokbench](https://github.com/Entelligentsia/tokbench) benchmark, where the
~13-token re-read previously had to be verified by hand.

## pi-mcp-adapter compatibility

If you prefer using [pi-mcp-adapter](https://github.com/nicobailon/pi-mcp-adapter) to manage your MCP servers, lean-ctx integrates automatically:

```bash
# Option A: lean-ctx writes the config for you
lean-ctx init --agent pi

# Option B: Manual configuration in ~/.pi/agent/mcp.json
```

```json
{
  "mcpServers": {
    "lean-ctx": {
      "command": "/path/to/lean-ctx",
      "lifecycle": "lazy",
      "directTools": true
    }
  }
}
```

When pi-mcp-adapter manages the lean-ctx MCP server, pi-lean-ctx detects this and only registers its CLI-based tool overrides, leaving MCP tool management to the adapter.

## Binary Resolution

The extension locates the `lean-ctx` binary in this order:

1. `LEAN_CTX_BIN` environment variable
2. `binary` in `~/.pi/agent/extensions/pi-lean-ctx/config.json`
3. `~/.cargo/bin/lean-ctx`
4. `~/.local/bin/lean-ctx` (Linux) or `%APPDATA%\Local\lean-ctx\lean-ctx.exe` (Windows)
5. `/usr/local/bin/lean-ctx` (macOS/Linux)
6. `lean-ctx` on PATH

## Smart Read Modes

The `ctx_read` tool automatically selects the optimal lean-ctx mode:

| File Type | Size | Mode |
|-----------|------|------|
| `.md`, `.json`, `.toml`, `.yaml`, etc. | Any | `full` |
| Code files (55+ extensions) | < 8 KB | `full` |
| Code files | 8–96 KB | `map` (deps + API signatures) |
| Code files | > 96 KB | `signatures` (AST extraction) |
| Other files | < 48 KB | `full` |
| Other files | > 48 KB | `map` |

## Slash Command

Use `/lean-ctx` in Pi to check:
- Which binary is being used
- MCP bridge status (disabled / embedded / adapter)
- Active `ctx_` tool names
- Coexistence info (#359): active tool prefix, tools handed to other extensions
  (`Disabled`), and tools skipped due to a name already taken (`Skipped`)

## Disabling specific tools

To disable specific MCP tools, configure `disabled_tools` in `~/.lean-ctx/config.toml`:

```toml
disabled_tools = ["ctx_graph", "ctx_benchmark"]
```

Or via environment variable:

```bash
LEAN_CTX_DISABLED_TOOLS=ctx_graph,ctx_benchmark pi
```

## Coexisting with AFT and magic-context

pi-lean-ctx is built to **stack** with other Pi extensions such as
[AFT](https://github.com/cortexkit/aft) and
[magic-context](https://github.com/cortexkit/magic-context) (issue #359).

**No more load crashes.** If another extension already registered a tool name
(e.g. magic-context's `ctx_expand`), pi-lean-ctx now **skips that tool with a
warning** instead of crashing the whole agent. The rest of lean-ctx keeps
working. Run `/lean-ctx` to see exactly which tools were skipped.

### Hand tool names to another extension

Use a deny-list so the other extension owns shared names while lean-ctx keeps
its compression + session-cache core (`ctx_read`, `ctx_shell`, …):

```bash
# env: comma/space separated, case-insensitive
export LEAN_CTX_PI_DISABLE_TOOLS="ctx_memory,ctx_expand,ctx_search"
```

…or in `~/.pi/agent/extensions/pi-lean-ctx/config.json` (merged with the env list):

```json
{
  "disableTools": ["ctx_memory", "ctx_expand", "ctx_search"]
}
```

> This is the **Pi-extension** deny-list — it controls which tools lean-ctx
> registers *in Pi* (including its own `ctx_*` tools like `ctx_grep`). It is
> separate from the engine-level `disabled_tools` / `LEAN_CTX_DISABLED_TOOLS`,
> which hides tools from the MCP server itself.

### Or namespace them with a prefix

Keep every tool but expose the bridge tools under your own prefix, so nothing
collides and small models see no duplicate names:

```bash
export LEAN_CTX_PI_TOOL_PREFIX="lc_"   # ctx_expand → lc_ctx_expand
```

The signature tools (`ctx_read`, `ctx_shell`, `ctx_ls`, `ctx_find`, `ctx_grep`)
keep their stable names; only the bridge-discovered MCP tools are prefixed.

### Curated profile (recommended division of labor)

| Concern | Owner | Why |
|---------|-------|-----|
| File reads, shell, grep/find/ls — **compression + session cache** | **lean-ctx** | ~13-token re-reads, 60–90% savings on every read/shell |
| **Long-horizon memory** (`ctx_memory`, `ctx_expand`) | magic-context | purpose-built long-term memory |
| **Symbol-aware file ops** (`aft_*`) | AFT | precise AST edits |

Copy-paste config for the profile above
(`~/.pi/agent/extensions/pi-lean-ctx/config.json`):

```json
{
  "mode": "additive",
  "enableMcp": true,
  "disableTools": ["ctx_memory", "ctx_expand", "ctx_search"]
}
```

Result: no duplicate search/memory tools in the tool list, no load crash, and
each extension does what it is best at. Verify with `/lean-ctx`, which now lists
the active prefix plus any handed-off (`Disabled`) and skipped tools.

## Links

- [lean-ctx](https://leanctx.com) — the Cognitive Context Layer for AI coding agents
- [GitHub](https://github.com/yvgude/lean-ctx)
- [Discord](https://discord.gg/pTHkG9Hew9)
