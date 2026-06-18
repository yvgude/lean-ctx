# **FAQ — MCP / Tools**

---

**Q: Where can I find docs for all 77 tools?**
- Tool overview: <https://leanctx.com/docs/tools/>
- Intelligence tools: <https://leanctx.com/docs/tools/intelligence/>
- Session & memory: <https://leanctx.com/docs/tools/session/>
- CLI reference: <https://leanctx.com/docs/cli-reference/>

**Q: Cache hits show 0 — is caching working?**
Important distinction:
- **MCP caching** (via `ctx_read`) — this is where the big savings happen. Check `lean-ctx gain` under "MCP Server".
- **Shell hook** — compresses output but doesn't cache across calls in the same way.

If you're using `pi-lean-ctx` (Pi editor), make sure you're on the latest version — earlier versions didn't route reads through the MCP cache.

**Q: `ctx_graph` / `ctx_callgraph` don't find anything!**
1. Build the graph first: use `ctx_graph` with action `build`
2. On **Windows**: path handling was fixed in v3.2.2 — make sure to update
3. Check that your project root is correct: `lean-ctx doctor`

**Q: "path escapes project root" error!**
This happens when the MCP server's project root is stuck from a previous session. Fixed in v3.2.5+:
- Update: `lean-ctx update`
- Restart your IDE/AI tool after switching projects
- Run `lean-ctx doctor` to verify the root

**Q: How do I use Unified mode vs Full Tools?**
- **Full (default)**: All 77 tools available as separate `ctx_*` tools
- **Unified** (`LEAN_CTX_UNIFIED=1`): 5 meta-tools only — `ctx`, `ctx_read`, `ctx_shell`, `ctx_search`, `ctx_tree`
- **Lazy** (`LEAN_CTX_LAZY_TOOLS=1`): Reduced set + `ctx_discover_tools` for on-demand loading

Set in your environment or config.

**Q: What are MCP Resources?**
lean-ctx exposes 5 MCP resources that supporting IDEs can subscribe to for live context state:

| Resource | Description |
|----------|-------------|
| `context://summary` | Current session summary (files, tokens, savings) |
| `context://pressure` | Context pressure gauge (how close to budget limits) |
| `context://plan` | Active task plan and progress |
| `context://pinned` | Pinned files and their read modes |
| `context://bounce` | Bounce tracker statistics and learned patterns |

IDEs with resource support (Cursor, Claude Code, Kiro, VS Code Copilot, Codex) can subscribe and get live updates without extra tool calls.

**Q: What are MCP Prompts?**
lean-ctx provides 5 slash commands (MCP prompts) for context management:

| Prompt | Description |
|--------|-------------|
| `/context-focus` | Set focus files/directories for the current task |
| `/context-review` | Review current context state and pressure |
| `/context-reset` | Reset session context (cache, graph, memory) |
| `/context-pin` | Pin a file to a specific read mode |
| `/context-budget` | Set or adjust the token budget for the session |

Available in IDEs that support MCP prompts (Cursor, Claude Code, Kiro, VS Code Copilot, Zed).

**Q: What are Dynamic Tool Categories?**
lean-ctx splits its 77 tools into 6 categories. In supporting IDEs, only the **core** category is loaded by default — additional categories are loaded on demand:

| Category | Tools | Loaded by default |
|----------|-------|-------------------|
| `core` | ctx_read, ctx_shell, ctx_search, ctx_tree, ctx_edit | yes |
| `intelligence` | ctx_graph, ctx_callgraph, ctx_refactor, ctx_semantic_search | no |
| `session` | ctx_session, ctx_knowledge, ctx_agent | no |
| `metrics` | ctx_metrics, ctx_gain, ctx_benchmark | no |
| `advanced` | ctx_compress, ctx_dedup, ctx_preload, ctx_overview | no |
| `experimental` | ctx_plan, ctx_control, ctx_discover_tools | no |

This keeps the tool list manageable and reduces schema overhead for agents that only need basic functionality.
