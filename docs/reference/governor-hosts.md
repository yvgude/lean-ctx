# Governor setup by host

The governor setup gives a host native tools, LeanCTX hooks and a short rules
block in the host's one LeanCTX rules channel, and **no** LeanCTX MCP server
(no tool schemas, no MCP instructions on every request). It is the default for
every host with tool hooks; hosts without hooks keep the MCP setup.

| Class | What the host's hooks can do | What LeanCTX installs |
|---|---|---|
| **A — full governor** | Pre-tool hook can rewrite or deny a shell command, **and** a post-tool hook can replace the newest tool result | `hook rewrite-enforce` (allowlist wrap or deny, secret-path deny) + `hook govern` (R1–R4 on the newest result), rules block, no MCP |
| **B — enforcement only** | Pre-tool hook can deny (rewrite only with an approval, or never); no post-tool result replacement | The host's enforcement hook (deny on an allowlist violation or a secret-like read; never a grant), rules block, no MCP. Tool results stay native |
| **C — unchanged** | No tool hooks, or a hook dialect LeanCTX has not verified | Today's setup (MCP server, rules, any existing hooks) |

"Mirror, don't grant": a governor hook never approves a call the host would
have asked about. Where a host accepts a rewrite only together with an
approval (Cursor, Copilot CLI, Codex), the governor does not rewrite there; it
denies an allowlist violation and otherwise answers with the host's
no-decision output.

## Hosts

| Host (`--agent`) | Class | Hooks written | Rules channel | Evidence |
|---|---|---|---|---|
| Claude Code (`claude`) | A | `~/.claude/settings.json`: PreToolUse → `hook rewrite-enforce`, PostToolUse → `hook govern` | none (zero prefix; `governor-mcp` keeps a block in `~/.claude/CLAUDE.md`) | Reference implementation (`hooks/agents/claude_governor.rs`) |
| CodeBuddy (`codebuddy`) | A | `~/.codebuddy/settings.json`: same as Claude Code (no PostCompact) | `~/.codebuddy/CODEBUDDY.md` | Claude-compatible contract: `updatedInput` without a decision, `updatedToolOutput` already used by `hook read-dedup` there (`hooks/agents/codebuddy.rs`, `hook_handlers/mod.rs`) |
| Cursor (`cursor`) | B | `~/.cursor/hooks.json`: `preToolUse` Shell and Read\|Grep\|Glob → `hook rewrite-enforce` | `~/.cursor/rules/lean-ctx.mdc` | `updated_input` honoured only with `permission: allow`; `postToolUse` cannot replace Shell/Read output and wrote hook stdout into edited files (#1200) |
| Codex CLI (`codex`) | B | `$CODEX_HOME/hooks.json`: PreToolUse `Bash\|Read\|Grep\|Glob` → `hook codex-enforce`; `config.toml` keeps `[features] hooks = true` | `$CODEX_HOME/AGENTS.md` | `updatedInput` only with `permissionDecision: allow` (developers.openai.com/codex/hooks); payload is Claude-shaped, so a dedicated Codex-dialect hook answers |
| Gemini CLI (`gemini`) | B | `~/.gemini/settings.json`: `BeforeTool` shell and file tools → `hook rewrite-enforce` | `~/.gemini/GEMINI.md` | `BeforeTool` merges `hookSpecificOutput.tool_input` and blocks on `decision: deny`; no hook decision approves (geminicli.com/docs/hooks/reference); no documented lossless `AfterTool` replacement |
| Copilot CLI (`copilot`) | B | `$COPILOT_HOME/hooks/hooks.json`: `preToolUse` → `hook rewrite-enforce` | `~/.copilot/instructions.md` | `modifiedArgs` only with `permissionDecision: allow`; empty output = default behaviour; `postToolUse` output does not replace the result |
| Qoder / QoderCLI (`qoder`) | C (unverified) | Today's `hook rewrite` / `hook deny` | — | Whether Qoder applies `updatedInput` without a decision is not documented in the repo; an ignored rewrite would let a violation run unchecked |
| Windsurf (`windsurf`) | C (unverified) | Today's `pre_mcp_tool_use` hooks | — | LeanCTX hooks only MCP tool calls; Cascade's `pre_run_command` (exit-code block) is not wired or verified |
| Mistral Vibe (`vibe`) | C (unverified) | Today's `pre_tool` hook | — | Vibe's hook is MCP-gated in the installer; no post-tool replacement |
| Antigravity IDE / CLI (`antigravity`, `antigravity-cli`) | C (unverified) | Today's Gemini-format hooks / `agy` plugin | — | Shares Gemini's dialect but its tool names and plugin contract are not verified for enforcement |
| OpenCode (`opencode`), Pi (`pi`), Hermes (`hermes`), OpenClaw (`openclaw`), Crush (`crush`) | C | Plugin or extension integrations | — | In-process plugin APIs, not command hooks; a governor would be a plugin change |
| Aider, Amazon Q, Amp, Augment, Cline, Roo, CodeWhale, Command Code, Continue, Emacs, Grok, JetBrains, Kiro, Neovim, Oh My Pi, QoderWork, Qwen, Sublime, Trae, Verdent, VS Code, VS Code Insiders, Zed | C | none | — | No tool hooks LeanCTX drives (MCP registration only) |

## Switching

- `lean-ctx init --agent <host> --mode governor` installs the governor setup
  on any class A or B host.
- `lean-ctx init --agent <host> --mode hybrid` (or `mcp`, `replace`) reverts
  it and installs the standard setup.
- New installs of class A/B hosts (`init --agent <host>` without `--mode`,
  `lean-ctx setup`) get the governor unless `hook_mode` /
  `LEAN_CTX_HOOK_MODE` pins a mode. Existing standard setups are switched by
  the update migration, which backs up every file it changes.
