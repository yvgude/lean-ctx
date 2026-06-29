# lean-ctx Integration Guides

Step-by-step guides for setting up lean-ctx with your AI coding agent.

## Quick Start

All agents follow the same pattern:

```bash
# 1. Install lean-ctx
curl -fsSL https://leanctx.com/install.sh | sh

# 2. Connect your AI tools (auto-detects everything installed)
lean-ctx onboard

# 3. Verify
lean-ctx doctor
```

`lean-ctx onboard` detects and configures every AI tool on your machine with
sensible defaults. Prefer step-by-step control, or only want one specific
agent? Use the guided wizard or the per-agent command instead:

```bash
lean-ctx setup                  # guided wizard, full control
lean-ctx init --agent cursor    # configure a single agent
```

## Agent Comparison

| Agent | Integration | Shell Hook | Rules File | Config Format | Setup Command |
|-------|------------|------------|------------|---------------|---------------|
| [Claude Code](claude-code.md) | Hybrid | ✅ | `~/.claude/CLAUDE.md` block + skill | `~/.claude.json` | `lean-ctx init --agent claude` |
| [Cursor](cursor.md) | Hybrid | ✅ | `~/.cursor/rules/lean-ctx.mdc` | Cursor Settings UI | `lean-ctx init --agent cursor` |
| [Aider](aider.md) | MCP-only | ❌ | Dedicated `.md` | `.aider.conf.yml` | `lean-ctx init --agent aider` |
| [Windsurf](windsurf.md) | Hybrid | ✅ | `~/.codeium/windsurf/rules/lean-ctx.md` | MCP JSON | `lean-ctx init --agent windsurf` |
| [Gemini CLI](gemini-cli.md) | Hybrid | ✅ | `~/.gemini/GEMINI.md` (shared) | `~/.gemini/settings.json` | `lean-ctx init --agent gemini` |
| [OpenCode](opencode.md) | Hybrid | ✅ | `~/.config/opencode/AGENTS.md` (shared) | `opencode.json` | `lean-ctx init --agent opencode` |
| [Codex CLI](codex-cli.md) | Hybrid | ✅ | `~/.codex/instructions.md` (shared) | `~/.codex/config.toml` | `lean-ctx init --agent codex` |
| [Pi Coding Agent](pi.md) | Hybrid | ✅ | `AGENTS.md` | Pi Package | `lean-ctx init --agent pi` |

## Integration Modes

### Hybrid Mode (recommended)

Available for agents with shell access. Combines:
- **MCP tools** for file reads and search (cached, compressed)
- **Shell hooks** for command output compression (git, npm, cargo, docker, etc.)

### MCP-Only Mode

For agents without direct shell access. All 80 tools available via MCP protocol.

## What lean-ctx Sets Up

Running `lean-ctx init --agent <name>` or `lean-ctx setup` configures:

1. **MCP server registration** — adds lean-ctx to the agent's MCP config
2. **Agent rules** — injects lean-ctx usage instructions into the agent's rules file
3. **Shell hooks** — installs command compression (hybrid mode agents only)
4. **SKILL.md** — installs the lean-ctx skill file (supported agents only)

## Common Tools Reference

Every agent gets access to the same 81 MCP tools. The most important ones:

| Tool | Purpose | When to Use |
|------|---------|-------------|
| `ctx_read(path, mode)` | Read files with 10 compression modes | Always — replaces native file reads |
| `ctx_search(pattern, path)` | Token-efficient code search | Finding code patterns |
| `ctx_shell(command)` | Compressed shell output | Running commands via MCP |
| `ctx_overview(task)` | Fast project orientation | Session start |
| `ctx_semantic_search(query)` | Meaning-based code search | Understanding code by concept |
| `ctx_knowledge(action, ...)` | Persistent knowledge graph | Remembering decisions/findings |
| `ctx_session(action, ...)` | Session state management | Task tracking across chats |
| `ctx_compress` | Memory checkpoint | When context grows large |
| `ctx_graph(action)` | Code relationship graph | Impact analysis |
| `ctx_refactor(action, ...)` | LSP-powered refactoring | Rename, references, go-to-definition |

## Read Modes

All agents use the same mode selection strategy:

| Mode | Use When | Token Savings |
|------|----------|---------------|
| `full` | You will edit the file | Baseline (cached) |
| `map` | Context only — deps + exports + key signatures | 60-80% |
| `signatures` | API surface only | 70-90% |
| `diff` | Re-reading after edits | 80-95% |
| `aggressive` | Large files, context only | 80-95% |
| `entropy` | Shannon + Jaccard filtering | 70-90% |
| `task` | Task-relevant filtering | 60-80% |
| `lines:N-M` | Specific line range | Varies |
| `reference` | Quote-friendly excerpts | 70-85% |
| `auto` | Unsure — system selects optimal | Varies |

## Troubleshooting

See the troubleshooting section in each individual guide. Common issues:

```bash
# Verify installation
lean-ctx doctor

# Check MCP server status
lean-ctx status

# See real-time token savings
lean-ctx gain --live

# Disable temporarily
lean-ctx-off

# Re-run setup
lean-ctx setup
```

## More Resources

- [Extending lean-ctx — which mechanism to use](extensions.md)
- [Addons — Community Extensions](addons.md)
- [Embed lean-ctx (Rust SDK)](embed-sdk.md)
- [Monorepo Guide](monorepo.md)
- [Custom Embedding Models](custom-embeddings.md)
- [Context Policy Packs](policy-packs.md)
- [Org Single Sign-On (OIDC) Setup](org-sso-setup.md)
- [Getting Started](https://leanctx.com/docs/getting-started)
- [Tools Reference](https://leanctx.com/docs/tools/)
- [CLI Reference](https://leanctx.com/docs/cli-reference/)
- [Discord Community](https://discord.gg/pTHkG9Hew9)

