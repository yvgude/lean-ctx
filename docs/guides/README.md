# LeanCTX integration guides

**Context Gateway for AI Systems**

**Control what your AI can see.**

**Select → Control → Learn → Prove:** the LeanCTX Engine prepares context, applies
supported controls, retains local knowledge and records observable evidence. The LeanCTX SDK embeds
supported interfaces; it does not replace the agent or become an agent platform.

## Local setup

```bash
curl -fsSL https://leanctx.com/install.sh | sh
lean-ctx onboard
lean-ctx doctor
```

Use `lean-ctx setup` for the guided path or `lean-ctx init --agent <name>` to
configure one detected coding agent. Codex, Claude Code, and Cursor are the
current first-class local setup paths; other wiring is an implementation
reference that requires a local compatibility check. Attach integrations are
local Runtime capabilities; their visibility and evidence coverage depend on the
integration.

## How the local Runtime helps

- MCP paths provide context-aware reads, search, and related local tooling.
- Shell hooks can reduce unnecessary command output for supported agents.
- Local configuration makes context selection, representation, reuse, and
  recovery inspectable for a project.

[Seeing what lean-ctx did](value-display.md) covers the status line, prompt
segment, milestone notifications and commit trailer, including how to
interpret each indicator, its measurement type, and its limits.

[Semantic code intelligence](semantic-intelligence.md) covers language-server
evidence on graph edges: modes, server installation, and how to read it.

The generated [MCP tool inventory](../reference/generated/mcp-tools.md) and
[configuration inventory](../reference/generated/config-keys.md) are the
current implementation reference.

## Read modes

Read modes let an integration ask for the representation that fits the task:

| Mode | Intended view |
| --- | --- |
| `full` / `raw` | Exact source when it is needed |
| `map` / `signatures` | Structure or API surface |
| `diff` / `lines:N-M` / `lines:-N` | A change, a precise slice, or the last N lines |
| `task` / `reference` / `auto` | A task-oriented or selected representation |

Context reduction depends on the file, mode, task, and recovery behavior. Do
not turn a local counter or a mode description into a universal savings claim.

## Integration depths

- **Attach — Available:** CLI, MCP, hooks, or proxy around an existing coding
  agent; evidence is limited to what the integration can observe.
- **Embed — Available:** the standalone SDK's Stable lifecycle and Agent Tools
  interfaces, within the published compatibility and license scope.
- **Wrap / older Embed — Preview:** historical in-tree reference wrappers and
  embedding experiments; these do not supersede the standalone SDK manifest.

## Scope-qualified material

- [Embedding guide](embed-sdk.md) — Stable SDK paths and explicit **Preview** limits.
- [Context Runtime overview](context-infrastructure.md) — current local scope.
- [Addons](addons.md) — **Research**; no public marketplace or managed
  distribution promise.
- [Context Kit/package material](publishing-packages.md) — signed local
  substrate; first-class Kits and hosted publication are **Research**.
- [Hosted index runbook](hosted-index-slo.md) and [organization SSO](org-sso-setup.md) — historical, unshipped service concepts.

For status and product boundaries, see the
[canonical positioning](../POSITIONING_CANONICAL.md) and [Vision](../../VISION.md).
