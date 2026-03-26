---
name: lean-ctx
description: Context Intelligence Engine with CEP + CCP — 21 MCP tools, 90+ shell patterns, tree-sitter AST for 14 languages, Cognitive Efficiency Protocol (CEP), cross-session memory (CCP), LITM-aware positioning. Compresses LLM context by up to 99%.
metadata: {"openclaw": {"requires": {"bins": ["lean-ctx"]}, "install": [{"id": "brew", "kind": "brew", "formula": "lean-ctx", "bins": ["lean-ctx"], "label": "Install LeanCTX (brew tap yvgude/lean-ctx)"}]}}
---

# LeanCTX v2.3.3 — Context Intelligence Engine + CEP + CCP + Scientific Optimizations

LeanCTX is a Rust binary that optimizes LLM context through 21 MCP tools, 90+ shell compression patterns, and tree-sitter AST parsing for 14 languages (TS/JS, Rust, Python, Go, Java, C, C++, Ruby, C#, Kotlin, Swift, PHP). It provides adaptive file reading, incremental deltas, intent detection, cross-file deduplication, a project intelligence graph, the **Cognitive Efficiency Protocol (CEP)** for optimized LLM communication, and the **Context Continuity Protocol (CCP)** for cross-session memory with LITM-aware information positioning.

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
Use `signatures` mode when you need the API surface of a module (tree-sitter for 14 languages).
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
```

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
