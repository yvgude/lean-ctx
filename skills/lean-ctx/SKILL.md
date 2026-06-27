---
name: lean-ctx
description: Context Engineering for AI Agents — 79 MCP tools, 10 read modes, 95+ shell patterns, tree-sitter AST for 18 languages. Compresses LLM context by up to 99%. Use when reading files, running shell commands, searching code, or exploring directories. Auto-installs if not present.
---

# LeanCTX — Context Engineering for AI Agents

LeanCTX optimizes LLM context through 79 MCP tools, 95+ shell compression patterns, and tree-sitter AST parsing for 18 languages. It provides adaptive file reading, cross-session memory (CCP), task-conditioned relevance scoring, and a feedback loop for learning optimal compression.

## Setup (run first)

Before using lean-ctx, verify it is installed:

```bash
which lean-ctx || bash scripts/install.sh
```

If the install script is not available locally, install manually:

```bash
curl -fsSL https://raw.githubusercontent.com/yvgude/lean-ctx/main/skills/lean-ctx/scripts/install.sh | bash
```

After installation, run the one-command setup (installs shell hook + editor wiring + rules + skills):

```bash
lean-ctx setup
```

lean-ctx supports two integration styles (auto-detected per agent):
- **Hybrid (default where shell access exists)**: MCP for cached reads/search + shell hooks that compress command output.
- **MCP (for IDE-extension agents without reliable shell hooks)**: cached reads + all tools via MCP.

## When to use lean-ctx

Always prefer `lean-ctx -c <command>` over running commands directly when:
- The command produces verbose output (build logs, git diffs, dependency trees, test results)
- You are reading files and only need the structure or API surface
- You want to check token savings for the current session

## Shell commands (use instead of raw exec)

```bash
lean-ctx -c git status          # Compressed git output
lean-ctx -c git diff            # Only meaningful diff lines
lean-ctx -c git log --oneline -10
lean-ctx -c npm install         # Strips progress bars, noise
lean-ctx -c cargo build
lean-ctx -c cargo test
lean-ctx -c docker ps
lean-ctx -c kubectl get pods
lean-ctx -c aws ec2 describe-instances
lean-ctx -c helm list
lean-ctx -c prisma migrate dev
lean-ctx -c curl -s <url>       # JSON schema extraction
lean-ctx -c ls -la <dir>        # Grouped directory listing
```

Supported: git, npm, pnpm, yarn, bun, deno, cargo, docker, kubectl, helm, gh, pip, ruff, go, eslint, prettier, tsc, aws, psql, mysql, prisma, swift, zig, cmake, ansible, composer, mix, bazel, systemd, terraform, make, maven, dotnet, flutter, poetry, rubocop, playwright, curl, wget, and more.

## File reading (compressed modes)

```bash
lean-ctx read <file>                    # Full content with structured header
lean-ctx read <file> -m map             # Dependency graph + exports + API (~5-15% tokens)
lean-ctx read <file> -m signatures      # Function/class signatures only (~10-20% tokens)
lean-ctx read <file> -m aggressive      # Syntax-stripped (~30-50% tokens)
lean-ctx read <file> -m entropy         # Shannon entropy filtered (~20-40% tokens)
lean-ctx read <file> -m diff            # Only changed lines since last read
```

Use `map` mode when you need to understand what a file does without reading every line.
Use `signatures` mode when you need the API surface of a module (tree-sitter for 18 languages).
Use `full` mode only when you will edit the file.

## AI Tool Integration

```bash
lean-ctx init --global                # Install shell aliases
lean-ctx init --agent cursor          # Hybrid (MCP reads/search + shell hooks)
lean-ctx init --agent claude          # Hybrid (Claude Code)
lean-ctx init --agent codebuddy       # Hybrid (CodeBuddy)
lean-ctx init --agent codex           # Hybrid (Codex CLI)
lean-ctx init --agent opencode        # Hybrid (OpenCode)

lean-ctx init --agent copilot         # MCP (VS Code / Copilot)
lean-ctx init --agent jetbrains       # MCP (JetBrains)
lean-ctx init --agent windsurf        # Hybrid (Windsurf)

# Mode is auto-detected; override with --mode mcp|hybrid if needed.
```

## Multi-Agent & Knowledge

CLI (works in Hybrid and MCP setups):

```bash
lean-ctx knowledge remember "value" --category <c> --key <k>
lean-ctx knowledge recall "query"
lean-ctx knowledge search "query"
lean-ctx knowledge export [--format json|jsonl|simple] [--output <path>]
lean-ctx knowledge import <path> [--merge replace|append|skip-existing] [--dry-run]
lean-ctx knowledge remove --category <c> --key <k>
lean-ctx knowledge consolidate [--all]

lean-ctx session task "what you're doing"
lean-ctx session finding "what you found"
lean-ctx session decision "what you decided"
lean-ctx session save
```

If MCP is enabled for your IDE, the same capabilities are also available as MCP tools
(`ctx_knowledge`, `ctx_session`, `ctx_agent`, ...).

## Additional Intelligence Tools

- `ctx_edit(path, old_string, new_string)` — search-and-replace file editing without native Read/Edit
- `ctx_overview(task)` — task-relevant project map at session start
- `ctx_preload(task)` — proactive context loader, caches task-relevant files
- `ctx_semantic_search(query)` — BM25 code search by meaning across the project
- `ctx_intent` now supports multi-intent detection and complexity classification
- Semantic cache: TF-IDF + cosine similarity for finding similar files across reads

## Session Continuity (CCP)

```bash
lean-ctx sessions list          # List all CCP sessions
lean-ctx sessions show          # Show latest session state
lean-ctx wrapped                # Weekly savings report card
lean-ctx wrapped --month        # Monthly savings report card
lean-ctx benchmark run          # Real project benchmark (terminal output)
lean-ctx benchmark run --json   # Machine-readable JSON output
lean-ctx benchmark report       # Shareable Markdown report
```

MCP tools for CCP:
- `ctx_session status` — show current session state (~400 tokens)
- `ctx_session load` — restore previous session (cross-chat memory)
- `ctx_session task "description"` — set current task
- `ctx_session finding "file:line — summary"` — record key finding
- `ctx_session decision "summary"` — record architectural decision
- `ctx_session save` — force persist session to disk
- `ctx_gain action=wrapped` — generate savings report card in chat
- `ctx_refactor` — LSP-powered rename, references, definition, implementations (requires language server)
- `ctx_expand action=search_all query="..."` — FTS5 cross-archive fulltext search

## Analytics

```bash
lean-ctx gain                   # Visual token savings dashboard
lean-ctx dashboard              # Web dashboard at localhost:3333
lean-ctx session                # Adoption statistics
lean-ctx discover               # Find uncompressed commands in shell history
```

## Tips

- The output suffix `[lean-ctx: 5029→197 tok, -96%]` shows original vs compressed token count
- For large outputs, lean-ctx automatically truncates while preserving relevant context
- JSON responses from curl/wget are reduced to schema outlines
- Build errors are grouped by type with counts
- Test results show only failures with summary counts
- Cached re-reads cost only ~13 tokens
