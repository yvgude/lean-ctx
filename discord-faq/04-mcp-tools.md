# **FAQ — MCP / Tools**

---

**Q: Where can I find docs for all 49 tools?**
- Tool overview: <https://leanctx.com/docs/tools/>
- Intelligence tools: <https://leanctx.com/docs/tools/intelligence/>
- Session & memory: <https://leanctx.com/docs/tools/session/>
- CLI reference: <https://leanctx.com/docs/cli-reference/>

**Q: Cache hits show 0 — is caching working?**
Important distinction:
- **MCP caching** (via `ctx_read`) — this is where the big savings happen. Check `lean-ctx gain` under "MCP Server".
- **Shell hook** — compresses output but doesn't cache across calls in the same way.

If you're using `pi-lean-ctx` (Pi editor), make sure you're on the latest version — earlier versions didn't route reads through the MCP cache.

**Q: `ctx_graph` / `ctx_callers` / `ctx_callees` don't find anything!**
1. Build the graph first: use `ctx_graph` with action `build`
2. On **Windows**: path handling was fixed in v3.2.2 — make sure to update
3. Check that your project root is correct: `lean-ctx doctor`

**Q: "path escapes project root" error!**
This happens when the MCP server's project root is stuck from a previous session. Fixed in v3.2.5+:
- Update: `lean-ctx update`
- Restart your IDE/AI tool after switching projects
- Run `lean-ctx doctor` to verify the root

**Q: How do I use Unified mode vs Full Tools?**
- **Full (default)**: All 49 tools available as separate `ctx_*` tools
- **Unified** (`LEAN_CTX_UNIFIED=1`): 5 meta-tools only — `ctx`, `ctx_read`, `ctx_shell`, `ctx_search`, `ctx_tree`
- **Lazy** (`LEAN_CTX_LAZY_TOOLS=1`): Reduced set + `ctx_discover_tools` for on-demand loading

Set in your environment or config.
