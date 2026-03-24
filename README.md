# lean-ctx

**Hybrid Context Optimizer with Token Dense Dialect (TDD). Shell Hook + MCP Server. Single Rust binary, zero dependencies.**

[![Crates.io](https://img.shields.io/crates/v/lean-ctx)](https://crates.io/crates/lean-ctx)
[![Downloads](https://img.shields.io/crates/d/lean-ctx)](https://crates.io/crates/lean-ctx)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](rust/LICENSE)
[![Discord](https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white)](https://discord.gg/pTHkG9Hew9)

[Website](https://leanctx.com) · [Install](#installation) · [Quick Start](#quick-start) · [CLI Reference](#cli-commands) · [MCP Tools](#8-mcp-tools) · [TDD](#token-dense-dialect-tdd) · [Dashboard](#persistent-stats--web-dashboard) · [Editor Setup](#editor-configuration) · [vs RTK](#lean-ctx-vs-rtk) · [Discord](https://discord.gg/pTHkG9Hew9)

---

lean-ctx reduces LLM token consumption by **89-99%** through two complementary strategies in a single binary:

1. **Shell Hook** — Transparently compresses CLI output before it reaches the LLM. Works without LLM cooperation.
2. **MCP Server** — 8 tools for cached file reads, dependency maps, entropy analysis, and session metrics. Works with Cursor, GitHub Copilot, Claude Code, Windsurf, OpenCode, and any MCP-compatible editor. Shell hook also benefits OpenClaw via transparent compression.

## Token Savings (Typical Cursor/Claude Code Session)

| Operation | Frequency | Standard | lean-ctx | Savings |
|---|---|---|---|---|
| File reads (cached) | 15x | 30,000 | 195 | **-99%** |
| File reads (map mode) | 10x | 20,000 | 2,000 | **-90%** |
| ls / find | 8x | 6,400 | 1,280 | **-80%** |
| git status/log/diff | 10x | 8,000 | 2,400 | **-70%** |
| grep / rg | 5x | 8,000 | 2,400 | **-70%** |
| cargo/npm build | 5x | 5,000 | 1,000 | **-80%** |
| Test runners | 4x | 10,000 | 1,000 | **-90%** |
| curl (JSON) | 3x | 1,500 | 165 | **-89%** |
| docker ps/build | 3x | 900 | 180 | **-80%** |
| **Total** | | **~89,800** | **~10,620** | **-88%** |

> Estimates based on medium-sized TypeScript/Rust projects. MCP cache hits reduce re-reads to ~13 tokens each.

## Installation

### Homebrew (macOS / Linux)

```bash
brew tap yvgude/lean-ctx
brew install lean-ctx
```

### Cargo

```bash
cargo install lean-ctx
```

### Build from Source

```bash
git clone https://github.com/yvgude/lean-ctx.git
cd lean-ctx/rust
cargo build --release
cp target/release/lean-ctx ~/.local/bin/
```

> Add `~/.local/bin` to your PATH if needed:
> ```bash
> echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc  # or ~/.bashrc
> ```

### Verify Installation

```bash
lean-ctx --version   # Should show "lean-ctx 1.4.1"
lean-ctx gain        # Should show token savings stats
```

## Token Dense Dialect (TDD)

lean-ctx introduces **TDD mode** — enabled by default. TDD compresses LLM communication using mathematical symbols and short identifiers:

| Symbol | Meaning |
|---|---|
| `λ` | function/handler |
| `§` | struct/class/module |
| `∂` | interface/trait |
| `τ` | type alias |
| `ε` | enum |
| `α1, α2...` | short identifier IDs |

**How it works:**
- Signatures use compact notation: `λ+handle(⊕,path:s)→s` instead of `fn pub async handle(&self, path: String) -> String`
- Long identifiers (>12 chars) are mapped to `α1, α2...` with a `§MAP` at the end
- MCP instructions tell the LLM to respond in Token Dense Dialect — shorter responses, less thinking tokens

**Result**: 8-25% additional savings on top of existing compression.

Configure with `LEAN_CTX_CRP_MODE`:
- `tdd` (default) — Maximum compression with symbol shorthand
- `compact` — Moderate: skip filler words, use abbreviations
- `off` — Standard output, no CRP instructions

## Quick Start

```bash
# 1. Install
cargo install lean-ctx

# 2. Set up shell hook (auto-installs aliases)
lean-ctx init --global

# 3. Configure your editor (example: Cursor)
# Add to ~/.cursor/mcp.json:
# { "mcpServers": { "lean-ctx": { "command": "lean-ctx" } } }

# 4. Restart your shell + editor, then test
git status       # Automatically compressed via shell hook
lean-ctx gain    # Check your savings
```

The shell hook transparently wraps commands (e.g., `git status` → `lean-ctx -c git status`) and compresses the output. The LLM never sees the rewrite — it just gets compact output.

## How It Works

```
  Without lean-ctx:                              With lean-ctx:

  LLM --"read auth.ts"--> Editor --> File        LLM --"ctx_read auth.ts"--> lean-ctx --> File
    ^                                  |           ^                           |            |
    |      ~2,000 tokens (full file)   |           |   ~13 tokens (cached)     | cache+hash |
    +----------------------------------+           +------ (compressed) -------+------------+

  LLM --"git status"-->  Shell  -->  git         LLM --"git status"-->  lean-ctx  -->  git
    ^                                 |            ^                       |              |
    |     ~800 tokens (raw output)    |            |   ~150 tokens         | compress     |
    +---------------------------------+            +------ (filtered) -----+--------------+
```

Four strategies applied per command type:

1. **Smart Filtering** — Removes noise (progress bars, ANSI codes, whitespace, boilerplate)
2. **Grouping** — Aggregates similar items (files by directory, errors by type)
3. **Truncation** — Keeps relevant context, cuts redundancy
4. **Deduplication** — Collapses repeated log lines with counts

## CLI Commands

### Shell Hook

```bash
lean-ctx -c "git status"       # Execute + compress output
lean-ctx exec "cargo build"    # Same as -c
lean-ctx shell                 # Interactive REPL with compression
```

### File Operations

```bash
lean-ctx read file.rs                    # Full content (with structured header)
lean-ctx read file.rs -m map             # Dependency graph + API signatures (~10% tokens)
lean-ctx read file.rs -m signatures      # Function/class signatures only (~15% tokens)
lean-ctx read file.rs -m aggressive      # Syntax-stripped content (~40% tokens)
lean-ctx read file.rs -m entropy         # Shannon entropy filtered (~30% tokens)
lean-ctx diff file1.rs file2.rs          # Compressed file diff
lean-ctx grep "pattern" src/             # Grouped search results
lean-ctx find "*.rs" src/                # Compact find results
lean-ctx ls src/                         # Token-optimized directory listing
lean-ctx deps .                          # Project dependencies summary
```

### Setup & Analytics

```bash
lean-ctx init --global         # Install 23 shell aliases (.zshrc/.bashrc/.config/fish)
lean-ctx gain                  # Persistent token savings (CLI)
lean-ctx dashboard             # Web dashboard at localhost:3333
lean-ctx dashboard --port=8080 # Custom port
lean-ctx discover              # Find uncompressed commands in shell history
lean-ctx session               # Show adoption statistics
lean-ctx config                # Show configuration (~/.lean-ctx/config.toml)
lean-ctx config init           # Create default config file
lean-ctx --version             # Show version
lean-ctx --help                # Full help
```

### MCP Server

```bash
lean-ctx                       # Start MCP server (stdio) — used by editors
```

## Shell Hook Patterns (50+)

The shell hook applies pattern-based compression for 50+ commands across 12 categories:

| Category | Commands | Savings |
|---|---|---|
| **Git** (19) | status, log, diff, add, commit, push, pull, fetch, clone, branch, checkout, switch, merge, stash, tag, reset, remote, blame, cherry-pick | -70-95% |
| **Docker** (10) | build, ps, images, logs, compose ps/up/down, exec, network, volume, inspect | -70-90% |
| **npm/pnpm/yarn** (6) | install, test, run, list, outdated, audit | -70-90% |
| **Cargo** (3) | build, test, clippy | -80% |
| **GitHub CLI** (9) | pr list/view/create/merge, issue list/view/create, run list/view | -60-80% |
| **Kubernetes** (8) | get pods/services/deployments, logs, describe, apply, delete, exec, top, rollout | -60-85% |
| **Python** (7) | pip install/list/outdated/uninstall/check, ruff check/format | -60-80% |
| **Linters** (4) | eslint, biome, prettier, stylelint | -60-70% |
| **Build Tools** (3) | tsc, next build, vite build | -60-80% |
| **Test Runners** (6) | jest, pytest, go test, playwright, cypress, rspec | -90% |
| **Utils** (5) | curl, grep/rg, find, ls, wget | -50-89% |
| **Data** (3) | env (filtered), JSON schema extraction, log deduplication | -50-80% |

Unrecognized commands get generic compression: ANSI stripping, empty line removal, and long output truncation.

### 23 Auto-Rewritten Aliases

After `lean-ctx init --global`, these commands are transparently compressed:

```
git, npm, pnpm, yarn, cargo, docker, docker-compose, kubectl, k,
gh, pip, pip3, ruff, go, golangci-lint, eslint, prettier, tsc,
ls, find, grep, curl, wget
```

Commands already using `lean-ctx` pass through unchanged.

## Examples

**Directory listing:**

```
# ls -la src/ (22 lines, ~239 tokens)      # lean-ctx -c "ls -la src/" (8 lines, ~46 tokens)
total 96                                     core/
drwxr-xr-x  4 user staff  128 ...           tools/
drwxr-xr-x  11 user staff 352 ...           cli.rs  9.0K
-rw-r--r--  1 user staff  9182 ...           main.rs  4.0K
-rw-r--r--  1 user staff  4096 ...           server.rs  11.9K
...                                          shell.rs  5.2K
                                             4 files, 2 dirs
                                             [lean-ctx: 239→46 tok, -81%]
```

**File reading (map mode):**

```
# Full read (284 lines, ~2078 tokens)       # lean-ctx read stats.rs -m map (~30 tokens)
use serde::{Deserialize, Serialize};         stats.rs [284L]
use std::collections::HashMap;                 deps: serde::
use std::path::PathBuf;                        exports: StatsStore, load, save, record, format_gain
                                               API:
#[derive(Serialize, Deserialize)]                cl ⊛ StatsStore
pub struct StatsStore {                          fn ⊛ load() → StatsStore
    pub total_commands: u64,                     fn ⊛ save(store:&StatsStore)
    pub total_input_tokens: u64,                 fn ⊛ record(command:s, input_tokens:n, output_tokens:n)
    ...                                          fn ⊛ format_gain() → String
(284 more lines)                             [2078 tok saved (100%)]
```

**curl (JSON):**

```
# curl -s httpbin.org/json (428 bytes)       # lean-ctx -c "curl -s httpbin.org/json"
{                                            JSON (428 bytes):
  "slideshow": {                             {
    "author": "Yours Truly",                   slideshow: {4K}
    "date": "date of publication",           }
    "slides": [                              [lean-ctx: 127→14 tok, -89%]
      {
        "title": "Wake up to WonderWidgets!",
        "type": "all"
      },
      ...
```

**Token savings dashboard:**

```
$ lean-ctx gain
lean-ctx Token Savings
══════════════════════════════════════════════════
Total commands:  47
Input tokens:    12.4K
Output tokens:   4.8K
Tokens saved:    7.6K (61.3%)
Tracking since:  2026-03-23

By Command:
──────────────────────────────────────────────────
Command               Count      Saved   Avg%
ls                       12      2.1K  74.1%
curl                      8      1.4K  89.2%
find                      9      1.2K  51.3%
git status                8        680  39.9%
cargo build               5        340  28.0%
grep                      5        180  12.3%

Recent Days:
──────────────────────────────────────────────────
Date          Cmds      Saved   Avg%
2026-03-23      47      7.6K  61.3%
══════════════════════════════════════════════════
```

## 8 MCP Tools

When configured as an MCP server, lean-ctx provides 8 tools that replace or augment your editor's built-in tools:

| Tool | Replaces | Savings |
|---|---|---|
| `ctx_read` | File reads — 6 modes: full, map, signatures, diff, aggressive, entropy | 74-99% |
| `ctx_tree` | Directory listings (ls, find, Glob) | 34-60% |
| `ctx_shell` | Shell commands | 60-90% |
| `ctx_search` | Code search (Grep) | 50-80% |
| `ctx_compress` | Context checkpoint for long conversations | 90-99% |
| `ctx_benchmark` | Compare all compression strategies with tiktoken counts | — |
| `ctx_metrics` | Session statistics with USD cost estimates | — |
| `ctx_analyze` | Shannon entropy analysis + mode recommendation | — |

### ctx_read Modes

| Mode | When to use | Token cost |
|---|---|---|
| `full` | Files you will edit (cached re-reads = ~13 tokens) | 100% first read, ~0% cached |
| `map` | Understanding a file without reading it — dependency graph + exports + API | ~5-15% |
| `signatures` | API surface with more detail than map | ~10-20% |
| `diff` | Re-reading files that changed | only changed lines |
| `aggressive` | Large files with boilerplate | ~30-50% |
| `entropy` | Files with repetitive patterns (Shannon + Jaccard filtering) | ~20-40% |

## Editor Configuration

### Cursor

Add to `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx"
    }
  }
}
```

### GitHub Copilot

Add `.github/copilot/mcp.json` to your project:

```json
{
  "servers": {
    "lean-ctx": {
      "command": "lean-ctx"
    }
  }
}
```

### Claude Code

```bash
claude mcp add lean-ctx lean-ctx
```

### Windsurf

Add to `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx"
    }
  }
}
```

### OpenCode

Add to `~/.config/opencode/opencode.json` (global) or `opencode.json` (project):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "lean-ctx": {
      "type": "local",
      "command": ["lean-ctx"],
      "enabled": true
    }
  }
}
```

### OpenClaw

OpenClaw uses a skills-based system instead of MCP. LeanCTX integrates via the **shell hook** — all commands OpenClaw runs through its `exec` tool are automatically compressed when the lean-ctx aliases are active.

```bash
# 1. Install shell aliases (if not done already)
lean-ctx init --global
source ~/.zshrc

# 2. (Optional) Install the LeanCTX skill for deeper integration
mkdir -p ~/.openclaw/skills/lean-ctx
cp skills/lean-ctx/SKILL.md ~/.openclaw/skills/lean-ctx/
```

The skill teaches OpenClaw to prefer `lean-ctx -c <command>` for shell operations, use compressed file reads, and leverage the dashboard for analytics.

### Cursor Terminal Profile

Add a lean-ctx terminal profile for automatic shell hook in Cursor:

```json
{
  "terminal.integrated.profiles.osx": {
    "lean-ctx": {
      "path": "lean-ctx",
      "args": ["shell"],
      "icon": "terminal"
    }
  }
}
```

### Cursor Rule (Optional)

For maximum token savings, add a Cursor rule to your project:

```bash
cp examples/lean-ctx.mdc .cursor/rules/lean-ctx.mdc
```

This instructs the LLM to prefer lean-ctx tools and use compact output patterns (CRP v2).

## Configuration

### Shell Hook Setup

```bash
lean-ctx init --global
```

This adds 23 aliases (git, npm, pnpm, yarn, cargo, docker, kubectl, gh, pip, ruff, go, golangci-lint, eslint, prettier, tsc, ls, find, grep, curl, wget, and more) to your `.zshrc` / `.bashrc` / `config.fish`.

Or add manually to your shell profile:

```bash
alias git='lean-ctx -c git'
alias npm='lean-ctx -c npm'
alias pnpm='lean-ctx -c pnpm'
alias cargo='lean-ctx -c cargo'
alias docker='lean-ctx -c docker'
alias kubectl='lean-ctx -c kubectl'
alias gh='lean-ctx -c gh'
alias pip='lean-ctx -c pip'
alias curl='lean-ctx -c curl'
# ... and 14 more (run lean-ctx init --global for all)
```

Or use the interactive shell:

```bash
lean-ctx shell
```

## Persistent Stats & Web Dashboard

lean-ctx tracks all compressions (both MCP tools and shell hook) in `~/.lean-ctx/stats.json`:

- Per-command breakdown with token counts
- MCP vs Shell Hook separation
- Daily statistics (last 90 days)
- Total lifetime savings
- First/last use timestamps

View in the terminal with `lean-ctx gain`, or open the web dashboard:

```bash
lean-ctx dashboard
```

Opens `http://localhost:3333` with:
- 5 KPI cards (tokens saved, savings rate, commands, days active, cost saved)
- 5 interactive charts (cumulative savings, daily rate, activity, top commands, distribution)
- MCP vs Shell Hook breakdown
- Command table with compression bars
- Daily history

## lean-ctx vs RTK

| Feature | RTK | lean-ctx |
|---|---|---|
| **Architecture** | Shell hook only | **Hybrid: Shell hook + MCP server** |
| **Language** | Rust | Rust |
| **CLI compression** | ~30 commands | **50+ patterns** (git, npm, cargo, docker, gh, kubectl, pip, ruff, eslint, prettier, tsc, go, playwright, curl, wget, JSON, logs...) |
| **File reading** | `rtk read` (signatures mode) | **6 modes: full (cached), map, signatures, diff, aggressive, entropy** |
| **File caching** | ✗ | ✓ MD5 session cache (re-reads = ~13 tokens) |
| **Dependency maps** | ✗ | ✓ import/export extraction (TS/JS/Rust/Python/Go) |
| **Context checkpoints** | ✗ | ✓ `ctx_compress` for long conversations |
| **Token counting** | Estimated | tiktoken-exact (o200k_base) |
| **Entropy analysis** | ✗ | ✓ Shannon entropy + Jaccard similarity |
| **Cost tracking** | ✗ | ✓ USD estimates per session |
| **Token Dense Dialect** | ✗ | ✓ TDD mode: symbol shorthand (λ, §, ∂) + identifier mapping (8-25% extra) |
| **Thinking reduction** | ✗ | ✓ CRP v2 (30-60% fewer thinking tokens via Cursor Rules) |
| **Persistent stats** | ✓ `rtk gain` | ✓ `lean-ctx gain` + web dashboard |
| **Auto-setup** | ✓ `rtk init` | ✓ `lean-ctx init` |
| **Editors** | Claude Code, OpenCode, Gemini CLI | **All MCP editors (Cursor, Copilot, Claude Code, Windsurf, OpenCode) + shell hook (OpenClaw, any terminal)** |
| **Config file** | TOML | ✓ TOML (`~/.lean-ctx/config.toml`) |
| **History analysis** | ✗ | ✓ `lean-ctx discover` — find uncompressed commands |
| **Homebrew** | ✓ | ✓ `brew tap yvgude/lean-ctx && brew install lean-ctx` |
| **Adoption tracking** | ✗ | ✓ `lean-ctx session` — adoption % |

**Key difference**: RTK compresses CLI output only. lean-ctx compresses CLI output *and* file reads, search results, and project context through the MCP protocol — reaching 89-99% savings where RTK reaches 60-90%.

## Uninstall

```bash
# Remove shell aliases
lean-ctx init --global  # re-run to see what was added, then remove from shell profile

# Remove binary (choose one)
brew uninstall lean-ctx       # if installed via Homebrew
cargo uninstall lean-ctx      # if installed via cargo

# Remove stats and config
rm -rf ~/.lean-ctx
```

## Contributing

Contributions welcome! Please open an issue or PR on [GitHub](https://github.com/yvgude/lean-ctx).

- [Discord](https://discord.gg/pTHkG9Hew9)
- [Buy me a coffee](https://buymeacoffee.com/yvgude)

## License

MIT License — see [LICENSE](rust/LICENSE) for details.
