```
  ██╗     ███████╗ █████╗ ███╗   ██╗     ██████╗████████╗██╗  ██╗
  ██║     ██╔════╝██╔══██╗████╗  ██║    ██╔════╝╚══██╔══╝╚██╗██╔╝
  ██║     █████╗  ███████║██╔██╗ ██║    ██║        ██║    ╚███╔╝ 
  ██║     ██╔══╝  ██╔══██║██║╚██╗██║    ██║        ██║    ██╔██╗ 
  ███████╗███████╗██║  ██║██║ ╚████║    ╚██████╗   ██║   ██╔╝ ██╗
  ╚══════╝╚══════╝╚═╝  ╚═╝╚═╝  ╚═══╝     ╚═════╝   ╚═╝   ╚═╝  ╚═╝
             Context Runtime for AI Agents
```

<h3 align="center">Reduce Claude Code, Cursor & Copilot Token Costs by 99% — Open Source MCP Server</h3>

<p align="center">
  <strong>Shell Hook + Context Server · 46 tools · 10 read modes · 90+ patterns · Single Rust binary</strong>
</p>

<p align="center">
  <a href="https://github.com/yvgude/lean-ctx/actions/workflows/ci.yml"><img src="https://github.com/yvgude/lean-ctx/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/yvgude/lean-ctx/actions/workflows/security-check.yml"><img src="https://github.com/yvgude/lean-ctx/actions/workflows/security-check.yml/badge.svg" alt="Security"></a>
  <a href="https://crates.io/crates/lean-ctx"><img src="https://img.shields.io/crates/v/lean-ctx?color=%23e6522c" alt="crates.io"></a>
  <a href="https://crates.io/crates/lean-ctx"><img src="https://img.shields.io/crates/d/lean-ctx?color=%23e6522c" alt="Downloads"></a>
  <a href="https://www.npmjs.com/package/lean-ctx-bin"><img src="https://img.shields.io/npm/v/lean-ctx-bin?label=npm&color=%23cb3837" alt="npm"></a>
  <a href="https://www.npmjs.com/package/pi-lean-ctx"><img src="https://img.shields.io/npm/v/pi-lean-ctx?label=pi-lean-ctx&color=%23cb3837" alt="pi-lean-ctx"></a>
  <a href="https://aur.archlinux.org/packages/lean-ctx"><img src="https://img.shields.io/aur/version/lean-ctx?color=%231793d1" alt="AUR"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="License"></a>
  <a href="https://discord.gg/pTHkG9Hew9"><img src="https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
  <a href="https://x.com/leanctx"><img src="https://img.shields.io/badge/𝕏-Follow-000000?logo=x&logoColor=white" alt="X/Twitter"></a>
  <img src="https://img.shields.io/badge/Telemetry-Opt--in%20Only-brightgreen?logo=shield&logoColor=white" alt="Opt-in Telemetry">
</p>

<p align="center">
  <a href="https://leanctx.com">Website</a> ·
  <a href="#-get-started-60-seconds">Install</a> ·
  <a href="#-how-lean-ctx-reduces-ai-token-costs">How It Works</a> ·
  <a href="#-46-intelligent-tools">Tools</a> ·
  <a href="#-shell-hook-patterns-90">Patterns</a> ·
  <a href="CHANGELOG.md">Changelog</a> ·
  <a href="https://discord.gg/pTHkG9Hew9">Discord</a>
</p>

---

<br>

> **lean-ctx** reduces LLM token consumption by **up to 99%** through three complementary strategies in a single binary — making AI coding faster, cheaper, and more effective.

<br>

## ⚡ How lean-ctx Reduces AI Token Costs

```
  Without lean-ctx:                              With lean-ctx:

  LLM ──"read auth.ts"──▶ Editor ──▶ File       LLM ──"ctx_read auth.ts"──▶ lean-ctx ──▶ File
    ▲                                  │           ▲                           │            │
    │      ~2,000 tokens (full file)   │           │   ~13 tokens (cached)     │ cache+hash │
    └──────────────────────────────────┘           └────── (compressed) ───────┴────────────┘

  LLM ──"git status"──▶  Shell  ──▶  git        LLM ──"git status"──▶  lean-ctx  ──▶  git
    ▲                                 │            ▲                       │              │
    │     ~800 tokens (raw output)    │            │   ~150 tokens         │ compress     │
    └─────────────────────────────────┘            └────── (filtered) ─────┴──────────────┘
```

| Strategy | How | Impact |
|:---|:---|:---|
| **Shell Hook** | Transparently compresses CLI output (90+ patterns) before it reaches the LLM | **60-95%** savings |
| **Context Server** | 46 MCP tools for cached reads, 10 read modes, deltas, dedup, memory, multi-agent sharing, adaptive compression | **74-99%** savings |
| **AI Tool Hooks** | One-command integration via `lean-ctx init --agent <tool>` | Works everywhere |

<br>

## 🎯 Token Savings — Real Numbers

| Operation | Freq | Without | With lean-ctx | Saved |
|:---|:---:|---:|---:|:---:|
| File reads (cached) | 15× | 30,000 | 195 | **99%** |
| File reads (map mode) | 10× | 20,000 | 2,000 | **90%** |
| ls / find | 8× | 6,400 | 1,280 | **80%** |
| git status/log/diff | 10× | 8,000 | 2,400 | **70%** |
| grep / rg | 5× | 8,000 | 2,400 | **70%** |
| cargo/npm build | 5× | 5,000 | 1,000 | **80%** |
| Test runners | 4× | 10,000 | 1,000 | **90%** |
| curl (JSON) | 3× | 1,500 | 165 | **89%** |
| docker ps/build | 3× | 900 | 180 | **80%** |
| **Session total** | | **~89,800** | **~10,620** | **88%** |

> Based on typical Cursor/Claude Code sessions with medium TypeScript/Rust projects. Cached re-reads cost ~13 tokens.

<br>

### Why lean-ctx?

AI coding tools like **Cursor**, **Claude Code**, **GitHub Copilot**, **Windsurf**, and **Gemini CLI** send massive amounts of context to LLMs with every request. Each file read, git command, or directory listing consumes tokens — and tokens cost money. lean-ctx sits transparently between your AI tool and the LLM, compressing everything before it reaches the model. No workflow changes needed. Install once, save on every API call.

**Common questions lean-ctx solves:**
- "How do I reduce Claude Code token costs?" → Shell hook + MCP server compress all context automatically
- "Why is Cursor using so many tokens?" → Uncompressed file reads and CLI output waste 60-99% of tokens  
- "How to optimize MCP server performance?" → lean-ctx IS an optimized MCP server with 46 intelligent tools
- "Best way to reduce AI coding API costs?" → Context compression saves $30-100+/month for active developers

<br>

## 🚀 Get Started (60 seconds)

```bash
# 1. Install (pick one)
curl -fsSL https://leanctx.com/install.sh | sh      # universal (sh or bash), no Rust needed
# or: curl -fsSL https://leanctx.com/install.sh | bash
brew tap yvgude/lean-ctx && brew install lean-ctx    # macOS / Linux
npm install -g lean-ctx-bin                          # Node.js
cargo install lean-ctx                               # Rust

# 2. Setup (auto-configures shell + ALL detected editors)
lean-ctx setup

# 3. Verify
lean-ctx doctor
```

<details>
<summary><strong>Troubleshooting</strong></summary>

| Problem | Fix |
|:---|:---|
| Commands broken? | Run `lean-ctx-off` (fixes current session) |
| Permanent fix? | Run `lean-ctx uninstall` (removes all hooks) |
| Binary missing? | Aliases auto-fallback to original commands (safe) |
| Piped output garbled? | Update + run `lean-ctx setup` (adds pipe guard to shell hooks) |
| Manual fix? | Edit `~/.zshrc`, remove the `lean-ctx shell hook` block |
| Preview changes? | `lean-ctx init --global --dry-run` |
| Diagnose? | `lean-ctx doctor` |

lean-ctx creates a backup of your shell config before modifying it (`~/.zshrc.lean-ctx.bak`).

</details>

<details>
<summary><strong>Updating lean-ctx</strong></summary>

```bash
lean-ctx update                # self-update (recommended — refreshes binary, hooks, and shell aliases)
brew upgrade lean-ctx          # Homebrew
npm update -g lean-ctx-bin     # npm
cargo install lean-ctx         # Cargo (rebuild from source)
```

After updating, restart your shell (`source ~/.zshrc`) and IDE to activate the new version.

> **Note:** Since v2.21.8, `lean-ctx update` automatically refreshes shell aliases in `~/.zshrc` / `~/.bashrc` / `config.fish`. If you update via Homebrew/npm/Cargo, run `lean-ctx setup` once to ensure your shell hooks are current.

</details>

<details>
<summary><strong>Supported editors (auto-detected by <code>lean-ctx setup</code>)</strong></summary>

| Editor | Method | Status |
|:---|:---|:---:|
| **Cursor** | MCP + hooks + rules | ✅ Auto |
| **Claude Code** | MCP + PreToolUse hooks + rules | ✅ Auto |
| **GitHub Copilot** | MCP | ✅ Auto |
| **Windsurf** | MCP + rules | ✅ Auto |
| **VS Code** | MCP + rules | ✅ Auto |
| **Zed** | Context Server (settings.json) | ✅ Auto |
| **Codex CLI** | config.toml + AGENTS.md | ✅ Auto |
| **Gemini CLI** | MCP + hooks + rules | ✅ Auto |
| **OpenCode** | MCP + rules | ✅ Auto |
| **Pi** | pi-lean-ctx npm package | ✅ Auto |
| **Qwen Code** | MCP + rules | ✅ Auto |
| **Trae** | MCP + rules | ✅ Auto |
| **Amazon Q Developer** | MCP + rules | ✅ Auto |
| **JetBrains IDEs** | MCP + rules | ✅ Auto |
| **Google Antigravity** | MCP + rules | ✅ Auto |
| **Cline / Roo Code** | MCP + rules | ✅ Auto |
| **Aider** | Shell hook + rules | ✅ Auto |
| **Amp** | Shell hook + rules | ✅ Auto |
| **AWS Kiro** | MCP + steering file | ✅ Auto |
| **Continue** | MCP + rules | ✅ Auto |
| **Crush** | MCP + rules | ✅ Auto |

</details>

<br>

## 🧠 Three Intelligence Protocols

<table>
<tr>
<td width="33%">

### CEP
**Cognitive Efficiency Protocol**

Adaptive LLM communication with compliance scoring (0-100), task complexity classification, quality scoring, auto-validation pipeline.

*Measurable efficiency gains*

</td>
<td width="33%">

### CCP
**Context Continuity Protocol**

Cross-session memory that persists tasks, findings, decisions across chats. LITM-aware positioning for optimal attention placement.

*-99.2% cold-start tokens*

</td>
<td width="33%">

### TDD
**Token Dense Dialect**

Symbol shorthand (`λ` `§` `∂` `τ` `ε`) and ROI-based identifier mapping for compact LLM communication.

*8-25% extra savings*

</td>
</tr>
</table>

<br>

## 🛠 46 Intelligent Tools

### Core

| Tool | Purpose | Savings |
|:---|:---|:---:|
| `ctx_read` | File reads — 10 modes (incl. `lines:N-M`), caching, `fresh=true` | 74-99% |
| `ctx_multi_read` | Multiple file reads in one round trip | 74-99% |
| `ctx_tree` | Directory listings (ls, find, Glob) | 34-60% |
| `ctx_shell` | Shell commands with 90+ compression patterns, cwd tracking | 60-90% |
| `ctx_search` | Code search (Grep) | 50-80% |
| `ctx_compress` | Context checkpoint for long conversations | 90-99% |

### Intelligence

| Tool | What it does |
|:---|:---|
| `ctx_smart_read` | Adaptive mode — auto-picks full/map/signatures/diff based on file type and cache |
| `ctx_delta` | Incremental updates — only sends changed hunks via Myers diff |
| `ctx_dedup` | Cross-file deduplication — finds shared imports and boilerplate |
| `ctx_fill` | Priority-based context filling — maximizes info within a token budget |
| `ctx_intent` | Semantic intent detection — classifies queries and auto-loads files |
| `ctx_response` | Response compression — removes filler, applies TDD |
| `ctx_context` | Multi-turn session overview — tracks what the LLM already knows |
| `ctx_graph` | Project intelligence graph — dependency analysis + related file discovery |
| `ctx_discover` | Shell history analysis — finds missed compression opportunities |
| `ctx_edit` | Search-and-replace file editing — works without native Read/Edit tools |
| `ctx_overview` | Task-relevant project map — use at session start |
| `ctx_preload` | Proactive context loader — caches task-relevant files, returns compact summary |
| `ctx_semantic_search` | BM25 code search by meaning — finds symbols and patterns across the project |
| `ctx_impact` | Measures impact of code changes via dependency chain analysis |
| `ctx_architecture` | Generates architectural overview from dependency graph and module structure |
| `ctx_heatmap` | File access heatmap — tracks read counts, compression ratios, access patterns |

### Memory & Multi-Agent

| Tool | What it does |
|:---|:---|
| `ctx_session` | Cross-session memory — persist task, findings, decisions across chats |
| `ctx_knowledge` | Persistent project knowledge — remember facts, recall by query/category |
| `ctx_agent` | Multi-agent coordination — register, post/read scratchpad, handoff tasks, sync status |
| `ctx_share` | Multi-agent context sharing — push/pull cached file contexts between agents |
| `ctx_wrapped` | Shareable savings report — "Spotify Wrapped" for your tokens |
| `ctx_task` | A2A task orchestration — create, assign, update, complete multi-agent tasks |
| `ctx_cost` | Cost attribution per agent — record, summarize, track token usage |

### Analysis

| Tool | What it does |
|:---|:---|
| `ctx_benchmark` | Single-file or project-wide benchmark with preservation scores |
| `ctx_metrics` | Session statistics with USD cost estimates |
| `ctx_analyze` | Shannon entropy analysis + mode recommendation |
| `ctx_cache` | Cache management: status, clear, invalidate |

### Integrations

| Package | What it provides |
|:---|:---|
| **VS Code Extension** (`packages/vscode-lean-ctx`) | Status bar token savings, one-click setup, MCP auto-config |
| **Chrome Extension** (`packages/chrome-lean-ctx`) | Auto-compress pastes in ChatGPT, Claude, Gemini via native messaging |

<br>

## 📖 ctx_read Modes

| Mode | When to use | Token cost |
|:---|:---|:---|
| `full` | Files you will edit (cached re-reads ≈ 13 tokens) | 100% first, ~0% cached |
| `map` | Understanding a file — deps + exports + API | ~5-15% |
| `signatures` | API surface with more detail than map | ~10-20% |
| `diff` | Re-reading files that changed | changed lines only |
| `aggressive` | Large files with boilerplate | ~30-50% |
| `entropy` | Repetitive patterns (Shannon + Jaccard filtering) | ~20-40% |
| `task` | Task-relevant content via Information Bottleneck + KG filtering | ~15-35% |
| `reference` | Compact function references (F1, F2…) for delta-only follow-ups | ~5-10% |
| `lines:N-M` | Specific ranges (e.g. `lines:10-50,80-90`) | proportional |

<br>

## 🔌 Shell Hook Patterns (90+)

Pattern-based compression for **90+ commands** across **34 categories**:

<details>
<summary><strong>View all 34 categories</strong></summary>

| Category | Commands | Savings |
|:---|:---|:---:|
| **Git** (19) | status, log, diff, add, commit, push, pull, fetch, clone, branch, checkout, switch, merge, stash, tag, reset, remote, blame, cherry-pick | 70-95% |
| **Docker** (10) | build, ps, images, logs, compose ps/up/down, exec, network, volume, inspect | 70-90% |
| **npm/pnpm/yarn** (6) | install, test, run, list, outdated, audit | 70-90% |
| **Cargo** (3) | build, test, clippy | 80% |
| **GitHub CLI** (9) | pr list/view/create/merge, issue list/view/create, run list/view | 60-80% |
| **Kubernetes** (8) | get pods/services/deployments, logs, describe, apply, delete, exec, top, rollout | 60-85% |
| **Python** (7) | pip install/list/outdated/uninstall/check, ruff check/format | 60-80% |
| **Ruby** (4) | rubocop, bundle install/update, rake test, rails test | 60-85% |
| **Linters** (4) | eslint, biome, prettier, stylelint | 60-70% |
| **Build Tools** (3) | tsc, next build, vite build | 60-80% |
| **Test Runners** (8) | jest, vitest, pytest, go test, playwright, cypress, rspec, minitest | 90% |
| **Terraform** | init, plan, apply, destroy, validate, fmt, state, import, workspace | 60-85% |
| **Make** | make targets, parallel jobs, dry-run | 60-80% |
| **Maven / Gradle** | compile, test, package, install, clean, dependency trees | 60-85% |
| **.NET** | dotnet build, test, restore, run, publish, pack | 60-85% |
| **Flutter / Dart** | flutter pub, analyze, test, build; dart pub, analyze, test | 60-85% |
| **Poetry / uv** | install, sync, lock, run, add, remove; uv pip/sync/run | 60-85% |
| **AWS** (7) | s3, ec2, lambda, cloudformation, ecs, logs, sts | 60-80% |
| **Databases** (2) | psql, mysql/mariadb | 50-80% |
| **Prisma** (6) | generate, migrate, db push/pull, format, validate | 70-85% |
| **Helm** (5) | list, install, upgrade, status, template | 60-80% |
| **Bun** (3) | test, install, build | 60-85% |
| **Deno** (5) | test, lint, check, fmt, task | 60-85% |
| **Swift** (3) | test, build, package resolve | 60-80% |
| **Zig** (2) | test, build | 60-80% |
| **CMake** (3) | configure, build, ctest | 60-80% |
| **Ansible** (2) | playbook recap, task summary | 60-80% |
| **Composer** (3) | install, update, outdated | 60-80% |
| **Mix** (5) | test, deps, compile, format, credo/dialyzer | 60-80% |
| **Bazel** (3) | test, build, query | 60-80% |
| **systemd** (2) | systemctl, journalctl | 50-80% |
| **Utils** (5) | curl, grep/rg, find, ls, wget | 50-89% |
| **Data** (3) | env (filtered), JSON schema extraction, log dedup | 50-80% |

</details>

After `lean-ctx init --global`, **23 commands** are transparently compressed via shell aliases:

```
git · npm · pnpm · yarn · cargo · docker · docker-compose · kubectl · k
gh · pip · pip3 · ruff · go · golangci-lint · eslint · prettier · tsc
ls · find · grep · curl · wget
```

<br>

## 👀 Examples

<details>
<summary><strong>Directory listing</strong> — 239 → 46 tokens (-81%)</summary>

```
# ls -la src/                               # lean-ctx -c "ls -la src/"
total 96                                     core/
drwxr-xr-x  4 user staff  128 ...           tools/
drwxr-xr-x  11 user staff 352 ...           cli.rs  9.0K
-rw-r--r--  1 user staff  9182 ...           main.rs  4.0K
-rw-r--r--  1 user staff  4096 ...           server.rs  11.9K
...                                          shell.rs  5.2K
                                             4 files, 2 dirs
                                             [lean-ctx: 239→46 tok, -81%]
```

</details>

<details>
<summary><strong>File reading (map mode)</strong> — 2,078 → ~30 tokens (-99%)</summary>

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

</details>

<details>
<summary><strong>curl (JSON)</strong> — 127 → 14 tokens (-89%)</summary>

```
# curl -s httpbin.org/json                   # lean-ctx -c "curl -s httpbin.org/json"
{                                            JSON (428 bytes):
  "slideshow": {                             {
    "author": "Yours Truly",                   slideshow: {4K}
    "date": "date of publication",           }
    "slides": [                              [lean-ctx: 127→14 tok, -89%]
      {
        "title": "Wake up to WonderWidgets!",
        ...
```

</details>

<details>
<summary><strong>Visual terminal dashboard</strong></summary>

```
$ lean-ctx gain

  ◆ lean-ctx  Token Savings Dashboard
  ────────────────────────────────────────────────────────

   1.7M          76.8%         520          $33.71
   tokens saved   compression    commands       USD saved

  Cost Breakdown  (@ $2.50/M input, $10/M output)
  ────────────────────────────────────────────────────────
  Without lean-ctx    $44.75  ($5.79 input + $38.96 output)
  With lean-ctx       $11.04  ($1.76 input + $9.28 output)
  Saved               $33.71  ($4.03 input + $29.68 output)

  Top Commands
  ────────────────────────────────────────────────────────
  curl                48x  ████████████████████ 728.1K  97%
  git commit          34x  ██████████▎          375.2K  50%
  ctx_read           103x  █▌                    59.1K  38%
    ... +33 more commands

  lean-ctx v3.4.2  |  leanctx.com  |  lean-ctx dashboard
```

</details>

<br>

## 🔬 Scientific Compression Engine

Built on information theory and attention modeling (v2.6):

| Feature | What it does | Impact |
|:---|:---|:---:|
| **Adaptive Entropy** | Per-language BPE entropy + Jaccard thresholds with Kolmogorov adjustment | 10-25% |
| **Attention Model** | Heuristic U-curve positional weighting + structural importance scoring | ↑ comprehension |
| **TF-IDF Codebook** | Cross-file pattern dedup via cosine similarity | 5-15% |
| **Feedback Loop** | Learns optimal thresholds per language/file type across sessions | auto-improving |
| **Info Bottleneck** | Entropy + task-relevance filtering (Tishby et al., 2000) | 20-40% |
| **ctx_overview** | Multi-resolution project map with graph-based relevance tiers | 90%+ |

<br>

## 🌳 tree-sitter Signature Engine

AST-based signature extraction for **18 languages**: TypeScript, JavaScript, Rust, Python, Go, Java, C, C++, Ruby, C#, Kotlin, Swift, PHP, Bash, Dart, Scala, Elixir, Zig.

| Capability | Regex (old) | tree-sitter |
|:---|:---:|:---:|
| Multi-line signatures | ✗ | ✓ |
| Arrow functions | ✗ | ✓ |
| Nested classes/methods | Heuristic | AST scope |
| Languages | 4 | **18** |

Build without tree-sitter for a smaller binary (~5.7 MB vs ~17 MB):

```bash
cargo install lean-ctx --no-default-features
```

<br>

## 📊 CLI Commands

<details>
<summary><strong>Shell Hook</strong></summary>

```bash
lean-ctx -c "git status"       # Execute + compress output
lean-ctx exec "cargo build"    # Same as -c
lean-ctx shell                 # Interactive REPL with compression
lean-ctx bypass "git diff"     # Guaranteed zero compression
lean-ctx safety-levels         # Show compression transparency table
```

</details>

<details>
<summary><strong>File Operations</strong></summary>

```bash
lean-ctx read file.rs                         # Full content (structured header)
lean-ctx read file.rs -m map                  # Deps + API signatures (~10% tokens)
lean-ctx read file.rs -m signatures           # Function/class signatures only
lean-ctx read file.rs -m aggressive           # Syntax-stripped (~40% tokens)
lean-ctx read file.rs -m entropy              # Shannon entropy filtered (~30%)
lean-ctx read file.rs -m "lines:10-50,80-90"  # Specific line ranges
lean-ctx diff file1.rs file2.rs               # Compressed file diff
lean-ctx grep "pattern" src/                  # Grouped search results
lean-ctx find "*.rs" src/                     # Compact find results
lean-ctx ls src/                              # Token-optimized directory listing
lean-ctx deps .                               # Project dependencies summary
```

</details>

<details>
<summary><strong>Setup & Analytics</strong></summary>

```bash
lean-ctx setup                 # One-command setup: shell + editors + verify
lean-ctx init --global         # Install shell aliases (file-based)
eval "$(lean-ctx init zsh)"   # Eval-based init (like starship/zoxide)
lean-ctx init --agent claude   # Claude Code hook
lean-ctx init --agent cursor   # Cursor hooks.json
lean-ctx init --agent gemini   # Gemini CLI hook
lean-ctx init --agent codex    # Codex AGENTS.md
lean-ctx init --agent windsurf # .windsurfrules
lean-ctx init --agent cline    # .clinerules
lean-ctx init --agent kiro     # AWS Kiro (MCP + steering file)
lean-ctx init --agent crush    # Crush by Charmbracelet
lean-ctx init --agent pi       # Pi Coding Agent extension
lean-ctx gain                  # Visual terminal dashboard
lean-ctx gain --live           # Live auto-updating dashboard
lean-ctx gain --graph          # ASCII chart (30 days)
lean-ctx gain --daily          # Day-by-day breakdown
lean-ctx gain --json           # Raw JSON export
lean-ctx dashboard             # Web dashboard (localhost:3333)
lean-ctx cheatsheet            # Quick reference
lean-ctx discover              # Find uncompressed commands
lean-ctx doctor                # Diagnostics
lean-ctx update                # Self-update
lean-ctx wrapped               # Shareable savings report
lean-ctx benchmark run         # Real project benchmark
lean-ctx benchmark report      # Markdown report
```

</details>

<details>
<summary><strong>Docker / CI (non-interactive bash)</strong></summary>

In Docker/CI, AI agents often run commands via `bash -c` (non-interactive). Many distros skip `~/.bashrc` entirely in that mode, so the reliable hook point is `BASH_ENV`.

Recommended Dockerfile order:

```dockerfile
# After installing lean-ctx
RUN lean-ctx bootstrap
ENV BASH_ENV="/root/.lean-ctx/env.sh"
```

If you install Claude Code in the container, lean-ctx’s `env.sh` includes a **self-heal** block that re-injects the `lean-ctx` MCP server if Claude overwrote `~/.claude.json` on first start.

Troubleshooting:

- `lean-ctx doctor`
- `lean-ctx doctor --fix`

</details>

<details>
<summary><strong>Multi-Agent Launcher</strong></summary>

```bash
lctx                              # Auto-detect agent, current dir
lctx --agent claude               # Launch Claude Code with lean-ctx
lctx --agent cursor               # Configure Cursor
lctx --agent gemini               # Launch Gemini CLI
lctx /path/to/project "prompt"    # Project + prompt
lctx --scan-only                  # Build project graph only
```

</details>

<br>

## ⚙️ Editor Configuration

> **`lean-ctx setup` handles this automatically.** Manual config below is only needed for edge cases.

<details>
<summary><strong>Cursor</strong></summary>

`~/.cursor/mcp.json`:
```json
{
  "mcpServers": {
    "lean-ctx": { "command": "lean-ctx" }
  }
}
```

</details>

<details>
<summary><strong>GitHub Copilot</strong></summary>

`.github/copilot/mcp.json`:
```json
{
  "servers": {
    "lean-ctx": { "command": "lean-ctx" }
  }
}
```

</details>

<details>
<summary><strong>Claude Code</strong></summary>

```bash
lean-ctx init --agent claude

# If you need manual wiring:
claude mcp add-json --scope user lean-ctx <<'JSON'
{"command":"lean-ctx"}
JSON
```

</details>

<details>
<summary><strong>Windsurf</strong></summary>

`~/.codeium/windsurf/mcp_config.json`:
```json
{
  "mcpServers": {
    "lean-ctx": { "command": "lean-ctx" }
  }
}
```

> If tools don't load, use the full path (e.g., `/Users/you/.cargo/bin/lean-ctx`). Windsurf spawns MCP servers with a minimal PATH.

</details>

<details>
<summary><strong>Zed</strong></summary>

`~/.config/zed/settings.json`:
```json
{
  "context_servers": {
    "lean-ctx": {
      "source": "custom",
      "command": "lean-ctx",
      "args": [],
      "env": {}
    }
  }
}
```

</details>

<details>
<summary><strong>OpenAI Codex</strong></summary>

`~/.codex/config.toml`:
```toml
[mcp_servers.lean-ctx]
command = "lean-ctx"
args = []
```

</details>

<details>
<summary><strong>Gemini CLI</strong></summary>

`~/.gemini/settings/mcp.json`:
```json
{
  "mcpServers": {
    "lean-ctx": { "command": "lean-ctx" }
  }
}
```

</details>

<details>
<summary><strong>Pi Coding Agent</strong></summary>

```bash
lean-ctx init --agent pi
# Or: pi install npm:pi-lean-ctx
```

Pi's `bash`, `read`, `grep`, `find`, and `ls` tools are automatically routed through lean-ctx. Supports 55+ file extensions with auto mode selection.

</details>

<details>
<summary><strong>OpenCode</strong></summary>

`~/.config/opencode/opencode.json`:
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

</details>

<details>
<summary><strong>Hermes Agent</strong></summary>

`~/.hermes/config.yaml`:
```yaml
mcp_servers:
  lean-ctx:
    command: "lean-ctx"
    args: ["mcp"]
```

**Tool naming**: Hermes prefixes all MCP tools with `mcp_<server>_`, so lean-ctx tools appear as `mcp_lean_ctx_ctx_read`, `mcp_lean_ctx_ctx_shell`, etc. This is standard Hermes behavior.

**Reduce tool count** (recommended for smaller models):
```yaml
mcp_servers:
  lean-ctx:
    command: "lean-ctx"
    args: ["mcp"]
    tools:
      include: [ctx_read, ctx_shell, ctx_search, ctx_tree, ctx_edit, ctx_session]
```

</details>

<br>

## 🎛 Tool Modes & Context Optimization

lean-ctx exposes tools in three modes. Choose based on your model's context window:

| Mode | Tools | Schema tokens | Best for | Env var |
|:---|:---:|:---:|:---|:---|
| **Granular** (default) | ~46 | ~15-20K | Cursor, Claude Code, Codex (large context) | _(default)_ |
| **Lazy** | 9 + discover | ~3-4K | Hermes, Ollama, smaller models | `LEAN_CTX_LAZY_TOOLS=1` |
| **Unified** | 5 | ~2K | Extreme optimization | `LEAN_CTX_UNIFIED=1` |

**Lazy mode** exposes only core tools (`ctx_read`, `ctx_shell`, `ctx_search`, `ctx_tree`, `ctx_edit`, `ctx_session`, `ctx_knowledge`, `ctx_multi_read`) plus `ctx_discover_tools` for on-demand loading of additional tools.

### Disabling specific tools

Via config (`~/.lean-ctx/config.toml`):
```toml
disabled_tools = ["ctx_graph", "ctx_architecture", "ctx_heatmap"]
```

Via environment variable:
```bash
export LEAN_CTX_DISABLED_TOOLS="ctx_graph,ctx_architecture,ctx_heatmap"
```

### Rules file placement

By default, `lean-ctx setup` and `lean-ctx init --agent` install agent rule files in **both** the home directory (global) and the current project (project-local). To avoid duplicates and save context tokens, configure the placement scope:

Via config (`~/.lean-ctx/config.toml`):
```toml
rules_scope = "global"    # only home-dir rules (~/.claude/rules/, ~/.cursor/rules/, etc.)
# rules_scope = "project" # only project-local rules (.claude/rules/, .cursor/rules/, etc.)
# rules_scope = "both"    # default — install everywhere
```

Via environment variable:
```bash
export LEAN_CTX_RULES_SCOPE="project"
```

Via CLI:
```bash
lean-ctx config set rules_scope global
```

Per-project override via `.lean-ctx.toml` in the project root:
```toml
rules_scope = "project"
```

### Path access outside project root

By default, lean-ctx restricts file access to the project root for security. To allow access to additional directories:

```bash
export LCTX_ALLOW_PATH="/home/user/.codex:/home/user/shared-data"
```

When running inside Codex CLI (`CODEX_CLI_SESSION` set), `~/.codex` is automatically allowed. When running inside Claude Code, `~/.claude` is automatically allowed.

<br>

## 🏆 lean-ctx vs Alternatives (Rust Token Killer, context-mode)

| Feature | Rust Token Killer | lean-ctx |
|:---|:---:|:---:|
| Architecture | Shell hook only | **Shell hook + MCP server** |
| Process model | Spawns per command | **Persistent server** (no EAGAIN) |
| CLI patterns | ~50 | **90+** |
| File reading | Signatures only | **10 modes** (full, map, signatures, diff, aggressive, entropy, task, reference, lines, auto) |
| File caching | ✗ | ✓ (re-reads ≈ 13 tokens) |
| Signature engine | Regex (4 langs) | **tree-sitter AST (18 langs)** |
| Dependency maps | ✗ | ✓ |
| Context checkpoints | ✗ | ✓ |
| Token counting | Estimated | **tiktoken-exact** |
| Entropy analysis | ✗ | ✓ |
| Cost tracking | ✗ | ✓ (USD estimates) |
| TDD mode | ✗ | ✓ (8-25% extra) |
| Thinking reduction | ✗ | ✓ (CRP v2) |
| Cross-session memory | ✗ | ✓ (CCP) |
| LITM positioning | ✗ | ✓ |
| Multi-agent sharing | ✗ | ✓ |
| Project knowledge store | ✗ | ✓ |
| Web dashboard | ✗ | ✓ |
| Savings reports | ✗ | ✓ (`wrapped`) |
| Raw mode / bypass | ✓ | ✓ (`raw=true`, `--raw`, `lean-ctx-raw`, `lean-ctx bypass`) |
| User-defined filters | TOML rules | **TOML rules** (priority over builtins) |
| Full output recovery | `tee` | ✓ (`tee_mode: always/failures/never`) |
| Truncation warnings | ✗ | ✓ (transparent markers) |
| ANSI auto-strip | ✗ | ✓ (pre-compression) |
| Telemetry | **Default ON (PII)** | **Opt-in only, no PII, no tracking** |
| Editor support | 3 editors | **24 editors/tools** |

<br>

## 🔐 Privacy & Security

lean-ctx is **privacy-first by design**:

- **No tracking, no analytics** — no PII, no hostnames, no project paths leave your system
- **Opt-in data sharing** — anonymous compression stats are only sent if you explicitly enable it during setup (default: off)
- **Update check** — a lightweight daily version check against `leanctx.com/version.txt` runs in the background (disable with `update_check_disabled = true` in config or `LEAN_CTX_NO_UPDATE_CHECK=1`)
- **Fully auditable** — Apache 2.0 licensed, single Rust binary, no hidden dependencies

See [SECURITY.md](SECURITY.md).

> **Note on VirusTotal:** Rust binaries are frequently flagged by ML-based heuristic scanners. This is a [known issue](https://users.rust-lang.org/t/rust-programs-flagged-as-malware/49799). Build from source with `cargo install lean-ctx` to verify.

<br>

## 🗑 Uninstall

```bash
lean-ctx init --global   # See what was added, then remove from shell profile
cargo uninstall lean-ctx # Remove binary
rm -rf ~/.lean-ctx       # Remove stats + config
```

<br>

## ❓ Frequently Asked Questions

<details>
<summary><strong>How much money does lean-ctx save?</strong></summary>

Based on real usage data: active developers save **$30-100+ per month** on API costs. The exact amount depends on your AI tool, usage patterns, and pricing tier. lean-ctx shows your exact savings with `lean-ctx gain`.

</details>

<details>
<summary><strong>Does lean-ctx work with Claude Code / Cursor / Copilot?</strong></summary>

Yes — lean-ctx supports **24 AI coding tools** out of the box with 46 MCP tools. Run `lean-ctx setup` and it auto-detects and configures all installed editors. No manual configuration needed.

</details>

<details>
<summary><strong>Does it slow down my AI tool?</strong></summary>

No. lean-ctx adds <1ms overhead per operation. The MCP server runs as a persistent process (no cold starts), and the shell hook uses compiled Rust patterns. Most users report their AI tools feel **faster** because less data means faster LLM responses.

</details>

<details>
<summary><strong>Is it safe? Does it send data anywhere?</strong></summary>

lean-ctx has **no tracking and no analytics**. All compression runs 100% locally. The only network activity is an optional daily version check (disable with `LEAN_CTX_NO_UPDATE_CHECK=1`) and opt-in anonymous compression stats sharing (off by default). No PII, hostnames, or project paths are ever transmitted. The code is Apache 2.0 licensed and fully auditable. See [SECURITY.md](SECURITY.md).

</details>

<details>
<summary><strong>What's the difference between lean-ctx and Rust Token Killer (RTK)?</strong></summary>

lean-ctx is a hybrid architecture (shell hook + MCP server) while Rust Token Killer is shell-hook only. lean-ctx offers 46 tools vs RTK's basic compression, supports 24 editors vs 3, has tree-sitter AST parsing for 18 languages, cross-session memory, multi-agent coordination, and no tracking or analytics (RTK has default-on telemetry with PII). lean-ctx's optional data sharing is fully opt-in and anonymized.

</details>

<details>
<summary><strong>How do I reduce my Claude Code / Cursor API bill?</strong></summary>

1. Install lean-ctx: `npm install -g lean-ctx-bin`
2. Run `lean-ctx setup` (auto-configures everything)
3. Use your AI tool normally — lean-ctx compresses context transparently
4. Check savings: `lean-ctx gain`

Most users see 70-90% reduction in token usage from day one.

</details>

<br>

## 🤝 Contributing

Contributions welcome! Open an issue or PR on [GitHub](https://github.com/yvgude/lean-ctx).

<p align="center">
  <a href="https://discord.gg/pTHkG9Hew9">Discord</a> ·
  <a href="https://x.com/leanctx">𝕏 / Twitter</a> ·
  <a href="https://buymeacoffee.com/yvgude">Buy me a coffee ☕</a>
</p>

<br>

## 📄 License

Apache License 2.0 — see [LICENSE](LICENSE).

Portions of this software were originally released under the MIT License. See [LICENSE-MIT](LICENSE-MIT) for the original text and [NOTICE](NOTICE) for attribution details.

<br>

<p align="center">
  <sub>Built with 🦀 Rust · Made in Switzerland 🇨🇭</sub>
</p>
