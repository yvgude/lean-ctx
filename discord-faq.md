# lean-ctx FAQ

> **Latest version: 3.4.5** — 49 MCP tools · 10 read modes · 90+ shell patterns
> Docs: https://leanctx.com/docs/getting-started

---

## Installation & Setup

**Q: How do I install lean-ctx?**
```bash
curl -fsSL https://leanctx.com/install.sh | sh   # universal, no Rust needed
brew tap yvgude/lean-ctx && brew install lean-ctx  # macOS / Linux
npm install -g lean-ctx-bin                        # Node.js
cargo install lean-ctx                             # Rust
```
Then run `lean-ctx setup` and `lean-ctx doctor` to verify.

**Q: Do I need Rust installed?**
No. Since v3.2.3 the install script auto-detects if `cargo` is missing and downloads a pre-built binary. Rust is only needed if you want to build from source.

**Q: Which editors/AI tools are supported?**
lean-ctx auto-configures for: **Cursor, Claude Code, GitHub Copilot, Windsurf, VS Code, Zed, Codex CLI, Gemini CLI, OpenCode, Pi, Qwen Code, Trae, Amazon Q, JetBrains, Antigravity, Cline/Roo Code, Aider, Amp, Kiro, Continue, Crush** — run `lean-ctx setup` and it detects everything.

**Q: How do I update?**
```bash
lean-ctx update   # recommended — refreshes binary, hooks, and aliases
```
After updating, restart your shell (`source ~/.zshrc`) and your IDE.

**Q: How do I uninstall or temporarily disable?**
- Disable for current session: `lean-ctx-off`
- Re-enable: `lean-ctx-on`
- Full uninstall: `lean-ctx uninstall`
- Disable for a single command: `LEAN_CTX_DISABLED=1 your-command`

---

## How It Works

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

---

## Shell Hook Issues

**Q: My commands are broken after installing!**
Run `lean-ctx-off` to fix your current session immediately. Then run `lean-ctx setup` again to refresh hooks. If the problem persists, run `lean-ctx uninstall` and reinstall.

**Q: The shell hook compresses too much — signal is lost!**
This was addressed in recent versions. If a command's output is too aggressively compressed:
1. Update to latest: `lean-ctx update`
2. Exclude specific commands in config:
```toml
# ~/.lean-ctx/config.toml
excluded_commands = ["git stash", "your-command"]
```
3. Or disable for a single run: `LEAN_CTX_DISABLED=1 your-command`

**Q: Auth flows (az login, gh auth, etc.) are broken — the device code is hidden!**
Fixed since v2.21.10. lean-ctx now auto-detects 21+ auth commands and preserves their output uncompressed. Update to latest: `lean-ctx update`.

Workaround for older versions:
```toml
# ~/.lean-ctx/config.toml
excluded_commands = ["az login", "gh auth"]
```

**Q: The `[lean-ctx: NNN→NNN tok, -XX%]` stats line wastes tokens!**
Fixed in v3.2.6. The stats line is no longer appended to stdout by default. Update: `lean-ctx update`.

**Q: lean-ctx blocks image viewing in Claude Code!**
Fixed in recent versions. Binary/image files are now passed through without compression. Update: `lean-ctx update`.

**Q: `git commit -m "$(cat <<'EOF' ...)"` fails with syntax error!**
Fixed in v3.2.0+. The shell hook now handles heredoc/EOF-style commit messages correctly. Update: `lean-ctx update`.

---

## MCP / Tools

**Q: Where can I find docs for all 49 tools?**
- Tool overview: https://leanctx.com/docs/tools/
- Intelligence tools: https://leanctx.com/docs/tools/intelligence/
- Session & memory: https://leanctx.com/docs/tools/session/
- CLI reference: https://leanctx.com/docs/cli-reference/

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

---

## Configuration

**Q: Where is the config file?**
`~/.lean-ctx/config.toml` — created on demand. If it doesn't exist, defaults are used.

**Q: What is `rules_scope` and how do I use it?**
`rules_scope` controls where lean-ctx places agent rule files during `lean-ctx init`:
```toml
# ~/.lean-ctx/config.toml
rules_scope = "local"    # rules in project dir (default)
rules_scope = "global"   # rules in home dir
```
This affects where CLAUDE.md, AGENTS.md, .cursorrules etc. are written.

**Q: How do I disable specific tools?**
```toml
# ~/.lean-ctx/config.toml
disabled_tools = ["ctx_execute", "ctx_edit"]
```

**Q: How do I exclude commands from compression?**
```toml
# ~/.lean-ctx/config.toml
excluded_commands = ["az login", "my-custom-tool"]
```

---

## Dashboard & Analytics

**Q: How do I see my savings?**
```bash
lean-ctx gain              # terminal dashboard
lean-ctx gain --live       # real-time mode
lean-ctx gain --web        # opens web dashboard at localhost:3333
```

**Q: Dashboard shows 0% / no results!**
- Make sure your AI tool is actually using lean-ctx tools (check `lean-ctx doctor`)
- Shell hook savings and MCP savings are tracked separately
- Run a few AI-assisted coding tasks first, then check again
- Fixed display issues in v3.2.6 — update: `lean-ctx update`

**Q: "Dashboard indicates update available" but the version doesn't exist yet?**
This was a bug in v3.2.4 where the update check compared against an unreleased version. Fixed in v3.2.5+.

---

## Docker & Remote

**Q: How do I use lean-ctx in Docker?**
```dockerfile
# Download pre-built binary
RUN curl -fsSL https://leanctx.com/install.sh | sh

# For Claude Code: set env file
ENV CLAUDE_ENV_FILE=/root/.lean-ctx/env
RUN lean-ctx setup
```
Important: Use `CLAUDE_ENV_FILE` (not just `BASH_ENV`) for Claude Code in Docker.
Full guide: https://leanctx.com/docs/remote-setup/

**Q: How do I use lean-ctx over SSH / remote?**
lean-ctx supports remote setups via SSH port-forwarding or running the MCP server directly on the remote machine. See: https://leanctx.com/docs/remote-setup/

---

## Windows

**Q: Is Windows supported?**
Yes! lean-ctx supports Windows with PowerShell and Git Bash. Some tips:
- Use the latest version — many Windows path-handling fixes were added in v3.2.2+
- The updater infinite-loop bug (GNU timeout conflict) was fixed in v3.2.0
- `ctx_graph` path normalization issues were fixed in v3.2.2

**Q: Bash hook strips slashes from paths on Windows!**
This was a path-handling bug in Claude Code's hook execution on Windows with Git Bash. Fixed in v3.2.4. Update: `lean-ctx update`.

---

## Troubleshooting

**Q: Something is broken — what do I do first?**
```bash
lean-ctx doctor            # diagnose everything
lean-ctx-off               # disable immediately (current session)
lean-ctx setup             # re-run setup to fix hooks
lean-ctx update            # get latest fixes
```

**Q: How do I report a bug?**
Run `lean-ctx report-issue` — this generates a diagnostic report you can paste into a GitHub issue. Or create an issue at: https://github.com/yvgude/lean-ctx/issues

**Q: Where can I get help?**
- Discord (you're here!)
- GitHub Issues: https://github.com/yvgude/lean-ctx/issues
- Docs: https://leanctx.com/docs/getting-started/
- Quick Reference: https://leanctx.com/docs/quick-reference/

---

## Useful Links

- Website: https://leanctx.com
- GitHub: https://github.com/yvgude/lean-ctx
- Docs: https://leanctx.com/docs/getting-started/
- Tool Reference: https://leanctx.com/docs/tools/
- CLI Reference: https://leanctx.com/docs/cli-reference/
- Benchmark: https://leanctx.com/benchmark
- crates.io: https://crates.io/crates/lean-ctx
- npm: https://www.npmjs.com/package/lean-ctx-bin
