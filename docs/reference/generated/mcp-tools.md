# Appendix — MCP Tools (generated)

<!-- GENERATED FILE — do not edit by hand. Run: `cargo run --example gen_docs --features dev-tools` -->

Source of truth: `rust/src/server/registry.rs` and the tool definitions it registers.

lean-ctx registers **77 MCP tools** (granular profile). Each entry below lists the tool name, what it does, and its parameters (`*` marks required).

## `ctx_agent`

Multi-agent coordination — shared message bus, persistent diaries, stigmergic scent field. Actions: register (agent_type+role), post (message+category), read (poll), status (active|idle|finished), handoff (task+summary), sync (agents+messages+scent), claim/release (file/task claim), brief (sub-agent briefing), return (distill report into knowledge), diary, recall_diary, list, info. Use when orchestrating multiple LLM agents across a shared workspace.

Parameters: `action`*, `agent_type`, `category`, `message`, `role`, `status`, `to_agent`

## `ctx_analyze`

Entropy analysis — recommends optimal compression mode for a file path. Use before ctx_read to pick the best mode (full/signatures/auto) that balances size vs information retention.

Parameters: `path`*

## `ctx_architecture`

Architecture analysis — understand module structure without reading every file.
action=overview→high-level; clusters|communities→groupings;
layers|cycles→dependency violations; entrypoints|hotspots→risk areas;
health→quality; module path='src/' to zoom into a specific module.

Parameters: `action`, `format`, `path`, `root`

## `ctx_artifacts`

Context artifact registry with BM25 search — manage and query indexed code artifacts. Actions: list|status|index|reindex|search|remove. Use search with query for semantic retrieval across artifacts.

Parameters: `action`*, `format`, `name`, `project_root`, `query`, `top_k`

## `ctx_benchmark`

Benchmark compression modes — measures token savings across all available modes for a file or project. Provide a file path, or use action=project format=json|markdown for project-wide results.

Parameters: `action`, `format`, `path`*

## `ctx_cache`

Cache operations — inspect, clear, or invalidate the read cache. Actions: status lists cached files; clear empties all; invalidate path=... refreshes a single entry. Use to diagnose stale content or recover budget.

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
Actions: snapshot (record current state), log (list checkpoints),
diff (compare against a checkpoint), restore (revert files).
Snapshot before+after changes to capture exactly what was modified.
Never touches the user's repository.

Parameters: `action`, `from`, `limit`, `message`, `path`, `ref`, `to`

## `ctx_compile`

Context compilation (CFT) — builds minimal context package via greedy knapsack + Boltzmann view selection. Modes: handles|compressed|full. Use to produce focused context for handoff or subagent tasks.

Parameters: `budget`, `mode`

## `ctx_compose`

PRIMARY TOOL — call FIRST for understanding code, before editing, debugging, or
answering 'how does X work'. Pass a task/question or symbol names. returns ranked files with 
relevant symbol source inline grouped by file. Combines BM25 lexical + semantic + associative
retrieval + submodular optimization. Do NOT chain search→read→symbol — one compose
does it all. Do NOT Read files whose source compose already returned — it IS the source.
Fire independent ctx_read or ctx_compose calls for different areas in PARALLEL.

Parameters: `path`, `task`*

## `ctx_compress`

Compress read cache to free token budget in long sessions.
include_signatures=true (default) preserves API surface in compressed state.
Does not affect session state or knowledge — only read cache compaction.
Use when nearing context limit to reclaim space for new content.

Parameters: `include_signatures`

## `ctx_compress_memory`

Compress a memory/config file (CLAUDE.md, .cursorrules) preserving code, URLs, and paths. Creates .original.md backup. Use to reduce token overhead of persistent instruction files.

Parameters: `path`*

## `ctx_context`

Session context overview — shows cached files, seen files, session state, and current CRP mode. No arguments needed. Call periodically to track what's in your context window and remaining budget.

Parameters: _none_

## `ctx_control`

Universal context manipulation (CFT) — fine-tune what appears in context. Actions: exclude|include|pin|unpin|set_view|set_priority|mark_outdated|reset|list|history. Overlay-based, reversible, scoped to call/session/project.

Parameters: `action`*, `reason`, `scope`, `target`, `value`

## `ctx_cost`

Cost attribution — track tokens and cost per agent and tool call. Actions: report (summary), agent (per-agent), tools (per-tool), json (machine-readable), reset (zero counters). Local-first, no external billing calls.

Parameters: `action`, `agent_id`, `limit`

## `ctx_dedup`

Cross-file deduplication — detect and eliminate repeated content across files. action=analyze (default) finds shared blocks; action=apply registers them for auto-dedup in ctx_read output.

Parameters: `action`

## `ctx_delta`

Incremental diff since last read — shows only changed lines after you edit.
Use INSTEAD of re-reading the whole file after modifications — saves 90%+ tokens
on unchanged content. Path must have a prior ctx_read in this session's cache.
For the full git diff against HEAD, use ctx_read(path, mode=diff) instead.

Parameters: `path`*

## `ctx_discover`

Identify compression misses in shell history — use when context feels bloated.
Shows commands that would save tokens via lean-ctx patterns. limit=N caps results.
No params needed for quick health check.

Parameters: `limit`

## `ctx_discover_tools`

Search available lean-ctx tools by keyword — use to find the right tool.
Empty query lists all tools. query="keyword" returns matching names and descriptions.
Use before ctx_call or when unsure which tool fits your task.

Parameters: `query`

## `ctx_edit`

Search-and-replace edit with TOCTOU safety — for simple text replacement in a single file.
Use INSTEAD of native Edit when Read is unavailable. old_string must be unique unless replace_all=true.
create=true writes new files. backup creates .bak. MD5/size/mtime pre-guards prevent race conditions.
Do NOT loop on failures — switch to ctx_edit. For LSP-aware refactoring (rename, move, inline), use ctx_refactor.

Parameters: `create`, `new_string`*, `old_string`, `path`*, `replace_all`

## `ctx_execute`

Run code in sandbox (11 languages) — use when compute beats shell glue.
action=code (default) for one-shot scripts; action=batch for parallel multi-language;
action=file to process a project file (extension auto-detects language).
Pass intent to focus large output. Prefer over ctx_shell for conditionals,
multi-line scripts, or cross-language data munging. Languages: javascript,
typescript, python, shell, ruby, go, rust, php, perl, r, elixir.

Parameters: `action`, `code`, `intent`, `items`, `language`, `path`, `timeout`

## `ctx_expand`

Retrieve archived tool output by ID (e.g. id=@F1 from [Archived:ID] hints).
Use when you see an [Archived:ID] reference and need the full content.
Supports head/tail/search to filter lines. action=search_all across all archives.
action=list shows available archives. Zero-loss: original preserved.
For reading files, use ctx_read or ctx_compose instead.

Parameters: `action`, `end_line`, `head`, `id`, `json_keys`, `json_path`, `query`, `search`, `session_id`, `start_line`, `tail`

## `ctx_feedback`

Record and report LLM token/latency metrics (local-first) — use to track efficiency.
Actions: record (log event), report (readable summary), json (machine-readable),
reset (clear data), status (storage info). record requires llm_input_tokens + llm_output_tokens.

Parameters: `action`, `agent_id`, `intent`, `latency_ms`, `limit`, `llm_input_tokens`, `llm_output_tokens`, `model`, `note`

## `ctx_fill`

Budget-aware context fill — given a list of paths, auto-compresses each to fit within a token budget.
Pass paths array + budget=N. task="..." enables intent-driven pruning for relevance.
Does NOT decide what to include (use ctx_plan for project-wide selection).
Use instead of reading each file individually when you have many files and a budget.

Parameters: `budget`*, `paths`*, `task`

## `ctx_gain`

Gain report — shows token savings from lean-ctx compression. Use to measure efficiency.
action=wrapped for periodic/annual summary. Other actions: status|report|score|cost|tasks|heatmap|agents|json.
period="week"|"month"|"all" scopes the report.

Parameters: `action`, `limit`, `model`, `period`

## `ctx_git_read`

Read remote git repos via cached shallow clone (not HTML scraping).
modes: overview (tree + README) | tree (file list) | read (file content) | grep (search).
Accepts repo URLs and GitHub/GitLab blob/tree links (ref+path auto-detected).
https-only, SSRF-guarded. Use instead of ctx_url_read for a whole repo.

Parameters: `max_tokens`, `mode`, `path`, `query`, `ref`, `timeout_secs`, `url`*

## `ctx_glob`

Find files by glob pattern — use to locate files by name or extension.
Respects .gitignore. Supports multi-root via paths array. max_results=N sets limit.
For file content search, use ctx_search (pattern) or ctx_semantic_search (meaning).

Parameters: `ignore_gitignore`, `max_results`, `path`, `paths`, `pattern`*

## `ctx_graph`

Code graph queries — find usages, relationships, and dependency chains.
action=symbol path="file.rs::fnName" finds all usages of a symbol.
action=neighbors shows adjacent nodes; action=path from→to shows dependency
chains between files. action=diff since=HEAD~1 for git change impact.
For understanding code end-to-end, use ctx_compose FIRST. Use ctx_graph for
targeted structural queries the graph index can answer directly.

Parameters: `action`*, `depth`, `format`, `kind`, `path`, `project_root`, `since`, `to`

## `ctx_handoff`

Context handoff protocol (hashed, deterministic, local-first).
Actions: create|show|list|pull|clear|export|import. Stores curated file refs with hashes.
Use before ending a session or when handing off work to another agent.

Parameters: `action`, `apply_knowledge`, `apply_session`, `apply_workflow`, `filename`, `format`, `path`, `paths`, `privacy`, `write`

## `ctx_heatmap`

File access heatmap — shows most frequently accessed files per session.
action=status (default) for summary, action=detail for per-file access counts.
Use to identify hot files and optimize context usage.

Parameters: `action`, `path`

## `ctx_impact`

Change impact analysis — assess blast radius before refactoring.
action=analyze path="file.rs" maps downstream dependents; action=diff compares git refs;
action=chain traces from→to dependency paths. depth controls traversal (default 5).
Use before any significant refactor to understand risk.

Parameters: `action`, `depth`, `format`, `path`, `root`

## `ctx_index`

Index orchestration — manage the code graph index.
Actions: status (current state), build (incremental update), build-full (complete rebuild).
Use when the graph index is stale and ctx_graph returns empty or outdated results.

Parameters: `action`*, `project_root`

## `ctx_intent`

Structured intent input (optional) — submit compact task goals as JSON or short text.
Server also auto-infers intent from tool calls. Use to guide context prioritization,
preloading, and cache optimization. query=task|JSON; project_root=scope.

Parameters: `project_root`, `query`*

## `ctx_knowledge`

Persistent memory across sessions — remember decisions, patterns, and facts for recall.
action=remember saves a fact; action=recall query='X' retrieves it.
Use to persist architecture decisions, gotchas, and patterns between sessions.
action=gotcha trigger='X' resolution='Y' for known pitfalls.
mode=semantic|exact for recall. category groups related facts.

Parameters: `action`*, `as_of`, `category`, `confidence`, `examples`, `key`, `mode`, `pattern_type`, `query`, `resolution`, `severity`, `trigger`, `value`

## `ctx_ledger`

Context ledger operations — track and manage persistent context pressure.
action=status shows pressure (%), top files by cost, and recommendations;
action=reset clears all entries; action=evict targets=paths removes files
and excludes them from re-accumulation. Use when context budget is tight.

Parameters: `action`*, `targets`

## `ctx_load_tools`

Load/unload specialized tool categories on demand to reduce tool surface area.
action=load|unload|list. Categories: arch, debug, memory, metrics, session.
Core is always loaded and cannot be unloaded. Use when you only need
a subset of tools for your current task.

Parameters: `action`*, `category`

## `ctx_metrics`

Session token statistics — cache hit rates, per-tool savings, pipeline metrics,
and signature backend ratios. No parameters needed. Use to understand token
efficiency and identify which tools cost the most. Complements ctx_radar
for full context budget analysis.

Parameters: _none_

## `ctx_multi_read`

Batch-read multiple files in one call — more token-efficient than N sequential
ctx_read calls. paths=['a.rs','b.rs'] reads them all at once.
mode=full for files you edit; mode=auto for general reading (compressed).
Use when you need the content of several files. For understanding code logic,
use ctx_compose FIRST — it returns relevant symbol source grouped by file.

Parameters: `fresh`, `mode`, `paths`*

## `ctx_multi_repo`

Multi-repository management — add, remove, and search across project directories.
action=add_root|remove_root|list_roots|search. Cross-repo search uses Reciprocal
Rank Fusion (RRF) to merge results from multiple repos. query=search term;
roots=filter to specific repos. max_results limits output (default 20).

Parameters: `action`*, `alias`, `max_results`, `path`, `query`, `roots`

## `ctx_outline`

List file symbols with signatures and line numbers — path='file.rs' returns fn, struct,
class, and trait declarations via tree-sitter extraction. kind=fn|struct|class|all
to filter. Use for a quick API overview of a file. For deeper understanding,
use ctx_compose. For full file content, use ctx_read.

Parameters: `kind`, `path`*

## `ctx_overview`

Task-relevant project map — use at session start to orient before diving into code.
task='your goal' scopes files/modules by relevance (PageRank on symbol graph).
For deeper code understanding, use ctx_compose instead — returns source + flow
in one call. ctx_overview is lighter: high-level structure only, no source body.

Parameters: `path`, `task`

## `ctx_pack`

Context Package Manager — create, install, and manage portable context packages.
Actions: pr (PR context), create (build from project), list, info, remove,
install, export, import, auto_load, summary. Use to bundle and share context
state including knowledge, graph, session patterns, and gotchas.

Parameters: `action`*, `apply`, `author`, `base`, `depth`, `description`, `diff`, `enable`, `file`, `format`, `layers`, `level`, `name`, `project_root`, `scope`, `tags`, `version`

## `ctx_package`

Save or resume portable context packages — self-contained JSON bundles with session
state, summaries, and knowledge. Use to hand off context between agents, persist
session snapshots for later, or onboard a new agent into a previous session.
Actions: save (export), resume (import), list, info (inspect without importing).

Parameters: `action`, `description`, `path`

## `ctx_plan`

Context planning (CFT) — selects WHICH files/modules to include in context via Phi scoring, budget
allocation, and policy-driven view selection. Scans the project to pick the most relevant content.
task=description (short English), budget=token limit (default 12000), profile=ultra_lean|balanced|forensic.
For compressing already-chosen files to fit a budget, use ctx_fill instead.

Parameters: `budget`, `profile`, `task`*

## `ctx_plugins`

Plugin management — list, enable, disable, and inspect plugins with their hooks.
Actions: list (show installed), enable (activate), disable (deactivate),
info (show details), hooks (list available hook points). name=plugin_name
required for enable, disable, info. Use to extend tool functionality.

Parameters: `action`*, `name`

## `ctx_prefetch`

Predictive prefetch — prewarm the cache for blast radius files using graph and
task signals. task=description for relevance scoring; changed_files=paths to
compute blast radius; budget_tokens=soft budget (default 3000); max_files=limit
(default 10). Use before context-heavy operations to minimize latency.

Parameters: `budget_tokens`, `changed_files`, `max_files`, `root`, `task`

## `ctx_preload`

Proactive context loader — caches task-relevant files and returns L-curve-optimized
summary (~50-100 tokens vs ~5000 for individual reads). task=short description;
path=project root. Use at session start or when switching tasks to efficiently
warm the context cache with the most relevant files.

Parameters: `path`, `task`*

## `ctx_proof`

Export machine-readable ContextProofV1 with Verifier, SLO, Pipeline, and Provenance.
Writes to .lean-ctx/proofs/ by default. action=export (required);
format=json|summary|both; write=true|false; filename=custom path;
max_evidence=max tool receipts (default 50). Use for audit trails.

Parameters: `action`*, `filename`, `format`, `max_evidence`, `max_ledger_files`, `project_root`, `write`

## `ctx_provider`

External context providers — query data from GitHub, GitLab, Jira, Postgres, MCP
bridges, and custom REST APIs. Actions: discover|list|status|refresh|configure|
query|mcp_resources|gitlab_issues. provider=id (github|gitlab|jira|mcp:<name>);
resource=issues|pull_requests. Data flows through consolidation pipeline.

Parameters: `action`*, `iid`, `labels`, `limit`, `mode`, `provider`, `resource`, `state`, `status`

## `ctx_radar`

Full context budget breakdown — system prompt, messages, tools, reads, shell,
all tracked token usage. format=display (human-readable) or json (structured).
Complements ctx_metrics for comprehensive budget analysis. Use when context
window is tight and you need to identify the biggest consumers.

Parameters: `format`

## `ctx_read`

Read source files. mode is REQUIRED — choose by intent:
full=verbatim (edit-ready, use before Edit), raw=exact bytes (no framing),
signatures=API surface only, map=structural overview of large files,
auto=smart (learns from task and session context, use for orientation),
diff=git delta, lines:N-M=window.
fresh=true bypasses cache; raw=true=verbatim+fresh.
For understanding code or finding answers, use ctx_compose FIRST instead.

Parameters: `aggressiveness`, `fresh`, `limit`, `mode`, `offset`, `path`*, `protect`, `raw`, `start_line`

## `ctx_refactor`

LSP/IDE refactoring — rename, move, safe_delete, inline, and read-only analyses
(references, definition, implementations, type_hierarchy, inspections).
Single-Phase edits (replace_symbol_body, reformat) work headless via name_path.
Two-Phase ops (rename/move/safe_delete/inline _preview+_apply) need JetBrains
IDE (else BACKEND_REQUIRED) with plan_hash TOCTOU guard. Conflicts blocked
unless force=true. See action enum for full list of pipe-delimited values.

Parameters: `action`*, `column`, `direction`, `end_line`, `expected_hash`, `force`, `keep_definition`, `line`, `mode`, `name_path`, `new_body`, `new_name`, `optimize_imports`, `path`, `plan_hash`, `propagate`, `scope`, `search_comments`, `search_text_occurrences`, `target_parent`, `target_path`, `text`

## `ctx_repomap`

PageRank symbol map ranked by structural importance and session relevance.
focus_files=['path/*.rs'] boosts specific areas; max_tokens controls size (default 2048).
Use for codebase-wide orientation; for task-scoped view use ctx_overview.

Parameters: `focus_files`, `max_tokens`, `path`

## `ctx_response`

Compress LLM response text via structural de-duplication.
Pass response text to remove repetitive patterns while preserving key information.
Use to reduce token waste before storing or forwarding responses.

Parameters: `text`*

## `ctx_retrieve`

Retrieve original uncompressed content from the session cache (CCR).
Use when a compressed ctx_read output is insufficient — restores full verbatim source.
Supports query='search' to find matching lines within cached content.
Requires path to a file previously read via ctx_read.

Parameters: `path`*, `query`

## `ctx_review`

Automated code review with impact analysis, caller tracking, and test discovery.
Actions: review (single file), diff-review (from git diff),
checklist (structured review questions). depth=N controls analysis breadth (default 3).

Parameters: `action`*, `depth`, `path`

## `ctx_routes`

Discover HTTP API endpoints without reading route definition files.
Auto-detects: Express, Flask, FastAPI, Actix, Spring, Rails, Next.js.
method=GET|POST filters by verb; path='/api' filters by prefix.

Parameters: `method`, `path`

## `ctx_rules`

Cross-agent rules governance (ContextOps).
Actions: sync (distribute rules to agents), diff (show drift),
lint (check consistency), status (sync state), init (create central config).

Parameters: `action`*, `agent`

## `ctx_search`

Regex pattern search — use when you know the exact pattern. For understanding code or
finding answers, use ctx_compose FIRST (one call replaces search+read+symbol chains).
pattern required; include='*.rs'; path scopes; max_results=N (default 20).
paths=['dir1','dir2'] for multi-root. ignore_gitignore bypasses .gitignore (needs role).

Parameters: `ignore_gitignore`, `include`, `max_results`, `path`, `paths`, `pattern`*

## `ctx_semantic_search`

Search code by MEANING (BM25+embeddings) — use when you know the concept but not the exact
symbol name. query='user auth' finds relevant code even with no keyword match.
Different from ctx_search (regex): use ctx_search for exact patterns, this for
fuzzy/conceptual. For understanding code end-to-end, use ctx_compose FIRST.
find_related(file_path, line) for context neighbors. mode=bm25|dense|hybrid.

Parameters: `action`, `file_path`, `languages`, `line`, `mode`, `path`, `path_glob`, `query`*, `top_k`

## `ctx_session`

Cross-session memory: action=task|finding|decision persists progress;
load session_id=X resumes prior work. Use at session end to persist;
at start to restore. action=status for snapshot; action=save commits state;
action=reset clears. Supports profile|role|budget|slo|diff|verify|episodes|procedures.

Parameters: `action`*, `session_id`, `value`

## `ctx_share`

Share cached file contexts between agents for collaborative workflows.
Actions: push (share files from your cache to another agent),
pull (receive files shared by others), list (show shared contexts),
clear (remove your shares). Omit to_agent for broadcast;
set to_agent='agent-id' for targeted sharing.

Parameters: `action`*, `message`, `paths`, `to_agent`

## `ctx_shell`

Run shell commands with automatic output compression (~95 patterns).
Optimized for build/test/log output (cargo, npm, pytest, go test).
raw=true disables compression for verbatim output. Lossless for errors
and exit codes — [exit:N] footer for failure codes. cwd persists.

Parameters: `command`*, `cwd`, `env`, `raw`

## `ctx_skillify`

Codify recurring patterns from session diary + knowledge into versioned,
git-committable .cursor/rules/skillify-*.mdc files.
Actions: mine (distill & write/merge rules), list (show generated rules),
status (config + counts), promote (copy to ~/.cursor/rules).
Precision-biased; re-runs are idempotent.

Parameters: `action`, `slug`

## `ctx_smart_read`

Auto-select optimal read mode (full|map|signatures|auto) based on file size,
type, and compression history. Use when you want smart defaults without
choosing a mode. For explicit control use ctx_read with mode= parameter directly.

Parameters: `path`*

## `ctx_smells`

Code smell detection engine.
Actions: scan (run all rules on project), summary (aggregate counts),
rules (list available rules with descriptions), file (scan a single file).
Supports rule='name' and path='file' filters for targeted analysis.

Parameters: `action`, `format`, `path`, `root`, `rule`

## `ctx_summary`

Record and recall AI session summaries — compact, semantically-recallable
digests of what was done (task, files, decisions, next steps).
Actions: recall (find past summaries by query), record (snapshot session),
list (recent summaries). Auto-captured on checkpoint cadence.
Use with ctx_session for cross-session continuity.

Parameters: `action`, `query`, `top_k`

## `ctx_symbol`

Get ONE symbol's body by name — exact, AST-precise (tree-sitter index). Use AFTER
ctx_compose gave you the overview and you need a specific symbol's full body.
For multiple symbols or understanding an area, use ctx_compose FIRST (returns
all relevant symbols grouped by file in one call). name='fnName' returns code block.
file='path.rs' narrows; kind='fn'|'struct'|'class'|'trait'|'enum' disambiguates.

Parameters: `file`, `kind`, `name`*

## `ctx_task`

Multi-agent task orchestration.
Actions: create (assign to agent), update (change state), list (active),
get (details), cancel, message (add note), info (metadata).
States: working|input-required|completed|failed|canceled.

Parameters: `action`*, `description`, `message`, `state`, `task_id`, `to_agent`

## `ctx_tools`

Gateway to downstream MCP servers — unlimited external tools at ~constant context cost.
actions: find (query → top-N relevant tools as ChoiceCards) |
call (proxy a server::tool) | list (servers+counts) | refresh.
Use find to discover, then call the chosen server::tool.
Off by default — enable via [gateway] config section.

Parameters: `action`, `arguments`, `query`, `tool`

## `ctx_transcript_compact`

Compact an OpenAI-format message array deterministically:
keep system + fresh tail verbatim, replace older turns with a recoverable
summary, offload raw turns into session memory (indexed for recall).
Built for the Hermes context-engine plugin. Returns JSON {messages, stats}.
tool_call/tool_result pairs are never split.

Parameters: `focus_topic`, `fresh_tail_tokens`, `messages`*, `protect_min_messages`

## `ctx_tree`

Directory tree with file counts per directory. depth=N (default 3);
show_hidden for dotfiles; paths for multi-root.
respect_gitignore filters ignored files (default true).
Use for lightweight project orientation before ctx_repomap or ctx_compose.

Parameters: `depth`, `path`, `paths`, `respect_gitignore`, `show_hidden`

## `ctx_url_read`

Fetch URL: pages→Markdown; PDF→text; YouTube→transcript; mode=auto best per type.
mode=facts|quotes for research (claims+confidence). query='topic' focuses extraction.
GitHub blob/raw URLs auto-resolve to raw file. SSRF-guarded (no private IPs).
max_tokens=6000; timeout_secs=20 (max 60).

Parameters: `max_items`, `max_tokens`, `mode`, `query`, `timeout_secs`, `url`*

## `ctx_verify`

Verification observability — tool call statistics and claim-based verification.
Actions: stats (tool call usage counts), proof|v2 (ContextProofV2 claim
verification with Lean4 proofs). Use to audit tool usage or verify claims.

Parameters: `action`, `format`

## `ctx_workflow`

Workflow rails — state machine with evidence tracking.
Actions: start|status|transition|complete|evidence_add|evidence_list|stop.
spec=WorkflowSpec JSON to define custom states/transitions.
Built-in plan_code_test workflow when spec omitted.
Use with ctx_task for multi-agent orchestration.

Parameters: `action`, `key`, `name`, `spec`, `to`, `value`

## `shell`

Shell command with auto-compression (~95 patterns). Alias for ctx_shell.
Output is compressed for token savings. For verbatim output pass raw=true.
Use when your MCP client prefers shell/bash over ctx_shell — transparently
delegates to ctx_shell internals.

Parameters: `command`*, `cwd`

