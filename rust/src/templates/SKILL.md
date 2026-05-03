---
name: lean-ctx
description: Context Runtime for AI Agents — 49 MCP tools, 10 read modes, 90+ shell patterns, tree-sitter AST for 18 languages. Compresses LLM context by up to 99%. Use when reading files, running shell commands, searching code, or exploring directories. Auto-installs if not present.
---

# LeanCTX — Context Runtime for AI Agents

LeanCTX optimizes LLM context through 49 MCP tools, 90+ shell compression patterns, and tree-sitter AST parsing for 18 languages. It provides adaptive file reading, cross-session memory (CCP), task-conditioned relevance scoring, and a feedback loop for learning optimal compression.

## Setup (run first)

Before using lean-ctx, verify it is installed:

```bash
which lean-ctx || bash scripts/install.sh
```

If the install script is not available locally, install manually:

```bash
curl -fsSL https://raw.githubusercontent.com/yvgude/lean-ctx/main/skills/lean-ctx/scripts/install.sh | bash
```

After installation, configure the MCP server:

```bash
lean-ctx init --global
lean-ctx doctor --fix
```

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
lean-ctx init --global          # Install shell aliases
lean-ctx init --agent claude    # Claude Code PreToolUse hook
lean-ctx init --agent cursor    # Cursor hooks.json
lean-ctx init --agent gemini    # Gemini CLI BeforeTool hook
lean-ctx init --agent codex     # Codex AGENTS.md
lean-ctx init --agent windsurf  # .windsurfrules
lean-ctx init --agent cline     # .clinerules
lean-ctx init --agent crush     # Crush MCP config
lean-ctx init --agent copilot   # VS Code / Copilot .vscode/mcp.json
```

## Multi-Agent & Knowledge (v2.7.0+)

MCP tools:
- `ctx_knowledge(action="remember", category, key, value)` — persistent cross-session project knowledge store
- `ctx_knowledge(action="recall", query)` — search stored facts by text or category
- `ctx_knowledge(action="consolidate")` — extract session findings into permanent knowledge
- `ctx_agent(action="register", agent_type, role)` — multi-agent context sharing with scratchpad messaging
- `ctx_agent(action="post", message, tags)` — share findings/warnings between concurrent agents
- `ctx_agent(action="read")` — read messages from other agents
- `ctx_agent(action="handoff", to_agent, message)` — transfer task to another agent
- `ctx_agent(action="sync")` — multi-agent sync status (active agents, pending messages, shared contexts)
- `ctx_share(action="push", paths, to_agent, message)` — push cached file contexts to another agent
- `ctx_share(action="pull")` — pull shared contexts from other agents
- `ctx_share(action="list")` — list all shared contexts
- `ctx_share(action="clear")` — remove contexts shared by this agent

## Additional Intelligence Tools (v2.19.0)

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
- `ctx_session role` — list/switch agent roles (governance)
- `ctx_session budget` — show budget status vs role limits
- `ctx_session slo` — show SLO status/violations (value=reload|history|clear)
- `ctx_session diff` — compare two sessions (value="<id_a> <id_b> [json]")
- `ctx_session verify` — show output verification statistics
- `ctx_session episodes` — episodic memory (value=record | "search <q>" | "file <path>" | "outcome <label>")
- `ctx_session procedures` — procedural memory (value=detect | "suggest <task>")
- `ctx_intent` — intent classification + model routing (returns dimension/tier/reasoning)
- `ctx_graph build` — index code into unified graph
- `ctx_graph related` — find connected files via graph
- `ctx_graph symbol` — lookup symbol definitions/usages
- `ctx_graph impact` — blast radius analysis
- `ctx_graph enrich` — add commits, tests, knowledge to graph
- `ctx_graph context` — task-based graph query for relevant context
- `ctx_wrapped` — generate savings report card in chat

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
