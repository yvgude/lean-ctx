# Product Context

## Warum das existiert
LLM-Agenten scheitern nicht primär an „Intelligenz“, sondern an **fehlender Process Authority** und **schlechtem Context Flow**:
- Sie lesen zu viel/zu wenig.
- Sie benutzen Tools inkonsistent.
- Sie liefern „Done“-Behauptungen ohne Evidence.

`lean-ctx` macht Context + Prozess **deterministisch**, **lokal**, **messbar**.

## Nutzererlebnis (Soll)
- Agent arbeitet **schneller** (weniger Tokens, weniger IO) und **zuverlässiger** (Rails + Evidence).
- Nutzer kann `lean-ctx` wie heute nutzen (Shell Hook + MCP stdio), oder als Runtime in Harnesses/Orchestratoren einbetten.
- Website/Docs zeigen **niemals** falsche Zahlen (SSOT/Manifest + i18n Gate).

## Haupt-User-Stories
- Als Agent: „Gib mir nur das Nötige“ → `ctx_read` mit `auto/task/reference/lines` + Cache.
- Als Developer: „Zeig Impact“ → `ctx_impact`, `ctx_architecture`, `ctx_graph`.
- Als Orchestrator: „Zwinge Process“ → `ctx_workflow` + Gatekeeper + Evidence Receipts.
- Als Team (local-first): „Was kostet uns Tooling?“ → `ctx_cost` report, ohne Cloud.

# Product Context

## Problem
Agentische Modelle sind unzuverlässig: sie lesen falsche Dinge, verlieren Kontext, halten Prozesse nicht ein und liefern keine überprüfbaren Zwischenergebnisse.

## Lösung
`lean-ctx` liefert ein lokales Runtime-Layer:
- **Context Orchestration**: optimierte Reads (Modes), Dedupe, Budget-Fill, Semantic Search
- **Process Rails**: Workflow State Machine + Tool Gatekeeper + Evidence Store
- **Knowledge**: persistente, versionierte Facts + Patterns + Gotchas
- **A2A**: Multi-Agent Coordination (Tasks, Share, Diaries)

## UX Ziele
- „It just works“ (lokal, keine Setup-Hürden)
- deterministische, kompakte Ausgaben
- klare, maschinenprüfbare Nachweise (Receipts/Manifests)

# Product Context — lean-ctx

## Why This Project Exists

In 2026, the performance bottleneck in AI-assisted development is **context window pollution**. LLMs receive too much irrelevant data (boilerplate, ANSI codes, verbose CLI output), which dilutes their attention mechanism and degrades reasoning quality.

lean-ctx acts as a **Cognitive Filter** between human developers and LLMs, ensuring maximum **Information Density** per token.

## Problems It Solves

1. **Token waste** — Raw CLI output (git status, docker build, npm install) is 60-95% noise
2. **Context window pollution** — Full file reads flood the LLM with boilerplate
3. **Cost** — Unnecessary tokens directly increase API costs ($2.50/1M tokens baseline)
4. **Performance** — More noise = worse LLM reasoning quality

## How It Works

### Two Modes of Operation

1. **MCP Server** (primary) — editors call lean-ctx tools via Model Context Protocol
   - `ctx_read` / `ctx_multi_read` — smart file reading with 6 modes + session cache
   - `ctx_tree` — compact directory listing
   - `ctx_shell` — compressed command execution
   - `ctx_search` — token-efficient code search
   - `ctx_compress` — context checkpoints
   - `ctx_benchmark` / `ctx_analyze` — optimization tools
   - `ctx_metrics` — session statistics
   - `ctx_cache` — cache management

2. **Shell Hook** (secondary) — transparent command compression via aliases
   - `lean-ctx -c "git status"` → compressed output
   - `lean-ctx init --global` installs 23 aliases

### Key Technical Innovations

- **Session caching** — re-reads cost 13 tokens instead of thousands
- **Tree-sitter AST** — extract only function signatures, not full files
- **Pattern compression** — 75+ regex patterns for CLI tools
- **Delta-loading** — only send changed lines
- **Token Dense Dialect** — symbol shorthand (⊛ = async, λ = function, etc.)

## User Experience Goals

- **Zero config** — `cargo install lean-ctx` + `lean-ctx init --global` = done
- **Transparent** — aliases work like normal commands, just with compressed output
- **Measurable** — `lean-ctx gain` shows exactly how many tokens/USD saved
- **Cross-platform** — works identically on macOS, Linux, Windows

## Vision

"A Lossless Minifier for Human Thought" — maximize Information Entropy per token. The winners aren't those who use the largest context windows, but those who achieve the same result with 10x fewer tokens.
