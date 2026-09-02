# LeanCTX integration guides

LeanCTX is a local Context SDK for existing AI agents. It controls the context
path before inference; it does not replace the agent or become an agent
platform.

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

The generated [MCP tool inventory](../reference/generated/mcp-tools.md) and
[configuration inventory](../reference/generated/config-keys.md) are the
current implementation reference.

## Read modes

Read modes let an integration ask for the representation that fits the task:

| Mode | Intended view |
| --- | --- |
| `full` / `raw` | Exact source when it is needed |
| `map` / `signatures` | Structure or API surface |
| `diff` / `lines:N-M` | A change or a precise slice |
| `task` / `reference` / `auto` | A task-oriented or selected representation |

Context reduction depends on the file, mode, task, and recovery behavior. Do
not turn a local counter or a mode description into a universal savings claim.

## Integration depths

- **Attach — Available:** CLI, MCP, hooks, or proxy around an existing coding
  agent; evidence is limited to what the integration can observe.
- **Wrap — Preview:** declared Python SDK v1 reference-wrapper scope only; not
  a general adapter framework.
- **Embed — Preview:** native integration into a custom application or agent;
  the current Engine is implementation substrate, not a general stable facade.

## Scope-qualified material

- [Embedding reference](embed-sdk.md) — **Preview**.
- [Context Runtime overview](context-infrastructure.md) — current local scope.
- [Addons](addons.md) — **Research**; no public marketplace or managed
  distribution promise.
- [Context Kit/package material](publishing-packages.md) — signed local
  substrate; first-class Kits and hosted publication are **Research**.
- [Hosted index runbook](hosted-index-slo.md) and [organization SSO](org-sso-setup.md) — historical, unshipped service concepts.

For the governing status and product boundary, see
`docs/internal/README.md` (internal, not in this repository).
