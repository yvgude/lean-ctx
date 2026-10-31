# Appendix — MCP Tool Map

A categorized overview of the MCP tools lean-ctx advertises. Your AI editor
calls these instead of its native file/search tools.

> **Full parameter detail:** [`generated/mcp-tools.md`](generated/mcp-tools.md)
> is generated from the registry (`rust/src/server/registry.rs`) and is the
> single source of truth for tool names, descriptions and parameters. This page
> only groups the tools and shows profile membership. Regenerate with
> `cargo run --example gen_docs --features dev-tools`.

## Tool profiles

The profile decides which tools are *advertised* in `tools/list`. Every
registered tool stays callable through `ctx_call`. Switch with
`lean-ctx tools <lean|minimal|standard|power|auto>`; `lean-ctx tools show`
prints the active set.

| Profile | Tools advertised | Notes |
|---------|------------------|-------|
| **lean** (default, no profile pinned) | 12 (lazy core) | Smallest surface with `ctx_call` for the rest |
| **minimal** | 5 | `ctx_read`, `ctx_shell`, `ctx_search`, `ctx_glob`, `ctx_tree` |
| **standard** | 15 | minimal + `ctx_compose`, `ctx_knowledge`, `ctx_session`, `ctx_callgraph`, `ctx_graph`, `ctx_delta`, `ctx_execute`, `ctx_expand`, `ctx_url_read`, `ctx_patch` |
| **power** | all public tools (51) | Every advertised tool |
| **auto** | 5 → all | Starts minimal, escalates with session complexity |

- **Lazy core (12):** `ctx_read`, `ctx_shell`, `shell`, `ctx_search`,
  `ctx_glob`, `ctx_tree`, `ctx_session`, `ctx_compose`, `ctx_callgraph`,
  `ctx_patch`, `ctx_call`, `ctx_expand`. Clients with a reliable native
  str-replace editor (Cursor, Zed, Windsurf, …) do not get `ctx_patch` in the
  lazy core.
- Profile definitions: `rust/src/core/tool_profiles.rs` (`MINIMAL_TOOLS`,
  `STANDARD_TOOLS`) and `rust/src/tool_defs/mod.rs` (`CORE_TOOL_NAMES`).

In the tables below, **Core** marks lazy-core membership and **Profile** the
smallest pinned profile that advertises the tool (`M` minimal, `S` standard,
`P` power).

---

## 1. Read, search, edit, shell

| Tool | Purpose | Key actions / modes | Core | Profile |
|------|---------|---------------------|------|---------|
| `ctx_read` | Read files with session cache and compression; batch via `paths[]` | `mode`: auto (default), full, anchored, raw, signatures, map, diff, lines:N-M, reference, task | ✓ | M |
| `ctx_delta` | Only the lines changed since the last `ctx_read` of a file | — | | S |
| `ctx_outline` | Syntax-aware symbol table of a file or directory | `kind`, `match`, `format` | | P |
| `ctx_search` | Code search; one entry for all engines | `action`: regex (default), semantic, symbol, find_related | ✓ | M |
| `ctx_glob` | Find files by glob pattern, gitignore-aware, multi-root | — | ✓ | M |
| `ctx_tree` | Compact directory tree with file counts | — | ✓ | M |
| `ctx_compose` | First-pass context for one task: ranked files with inline source | `task`* | ✓ | S |
| `ctx_patch` | Hash-anchored edits from `ctx_read mode="anchored"`; `replace_unique`, `create` | `op` / `ops[]` | ✓ | S |
| `ctx_edit` | Legacy search-and-replace edit with race guards | — | | P |
| `ctx_shell` | Run shell commands with output compression | `raw`, `inline`, `run_in_background` | ✓ | M |
| `shell` | Alias of `ctx_shell` for clients whose model prefers a `shell` tool | — | ✓ | P |
| `ctx_execute` | Sandboxed code execution (11 languages) | code \| batch \| file | | S |
| `ctx_expand` | Retrieve archived or withheld tool output | retrieve \| list \| search_all | ✓ | S |
| `ctx_url_read` | Web page, PDF, feed or YouTube transcript as compressed context; SSRF-guarded | `mode`: auto \| markdown \| text \| links \| facts \| quotes \| transcript | | S |
| `ctx_git_read` | Read a remote git repo via cached shallow clone | `mode`: overview \| tree \| read \| grep | | P |
| `ctx_multi_repo` | Register several repositories and search across them | add_root \| remove_root \| list_roots \| search \| status \| save_config | | P |

## 2. Session, memory, knowledge

| Tool | Purpose | Key actions | Core | Profile |
|------|---------|-------------|------|---------|
| `ctx_session` | Session memory: tasks, findings, decisions | status \| handoff \| load \| save \| task \| finding \| decision \| compact \| restore \| list \| … | ✓ | S |
| `ctx_knowledge` | Persistent project knowledge across sessions | remember \| recall \| search \| pattern \| gotcha \| relate \| relations \| consolidate \| restore \| status \| timeline \| rooms \| wakeup \| remove \| export \| import | | S |
| `ctx_memory` | Durable cross-agent facts with deduplication | put \| get \| list \| stats \| prune | | P |
| `ctx_compress` | Compress the read cache to free token budget | `include_signatures` | | P |
| `ctx_compress_memory` | Compress a memory/config file (CLAUDE.md, .cursorrules); `.original.md` backup | `path`* | | P |
| `ctx_transcript_compact` | Deterministically compact an OpenAI-format message array | `messages`* | | P |
| `ctx_artifacts` | Context-artifact registry with BM25 search | list \| status \| index \| reindex \| search \| remove | | P |
| `ctx_index` | Code index state and builds; explain why a file is not indexed | status \| build \| build-full \| why | | P |
| `ctx_checkpoint` | Shadow git history of the agent's changes, separate from the project's `.git` | snapshot \| log \| diff \| restore | | P |
| `ctx_package` | Portable session/summary/knowledge bundles for handoff | save \| resume \| list \| info | | P |
| `ctx_pack` | Context Package Manager (PR packs, installable context, budgeted bundle) | pr \| create \| list \| info \| remove \| install \| export \| import \| auto_load \| summary \| bundle | | P |

## 3. Code graph

| Tool | Purpose | Key actions | Core | Profile |
|------|---------|-------------|------|---------|
| `ctx_graph` | File-level dependency graph: definitions, impact, chains, diagrams | build \| related \| symbol \| impact \| status \| enrich \| context \| diagram \| neighbors \| path \| explain \| diff | | S |
| `ctx_callgraph` | Function call edges for one symbol | callers \| callees \| trace \| risk | ✓ | S |

## 4. Context control and proof

| Tool | Purpose | Key actions | Core | Profile |
|------|---------|-------------|------|---------|
| `ctx_control` | Reversible overlays on the delivered context | exclude \| include \| pin \| unpin \| evict \| set_view \| set_priority \| mark_outdated \| reset \| list \| history | | P |
| `ctx_context` | Session context overview: cached files, seen files, state | — | | P |
| `ctx_preload` | Cache task-relevant files and return a compact summary | `task`* | | P |
| `ctx_prefetch` | Warm the cache for blast-radius files | `task`, `changed_files`, `budget_tokens` | | P |
| `ctx_dedup` | Cross-file deduplication hints for read output | analyze \| apply | | P |
| `ctx_verify` | Tool-call statistics and ContextProofV2 claims | stats \| proof \| v2 | | P |
| `ctx_proof` | Export machine-readable ContextProofV1 | export | | P |

## 5. Usage and analytics

| Tool | Purpose | Key actions | Core | Profile |
|------|---------|-------------|------|---------|
| `ctx_metrics` | Session token statistics and cache rates | — | | P |
| `ctx_radar` | Context-budget breakdown (prompt, messages, tools, reads, shell) | `format` | | P |
| `ctx_cost` | Local cost attribution per agent/tool | report \| agent \| tools \| json \| reset \| status | | P |
| `ctx_gain` | Local context-usage report incl. "wrapped" summary | status \| report \| score \| cost \| tasks \| heatmap \| wrapped \| agents \| json | | P |
| `ctx_heatmap` | File-access heatmap | status \| detail | | P |
| `ctx_feedback` | Record and report LLM token/latency metrics | record \| report \| json \| reset \| status | | P |
| `ctx_discover` | Shell commands not yet using lean-ctx compression | `limit` | | P |

## 6. External context gateways

| Tool | Purpose | Key actions | Core | Profile |
|------|---------|-------------|------|---------|
| `ctx_provider` | GitHub, GitLab, Jira, Postgres, MCP bridges, custom REST | discover \| list \| status \| refresh \| configure \| query \| mcp_resources \| gitlab_issues \| gitlab_issue \| gitlab_mrs \| gitlab_pipelines | | P |
| `ctx_tools` | Gateway to downstream MCP servers at near-constant context cost | find \| call \| list \| refresh | | P |

## 7. Meta and coordination

| Tool | Purpose | Key actions | Core | Profile |
|------|---------|-------------|------|---------|
| `ctx_call` | Invoke any registered tool by name | `name`*, `arguments` | ✓ | P |
| `ctx_discover_tools` | Keyword search across all tools | `query` | | P |
| `ctx_load_tools` | Load/unload tool categories at runtime | load \| unload \| list | | P |
| `ctx_tasks` | From 3.12.1 (hidden in 3.12.0): Hive tasks of the project: read, claim (under the opt-in), report, submit with evidence, release, propose | list \| show \| claim \| update \| submit \| release \| propose | | P |
| `ctx_work_graph` | Project-scoped local Work Graph for multi-agent coordination; the host launches agents, `start` binds and controls them | create \| delegate \| claim \| start \| consume \| complete \| receipt \| accept \| fuse \| attribution \| value_report \| cancel \| get \| observe \| list | | P |

`*` = required parameter.

## Notes

1. `power` enables every tool; `ToolProfile::is_tool_enabled()` returns `true`
   for everything under power.
2. `ctx_load_tools` controls the dynamic categories (`arch`, `debug`, `memory`,
   `metrics`, `session`; `rust/src/server/dynamic_tools.rs`). The category gate
   applies only to the default lean surface on clients that support
   `list_changed`; a pinned profile is authoritative.
3. Not advertised on any profile, but callable: the local collaboration
   compatibility tools `ctx_agent`, `ctx_share`, `ctx_handoff`, and
   the host-integration hook `ctx_shape`.

## Removed in the focus release

The focus release (2026-10, [`docs/migration/focus-2026-10.md`](../migration/focus-2026-10.md))
removed the tools below. Only the names survive: a call, directly or through
`ctx_call`, returns the way forward instead of "Unknown tool"
(`removed_tool` in `rust/src/server/dynamic_tools.rs`).

| Removed tool | Use instead |
|--------------|-------------|
| `ctx_smart_read` | `ctx_read` — auto-selects the mode (omit `mode`, or `mode="auto"`) |
| `ctx_multi_read` | `ctx_read` — batch-reads via `paths=[…]` |
| `ctx_semantic_search` | `ctx_search action="semantic"`; reindex/find_related are actions too |
| `ctx_symbol` | `ctx_search action="symbol"` (`name=…`, optional `file`/`kind`) |
| `ctx_overview`, `ctx_summary`, `ctx_explore` | `ctx_compose(task=…)` — ranked files, symbols and structure for a task |
| `ctx_plan`, `ctx_compile`, `ctx_fill` | `ctx_compose` selects under the turn budget; read chosen files with `ctx_read` |
| `ctx_impact` | `ctx_graph action="impact"` (`path=…`), `action="path"` for chains, `action="diff"` for changes |
| `ctx_architecture`, `ctx_repomap` | `ctx_graph action="diagram"\|"explain"\|"related"`; `ctx_tree` for the directory map |
| `ctx_routes` | `ctx_search` for route definitions, `ctx_graph action="related"` for handlers |
| `ctx_retrieve` | `ctx_expand id=…` — withheld or archived content |
| `ctx_cache` | `ctx_read fresh=true` re-reads; maintenance via `lean-ctx cache prune\|reset` |
| `ctx_smells`, `ctx_quality`, `ctx_optimize`, `ctx_review` | Retired — use the host's own review and linting |
| `ctx_quality_lab`, `ctx_benchmark`, `ctx_perf`, `ctx_cognitive`, `ctx_ledger`, `ctx_live_zone` | Retired — `lean-ctx gain`, `lean-ctx doctor`, dashboard |
| `ctx_refactor` | Retired — write refactorings are host execution; `ctx_callgraph` keeps references |
| `ctx_analyze`, `ctx_compare` | Retired — `ctx_read mode="auto"`; `lean-ctx compress diff <file>` previews compression |
| `ctx_crush` | Retired — `ctx_read`/`ctx_shell` apply the JSON/CSV/YAML crushers automatically |
| `ctx_response` | Retired — LeanCTX shapes the context the model reads, not its responses |
| `ctx_skillify`, `ctx_rules` | Retired — `lean-ctx rules status\|diff` or `lean-ctx doctor` |
| `ctx_workflow` | Retired — workflow execution belongs to the host; `ctx_work_graph` keeps shared coordination state |
| `ctx_task` | Retired — task assignment is host execution; `ctx_work_graph` keeps shared coordination state |
| `ctx_intent` | Retired — intent is inferred from tool calls; `ctx_compose(task=…)` selects context for a task |

`ctx_plugins` was also removed; it has no entry in the table above, so a call
returns "Unknown tool".

---

## lean-md addon (`.lmd.md` render pipeline)

`.lmd.md` / `.lean-md` rendering is provided by the **external lean-md addon**
(`dasTholo/lean-md`), not by lean-ctx itself. `ctx_md_render` / `ctx_md_check` are
exposed by the addon's MCP server once installed (`lean-ctx addon add @dasTholo/lean-md`); a
`.lmd.md` passed to `ctx_read` is returned **raw** — lean-ctx never renders it
(rendering is an explicit addon call). The `@directive` catalog and `@lean-md` header fields live in the
addon repo.

- **Integration reference:** [`21-lean-md.md`](21-lean-md.md)
- **Addon repo:** https://github.com/dasTholo/lean-md

---
