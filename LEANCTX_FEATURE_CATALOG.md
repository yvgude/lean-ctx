# LeanCTX Feature Catalog (SSOT Snapshot)

**Version:** `3.4.5`  
**Updated:** `2026-04-29`  
**Primary Sources:** `website/generated/mcp-tools.json`, `rust/src/tool_defs/granular.rs`, `README.md`

---

## Purpose

This catalog is the single feature inventory for LeanCTX at release/runtime level:

- Which MCP tools exist now
- Which entry points are canonical vs deprecated aliases
- Which read modes are supported
- Which capabilities are part of the shipped product surface

---

## Runtime Surface (Current)

- Granular MCP tools: **49**
- Unified MCP tools: **5**
- Read modes: **10** (`auto`, `full`, `map`, `signatures`, `diff`, `aggressive`, `entropy`, `task`, `reference`, `lines:N-M`)
- Positioning: Context Runtime for AI agents (shell hook + context server + setup integrations)

---

## Unified MCP Tools (5)

- `ctx`
- `ctx_read`
- `ctx_search`
- `ctx_shell`
- `ctx_tree`

---

## Granular MCP Tools (49)

### A) Read / Search / IO Surface

- `ctx_read`
- `ctx_multi_read`
- `ctx_smart_read`
- `ctx_tree`
- `ctx_search`
- `ctx_semantic_search`
- `ctx_shell`
- `ctx_edit`
- `ctx_delta`
- `ctx_dedup`
- `ctx_fill`
- `ctx_outline`
- `ctx_symbol`
- `ctx_routes`
- `ctx_context`

### B) Architecture / Analysis / Discovery

- `ctx_graph`
- `ctx_graph_diagram` _(deprecated alias -> `ctx_graph action=diagram`)_
- `ctx_callgraph` _(canonical)_
- `ctx_callers` _(deprecated alias -> `ctx_callgraph direction=callers`)_
- `ctx_callees` _(deprecated alias -> `ctx_callgraph direction=callees`)_
- `ctx_architecture`
- `ctx_impact`
- `ctx_review`
- `ctx_intent`
- `ctx_task`
- `ctx_overview`
- `ctx_preload`
- `ctx_prefetch`
- `ctx_discover`
- `ctx_analyze`

### C) Session / Knowledge / Multi-Agent

- `ctx_session`
- `ctx_knowledge`
- `ctx_agent`
- `ctx_share`
- `ctx_handoff`
- `ctx_workflow`
- `ctx_feedback`

### D) Compression / Metrics / Runtime Ops

- `ctx_cache`
- `ctx_compress`
- `ctx_expand`
- `ctx_compress_memory`
- `ctx_metrics`
- `ctx_cost`
- `ctx_heatmap`
- `ctx_gain` _(canonical for wrapped report via `action=wrapped`)_
- `ctx_wrapped` _(deprecated alias -> `ctx_gain action=wrapped`)_
- `ctx_execute`
- `ctx_benchmark`
- `ctx_response`

---

## Deprecation Map (Canonical Paths)

- `ctx_callers` -> `ctx_callgraph direction=callers`
- `ctx_callees` -> `ctx_callgraph direction=callees`
- `ctx_graph_diagram` -> `ctx_graph action=diagram`
- `ctx_wrapped` -> `ctx_gain action=wrapped`

---

## Notes For Releases

- Tool counts and tool names must match `website/generated/mcp-tools.json`.
- Any new tool or alias change requires synchronized updates in:
  - `README.md` and relevant package READMEs
  - `rust/src/templates/*` where applicable
  - this catalog
- Historical counts in old CHANGELOG entries remain unchanged by design.
