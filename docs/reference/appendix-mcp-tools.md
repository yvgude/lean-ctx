# Appendix — MCP Tool Map (all 80 tools)

Every tool lean-ctx registers via `rust/src/server/registry.rs`. Your AI editor
calls these instead of its native file/search tools. The **Profile** column
shows the smallest tool profile that exposes the tool (`M` minimal, `S` standard,
`P` power). Set your profile with `lean-ctx tools <minimal|standard|power>`.

> Authoritative parameter schemas live in `rust/src/tools/registered/<tool>.rs`
> (`tool_def()`). This map is the human index.

## Tool profiles at a glance

| Profile | Count | Who it's for |
|---------|-------|--------------|
| **minimal** | 5 | Lowest context overhead; the absolute essentials |
| **standard** | 15 | Balanced default for most coding workflows |
| **power** | 76 | Everything (default for existing installs) |

- **minimal (5):** `ctx_read`, `ctx_shell`, `ctx_search`, `ctx_glob`, `ctx_tree`
- **standard (+10):** + `ctx_compose`, `ctx_explore`, `ctx_knowledge`, `ctx_callgraph`, `ctx_graph`, `ctx_delta`, `ctx_execute`, `ctx_expand`, `ctx_overview`, `ctx_url_read`
- **power (+48):** all remaining tools.

---

## 1. Core — read / search / shell

| Tool | Purpose | Key params / actions | Profile |
|------|---------|----------------------|---------|
| `ctx_read` | Read a file with session cache + compression; re-reads ~13 tokens when unchanged | `path`*, `mode` (full\|raw\|map\|signatures\|diff\|aggressive\|entropy\|task\|reference\|lines:N-M\|auto), `start_line`, `fresh` | M |
| `ctx_multi_read` | Read many files in one call (same modes) | `paths[]`*, `mode`, `fresh` | S |
| `ctx_smart_read` | Auto-pick the optimal read mode for a file | `path`* | P |
| `ctx_delta` | Incremental diff — only lines changed since last read | `path`* | S |
| `ctx_edit` | Search-and-replace edit (no native read/edit); preimage guards, backup | `path`*, `new_string`*, `old_string`, `replace_all`, `create` | S |
| `ctx_patch` | Hash-anchored line edits — `LINE:HASH` anchors from `ctx_read mode=anchored`; no exact-recall, batch-atomic, tree-sitter gate | `path`*, `ops[]` (set_line\|replace_lines\|insert_after\|delete\|replace_symbol), `validate_syntax` | P |
| `ctx_fill` | Budget-aware context fill within a token limit | `paths[]`, `budget`*, `task` | P |
| `ctx_symbol` | Read just one named symbol block (fn/struct/class) | `name`*, `file`, `kind` | P |
| `ctx_outline` | List all symbols of a file with signatures | `path`*, `kind` | P |
| `ctx_retrieve` | Fetch uncompressed original from cache (CCR) | `path`*, `query` | P |
| `ctx_shell` | Run shell commands with pattern compression | `command`*, `raw`, `cwd` | M |
| `shell` | Alias of `ctx_shell` (same compression) for clients whose model reaches for a native `shell`/`bash` tool — e.g. Codex Desktop / Codex Cloud | `command`*, `raw`, `cwd` | M |
| `ctx_search` | Regex search across the codebase, token-efficient | `pattern`*, `path`, `include` (glob, e.g. `*.{rs,ts}`), `ext` (deprecated alias), `max_results`, `ignore_gitignore` | M |
| `ctx_glob` | Find files by glob pattern (path match), gitignore-aware, multi-root, deterministically sorted | `pattern`*, `path`, `paths[]`, `max_results`, `ignore_gitignore` | P |
| `ctx_tree` | Compact directory tree with file counts | `path`, `depth`, `show_hidden` | M |
| `ctx_semantic_search` | Semantic search (BM25 + embeddings / hybrid) | `query`*, `action` (search\|reindex\|find_related), `mode` (bm25\|dense\|hybrid), `top_k` | S |
| `ctx_compose` | Task composer: keywords + ranked files + matches + top symbol | `task`*, `path` | P |
| `ctx_explore` | Iterative, deterministic exploration → compact `path:start-end` citations (BM25 + static graph + AST symbols, bounded turns); cheaper than `ctx_compose` for locating code across files | `query`*, `path`, `max_turns`, `citation` | S |
| `ctx_execute` | Sandboxed code execution (11 languages); only stdout enters context | `language`*, `code`*, `action`, `timeout` | S |
| `ctx_multi_repo` | Multi-repo management + cross-repo search (RRF) | `action` (add_root\|remove_root\|list_roots\|search\|status\|save_config) | P |
| `ctx_url_read` | Fetch a web page, PDF, RSS/Atom feed, or YouTube video as compressed, cited context (HTML→Markdown incl. GFM tables, PDF→text, feeds→dated items, transcript; GitHub blob/raw URLs auto-resolve to the raw file; facts/quotes carry confidence + source); SSRF-guarded | `url`*, `mode` (auto\|markdown\|text\|links\|facts\|quotes\|transcript), `query`, `max_tokens`, `max_items`, `timeout_secs` | S |
| `ctx_git_read` | Read a remote git repo via a cached shallow clone instead of scraping its web page | `url`*, `mode` (overview\|tree\|read\|grep), `path`, `ref`, `query`, `max_tokens` | P |

## 2. Memory & knowledge

| Tool | Purpose | Key actions | Profile |
|------|---------|-------------|---------|
| `ctx_knowledge` | Persistent project knowledge base across sessions | remember\|recall\|search\|relate\|consolidate\|timeline\|rooms\|wakeup\|status\|export\|remove | S |
| `ctx_compress` | Context checkpoint for long conversations | `include_signatures` | S |
| `ctx_compress_memory` | Compress memory/config files (CLAUDE.md, .cursorrules); backs up `.original.md` | `path`* | P |
| `ctx_artifacts` | Context-artifact registry with BM25 search | list\|status\|index\|reindex\|search\|remove | P |
| `ctx_index` | Build & manage the code index | status\|build\|build-full | P |

## 3. Session & multi-agent

| Tool | Purpose | Key actions | Profile |
|------|---------|-------------|---------|
| `ctx_session` | Cross-session memory (CCP): tasks, findings, decisions, snapshots | status\|load\|save\|task\|finding\|decision\|snapshot\|restore\|resume\|diff\|verify | M |
| `ctx_checkpoint` | Snapshot / diff / restore the agent's code changes via a shadow git history kept outside the project's `.git` | snapshot\|log\|diff\|restore; `message`, `from`, `to`, `ref`, `path`, `limit` | P |
| `ctx_agent` | Multi-agent coordination + message bus | register\|list\|post\|read\|handoff\|sync\|diary\|share_knowledge | S |
| `ctx_share` | Share cached file contexts between agents | push\|pull\|list\|clear | P |
| `ctx_task` | Multi-agent task orchestration (A2A) | create\|update\|list\|get\|cancel\|message | P |
| `ctx_handoff` | Context Ledger Protocol — deterministic handoff bundles | create\|show\|list\|pull\|export\|import | P |
| `ctx_workflow` | Workflow state machine with evidence tracking | start\|status\|transition\|complete\|evidence_add | P |

## 4. Code intelligence & graph

| Tool | Purpose | Key actions | Profile |
|------|---------|-------------|---------|
| `ctx_graph` | Unified code graph: deps, symbols, impact | build\|related\|symbol\|impact\|context\|diagram | P |
| `ctx_callgraph` | Call-graph queries (BFS, trace, risk) | callers\|callees\|trace\|risk | S |
| `ctx_impact` | Graph-based impact / blast-radius analysis | analyze\|diff\|chain\|build\|update\|status | S |
| `ctx_architecture` | Architecture analysis over the property graph | overview\|clusters\|layers\|cycles\|entrypoints\|hotspots\|health | S |
| `ctx_repomap` | PageRank-ranked map of the most important symbols | `max_tokens`, `focus_files[]` | S |
| `ctx_routes` | Extract HTTP routes (Express, Flask, FastAPI, Actix, Spring, Rails, Next.js) | `method`, `path` | S |
| `ctx_refactor` | LSP-backed refactoring | rename\|references\|definition\|implementations | S |
| `ctx_review` | Automated code review (impact, callers, tests, smells) | review\|diff-review\|checklist | P |
| `ctx_smells` | Code-smell detection (8 rules over property graph) | scan\|summary\|rules\|file | P |
| `ctx_pack` | Context Package Manager (PR packs, installable context) | pr\|create\|list\|info\|install\|export\|import\|auto_load | S |

## 5. Analytics & gain

| Tool | Purpose | Key actions | Profile |
|------|---------|-------------|---------|
| `ctx_metrics` | Session token stats, cache rates, per-tool savings | — | P |
| `ctx_radar` | Full context-budget breakdown (prompt, messages, tools, reads, shell) | `format` | P |
| `ctx_cost` | Local cost attribution per agent/tool | report\|agent\|tools\|reset | P |
| `ctx_gain` | Gain report incl. "Wrapped" summary | status\|report\|score\|wrapped\|agents\|json | P |
| `ctx_heatmap` | File-access heatmap | status\|directory\|cold\|json | P |
| `ctx_benchmark` | Benchmark compression modes for a file/project | `path`*, `action`, `format` | P |
| `ctx_analyze` | Entropy analysis — recommends optimal compression mode | `path`* | P |
| `ctx_compare` | Preview compression — original vs the bytes lean-ctx would emit, with token counts + line diff (read-only) | `path` \| `content`+`ext` \| `command`+`output` | P |
| `ctx_feedback` | Harness feedback for LLM output tokens & latency | record\|report\|reset | P |
| `ctx_discover` | Find missed compression opportunities in shell history | `limit` | P |
| `ctx_verify` | Verification observability + ContextProofV2 | stats\|proof\|v2 | P |
| `ctx_proof` | Export machine-readable ContextProofV1 | export* | P |

## 6. Advanced — providers / plugins / proactive context

| Tool | Purpose | Key actions | Profile |
|------|---------|-------------|---------|
| `ctx_provider` | External context providers (GitHub, GitLab, Jira, Postgres, MCP bridges) | discover\|list\|status\|refresh\|configure\|query\|gitlab_issues\|gitlab_mrs | P |
| `ctx_tools` | MCP Tool-Catalog Gateway — route/proxy unlimited downstream MCP servers at constant context cost | find\|call\|list\|refresh | P |
| `ctx_plugins` | Plugin management | list\|enable\|disable\|info\|hooks | P |
| `ctx_rules` | Cross-agent rules governance (ContextOps) | sync\|diff\|lint\|status\|init | P |
| `ctx_skillify` | Codify recurring session-diary + knowledge patterns into versioned, git-committable `.cursor/rules/skillify-*.mdc` (precision-biased, idempotent) | mine\|list\|status\|promote; `slug` | P |
| `ctx_summary` | Record + recall AI session summaries (semantic when warm, else lexical); auto-captured on the checkpoint cadence | recall\|record\|list; `query`, `top_k` | P |
| `ctx_package` | Save/resume portable context packages (session + summaries + knowledge bundle) for agent handoffs or session persistence | save\|resume\|list\|info; `path`, `description` | P |
| `ctx_overview` | Task-relevant project map — ideal at session start | `task`, `path` | S |
| `ctx_preload` | Proactively load task-relevant files; compact L-curve summary | `task`*, `path` | P |
| `ctx_prefetch` | Predictive prefetch for blast-radius files | `root`, `task`, `changed_files[]`, `budget_tokens` | P |

## 7. Meta — context-field-theory / dispatch / dynamic tools

| Tool | Purpose | Key actions | Profile |
|------|---------|-------------|---------|
| `ctx_call` | Call any lean-ctx tool by name (lazy-loading) | `name`*, `arguments` | P |
| `ctx_discover_tools` | Keyword search across all available tools | `query` | P |
| `ctx_load_tools` | Load/unload dynamic tool categories at runtime | load\|unload\|list; `category` | P |
| `ctx_control` | Context Field Theory — overlay-based context manipulation | exclude\|include\|pin\|unpin\|set_view\|set_priority\|reset\|list | P |
| `ctx_plan` | Context planning (CFT) with Phi scoring + budget allocation | `task`*, `budget`, `profile` | P |
| `ctx_compile` | Context compilation via knapsack + Boltzmann view selection | `mode`, `budget` | P |
| `ctx_context` | Session-context overview (cache, seen files, state) | — | P |
| `ctx_ledger` | Context-ledger ops for pressure management | status\|reset\|evict | P |
| `ctx_cache` | Session-cache operations | status\|clear\|invalidate | P |
| `ctx_dedup` | Cross-file deduplication | analyze\|apply | P |
| `ctx_intent` | Structured intent input with routing policy | `query`*, `format` | P |
| `ctx_response` | Compress LLM response text (strip filler, TDD) | `text`* | P |
| `ctx_expand` | Zero-loss retrieval of archived tool outputs | retrieve\|list\|search_all | P |

`*` = required parameter.

## Notes

1. `power` enables all 80 tools; `ToolProfile::is_tool_enabled()` returns `true`
   for everything under power.
2. `ctx_load_tools` controls *dynamic* categories (`arch`, `debug`, `memory`,
   `metrics`, `session`) independently of the static profile filter.
3. Lazy clients use `ctx_call` + `ctx_discover_tools` + `ctx_load_tools` to reach
   tools not in their active profile without listing all 79 upfront.
