# MCP tool consolidation plan

> Historical plan: the consolidation shipped in 3.11.5 — see [docs/migration/focus-2026-10.md](migration/focus-2026-10.md) for what was removed and the successors.

Issue: #1376

## Problem and audit

Agents currently receive too many overlapping MCP tool definitions.  Large tool
lists consume context, duplicate concepts across schemas, and make tool choice
less reliable.

`rust/src/tool_defs/` is the serving-layer source: `granular_tool_defs()` derives
its full list from `server::registry::build_registry()`.  The source audit on
this branch found **82 built-in entry points** in that registry.  Plugin tools
are appended dynamically and are not part of that fixed count.  The 26 tool
identifiers mentioned directly in `tool_defs/` are profile, annotation, or
unified-surface references—not the full inventory.  The existing MCP appendix
still says 80, so it needs updating as part of implementation.

Current built-ins (82):

`ctx_agent`, `ctx_analyze`, `ctx_architecture`, `ctx_artifacts`,
`ctx_benchmark`, `ctx_cache`, `ctx_call`, `ctx_callgraph`, `ctx_checkpoint`,
`ctx_compare`, `ctx_compile`, `ctx_compose`, `ctx_compress`,
`ctx_compress_memory`, `ctx_context`, `ctx_control`, `ctx_cost`, `ctx_dedup`,
`ctx_delta`, `ctx_discover`, `ctx_discover_tools`, `ctx_edit`, `ctx_execute`,
`ctx_expand`, `ctx_explore`, `ctx_feedback`, `ctx_fill`, `ctx_gain`,
`ctx_git_read`, `ctx_glob`, `ctx_graph`, `ctx_handoff`, `ctx_heatmap`,
`ctx_impact`, `ctx_index`, `ctx_intent`, `ctx_knowledge`, `ctx_ledger`,
`ctx_load_tools`, `ctx_metrics`, `ctx_multi_read`, `ctx_multi_repo`,
`ctx_outline`, `ctx_overview`, `ctx_pack`, `ctx_package`, `ctx_patch`,
`ctx_plan`, `ctx_plugins`, `ctx_prefetch`, `ctx_preload`, `ctx_proof`,
`ctx_provider`, `ctx_quality`, `ctx_quality_lab`, `ctx_radar`, `ctx_read`,
`ctx_refactor`, `ctx_repomap`, `ctx_response`, `ctx_retrieve`, `ctx_review`,
`ctx_routes`, `ctx_rules`, `ctx_search`, `ctx_semantic_search`, `ctx_session`,
`ctx_share`, `ctx_shell`, `ctx_skillify`, `ctx_smart_read`, `ctx_smells`,
`ctx_summary`, `ctx_symbol`, `ctx_task`, `ctx_tools`,
`ctx_transcript_compact`, `ctx_tree`, `ctx_url_read`, `ctx_verify`,
`ctx_workflow`, and `shell`.

## Design goals

- Advertise **35 focused tools**: within the 25–35 target and small enough for
  a useful default surface.
- Keep one tool per stable user intent; put variants behind an explicit
  `action` rather than publishing near-identical schemas.
- Preserve direct tools when they represent a distinct safety boundary
  (`ctx_shell`, `ctx_edit`, `ctx_execute`) or a frequent code-intelligence
  workflow.
- Make `ctx_call` the escape hatch for disabled profile tools, not the normal
  path for ordinary work.
- Retain every existing name as a forwarding alias during migration.

## Proposed advertised surface

| Area | Focused tools | Responsibility |
|---|---|---|
| File and research | `ctx_read`, `ctx_search`, `ctx_files`, `ctx_shell`, `ctx_edit`, `ctx_explore`, `ctx_source`, `ctx_execute` | Read, search, locate files, run commands, edit, orient, fetch sources, and run code. |
| Context and persistence | `ctx_session`, `ctx_knowledge`, `ctx_context`, `ctx_compress`, `ctx_package`, `ctx_checkpoint` | Session state, durable facts, context operations, compression, portable packs, and recoverable snapshots. |
| Collaboration | `ctx_agents`, `ctx_handoff`, `ctx_workflow`, `ctx_rules` | Agent coordination, handoffs, task state, and shared policy. |
| Code intelligence | `ctx_code`, `ctx_callgraph`, `ctx_impact`, `ctx_routes`, `ctx_refactor`, `ctx_review`, `ctx_quality` | Structural analysis, callers, blast radius, routes, refactors, review, and code health. |
| Platform | `ctx_index`, `ctx_provider`, `ctx_tools`, `ctx_plugins`, `ctx_profile`, `ctx_call` | Indexes, external providers, downstream tools, plugins, tool visibility, and lazy invocation. |
| Observability | `ctx_metrics`, `ctx_benchmark`, `ctx_verify`, `ctx_discover` | Usage/cost telemetry, compression evaluation, proofs, and missed-opportunity discovery. |

The table contains 35 tools.  `ctx_files`, `ctx_source`, `ctx_agents`, and
`ctx_profile` are new canonical names; the other canonical names keep their
existing implementation where possible.

### Action contracts

Consolidation must use typed, documented `action` enums—not an unbounded bag of
optional fields.  Shared fields remain common only when their meanings match.

| Canonical tool | Actions / absorbed responsibilities |
|---|---|
| `ctx_read` | `read`, `multi`, `smart`, `delta`, `symbol`, `outline`, `retrieve` |
| `ctx_search` | `regex`, `semantic` |
| `ctx_files` | `glob`, `tree` |
| `ctx_edit` | `replace`, `patch` |
| `ctx_explore` | `compose`, `explore`, `overview`, `repomap` |
| `ctx_source` | `url`, `git` |
| `ctx_session` | `status`, `load`, `save`, `task`, `finding`, `decision`, `summary` |
| `ctx_context` | `fill`, `expand`, `cache`, `ledger`, `control`, `dedup`, `intent`, `plan`, `compile`, `response`, `preload`, `prefetch` |
| `ctx_compress` | `checkpoint`, `memory`, `transcript` |
| `ctx_package` | `context_package`, `pack`, `artifacts`, `skillify` |
| `ctx_agents` | `agent`, `share`, `task` |
| `ctx_code` | `graph`, `architecture` |
| `ctx_quality` | `quality`, `lab`, `smells` |
| `ctx_profile` | `load`, `unload`, `list` |
| `ctx_metrics` | `metrics`, `radar`, `cost`, `gain`, `heatmap`, `feedback` |
| `ctx_benchmark` | `benchmark`, `analyze`, `compare` |
| `ctx_verify` | `verify`, `proof` |
| `ctx_tools` | `find`, `call`, `list`, `refresh`, `discover` |
| `ctx_index` | `status`, `build`, `build_full`, `multi_repo` |

All other canonical tools keep their present action contracts.

## Migration aliases

Old names remain registered as thin aliases.  An alias validates its legacy
schema, translates it to the canonical request, emits a one-time deprecation
notice outside the tool result body, and invokes the same handler.  It must not
duplicate business logic or alter output bytes.

| Legacy tool | Canonical route |
|---|---|
| `ctx_multi_read`, `ctx_smart_read`, `ctx_delta`, `ctx_symbol`, `ctx_outline`, `ctx_retrieve` | `ctx_read action=multi|smart|delta|symbol|outline|retrieve` |
| `ctx_semantic_search` | `ctx_search action=semantic` |
| `ctx_glob`, `ctx_tree` | `ctx_files action=glob|tree` |
| `shell` | `ctx_shell` |
| `ctx_patch` | `ctx_edit action=patch` |
| `ctx_compose`, `ctx_overview`, `ctx_repomap` | `ctx_explore action=compose|overview|repomap` |
| `ctx_url_read`, `ctx_git_read` | `ctx_source action=url|git` |
| `ctx_summary` | `ctx_session action=summary` |
| `ctx_fill`, `ctx_expand`, `ctx_cache`, `ctx_ledger`, `ctx_control`, `ctx_dedup`, `ctx_intent`, `ctx_plan`, `ctx_compile`, `ctx_response`, `ctx_preload`, `ctx_prefetch` | `ctx_context` with the matching action |
| `ctx_compress_memory`, `ctx_transcript_compact` | `ctx_compress action=memory|transcript` |
| `ctx_pack`, `ctx_artifacts`, `ctx_skillify` | `ctx_package action=pack|artifacts|skillify` |
| `ctx_agent`, `ctx_share`, `ctx_task` | `ctx_agents action=agent|share|task` |
| `ctx_graph`, `ctx_architecture` | `ctx_code action=graph|architecture` |
| `ctx_quality_lab`, `ctx_smells` | `ctx_quality action=lab|smells` |
| `ctx_load_tools` | `ctx_profile action=load|unload|list` |
| `ctx_radar`, `ctx_cost`, `ctx_gain`, `ctx_heatmap`, `ctx_feedback` | `ctx_metrics action=radar|cost|gain|heatmap|feedback` |
| `ctx_analyze`, `ctx_compare` | `ctx_benchmark action=analyze|compare` |
| `ctx_proof` | `ctx_verify action=proof` |
| `ctx_discover_tools` | `ctx_tools action=discover` |
| `ctx_multi_repo` | `ctx_index action=multi_repo` |

Tools not named in the table are already canonical and retain their existing
name: `ctx_read`, `ctx_search`, `ctx_shell`, `ctx_edit`, `ctx_explore`,
`ctx_execute`, `ctx_session`, `ctx_knowledge`, `ctx_context`, `ctx_compress`,
`ctx_package`, `ctx_checkpoint`, `ctx_handoff`, `ctx_workflow`, `ctx_rules`,
`ctx_callgraph`, `ctx_impact`, `ctx_routes`, `ctx_refactor`, `ctx_review`,
`ctx_quality`, `ctx_index`, `ctx_provider`, `ctx_tools`, `ctx_plugins`,
`ctx_call`, `ctx_benchmark`, `ctx_verify`, and `ctx_discover`.

## Rollout and acceptance criteria

1. Add canonical handlers and alias adapters while retaining all 82 current
   names; mark aliases as deprecated in descriptions and generated reference
   docs.
2. Make the 35-tool surface the default for new installations.  Keep `power`
   and an explicit compatibility profile able to advertise legacy aliases.
3. Update `CORE_TOOL_NAMES`, profiles, `ctx_discover_tools`, and the MCP tool
   appendix from the registry-derived inventory; do not maintain counts by
   hand.
4. Add table-driven tests that assert every legacy name routes to exactly one
   canonical action and that alias and canonical results are byte-identical.
5. Add an integration test that `tools/list` contains 35 names in the default
   profile and that the compatibility profile exposes every legacy alias.
6. Remove aliases only in a major version after telemetry shows no alias use
   for two release cycles; publish the removal date and replacement route in
   release notes first.

This sequencing preserves existing agents while allowing new agents to receive
a coherent, bounded MCP surface immediately.
