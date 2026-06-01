You just installed the `lean-ctx` binary and nothing is wired up yet. This journey covers every command that connects LeanCTX to your editors and your shell — and exactly what each one does.

## Quick path

For most users, setup is three commands:

```bash
# 1. Install
curl -fsSL https://leanctx.com/install.sh | sh

# 2. Auto-configure everything
lean-ctx onboard

# 3. Restart your shell
source ~/.zshrc
```

That's it. `onboard` auto-detects every AI tool on your machine and writes the MCP config for each one.

## What "being set up" means

For LeanCTX to work, three things must be true:

1. **Your AI tool knows about LeanCTX** — its MCP config lists the `lean-ctx` server
2. **Your shell knows about LeanCTX** — a hook compresses command output
3. **A data directory exists** — `~/.lean-ctx/` holds stats, sessions, and config

## Available setup commands

| Command | When to use |
|---------|-------------|
| `lean-ctx onboard` | First-time setup, zero questions |
| `lean-ctx setup` | Full 12-step wizard with all options |
| `lean-ctx install --repair` | Fix drift without prompts |
| `lean-ctx bootstrap` | CI/automation (non-interactive) |
| `lean-ctx init --agent <name>` | Wire up one specific editor |
| `lean-ctx doctor` | Diagnose connection issues |
| `lean-ctx status` | Quick "am I connected?" check |

## Full documentation

The complete setup guide — including platform-specific installation, manual editor configs, troubleshooting, and the detailed 12-step reference — lives on the [Getting Started](/docs/getting-started/) page.
