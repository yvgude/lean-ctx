# Appendix — MCP Tools (generated)

<!-- GENERATED FILE — do not edit by hand. Run: `cargo run --example gen_docs --features dev-tools` -->

Source of truth: `rust/src/server/registry.rs` and the tool definitions it registers.

lean-ctx registers **49 MCP tools** (granular profile). Each entry below lists the tool name, what it does, and its parameters (`*` marks required).

## `ctx_artifacts`

Context artifact registry with BM25 search — manage and query indexed code artifacts.
WORKFLOW: index artifacts first (index/reindex), then search with query for semantic retrieval.
Actions: list|status|index|reindex|search|remove.
ANTIPATTERN: NOT for general code search — use ctx_search (action="semantic") for codebase queries.

Parameters: `action`*, `format`, `name`, `project_root`, `query`, `top_k`

## `ctx_call`

Invoke any non-core lean-ctx tool by name — for tools not exposed as standalone MCP tools.
Categories: arch, debug, memory, batch, agent, util. Find exact names with
ctx_discover_tools (query=keyword; empty query lists all). Cannot invoke itself.

Parameters: `arguments`, `name`*

## `ctx_callgraph`

Callers/callees for one symbol (function call edges, not const/var refs).
action=callers|callees symbol='fn' → every call site with file:line.
action=trace from→to finds the path between two symbols (depth=N).
For end-to-end flow understanding use ctx_compose FIRST.

Parameters: `action`*, `depth`, `file`, `from`, `symbol`, `to`

## `ctx_checkpoint`

Local shadow git history of the agent's changes — separate from the user's .git.
WORKFLOW: snapshot before+after changes to capture exactly what was modified.
Actions: snapshot (record current state), log (list checkpoints with SHAs),
diff from=... to=... (compare checkpoints), restore ref=... (revert files).
ANTIPATTERN: Never touches the user's repository — completely isolated shadow history.

Parameters: `action`, `from`, `limit`, `message`, `path`, `ref`, `to`

## `ctx_compose`

First-pass context for one task; returns ranked files and inline source — use instead of search→read chains.

Parameters: `path`, `task`*, `task_aware`

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
WORKFLOW: track context budget periodically — use before ctx_compress or ctx_control(action="evict").
ANTIPATTERN: not for reading file content — use ctx_read or ctx_compose.

Parameters: _none_

## `ctx_control`

Fine-tune context — exclude, include, pin, unpin, evict, set_view, set_priority, mark_outdated, reset, list, history.
Overlay-based, reversible, scoped to call/session/project.
WORKFLOW: after ctx_compose, exclude low-relevance files.
ANTIPATTERN: not for initial context building — use ctx_compose/ctx_read first.

Parameters: `action`*, `reason`, `scope`, `target`, `targets`, `value`

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
Use INSTEAD of re-reading the whole file after modifications — it returns only changed
lines and avoids resending unchanged content. Path must have a prior ctx_read in this session's cache.
ctx_read(mode=diff) diffs this same cache, not git; for HEAD use git diff.

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
For editing code you've read, prefer ctx_patch (hash-anchored): it never makes you reproduce old text byte-for-byte. Read with ctx_read(mode="anchored") first.
old_string must be unique unless replace_all=true. create=true writes new files.
backup creates .bak. MD5/size/mtime pre-guards prevent race conditions.
ANTIPATTERN: Do NOT loop on failures — switch to ctx_patch (anchored), or verify file content and adjust old_string.
To replace a whole symbol body use ctx_patch(op="replace_symbol"); find its references with ctx_callgraph.

Parameters: `create`, `new_string`*, `old_string`, `path`*, `replace_all`

## `ctx_execute`

Run code in sandbox (11 languages) — use when conditionals, multi-line or cross-language transforms.
ANTIPATTERN: for simple one-liners, prefer ctx_shell (lower overhead, auto-compressed).
language=shell supports multi-line scripts but shares ctx_shell's security policy;
other languages run only after the user approves them.
action=code (default) for one-shot; action=batch for parallel multi-language;
action=file to summarize admitted project text as data, without executing it.
For code/batch, intent annotates large output; file records the intent alongside its preview. Languages: javascript,
typescript, python, shell, ruby, go, rust, php, perl, r, elixir.

Parameters: `action`, `code`, `intent`, `items`, `language`, `path`, `timeout`

## `ctx_expand`

Retrieve archived tool output: see [Archived:ID] → ctx_expand id=ID (zero-loss, original preserved). head/tail/search filter lines; action=list|search_all browses/queries archives.
ANTIPATTERN: not for project files — use ctx_read or ctx_compose.

Parameters: `action`, `end_line`, `head`, `id`, `json_keys`, `json_path`, `query`, `search`, `session_id`, `start_line`, `tail`

## `ctx_feedback`

Record and report LLM token/latency metrics — use to track efficiency and optimize context usage.
WORKFLOW: action=record during each LLM call, then action=report for readable summary.
Actions: record (log event), report (readable summary), json (machine-readable),
reset (clear data), status (storage info).
ANTIPATTERN: not for debugging code behavior — this tracks token/latency stats only.
record requires llm_input_tokens + llm_output_tokens.

Parameters: `action`, `agent_id`, `intent`, `latency_ms`, `limit`, `llm_input_tokens`, `llm_output_tokens`, `model`, `note`

## `ctx_gain`

Local context-usage report — reports observed or estimated representation changes.
It is not a matched-workload benchmark or a universal savings claim.
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

Find files by glob pattern (respects .gitignore; multi-root via paths).
For file CONTENT search use ctx_search.

Parameters: `ignore_gitignore`, `max_results`, `path`, `paths`, `pattern`*

## `ctx_graph`

File-level dependency graph queries.
action=symbol path="file.rs::fnName" returns the DEFINITION (not usages — use ctx_search for references). neighbors=imports±direction, impact=reverse-dep blast radius, path from→to=dependency chain, diff since=HEAD~1=git change impact, diagram kind=deps|calls (Mermaid).
For understanding code use ctx_compose FIRST.

Parameters: `action`*, `depth`, `format`, `kind`, `path`, `project_root`, `since`, `to`

## `ctx_heatmap`

File access heatmap — shows most frequently accessed files per session.
action=status (default) for summary, action=detail for per-file access counts.
Identify hot files to optimize context usage.

Parameters: `action`, `path`

## `ctx_index`

Index orchestration — manage code graph index.
WORKFLOW: status → build → build-full (escalate if stale).
ANTI-PATTERN: build-full is expensive — use incremental build first.
Actions: status (state), build (incremental), build-full (rebuild),
why (is `path` indexed? if not, the rule that dropped it).

Parameters: `action`*, `path`, `project_root`

## `ctx_knowledge`

Persistent memory across sessions — remember decisions, patterns, and facts for recall.
WORKFLOW: save after completing significant tasks; recall at session start.
action=remember value='Y' saves a fact (key optional — derived from value; content= is an accepted alias).
action=recall query='X' retrieves it (bare recall lists recent facts). action=status shows all categories.
action=consolidate imports latest session if present, runs lifecycle, then frees 25% facts/history/procedures capacity.
action=gotcha trigger='X' resolution='Y' for known pitfalls.
mode=semantic|exact for recall. category groups related facts.

Parameters: `action`*, `anchors`, `as_of`, `category`, `commands`, `confidence`, `content`, `dry_run`, `evidence`, `examples`, `format`, `key`, `limit`, `merge`, `mode`, `path`, `pattern_type`, `query`, `refs`, `resolution`, `severity`, `sources`, `steps`, `store`, `trigger`, `type`, `validity`, `value`

## `ctx_load_tools`

Load/unload specialized tool categories to reduce surface area.
WORKFLOW: list → load → unload when done.
ANTI-PATTERN: don't unload categories you're actively using.
Actions: load|unload|list. Categories: arch, debug, memory, metrics, session.
Core is always loaded.

Parameters: `action`*, `category`

## `ctx_memory`

Store and retrieve durable cross-agent facts with BLAKE3 deduplication.

Parameters: `action`*, `agent`, `category`, `content`, `limit`, `max_age_seconds`, `max_entries`, `query`

## `ctx_metrics`

Session token statistics — cache hit rates, per-tool savings, pipeline metrics,
and signature backend ratios.
ANTI-PATTERN: not for real-time monitoring — snapshot of current session.
Complements ctx_radar for budget analysis.

Parameters: _none_

## `ctx_multi_repo`

Multi-repository — add, remove, search project directories.
WORKFLOW: list_roots → add_root/remove_root → search.
ANTI-PATTERN: not for single-repo projects — use ctx_search.
Actions: add_root|remove_root|list_roots|search|status|save_config.
Cross-repo search runs hybrid retrieval per root (BM25+dense+SPLADE)
and merges with RRF; mode="bm25" forces lexical-only.
ctx_search/ctx_glob/ctx_tree/ctx_read all accept a repo=<alias>
arg (not in their own schema) to target a registered root by
alias instead of the project root — list_roots shows the aliases.

Parameters: `action`*, `alias`, `max_results`, `mode`, `path`, `query`, `roots`

## `ctx_outline`

WORKFLOW: call BEFORE ctx_read to map code structure (a syntax-aware table of contents).
Accepts a FILE or a DIRECTORY (folder surface — per-file symbols). Symbols come from
tree-sitter (27 languages, real line spans); a conservative regex fallback covers the rest.
kind=fn|struct|class|trait|enum|impl|all filters by kind; match=<substr> filters by name
(case-insensitive); format=json emits deterministic JSON labelling the backend per file.
ANTIPATTERN: NOT for file content (use ctx_read) or deep understanding (use ctx_compose).

Parameters: `format`, `kind`, `match`, `path`*

## `ctx_pack`

WORKFLOW: create -> export -> import -> install for sharing context state.
ANTIPATTERN: NOT for ephemeral session save (use ctx_session).
Context Package Manager — create, install, manage portable context packages
with knowledge, graph, session patterns, and gotchas.
Actions: pr, create, list, info, remove, install, export, import, auto_load, summary, bundle.
create: select layers/categories/keys/facts; preview:true lists what would be packed, writes nothing.
bundle: one budgeted XML document (task-ranked files, signatures, tree) that fits a chat input box.
Saves tokens: pre-built context state (avoids re-building).

Parameters: `action`*, `apply`, `author`, `base`, `categories`, `depth`, `description`, `diff`, `emit`, `enable`, `facts`, `file`, `format`, `ignore`, `include`, `intent`, `keys`, `knowledge_limit`, `layers`, `level`, `limit`, `name`, `path`, `preview`, `project_root`, `redistributable`, `scope`, `tags`, `unit`, `version`, `with_auto`, `with_knowledge`

## `ctx_package`

WORKFLOW: save -> resume in new session for agent handoff.
ANTIPATTERN: NOT for internal session persistence (use ctx_session).
Self-contained JSON bundles: session state, summaries,
knowledge. Actions: save, resume, list, info.
Saves tokens: portable across sessions/agents.

Parameters: `action`, `description`, `path`

## `ctx_patch`

Safe file edit, project-relative paths. replace_unique(path,old_text,new_text): exact unique replacement, needs no anchors. Line ops take line+hash from ctx_read(mode="anchored:N-M"); CONFLICT means re-read. ops[] batch (cross-file, incl. replace_unique/replace_symbol); create and replace_all only as top-level calls.

Parameters: `dry_run`, `end_hash`, `end_line`, `find`, `hash`, `line`, `name`, `new_text`, `old_text`, `op`, `ops`, `path`, `replace`, `start_hash`, `start_line`

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

Export machine-readable ContextProofV1 (Verifier, Pipeline, Provenance).
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
Data flows through consolidation pipeline; results searchable via ctx_search (action="semantic").

Parameters: `action`*, `id`, `iid`, `labels`, `limit`, `mode`, `project`, `provider`, `query`, `resource`, `state`, `status`

## `ctx_radar`

Context budget breakdown — system prompt, messages, tools, reads, shell.
WORKFLOW: call when context window tight to find biggest consumers.
ANTIPATTERN: not for per-call timing — use ctx_metrics instead.
format=display (human-readable) or json (structured). Complements ctx_metrics
for comprehensive budget analysis. Saves tokens vs manual budget estimation.

Parameters: `format`

## `ctx_read`

Read files. No mode: ≤300 lines whole, larger files as outline with line ranges.
symbol=Name or lines:N-M reads just that part, also for edits.
To UNDERSTAND code run ctx_compose FIRST.

Parameters: `aggressiveness`, `engine_interface`, `fresh`, `limit`, `mode`, `offset`, `path`, `paths`, `protect`, `raw`, `start_line`, `symbol`

## `ctx_search`

Search code: regex(pattern, default) | semantic(query) | symbol(name|handle) | find_related(file_path,line). Read-only. anchored=true enables ctx_patch refs; queries batches regex searches. Run ctx_compose FIRST.

Parameters: `action`, `anchored`, `exclude`, `exclude_pattern`, `file`, `file_path`, `handle`, `include`, `kind`, `line`, `max_results`, `mode`, `name`, `path`, `paths`, `pattern`, `queries`, `query`, `top_k`

## `ctx_session`

Session memory. save at session end, load at start, status = snapshot;
task|finding|decision record progress (value=text).
ANTIPATTERN: permanent project knowledge → ctx_knowledge.

Parameters: `action`*, `keep_recent_turns`, `session_id`, `tag`, `value`

## `ctx_shell`

WORKFLOW: preferred — auto-compresses output (build/test/log).
Big logs: narrow the command (grep, sed -n, head/tail).
[exit:N] on errors (lossless).
POLICY: allowlisted read-only path.
A [BLOCKED] command is permanent; ctx_execute shares the same allowlist — fix with `lean-ctx allow <cmd>`.
ANTIPATTERN: multi-line scripts, sh/bash script.sh, $var-as-command → ctx_execute (when exposed).

Parameters: `background_action`, `command`, `cwd`, `env`, `inline`, `job_id`, `raw`, `run_in_background`, `timeout_ms`

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
WORKFLOW: lightweight orientation before ctx_compose.

Parameters: `depth`, `path`, `paths`, `respect_gitignore`, `show_hidden`

## `ctx_url_read`

Fetch URL: pages→Markdown; PDF→text; YouTube→transcript; mode=auto best per type.
mode=facts|quotes for research (claims+confidence). query='topic' focuses extraction.
GitHub blob/raw URLs auto-resolve to raw file. SSRF-guarded (no private IPs).
max_tokens=6000; timeout_secs=20 (max 60).

Parameters: `max_items`, `max_tokens`, `mode`, `query`, `timeout_secs`, `url`*

## `ctx_verify`

Verification observability — tool call statistics and claim-based verification.
WORKFLOW: action=stats to monitor tool usage; action=proof|v2 for a ContextProofV2 of runtime-checked policy claims (PathJail per touched file).
Actions: stats (format=summary|json|both, default summary) | proof|v2 (format=summary|json, default json).
ANTIPATTERN: not for runtime verification during active development — use for periodic audit.

Parameters: `action`, `format`

## `ctx_work_graph`

Free persistent project-scoped bounded local Work Graph for multi-agent coordination. The host launches agents; start binds a host-launched child to its node (verified context capsule, leases, launch env with the child's delivery profile), receipt or cancel ends it and a stop revokes its delivery. Actions: create|delegate|claim|start|consume|complete|receipt|accept|fuse|attribution|value_report|cancel|get|observe|list. Observe returns compact state without context payloads or execution fences. Node path claims share one lease namespace with ctx_agent path leases: an agent holding src blocks starting a node that claims src/x.

Parameters: `accepted_nodes`, `action`*, `capsule_ref`, `claims`, `cost_micros`, `execution_fence`, `expected_outcome_ref`, `graph_id`, `if_revision`, `max_concurrency`, `node_id`, `outcome`, `outcome_ref`, `parent_node_id`, `path_claims`, `policy_ref`, `receipt_id`, `stop_reason`, `task_ref`, `timeout_ms`, `to_agent`, `tokens`

## `shell`

Shell command with auto-compression (~95 patterns). Alias for ctx_shell.
Output is compressed for token savings. Big logs: narrow the command (grep, sed -n, head/tail).
Use when your MCP client prefers shell/bash over ctx_shell — transparently
delegates to ctx_shell internals.

Parameters: `command`*, `cwd`, `raw`

