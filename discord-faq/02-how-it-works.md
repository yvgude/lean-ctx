# **FAQ — How It Works**

---

**Q: What does lean-ctx actually do?**
lean-ctx sits between your AI tool and the system. It has two layers:
1. **Shell Hook** — transparently compresses CLI output (git, ls, npm, cargo, etc.) using 90+ patterns before it reaches the LLM
2. **MCP Server** — 49 tools for cached file reads, 10 read modes, deltas, dedup, memory, multi-agent coordination, and more

Result: **60–99% fewer tokens** per session.

**Q: What's the difference between Shell Hook and MCP Server?**
- **Shell Hook**: Compresses output of regular shell commands (git status, ls, npm test, etc.). Works automatically once installed. No code changes needed.
- **MCP Server**: Provides specialized `ctx_*` tools (ctx_read, ctx_shell, ctx_search, etc.) that your AI tool calls instead of native file/shell tools. Offers caching, read modes, and intelligence features.

Both work together for maximum savings.

**Q: What are the 10 read modes?**
| Mode | Use when... |
|------|-------------|
| `auto` | You don't know — lean-ctx picks the best mode |
| `full` | You need the complete file content |
| `map` | You need the structure (deps, exports, functions) |
| `signatures` | You need the API surface only |
| `diff` | You only want changes since last read |
| `aggressive` | Maximum compression, task-aware |
| `entropy` | Focus on high-information fragments |
| `task` | Filtered by current task context |
| `reference` | Minimal citation-style excerpts |
| `lines:N-M` | Specific line range |

**Q: Does lean-ctx send my code anywhere?**
No. lean-ctx runs 100% locally. Zero telemetry. Your code never leaves your machine. The only exception is if you explicitly opt into `lean-ctx cloud` for cross-device sync.
