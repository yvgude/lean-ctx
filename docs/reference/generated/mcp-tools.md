# Appendix — MCP Tools (generated)

<!-- GENERATED FILE — do not edit by hand. Run: `cargo run --example gen_docs --features dev-tools` -->

Source of truth: `rust/src/server/registry.rs` and the tool definitions it registers.

lean-ctx registers **76 MCP tools** (granular profile). Each entry below lists the tool name, what it does, and its parameters (`*` marks required).

## `ctx_agent`

Multi-agent coordination: shared message bus, persistent diaries, stigmergic scent field. Actions: register (agent_type+role), post (message+category), read (poll), status (active|idle|finished), handoff (transfer task+summary), sync (agents + messages + scent: claims/stuck/hot), claim/release (atomic file/task claim, message=target), brief (sub-agent briefing pack: message=task, priority=budget), return (distill sub-agent report into knowledge: message='category/key: value' lines), diary (log discovery/decision/blocker/progress/insight), recall_diary, diaries, list, info.

Parameters: `action`*, `agent_type`, `category`, `message`, `role`, `status`, `to_agent`

## `ctx_analyze`

Entropy analysis — recommends optimal compression mode for a file.

Parameters: `path`*

## `ctx_architecture`

Graph-based architecture analysis. Actions: overview|clusters|communities|layers|cycles|entrypoints|hotspots|health|module.

Parameters: `action`, `format`, `path`, `root`

## `ctx_artifacts`

Context artifact registry + BM25 index. Actions: list|status|index|reindex|search|remove.

Parameters: `action`*, `format`, `name`, `project_root`, `query`, `top_k`

## `ctx_benchmark`

Benchmark compression modes for a file or project.

Parameters: `action`, `format`, `path`*

## `ctx_cache`

Cache ops: status|clear|invalidate.

Parameters: `action`*, `path`

## `ctx_call`

Invoke any non-core lean-ctx tool by name.
arch: ctx_architecture, ctx_impact, ctx_callgraph, ctx_refactor, ctx_symbol, ctx_routes, ctx_smells
debug: ctx_benchmark, ctx_verify, ctx_analyze, ctx_profile, ctx_review
memory: ctx_semantic_search, ctx_artifacts
batch: ctx_fill, ctx_execute, ctx_pack, ctx_plan, ctx_compile
agent: ctx_agent, ctx_share, ctx_task, ctx_handoff, ctx_workflow
util: ctx_compress, ctx_cache, ctx_metrics, ctx_dedup, ctx_cost, ctx_heatmap, ctx_preload
Discover more: name=ctx_discover_tools, arguments={query}.

Parameters: `arguments`, `name`*

## `ctx_callgraph`

Call graph query: callers/callees (multi-hop BFS), trace path between symbols, risk classification by caller count.

Parameters: `action`, `depth`, `direction`, `file`, `from`, `symbol`, `to`

## `ctx_checkpoint`

Local shadow git history of the agent's changes (separate from the user's .git).
actions: snapshot (record current state) | log (list checkpoints) | diff (vs a checkpoint) | restore (revert files).
Snapshot before+after a change to capture exactly what the LLM modified; diff/restore to review or roll back.
Never touches the user's repository.

Parameters: `action`, `from`, `limit`, `message`, `path`, `ref`, `to`

## `ctx_compile`

Context compilation (CFT). Builds minimal context package via greedy knapsack + Boltzmann view selection. Modes: handles|compressed|full.

Parameters: `budget`, `mode`

## `ctx_compose`

Task composer: one call returns keywords + semantically ranked files + exact match locations + the top symbol's body inline. Replaces the search→read→outline→read chain.

Parameters: `path`, `task`*

## `ctx_compress`

Context checkpoint for long conversations.

Parameters: `include_signatures`

## `ctx_compress_memory`

Compress a memory/config file (CLAUDE.md, .cursorrules) preserving code, URLs, paths. Creates .original.md backup.

Parameters: `path`*

## `ctx_context`

Session context overview — cached files, seen files, session state.

Parameters: _none_

## `ctx_control`

Universal context manipulation (Context Field Theory). Actions: exclude|include|pin|unpin|set_view|set_priority|mark_outdated|reset|list|history. Overlay-based, reversible, scoped.

Parameters: `action`*, `reason`, `scope`, `target`, `value`

## `ctx_cost`

Cost attribution (local-first). Actions: report|agent|tools|json|reset.

Parameters: `action`, `agent_id`, `limit`

## `ctx_dedup`

Cross-file dedup: analyze or apply shared block references.

Parameters: `action`

## `ctx_delta`

Incremental diff — sends only changed lines since last read.

Parameters: `path`*

## `ctx_discover`

Find missed compression opportunities in shell history.

Parameters: `limit`

## `ctx_discover_tools`

Search available lean-ctx tools by keyword. Returns matching tool names and descriptions.

Parameters: `query`

## `ctx_edit`

Edit a file via search-and-replace. Use when the IDE's Edit tool requires Read but Read is unavailable.

Parameters: `create`, `new_string`*, `old_string`, `path`*, `replace_all`

## `ctx_execute`

Run code in sandbox (11 languages). Only stdout enters context. Raw data never leaves subprocess. Languages: javascript, typescript, python, shell, ruby, go, rust, php, perl, r, elixir.

Parameters: `action`, `code`, `intent`, `items`, `language`, `path`, `timeout`

## `ctx_expand`

Retrieve archived/firewalled tool output (zero-loss). Use the ID from an [Archived:/Firewalled: ...] hint.

Parameters: `action`, `end_line`, `head`, `id`, `json_keys`, `json_path`, `query`, `search`, `session_id`, `start_line`, `tail`

## `ctx_feedback`

Harness feedback for LLM output tokens/latency (local-first). Actions: record|report|json|reset|status.

Parameters: `action`, `agent_id`, `intent`, `latency_ms`, `limit`, `llm_input_tokens`, `llm_output_tokens`, `model`, `note`

## `ctx_fill`

Budget-aware context fill — auto-selects compression per file within token limit.

Parameters: `budget`*, `paths`*, `task`

## `ctx_gain`

Gain report (includes Wrapped via action=wrapped).

Parameters: `action`, `limit`, `model`, `period`

## `ctx_git_read`

Read a remote git repository via a cached shallow clone (not HTML scraping).
modes: overview (tree + README) | tree (file list) | read (a file) | grep (search).
Accepts repo URLs and GitHub/GitLab blob/tree links (ref + path auto-detected). https-only, SSRF-guarded, bounded.
Use instead of ctx_url_read when you need a whole repo's files/structure.

Parameters: `max_tokens`, `mode`, `path`, `query`, `ref`, `timeout_secs`, `url`*

## `ctx_glob`

Find files by glob pattern. Prefer over native Glob for consistency.
Respects .gitignore; supports multi-root via `paths` array.

Parameters: `ignore_gitignore`, `max_results`, `path`, `paths`, `pattern`*

## `ctx_graph`

Code graph: dependencies, symbol usages, impact/blast radius, Mermaid diagrams, git-diff impact.

Parameters: `action`*, `depth`, `format`, `kind`, `path`, `project_root`, `since`, `to`

## `ctx_handoff`

Context Ledger Protocol (hashed, deterministic, local-first). Actions: create|show|list|pull|clear|export|import.

Parameters: `action`, `apply_knowledge`, `apply_session`, `apply_workflow`, `filename`, `format`, `path`, `paths`, `privacy`, `write`

## `ctx_heatmap`

File access heatmap — shows most frequently accessed files.

Parameters: `action`, `path`

## `ctx_impact`

Graph-based impact analysis. Actions: analyze|diff|chain|build|update|status.

Parameters: `action`, `depth`, `format`, `path`, `root`

## `ctx_index`

Index orchestration. Actions: status|build|build-full.

Parameters: `action`*, `project_root`

## `ctx_intent`

Structured intent input (optional) — submit compact JSON or short text; server also infers intents automatically from tool calls.

Parameters: `project_root`, `query`*

## `ctx_knowledge`

Persistent project knowledge across sessions (facts, patterns, gotchas, typed relations).

Parameters: `action`*, `as_of`, `category`, `confidence`, `examples`, `key`, `mode`, `pattern_type`, `query`, `resolution`, `severity`, `trigger`, `value`

## `ctx_ledger`

Context ledger ops: status|reset|evict. Manages persistent context pressure.

Parameters: `action`*, `targets`

## `ctx_load_tools`

Load/unload specialized tool categories on demand. Categories: arch, debug, memory, metrics, session. Core is always loaded.

Parameters: `action`*, `category`

## `ctx_metrics`

Session token stats, cache rates, per-tool savings.

Parameters: _none_

## `ctx_multi_read`

Batch read files in one call. Same modes as ctx_read.

Parameters: `fresh`, `mode`, `paths`*

## `ctx_multi_repo`

Multi-repo management: add/remove roots, cross-repo search with Reciprocal Rank Fusion (RRF). Enables searching across multiple project directories simultaneously.

Parameters: `action`*, `alias`, `max_results`, `path`, `query`, `roots`

## `ctx_outline`

List all symbols in a file (functions, structs, classes, methods) with signatures. Much fewer tokens than reading the full file.

Parameters: `kind`, `path`*

## `ctx_overview`

Task-relevant project map — use at session start.

Parameters: `path`, `task`

## `ctx_pack`

Context Package Manager. Actions: pr (PR context), create (build package from project), list, info, remove, install, export, import, auto_load, summary.

Parameters: `action`*, `apply`, `author`, `base`, `depth`, `description`, `diff`, `enable`, `file`, `format`, `layers`, `level`, `name`, `project_root`, `scope`, `tags`, `version`

## `ctx_package`

Save or resume portable context packages — self-contained JSON bundles with session state, summaries, and knowledge. Use to hand off context between agents, persist session snapshots for later, or onboard a new agent into a previous session's context. Actions: save (export current session), resume (import from a package file), list (show saved packages), info (inspect a package without importing).

Parameters: `action`, `description`, `path`

## `ctx_plan`

Context planning (CFT). Computes optimal context plan with Phi scoring, budget allocation, and policy-driven view selection.

Parameters: `budget`, `profile`, `task`*

## `ctx_plugins`

Plugin management. Actions: list (show installed plugins), enable (activate a plugin), disable (deactivate a plugin), info (show plugin details), hooks (list available hook points).

Parameters: `action`*, `name`

## `ctx_prefetch`

Predictive prefetch — prewarm cache for blast radius files (graph + task signals) within budgets.

Parameters: `budget_tokens`, `changed_files`, `max_files`, `root`, `task`

## `ctx_preload`

Proactive context loader — caches task-relevant files, returns L-curve-optimized summary (~50-100 tokens vs ~5000 for individual reads).

Parameters: `path`, `task`*

## `ctx_proof`

Export a machine-readable ContextProofV1 (Verifier + SLO + Pipeline + Provenance). Writes to .lean-ctx/proofs/ by default.

Parameters: `action`*, `filename`, `format`, `max_evidence`, `max_ledger_files`, `project_root`, `write`

## `ctx_provider`

External context providers (GitHub, GitLab, Jira, Postgres, MCP, custom REST).

Parameters: `action`*, `iid`, `labels`, `limit`, `mode`, `provider`, `resource`, `state`, `status`

## `ctx_radar`

Full context budget breakdown: system prompt, messages, tools, reads, shell — all tracked token usage.

Parameters: `format`

## `ctx_read`

Read a file. Prefer over native Read/cat/head/tail (cached, compressed).
Unchanged re-reads cost ~13 tokens. Auto-selects mode. fresh=true forces a disk re-read.

Parameters: `fresh`, `mode`, `path`*, `start_line`

## `ctx_refactor`

LSP/IDE refactoring. action=one pipe-delimited value below. Reads (references/definition/implementations/declaration/type_hierarchy/symbols_overview/inspections) need a language server or the JetBrains backend. Symbol edits (replace/insert_before/insert_after_symbol) are name_path-addressed, IDE-first with a lossless headless fallback. Two-Phase ops (rename/move/safe_delete/inline _preview+_apply) need a JetBrains IDE (else BACKEND_REQUIRED) with a stateless plan_hash TOCTOU guard. rename/move/safe_delete block conflicts unless force=true; inline cannot be forced (→ UNSUPPORTED). reformat is Single-Phase, by name_path | path | path+line.

Parameters: `action`*, `column`, `direction`, `end_line`, `expected_hash`, `force`, `keep_definition`, `line`, `mode`, `name_path`, `new_body`, `new_name`, `optimize_imports`, `path`, `plan_hash`, `propagate`, `scope`, `search_comments`, `search_text_occurrences`, `target_parent`, `target_path`, `text`

## `ctx_repomap`

PageRank-based repo map showing the most important symbols across the codebase, ranked by structural importance and session relevance.

Parameters: `focus_files`, `max_tokens`, `path`

## `ctx_response`

Compress LLM response text (structural de-duplication).

Parameters: `text`*

## `ctx_retrieve`

Retrieve original uncompressed content from the session cache (CCR). Use when a compressed ctx_read output is insufficient.

Parameters: `path`*, `query`

## `ctx_review`

Automated code review: combines impact analysis, caller tracking, and test discovery. Actions: review (single file), diff-review (from git diff), checklist (structured review questions).

Parameters: `action`*, `depth`, `path`

## `ctx_routes`

List HTTP routes/endpoints extracted from the project. Supports Express, Flask, FastAPI, Actix, Spring, Rails, Next.js.

Parameters: `method`, `path`

## `ctx_rules`

Cross-agent rules governance (ContextOps). Actions: sync (distribute rules to agents), diff (show drift), lint (check consistency), status (show sync state), init (create central config).

Parameters: `action`*, `agent`

## `ctx_search`

Regex code search. Prefer over native Grep/rg/find (compact, .gitignore-aware).

Parameters: `ext`, `ignore_gitignore`, `include`, `max_results`, `path`, `paths`, `pattern`*

## `ctx_semantic_search`

Semantic code search (BM25 + embeddings/hybrid + reranking). action=reindex|find_related.

Parameters: `action`, `file_path`, `languages`, `line`, `mode`, `path`, `path_glob`, `query`*, `top_k`

## `ctx_session`

Cross-session memory: record task/finding/decision, restore previous session state.

Parameters: `action`*, `session_id`, `value`

## `ctx_share`

Share cached file contexts between agents. Actions: push (share files from your cache to another agent), pull (receive files shared by other agents), list (show all shared contexts), clear (remove your shared contexts).

Parameters: `action`*, `message`, `paths`, `to_agent`

## `ctx_shell`

Run a shell command. Prefer over native Shell/Bash (compressed output).
cwd persists across calls.

Parameters: `command`*, `cwd`, `env`, `raw`

## `ctx_skillify`

Codify recurring patterns from this project's session diary + knowledge into versioned, git-committable .cursor/rules/skillify-*.mdc files. Actions: mine (distill & write/merge rules), list (show generated rules), status (config + counts), promote (copy a project rule to ~/.cursor/rules). Precision-biased; only acts when invoked; re-runs are idempotent.

Parameters: `action`, `slug`

## `ctx_smart_read`

Auto-select optimal read mode for a file.

Parameters: `path`*

## `ctx_smells`

Code smell detection. Actions: scan|summary|rules|file.

Parameters: `action`, `format`, `path`, `root`, `rule`

## `ctx_summary`

Record and recall AI session summaries — compact, semantically-recallable digests of what was done (task, files, decisions, next steps). Actions: recall (find past summaries by query; semantic when embeddings are warm, else lexical), record (snapshot the current session now), list (recent summaries). Summaries are also captured automatically on the checkpoint cadence.

Parameters: `action`, `query`, `top_k`

## `ctx_symbol`

Read a specific symbol (function, struct, class) by name. Returns only the symbol code block instead of the entire file. 90-97% fewer tokens than full file read.

Parameters: `file`, `kind`, `name`*

## `ctx_task`

Multi-agent task orchestration. Actions: create|update|list|get|cancel|message|info.

Parameters: `action`*, `description`, `message`, `state`, `task_id`, `to_agent`

## `ctx_tools`

Gateway to downstream MCP servers — unlimited external tools at ~constant context cost.
actions: find (query → top-N relevant tools as ChoiceCards) | call (proxy a `server::tool`) | list (servers+counts) | refresh.
Use find to discover, then call the chosen `server::tool`. Off by default ([gateway] config).

Parameters: `action`, `arguments`, `query`, `tool`

## `ctx_tree`

List a directory. Prefer over native ls/find (counts, compact tree).

Parameters: `depth`, `path`, `paths`, `respect_gitignore`, `show_hidden`

## `ctx_url_read`

Fetch a web page, PDF, RSS/Atom feed, or YouTube URL as compressed, cited context.
HTML→clean Markdown (tables→GFM), PDF→text, feeds→dated item list, YouTube→transcript; modes: auto|markdown|text|links|facts|quotes|transcript.
GitHub blob/raw page URLs auto-resolve to the raw file. facts/quotes return claims with confidence + source. SSRF-guarded (http/https only, blocks private/loopback).
Use for research/crawl instead of raw fetch.

Parameters: `max_items`, `max_tokens`, `mode`, `query`, `timeout_secs`, `url`*

## `ctx_verify`

Verification observability. Actions: stats (tool call statistics), proof|v2 (ContextProofV2 claim-based verification with Lean4 proofs).

Parameters: `action`, `format`

## `ctx_workflow`

Workflow rails (state machine + evidence). Actions: start|status|transition|complete|evidence_add|evidence_list|stop.

Parameters: `action`, `key`, `name`, `spec`, `to`, `value`

## `shell`

Execute a shell command. Returns token-optimized compressed output (95+ patterns for git, npm, cargo, docker, tsc, etc). Equivalent to running the command in a terminal but with automatic output compression for efficiency.

Parameters: `command`*, `cwd`

