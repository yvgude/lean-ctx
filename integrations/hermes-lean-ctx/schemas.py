"""Engine tool schemas injected into the Hermes agent tool list.

Each schema is ``{"name", "description", "parameters"}`` per the Context Engine
plugin guide. Parameters mirror the daemon's real ``/v1`` tool argument schemas
so calls dispatched through :mod:`tools` are accepted as-is. These are
lean-ctx's recall / code-intelligence surface — the agent pulls exactly the
context it needs instead of holding it in the window.
"""

from __future__ import annotations

from typing import Any, Dict, List

CTX_SEARCH: Dict[str, Any] = {
    "name": "ctx_search",
    "description": (
        "Code/content search across the project (compact, .gitignore-aware). "
        "Regex by default; action=semantic for concept search (hybrid BM25 + "
        "embeddings) when keywords are insufficient."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "action": {"type": "string", "description": "regex (default)|semantic|symbol"},
            "pattern": {"type": "string", "description": "Regex pattern (action=regex)"},
            "query": {"type": "string", "description": "Concept query (action=semantic)"},
            "path": {"type": "string", "description": "Directory to search"},
            "include": {"type": "string", "description": "Glob filter, e.g. *.ts, src/**/*.rs"},
            "mode": {
                "type": "string",
                "enum": ["bm25", "dense", "hybrid"],
                "description": "Semantic ranking (default hybrid)",
            },
            "max_results": {"type": "integer", "description": "Default 20"},
            "top_k": {"type": "integer", "description": "Semantic result count (default 10)"},
        },
        "required": [],
    },
}

CTX_READ: Dict[str, Any] = {
    "name": "ctx_read",
    "description": (
        "Read a file (cached + compressed). Prefer over loading whole files into "
        "context; use mode/line ranges to read only what is needed."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Absolute file path"},
            "mode": {
                "type": "string",
                "description": "auto (default)|full|map|signatures|lines:N-M|...",
            },
            "start_line": {"type": "integer", "description": "First line, 1-based"},
            "limit": {"type": "integer", "description": "Max lines to read"},
        },
        "required": ["path"],
    },
}

CTX_EXPAND: Dict[str, Any] = {
    "name": "ctx_expand",
    "description": (
        "Retrieve archived/firewalled tool output (zero-loss) by id or handle "
        "(@F1). Use to recover full detail that was compacted away."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "action": {"type": "string", "description": "retrieve (default)|list|search_all"},
            "id": {"type": "string", "description": "Archive id or handle ref (@F1)"},
            "query": {"type": "string", "description": "search_all query"},
            "head": {"type": "integer", "description": "First N lines"},
            "tail": {"type": "integer", "description": "Last N lines"},
        },
        "required": [],
    },
}

CTX_KNOWLEDGE: Dict[str, Any] = {
    "name": "ctx_knowledge",
    "description": (
        "Persistent cross-session project knowledge: remember/recall facts, "
        "patterns and gotchas with temporal validity."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "description": "remember|recall|search|pattern|gotcha|status|timeline|remove",
            },
            "query": {"type": "string", "description": "For recall/search"},
            "key": {"type": "string"},
            "value": {"type": "string"},
            "category": {"type": "string", "description": "Fact category"},
            "mode": {"type": "string", "description": "Recall: auto|exact|semantic|hybrid"},
        },
        "required": ["action"],
    },
}

# Order is stable for deterministic tool-list injection.
ALL_SCHEMAS: List[Dict[str, Any]] = [
    CTX_SEARCH,
    CTX_READ,
    CTX_EXPAND,
    CTX_KNOWLEDGE,
]

TOOL_NAMES: List[str] = [s["name"] for s in ALL_SCHEMAS]


def recall_hint() -> str:
    """One-line hint appended to compaction summaries listing recovery tools."""
    return "Recover detail with: " + ", ".join(f"{n}()" for n in TOOL_NAMES) + "."
