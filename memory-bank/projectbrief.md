# lean-ctx — Project Brief

## Ziel
`lean-ctx` ist eine **Context Runtime for AI Agents**: ein lokales, auditierbares Single-Binary, das Agenten zuverlässig mit **relevantem Kontext**, **Workflows/Rails**, **Evidence**, **Knowledge** und **Multi-Agent Coordination** versorgt – bei minimalen Token-Kosten.

## Nicht-Ziele / Prinzipien
- **Keine Tiers / Paywalls**: alles für alle.
- **Local-first, zero telemetry**: keine verpflichtende Cloud, keine heimlichen Netzwerkcalls.
- **Qualität vor Scope**: SSOT/CI-Gates verhindern Drift; Features müssen end-to-end nutzbar sein (keine „toten“ Tools ohne Instrumentation).

## Kernversprechen
- **MCP Tooling**: Granular **42 Tools** + Unified **5 Tools** (SSOT via `rust/src/tool_defs.rs` → `website/generated/mcp-tools.json`).
- **Read Modes**: **10** (`auto`, `full`, `map`, `signatures`, `diff`, `aggressive`, `entropy`, `task`, `reference`, `lines:N-M`).
- **Agent Rails**: Workflow State Machine + Tool Gatekeeper + Evidence Store (`ctx_workflow`), damit Agenten prozess-konform arbeiten.
- **Observability**: `ctx_cost` + `ctx_heatmap` default-on (lokal), Retention-Limits, deterministische Token-Zählung.

## Lieferobjekte
- Rust crate `lean_ctx` (Library-first) + Binary `lean-ctx`.
- Website (Astro) liegt im separaten Deploy-Worktree/Branch; zählt/darstellt nur Manifest/SSOT-getrieben.

# lean-ctx — Project Brief

## Ziel
`lean-ctx` ist eine **Context Runtime for AI Agents**: lokale, deterministische Context-Orchestrierung, Tool-Governance und Knowledge/Workflow Rails über MCP (Model Context Protocol).

## Nicht-Ziele
- Keine Paywalls / Tiers / Feature-Gates
- Keine Cloud-Telemetrie (local-first)

## Kern-Fakten (April 2026)
- **MCP Tools**: 42 granular + 5 unified (`ctx` Meta-Tool + 4 primitives)
- **Read modes**: 10 (`auto|full|map|signatures|diff|aggressive|entropy|task|reference|lines:N-M`)
- **Runtime**: Rust, single-binary, SSOT für Tool-Liste über generiertes Manifest

## Qualitätsprinzipien
- Token-/Kontext-Effizienz als Default (kompakt, deterministisch, pagination-friendly)
- Evidence/Workflow: Fortschritt nur über nachweisbare Artefakte (Tool receipts / manuelle Evidence)
- Tool-Schemas und Tool-Dispatch müssen konsistent sein (Drift wird getestet)

# Project Brief — lean-ctx

## Core Identity

**lean-ctx** is a **Cognitive Filter for AI Engineering** — a hybrid MCP Server + Shell Hook written in Rust that reduces LLM token consumption by up to 99% through intelligent compression, caching, and AST-based extraction.

## Core Requirements

1. **MCP Server** (stdio) with 10 tools for AI editors (Cursor, Claude Code, Copilot, Windsurf, etc.)
2. **Shell Hook** via `lean-ctx -c` or aliases that compress CLI output using 75+ patterns across 20 categories
3. **Cross-platform** — native binaries for macOS (Intel + ARM), Linux (x64 + ARM), Windows (x64)
4. **Tree-sitter AST parsing** for 14 languages (TS/JS, Rust, Python, Go, Java, C, C++, Ruby, C#, Kotlin, Swift, PHP)
5. **Persistent stats** in `~/.lean-ctx/stats.json` with visual dashboards (terminal + web)
6. **Token Dense Dialect (TDD)** — compact output format using symbol shorthand

## Target Audience

- Developers using AI coding assistants (Cursor, Claude Code, GitHub Copilot, Windsurf, etc.)
- Power users who want to optimize LLM performance and reduce API costs
- Teams managing token budgets across multiple developers

## Business Model

- **Open Source** (MIT License) — core tool is free
- Revenue via future Pro/Team features (cloud dashboard, team analytics)

## Key Metrics

- 75+ compression patterns across 20 command categories
- 10 MCP tools
- 14 tree-sitter languages
- 23 auto-installed shell aliases
- 6 read modes (full, map, signatures, diff, aggressive, entropy) + line ranges

## Repository

- **Primary**: GitHub — https://github.com/yvgude/lean-ctx
- **Mirror**: GitLab — https://gitlab.pounce.ch/root/lean-ctx
- **Website**: https://leanctx.com
- **Crate**: https://crates.io/crates/lean-ctx
