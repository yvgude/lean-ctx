# **FAQ — How It Works**

---

**Q: What does lean-ctx actually do?**
lean-ctx sits between your AI tool and the system. It has two layers:
1. **Shell Hook** — transparently compresses CLI output (git, ls, npm, cargo, etc.) using 95+ patterns before it reaches the LLM
2. **MCP Server** — 77 tools for cached file reads, 10 read modes, deltas, dedup, memory, multi-agent coordination, graph-powered intelligence, and more

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

**Q: What is Bounce Detection?**
lean-ctx tracks when a compressed read (e.g. `map` or `signatures` mode) gets immediately re-read in `full` mode. These "bounces" waste tokens — you pay for the compressed read *and* the full re-read. The bounce tracker learns these patterns and proactively forces `full` mode for files that consistently bounce, preventing the double-read waste.

**Q: What is the Context Gate?**
Every `ctx_read` call goes through a context gate before the read mode is selected. The gate checks:
1. **Bounce history** — has this file bounced before? → force full mode
2. **Intent targets** — is the file explicitly mentioned in the current task? → prefer full/map
3. **Graph proximity** — how close is the file to files being edited? → weight toward more detail
4. **Knowledge relevance** — does the file match known facts/decisions in session memory? → adjust mode

The gate chooses the optimal read mode automatically, so `auto` mode becomes genuinely intelligent rather than a simple heuristic.

**Q: Does lean-ctx send my code anywhere?**
No. lean-ctx runs 100% locally. Zero telemetry. Your code never leaves your machine. The only exception is if you explicitly opt into `lean-ctx cloud` for cross-device sync.
