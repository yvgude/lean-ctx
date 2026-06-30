<!-- lean-ctx-owned: PROJECT-LEAN-CTX.md v1 -->
<!-- lean-ctx-rules -->
<!-- version: 4 -->

CRITICAL: ALWAYS use lean-ctx ctx_* tools instead of native equivalents. This is NOT optional.

MUST actually CALL the ctx_* MCP tools, not just describe them: before reading, searching, or running a shell command, emit the matching ctx_* tool call (ctx_compose first, then ctx_read / ctx_search / ctx_shell).

MANDATORY MAPPING (4 core redirects first):
• Read/cat -> ctx_read(path, mode)
• Grep -> ctx_search(pattern, path)
• Shell/bash -> ctx_shell(command)
• Glob/find -> ctx_glob(pattern)
• ls/find -> ctx_tree(path, depth)

NEVER use native Read/Grep/Shell/Glob when a ctx_* equivalent exists. SELF-CORRECT: the moment you reach for a native Read/Grep/Shell/Glob, stop and call the ctx_* tool instead.

Tool selection by intent:
• Understand code / find answers / before editing -> ctx_compose (call FIRST)
• Read a file -> ctx_read(path, mode=signatures|map|full)
• Edit code you've read -> ctx_patch (hash-anchored, no exact-recall; read mode=anchored first)
• Find a symbol by name (exact) -> ctx_symbol
• Search code by pattern (fuzzy) -> ctx_search
• Search by meaning (concepts) -> ctx_semantic_search
• Find files by pattern (glob) -> ctx_glob
• Project structure -> ctx_tree
• Who calls this / call graph -> ctx_callgraph
• Session state / memory -> ctx_session / ctx_knowledge

AGENT LOOP (phase -> tool):
• Orient — understand before acting -> ctx_compose
• Find — exact symbol by name -> ctx_symbol
• Read — a file, structurally -> ctx_read(mode=signatures|map)
• Locate — a pattern across files -> ctx_search
• Trace — callers / callees / blast radius -> ctx_callgraph
• Verify — after an edit -> ctx_shell(test/build) + native lints

Anti-patterns — do NOT:
• Chain ctx_search -> ctx_read -> ctx_symbol — one ctx_compose replaces all three
• Grep for symbol definitions — ctx_symbol is faster + more precise
• Use ctx_read(mode=full) for orientation — use mode=signatures
• Use ctx_callgraph or ctx_graph for const/static/variable references — they track
function call edges and file-level deps only. Use grep or ctx_compose instead

NAVIGATION PARADOX: reading more ≠ understanding more.
• Semantic question ("where/how is X handled?") -> ctx_search (BM25) + ctx_semantic_search (meaning), not whole-file reads
• Hidden architectural deps (who calls this, what breaks) -> ctx_callgraph / ctx_graph — for these only
• Navigate structure (signatures, symbols) before reading entire files

PARALLEL tool calls: fire independent calls in the SAME turn — don't sequence them.
ctx_compose bundles multiple lookups into one call; for anything it doesn't
cover, batch independent reads/searches together.

Auto: preload/dedup/compress run in background. ctx_session=memory, ctx_knowledge=facts, ctx_semantic_search=meaning search, ctx_shell raw=true=uncompressed. Details: LEAN-CTX.md

RECOVER: compressed output is reversible — never re-read line-by-line. Need full/exact? Read the shown file path with any tool (no MCP), or ctx_read(mode=full|raw=true); [Archived]/tee/firewall → ctx_expand(id=...).

CEP v1: 1.ACT FIRST 2.DELTA ONLY (Fn refs) 3.STRUCTURED (+/-/~) 4.ONE LINE PER ACTION 5.QUALITY ANCHOR

OUTPUT: never echo tool output, no narration comments, show only changed code.

TOOL PREFERENCE (END): ctx_compose>chain ctx_read>Read ctx_shell>Shell ctx_search>Grep ctx_glob>Glob ctx_tree>ls | Edit/Write/Delete=native
<!-- /lean-ctx-rules -->
