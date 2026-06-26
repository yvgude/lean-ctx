# Appendix — MCP Tools (generated)

<!-- GENERATED FILE — do not edit by hand. Run: `cargo run --example gen_docs --features dev-tools` -->

Source of truth: `rust/src/server/registry.rs` and the tool definitions it registers.

lean-ctx registers **78 MCP tools** (granular profile). Each entry below lists the tool name, what it does, and its parameters (`*` marks required).

## `ctx_agent`

Multi-agent coordination — shared message bus, persistent diaries, stigmergic scent field.
WORKFLOW: register agents first, then post/read messages, sync for state alignment.
Actions: register (agent_type+role), post (message+category), read (poll),
status (active|idle|finished), handoff (task+summary), sync (agents+messages+scent),
claim/release (file/task), brief (sub-agent briefing),
return (distill→knowledge), diary|recall_diary|diaries (agent journal),
share_knowledge|receive_knowledge (cross-agent), list, info.
ANTIPATTERN: NOT for single-agent workflows. Use ctx_compose for code understanding.

Parameters: `action`*, `agent_type`, `category`, `message`, `role`, `status`, `to_agent`

## `ctx_analyze`

Entropy analysis — recommends optimal compression mode for a file path.
WORKFLOW: Use BEFORE ctx_read to pick the best mode (full/signatures/auto).
Saves tokens by selecting the mode that minimizes size while retaining information.

Parameters: `path`*

## `ctx_architecture`

Architecture analysis — understand module structure without reading every file.
WORKFLOW: use ctx_compose FIRST for code understanding; ctx_architecture for high-level structure.
action=overview→high-level; clusters|communities→groupings;
layers|cycles→dependency violations; entrypoints|hotspots→risk areas;
health→quality; module path='src/' to zoom into a specific module.
ANTIPATTERN: does NOT show source code — only structural relationships.

Parameters: `action`, `format`, `path`, `root`

## `ctx_artifacts`

Context artifact registry with BM25 search — manage and query indexed code artifacts.
WORKFLOW: index artifacts first (index/reindex), then search with query for semantic retrieval.
Actions: list|status|index|reindex|search|remove.
ANTIPATTERN: NOT for general code search — use ctx_semantic_search for codebase queries.

Parameters: `action`*, `format`, `name`, `project_root`, `query`, `top_k`

## `ctx_benchmark`

Benchmark compression modes — measures token savings across all available modes for a file or project.
WORKFLOW: use BEFORE ctx_read to pick the optimal compression strategy.
Provide a file path, or use action=project for project-wide results.
ANTIPATTERN: NOT for production profiling — measures compression, not runtime performance.

Parameters: `action`, `format`, `path`*

## `ctx_cache`

Cache operations — inspect, clear, or invalidate the read cache.
Actions: status lists cached files; clear empties all (recover token budget);
invalidate path=... refreshes a single entry.
Use to diagnose stale content or recover budget after large reads.
ANTIPATTERN: does NOT affect disk files — only cached read content.

Parameters: `action`*, `path`

## `ctx_call`

Invoke any non-core lean-ctx tool by name — for tools not exposed as standalone MCP tools.
Categories: arch, debug, memory, batch, agent, util. Find exact names with
ctx_discover_tools (query=keyword; empty query lists all). Cannot invoke itself.

Parameters: `arguments`, `name`*

## `ctx_callgraph`

Callers/callees analysis — who calls a function and what it calls.
action=callers symbol='fn' returns every call site with file:line.
For END-TO-END flow tracing (how does X reach Y), use ctx_compose FIRST
— one call returns the path + source. Use ctx_callgraph only when you need
exhaustive enumeration of ALL callers/callees for a single symbol.
action=trace from→to finds path between two symbols. depth=N for BFS depth.

Parameters: `action`, `depth`, `file`, `from`, `symbol`, `to`

## `ctx_checkpoint`

Local shadow git history of the agent's changes — separate from the user's .git.
WORKFLOW: snapshot before+after changes to capture exactly what was modified.
Actions: snapshot (record current state), log (list checkpoints with SHAs),
diff from=... to=... (compare checkpoints), restore ref=... (revert files).
ANTIPATTERN: Never touches the user's repository — completely isolated shadow history.

Parameters: `action`, `from`, `limit`, `message`, `path`, `ref`, `to`

## `ctx_compile`

Build minimal context package within token budget. Modes: handles (references), compressed (content), full (all cached).
WORKFLOW: after ctx_read/ctx_compose, package focused context for handoff/subagent.
ANTIPATTERN: not for exploration — use ctx_compose/ctx_read first.

Parameters: `budget`, `mode`

## `ctx_compose`

PRIMARY TOOL — call FIRST for understanding code (before editing/debugging/'how does X work').
Returns ranked files with relevant symbol source inline grouped by file.
Combines BM25 lexical+semantic+associative retrieval+submodular optimization.
ANTIPATTERN: Do NOT chain search→read→symbol — one compose replaces the whole chain.
ANTIPATTERN: Do NOT Read files whose source compose already returned — it IS the source.
WORKFLOW: Fire parallel ctx_read or ctx_compose for different areas.

Parameters: `path`, `task`*

## `ctx_compress`

Compress read cache to free token budget. Does not affect session state or knowledge.
WORKFLOW: check budget with ctx_context first, then reclaim space.

Parameters: `include_signatures`

## `ctx_compress_memory`

Compress memory/config file (CLAUDE.md, .cursorrules) preserving code, URLs, and paths. Creates .original.md backup.
WORKFLOW: check token overhead with ctx_context, then compress to reduce persistent instruction cost.

Parameters: `path`*

## `ctx_context`

Session context overview — cached files, seen files, session state, CRP mode.
WORKFLOW: track context budget periodically — use before ctx_compress/ctx_compile.
ANTIPATTERN: not for reading file content — use ctx_read or ctx_compose.

Parameters: _none_

## `ctx_control`

Fine-tune context — exclude, include, pin, unpin, set_view, set_priority, mark_outdated, reset, list, history.
Overlay-based, reversible, scoped to call/session/project.
WORKFLOW: after ctx_compose, exclude low-relevance files.
ANTIPATTERN: not for initial context building — use ctx_compose/ctx_read first.

Parameters: `action`*, `reason`, `scope`, `target`, `value`

## `ctx_cost`

Cost attribution — track tokens and cost per agent/tool call. Local-first, no external billing.
Actions: report (summary), agent (per-agent), tools (per-tool), json (machine), status (live), reset (zero).
WORKFLOW: call report to find top cost drivers, then agent/tools for detail.

Parameters: `action`, `agent_id`, `limit`

## `ctx_dedup`

WORKFLOW: action=analyze first to find shared imports/code across files, then action=apply to register dedup hints for ctx_read output.
ANTIPATTERN: NOT for permanent dedup — only compression hints for read output.

Parameters: `action`

## `ctx_delta`

Incremental diff since last read — shows only changed lines after you edit.
WORKFLOW: ctx_read(mode=full) -> edit -> ctx_delta (no re-read needed).
Use INSTEAD of re-reading the whole file after modifications — saves 90%+ tokens
on unchanged content. Path must have a prior ctx_read in this session's cache.
For the full git diff against HEAD, use ctx_read(path, mode=diff) instead.

Parameters: `path`*

## `ctx_discover`

Find shell commands not yet using lean-ctx compression — use when context feels bloated.
Shows which commands would save tokens via lean-ctx patterns. limit=N caps results.
ANTIPATTERN: not for finding compression bugs — reports missed opportunities only.
Run 'lean-ctx init --global' to auto-compress all commands.

Parameters: `limit`

## `ctx_discover_tools`

WORKFLOW: call FIRST when unsure which tool fits your task — lists all tools on empty query.
Then use ctx_call to invoke discovered tools (for static-tool-list clients).
ANTIPATTERN: not for runtime invocation — use ctx_call(name=..., arguments=...) directly.

Parameters: `query`

## `ctx_edit`

Search-and-replace edit with race-condition guards — for simple text replacement in a single file.
old_string must be unique unless replace_all=true. create=true writes new files.
backup creates .bak. MD5/size/mtime pre-guards prevent race conditions.
ANTIPATTERN: Do NOT loop on failures — verify file content and adjust old_string, or use native Edit with prior Read.
For LSP-aware refactoring (rename, move, inline), use ctx_refactor.

Parameters: `create`, `new_string`*, `old_string`, `path`*, `replace_all`

## `ctx_execute`

Run code in sandbox (11 languages) — use when conditionals, multi-line or cross-language transforms.
ANTIPATTERN: for simple one-liners, prefer ctx_shell (lower overhead, auto-compressed).
action=code (default) for one-shot; action=batch for parallel multi-language;
action=file to process a project file (extension auto-detects).
Pass intent to focus large output and save tokens. Languages: javascript,
typescript, python, shell, ruby, go, rust, php, perl, r, elixir.

Parameters: `action`, `code`, `intent`, `items`, `language`, `path`, `timeout`

## `ctx_expand`

Retrieve archived tool output by ID (e.g. id=@F1 from [Archived:ID] hints).
WORKFLOW: see [Archived:ID] → ctx_expand id=ID to restore full content.
Supports head/tail/search to filter lines and save tokens on re-read.
action=list browses all archives. action=search_all queries across archives.
Zero-loss: original preserved.
ANTIPATTERN: not for reading project files — use ctx_read or ctx_compose.

Parameters: `action`, `end_line`, `head`, `id`, `json_keys`, `json_path`, `query`, `search`, `session_id`, `start_line`, `tail`

## `ctx_explore`

Iterative, deterministic code exploration → compact file:line citations.
Runs a bounded multi-turn loop (BM25 + static call/import graph + AST symbols)
and returns a <final_answer> block of `path:start-end` spans instead of bodies.
USE WHEN: locating WHERE behavior lives across many files, cheaply.
vs ctx_compose: compose inlines bodies in one shot; explore returns citations
over N turns (far fewer tokens). citation=true emits only the block.

Parameters: `citation`, `max_turns`, `path`, `query`*

## `ctx_feedback`

Record and report LLM token/latency metrics — use to track efficiency and optimize context usage.
WORKFLOW: action=record during each LLM call, then action=report for readable summary.
Actions: record (log event), report (readable summary), json (machine-readable),
reset (clear data), status (storage info).
ANTIPATTERN: not for debugging code behavior — this tracks token/latency stats only.
record requires llm_input_tokens + llm_output_tokens.

Parameters: `action`, `agent_id`, `intent`, `latency_ms`, `limit`, `llm_input_tokens`, `llm_output_tokens`, `model`, `note`

## `ctx_fill`

Budget-aware context fill — compress N files to fit a token budget.
WORKFLOW: pass paths[] + budget=N; task="..." enables intent-driven pruning.
ANTIPATTERN: does NOT decide which files to include — use ctx_plan for project-wide selection.
Saves tokens vs per-file reads (for many files with a budget).

Parameters: `budget`*, `paths`*, `task`

## `ctx_gain`

Gain report — shows token savings from lean-ctx compression.
action=wrapped for periodic/annual summary. Other actions: status|report|score|cost|tasks|heatmap|agents|json.
period="week"|"month"|"all" scopes the report.

Parameters: `action`, `limit`, `model`, `period`

## `ctx_git_read`

Read remote git repos via cached shallow clone (not HTML scraping).
modes: overview (tree + README) | tree (file list) | read (file content) | grep (search).
Accepts repo URLs and GitHub/GitLab blob/tree links (ref+path auto-detected).
https-only, SSRF-guarded. Prefer over ctx_url_read for whole-repo access.

Parameters: `max_tokens`, `mode`, `path`, `query`, `ref`, `timeout_secs`, `url`*

## `ctx_glob`

Find files by glob pattern — locate by name or extension.
Respects .gitignore. Supports multi-root via paths array. max_results=N sets limit.
For file content search, use ctx_search (pattern) or ctx_semantic_search (meaning).

Parameters: `ignore_gitignore`, `max_results`, `path`, `paths`, `pattern`*

## `ctx_graph`

Graph queries — find dependencies, relationships, and symbols.
action=symbol path="file.rs::fnName" returns the source (NOT usages).
action=neighbors path="file.rs" shows import neighbors with direction & confidence.
action=impact path="file.rs" shows reverse dependency tree (blast radius).
action=path from→to shows shortest dependency chain between two files.
action=diff since=HEAD~1 for git change impact.
action=diagram kind=deps|calls renders a Mermaid diagram.
For understanding code, use ctx_compose FIRST. Use ctx_graph for targeted structural queries.
ANTIPATTERN: symbol returns only the DEFINITION — not usages. For REFERENCES use grep or ctx_compose.

Parameters: `action`*, `depth`, `format`, `kind`, `path`, `project_root`, `since`, `to`

## `ctx_handoff`

Context handoff protocol (hashed, deterministic, local-first).
Actions: create|show|list|pull|clear|export|import. Stores curated file refs with hashes.
Before ending a session or handing off to another agent.

Parameters: `action`, `apply_knowledge`, `apply_session`, `apply_workflow`, `filename`, `format`, `path`, `paths`, `privacy`, `write`

## `ctx_heatmap`

File access heatmap — shows most frequently accessed files per session.
action=status (default) for summary, action=detail for per-file access counts.
Identify hot files to optimize context usage.

Parameters: `action`, `path`

## `ctx_impact`

Change impact analysis — assess blast radius before refactoring.
action=analyze path="file.rs" maps downstream dependents; action=diff compares git refs;
action=chain traces from→to dependency paths. depth controls traversal (default 5).

Parameters: `action`, `depth`, `format`, `path`, `root`

## `ctx_index`

Index orchestration — manage code graph index.
WORKFLOW: status → build → build-full (escalate if stale).
ANTI-PATTERN: build-full is expensive — use incremental build first.
Actions: status (state), build (incremental), build-full (rebuild).

Parameters: `action`*, `project_root`

## `ctx_intent`

Submit task goals as JSON or short text — server infers from tool calls.
ANTI-PATTERN: not needed for simple tasks.
query=task|JSON; format=json for JSON output; project_root=scope.

Parameters: `format`, `project_root`, `query`*

## `ctx_knowledge`

Persistent memory across sessions — remember decisions, patterns, and facts for recall.
WORKFLOW: save after completing significant tasks; recall at session start.
action=remember key='X' value='Y' saves a fact (both required).
action=recall query='X' retrieves it. action=status shows all categories.
action=gotcha trigger='X' resolution='Y' for known pitfalls.
mode=semantic|exact for recall. category groups related facts.

Parameters: `action`*, `as_of`, `category`, `confidence`, `examples`, `key`, `mode`, `pattern_type`, `query`, `resolution`, `severity`, `trigger`, `value`

## `ctx_ledger`

Context ledger — track persistent context pressure.
WORKFLOW: status → evict → reset (reset only if budget needs full flush).
ANTI-PATTERN: don't evict files you actively need — check status first.
Actions: status, reset, evict.

Parameters: `action`*, `targets`

## `ctx_load_tools`

Load/unload specialized tool categories to reduce surface area.
WORKFLOW: list → load → unload when done.
ANTI-PATTERN: don't unload categories you're actively using.
Actions: load|unload|list. Categories: arch, debug, memory, metrics, session.
Core is always loaded.

Parameters: `action`*, `category`

## `ctx_metrics`

Session token statistics — cache hit rates, per-tool savings, pipeline metrics,
and signature backend ratios.
ANTI-PATTERN: not for real-time monitoring — snapshot of current session.
Complements ctx_radar for budget analysis.

Parameters: _none_

## `ctx_multi_read`

DEPRECATED → use ctx_read with paths=['a.rs','b.rs']. Folded into ctx_read
(#509); hidden from tools/list, still callable for one release.

Parameters: `fresh`, `mode`, `paths`*

## `ctx_multi_repo`

Multi-repository — add, remove, search project directories.
WORKFLOW: list_roots → add_root/remove_root → search.
ANTI-PATTERN: not for single-repo projects — use ctx_search.
Actions: add_root|remove_root|list_roots|search|status|save_config.
Cross-repo search uses RRF to merge results.

Parameters: `action`*, `alias`, `max_results`, `path`, `query`, `roots`

## `ctx_outline`

WORKFLOW: call BEFORE ctx_read to preview API surface.
ANTIPATTERN: NOT for file content (use ctx_read) or deep understanding (use ctx_compose).
Returns fn/struct/class/trait signatures + line numbers via tree-sitter.
kind=fn|struct|class|all filters. Saves tokens: only the API surface.

Parameters: `kind`, `path`*

## `ctx_overview`

WORKFLOW: call at session START before ctx_compose/ctx_read.
ANTIPATTERN: NOT for source code — structure only. Use ctx_compose for code understanding.
Project map — task='your goal' scopes files by relevance (PageRank on symbol graph).
High-level structure only, no source body. ~10x cheaper than ctx_compose.

Parameters: `path`, `task`

## `ctx_pack`

WORKFLOW: create -> export -> import -> install for sharing context state.
ANTIPATTERN: NOT for ephemeral session save (use ctx_session).
Context Package Manager — create, install, manage portable context packages
with knowledge, graph, session patterns, and gotchas.
Actions: pr, create, list, info, remove, install, export, import, auto_load, summary.
Saves tokens: pre-built context state (avoids re-building).

Parameters: `action`*, `apply`, `author`, `base`, `depth`, `description`, `diff`, `enable`, `file`, `format`, `layers`, `level`, `name`, `project_root`, `scope`, `tags`, `version`

## `ctx_package`

WORKFLOW: save -> resume in new session for agent handoff.
ANTIPATTERN: NOT for internal session persistence (use ctx_session).
Self-contained JSON bundles: session state, summaries,
knowledge. Actions: save, resume, list, info.
Saves tokens: portable across sessions/agents.

Parameters: `action`, `description`, `path`

## `ctx_plan`

WORKFLOW: set task+profile -> ctx_plan -> use results with ctx_read/ctx_compose.
ANTIPATTERN: NOT for compressing already-selected files (use ctx_fill).
Selects files for context via Phi scoring + budget + policy.
task=short English; budget=token limit (default 12000);
profile=ultra_lean|balanced|forensic. Saves tokens by prioritizing relevant files.

Parameters: `budget`, `profile`, `task`*

## `ctx_plugins`

WORKFLOW: list -> info/name -> enable/disable.
ANTIPATTERN: NOT for tool listing (use ctx_discover_tools).
Plugin management — list, enable, disable, info, hooks.
name required for enable/disable/info. Extends tool functionality.
Saves tokens: loads only needed plugins.

Parameters: `action`*, `name`

## `ctx_prefetch`

WORKFLOW: call BEFORE context-heavy operations to minimize latency.
ANTIPATTERN: NOT for normal reads — only for proactive cache warming.
Prewarms cache for blast radius files via graph + task signals.
task=description; changed_files=paths for blast radius;
budget_tokens=soft budget (default 3000); max_files=limit (default 10).
Saves latency (not tokens): preloads files before needed.

Parameters: `budget_tokens`, `changed_files`, `max_files`, `root`, `task`

## `ctx_preload`

Caches task-relevant files, returns L-curve-optimized summary.
WORKFLOW: call at session start or when switching tasks, before ctx_read.
ANTIPATTERN: not for reading individual files — use ctx_read instead.
~50-100 tokens vs ~5000 for individual reads (~50x savings).

Parameters: `path`, `task`*

## `ctx_proof`

Export machine-readable ContextProofV1 (Verifier, SLO, Pipeline, Provenance).
WORKFLOW: call after completing a task to generate audit trail.
ANTIPATTERN: not for budget analysis — use ctx_radar/ctx_metrics instead.
action=export (only valid); format=json|summary|both; write=true|false;
max_evidence=max tool receipts (default 50). Writes to .lean-ctx/proofs/.

Parameters: `action`*, `filename`, `format`, `max_evidence`, `max_ledger_files`, `project_root`, `write`

## `ctx_provider`

Query GitHub, GitLab, Jira, Postgres, MCP bridges, custom REST.
WORKFLOW: action=list first to discover configured providers.
ANTIPATTERN: not for file content — use ctx_compose/ctx_read instead.
provider=id (github|gitlab|jira|mcp:<name>); resource=issues|pull_requests.
Data flows through consolidation pipeline; results searchable via ctx_semantic_search.

Parameters: `action`*, `iid`, `labels`, `limit`, `mode`, `provider`, `resource`, `state`, `status`

## `ctx_radar`

Context budget breakdown — system prompt, messages, tools, reads, shell.
WORKFLOW: call when context window tight to find biggest consumers.
ANTIPATTERN: not for per-call timing — use ctx_metrics instead.
format=display (human-readable) or json (structured). Complements ctx_metrics
for comprehensive budget analysis. Saves tokens vs manual budget estimation.

Parameters: `format`

## `ctx_read`

Read source files. mode REQUIRED — choose by intent.
WORKFLOW: after ctx_compose identified relevant files.
ANTIPATTERN: not for understanding code — use ctx_compose FIRST (saves tokens).
full=verbatim (edit-ready), raw=exact bytes (no framing), signatures=API,
map=structure, auto=smart (learns from task context), diff=git delta,
lines:N-M=window. fresh=true bypasses cache; raw=true=verbatim+fresh.

Parameters: `aggressiveness`, `fresh`, `limit`, `mode`, `offset`, `path`, `paths`, `protect`, `raw`, `start_line`

## `ctx_refactor`

Rename, move, safe_delete, inline, read-only analyses via LSP/IDE.
WORKFLOW: use action=references first to find usages before refactoring.
ANTIPATTERN: not for symbol discovery — use ctx_symbol/ctx_compose.
Single-phase edits (replace_symbol_body, reformat) work headless via name_path.
Two-phase ops (_preview+_apply) need JetBrains IDE (else BACKEND_REQUIRED).
Conflicts blocked unless force=true. See `action` parameter for full list.

Parameters: `action`*, `column`, `direction`, `end_line`, `expected_hash`, `force`, `keep_definition`, `line`, `mode`, `name_path`, `new_body`, `new_name`, `optimize_imports`, `path`, `plan_hash`, `propagate`, `scope`, `search_comments`, `search_text_occurrences`, `target_parent`, `target_path`, `text`

## `ctx_repomap`

PageRank symbol map ranked by structural importance + session relevance.
WORKFLOW: call for codebase-wide orientation at session start.
ANTIPATTERN: not for task-scoped views — use ctx_overview instead.
focus_files=['path/*.rs'] boosts specific areas; max_tokens controls size
(default 2048). Saves tokens vs reading all files individually.

Parameters: `focus_files`, `max_tokens`, `path`

## `ctx_response`

Compress LLM response text via structural de-duplication.
Removes repetitive patterns while preserving key information.
WORKFLOW: use after receiving a response, before storing/forwarding.
ANTIPATTERN: no-op when CRP mode is off — use ctx_read compression instead.

Parameters: `text`*

## `ctx_retrieve`

Retrieve original uncompressed content from the session cache (CCR) —
restores full verbatim source when compressed ctx_read output is insufficient.
WORKFLOW: call ctx_read FIRST to populate cache, then ctx_retrieve for verbatim.
query='text' to find matching lines within cached content.
ANTIPATTERN: not for reading files directly — use ctx_read.

Parameters: `path`*, `query`

## `ctx_review`

Automated code review with impact analysis, caller tracking, and test discovery.
Actions: review (single file), diff-review (from git diff text),
checklist (structured review questions). depth=N (default 3).
WORKFLOW: run tests first, then use review for structured analysis.
ANTIPATTERN: not a substitute for actual test execution.

Parameters: `action`*, `depth`, `path`

## `ctx_routes`

Discover HTTP API endpoints without reading route definition files.
Auto-detects: Express, Flask, FastAPI, Actix, Spring, Rails, Next.js.
method=GET|POST filters by verb; path='/api' filters by prefix.
ANTIPATTERN: not for filesystem paths — use ctx_tree.
Saves tokens vs grepping route definitions.

Parameters: `method`, `path`

## `ctx_rules`

Cross-agent rules governance (ContextOps).
Actions: sync (distribute rules to agents), diff (show drift),
lint (check consistency), status (sync state), init (create central config).
WORKFLOW: run status first to check state, then sync if out of date.

Parameters: `action`*, `agent`

## `ctx_search`

Search code; `action` picks the engine. regex (default): exact pattern, `pattern`
required, include='*.rs', paths=[..] multi-root. semantic: by meaning (BM25+embeddings),
`query`, mode=bm25|dense|hybrid. symbol: one symbol's body by `name` (AST-precise),
file/kind narrow. reindex / find_related(file_path,line). For end-to-end understanding,
use ctx_compose FIRST.

Parameters: `action`, `file`, `file_path`, `include`, `kind`, `line`, `max_results`, `mode`, `name`, `path`, `paths`, `pattern`, `query`, `top_k`

## `ctx_semantic_search`

[Deprecated → ctx_search action="semantic"] Search code by meaning (BM25+embeddings);
reindex / find_related are ctx_search actions too. Hidden from tools/list but still
callable for one release — prefer ctx_search.

Parameters: `action`, `file_path`, `languages`, `line`, `mode`, `path`, `path_glob`, `query`*, `top_k`

## `ctx_session`

WORKFLOW: action=save at session end; action=load at session start.
action=status (snapshot); task|finding|decision (progress).
ANTIPATTERN: permanent project knowledge → ctx_knowledge.
Also supports: profile|role|budget|slo|diff|verify|episodes|procedures.

Parameters: `action`*, `session_id`, `value`

## `ctx_share`

WORKFLOW: push from agent A → pull from agent B shares cached file contexts.
Actions: push|pull|list|clear. Omit to_agent for broadcast.
ANTIPATTERN: NOT file transfer — shares lean-ctx cache entries only.

Parameters: `action`*, `message`, `paths`, `to_agent`

## `ctx_shell`

WORKFLOW: preferred — auto-compresses output (build/test/log).
raw=true for verbatim output.
[exit:N] on errors (lossless).
ANTIPATTERN: multi-line scripts → ctx_execute.

Parameters: `command`*, `cwd`, `env`, `raw`

## `ctx_skillify`

WORKFLOW: mine to extract patterns → list to review → promote to activate.
Codifies patterns into .cursor/rules/skillify-*.mdc.
Actions: mine|list|status|promote. Idempotent.
ANTIPATTERN: one-off rules → write .mdc by hand.

Parameters: `action`, `slug`

## `ctx_smart_read`

DEPRECATED → use ctx_read (it auto-selects the mode; omit `mode`). Folded
into ctx_read (#509); hidden from tools/list, still callable for one release.

Parameters: `path`*

## `ctx_smells`

WORKFLOW: rules (list detectors) → scan (run on project).
Code smell detection: dead_code, long_function, god_file, complexity, etc.
rule='name' or path='file' to filter.
ANTIPATTERN: NOT a linter — no style/format enforcement.

Parameters: `action`, `format`, `path`, `root`, `rule`

## `ctx_summary`

WORKFLOW: record after tasks → recall with query.
Compact session digests (task, files, decisions, next steps).
Actions: recall|record|list. Auto-captured on checkpoints.
ANTIPATTERN: structured facts → ctx_knowledge.

Parameters: `action`, `query`, `top_k`

## `ctx_symbol`

[Deprecated → ctx_search action="symbol"] Get one symbol's body by name (AST-precise);
optional file/kind narrow. Hidden from tools/list but still callable for one release —
prefer ctx_search.

Parameters: `file`, `kind`, `name`*

## `ctx_task`

Multi-agent task orchestration.
WORKFLOW: action=create → action=list to review → action=update to change state.
Actions: create|update|list|get|cancel|message|info.
States: working|input-required|completed|failed|canceled.
ANTIPATTERN: not for code execution — use ctx_shell or ctx_execute.

Parameters: `action`*, `description`, `message`, `state`, `task_id`, `to_agent`

## `ctx_tools`

Gateway to downstream MCP servers — unlimited external tools at ~constant context cost.
actions: find (query → top-N relevant tools) | call (proxy a server::tool) |
list (servers+counts) | refresh.
WORKFLOW: find to discover, then call the chosen server::tool.
ANTIPATTERN: not for built-in tools — use those directly.

Parameters: `action`, `arguments`, `query`, `tool`

## `ctx_transcript_compact`

Compact an OpenAI-format message array deterministically:
keep system + fresh tail verbatim, replace older turns with a recoverable
summary, offload raw turns into session memory (indexed for recall).
Returns JSON {messages, stats}. tool_call/tool_result pairs never split.

Parameters: `focus_topic`, `fresh_tail_tokens`, `messages`*, `protect_min_messages`

## `ctx_tree`

Directory tree with file counts per directory. depth=N (default 3);
show_hidden for dotfiles; paths for multi-root.
respect_gitignore filters ignored files (default true).
WORKFLOW: lightweight orientation before ctx_repomap or ctx_compose.

Parameters: `depth`, `path`, `paths`, `respect_gitignore`, `show_hidden`

## `ctx_url_read`

Fetch URL: pages→Markdown; PDF→text; YouTube→transcript; mode=auto best per type.
mode=facts|quotes for research (claims+confidence). query='topic' focuses extraction.
GitHub blob/raw URLs auto-resolve to raw file. SSRF-guarded (no private IPs).
max_tokens=6000; timeout_secs=20 (max 60).

Parameters: `max_items`, `max_tokens`, `mode`, `query`, `timeout_secs`, `url`*

## `ctx_verify`

Verification observability — tool call statistics and claim-based verification.
WORKFLOW: action=stats to monitor tool usage; action=proof|v2 for Lean4 proof verification.
Actions: stats|proof|v2 (format=summary|json|both, default summary).
ANTIPATTERN: not for runtime verification during active development — use for periodic audit.

Parameters: `action`, `format`

## `ctx_workflow`

Workflow rails — state machine with evidence tracking.
WORKFLOW: start → transition (multiple) → complete. evidence_add before
transition to attach proof. Built-in plan_code_test when spec omitted.
Actions: start|status|transition|complete|evidence_add|evidence_list|stop.
spec=WorkflowSpec JSON for custom states/transitions.
ANTIPATTERN: NOT for one-shot tasks — use direct tool calls instead.

Parameters: `action`, `key`, `name`, `spec`, `to`, `value`

## `shell`

Shell command with auto-compression (~95 patterns). Alias for ctx_shell.
Output is compressed for token savings. For verbatim output pass raw=true.
Use when your MCP client prefers shell/bash over ctx_shell — transparently
delegates to ctx_shell internals.

Parameters: `command`*, `cwd`

