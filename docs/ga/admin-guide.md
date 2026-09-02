# Administrator Guide

> **Status: historical operations guide — not an organization-product promise.**
> Follow only behavior supported by the installed local Runtime. LeanCTX is **The
> Context SDK for AI Agents**; Cloud, managed/team, SSO, and control-plane
> surfaces are not current availability. See
> `docs/internal/README.md` (internal, not in this repository).

## Overview

This guide is for administrators who install, configure, secure, and operate
lean-ctx for one user or a managed team. It covers the local components and
their operational boundaries; it does not replace the detailed security or
performance references.

lean-ctx has four cooperating components:

| Component | Responsibility | Operational owner |
| --- | --- | --- |
| Proxy | Intercepts supported LLM traffic and compresses tool results in flight; runs as a macOS LaunchAgent or Linux systemd user service. | Administrator |
| Daemon | Owns local sessions and is started by the CLI or an editor when needed. | User or editor |
| MCP server | A child process of Cursor or another MCP client that exposes the `ctx_*` tools. | Editor / agent |
| CLI | The `lean-ctx` binary used for setup, policy, lifecycle, and diagnostics. | Administrator and user |

The proxy is optional for MCP-tool compression. A healthy MCP server can still
serve `ctx_*` tools when the proxy is intentionally disabled.

Related references: [security and governance](../reference/13-security-and-governance.md),
[performance tuning](../reference/14-performance-tuning.md), and
[customization and governance](../reference/10-customization-and-governance.md).

## Prerequisites

- Install a supported lean-ctx binary and ensure `lean-ctx` is on the user's
  `PATH`.
- Run commands as the user that owns the editor and its configuration; a
  LaunchAgent or systemd *user* service is not a system-wide daemon.
- Identify the directories agents may read and the commands they may execute
  before enabling restrictive policies.
- Keep the proxy loopback-bound unless an authenticated gateway deployment has
  been designed and reviewed.

Start with a diagnostic and the effective configuration:

```bash
lean-ctx doctor
lean-ctx config show
lean-ctx config schema
```

`lean-ctx config schema` is authoritative for the installed release. It avoids
copying a key from an older config format into a newer binary.

## Steps

### 1. Initialize and inspect configuration

The primary user configuration file is:

```text
~/.config/lean-ctx/config.toml
```

Create a starter file and validate every change before applying it:

```bash
lean-ctx config init
lean-ctx config validate
lean-ctx config apply
```

Use `lean-ctx config set <key> <value>` for a single audited change. For
changes read by a running process, use `lean-ctx restart` after validation.

Configuration concepts commonly managed by administrators are:

| Policy area | Purpose | Verify with |
| --- | --- | --- |
| `hook_mode` | Select `replace`, `hybrid`, or `passthrough` behavior for native tools. | `lean-ctx config show` |
| `compression_level` | Select the installed release's supported level; current releases expose `off`, `lite`, `standard`, and `max`. | `lean-ctx compression` |
| `proxy.port` / `proxy.bind_address` | Select listener port and bind address; current schema may expose these as `proxy_port` and `proxy_bind_host`. | `lean-ctx proxy status` |
| `cache.max_size_mb` / `cache.ttl_seconds` | Set cache retention limits where supported by the installed schema. | `lean-ctx config schema` |
| `security.path_jail` | Restrict filesystem access to approved roots; current schema uses `path_jail` with allowed-root settings. | `lean-ctx security status` |
| `security.shell_allowlist` | Limit commands agents can invoke; current schema uses `shell_allowlist` and `shell_allowlist_extra`. | `lean-ctx allow list` |

Do not assume dotted examples are interchangeable with flat keys. Confirm the
exact key names with `lean-ctx config schema` before writing `config.toml`.

### 2. Choose hook and compression behavior

Set `hook_mode` according to the team policy:

| Mode | Use |
| --- | --- |
| `replace` | Enforce `ctx_*` tools for supported read, search, and shell work. |
| `hybrid` | Permit native tools where compatibility requires them. |
| `passthrough` | Troubleshoot integration without normal interception. |

Set and inspect compression with the CLI rather than guessing level names:

```bash
lean-ctx compression
lean-ctx compression lite
lean-ctx compression standard
```

For a project-specific policy, place the approved override in that project's
`.lean-ctx.toml`. Project settings should narrow or tune the global policy, not
silently bypass it. See [performance tuning](../reference/14-performance-tuning.md).

### 3. Manage proxy and daemon processes

Use the scoped commands to start a component:

```bash
lean-ctx proxy start
lean-ctx daemon start
lean-ctx proxy status
```

Use the top-level lifecycle commands for the installed local service set:

```bash
lean-ctx stop
lean-ctx restart
```

Some deployment runbooks abbreviate the first action as `lean-ctx start`.
Use `lean-ctx proxy start` or `lean-ctx daemon start` on current releases,
because those are the explicit component start commands.

On macOS, the proxy is normally managed by:

```bash
launchctl load ~/Library/LaunchAgents/com.leanctx.proxy.plist
launchctl unload ~/Library/LaunchAgents/com.leanctx.proxy.plist
```

Prefer `lean-ctx proxy enable`, `lean-ctx proxy start`, and `lean-ctx stop`
for regular operation; direct `launchctl` commands are for recovery or managed
installation workflows. The LaunchAgent uses `KeepAlive`: killing its process
causes macOS to respawn it.

On Linux, the proxy user unit is
`~/.config/systemd/user/lean-ctx-proxy.service`. `lean-ctx proxy enable`
installs the unit. Manage it with the user service manager:

```bash
lean-ctx proxy enable
systemctl --user status lean-ctx-proxy.service
systemctl --user restart lean-ctx-proxy.service
```

Before replacing a binary manually, always stop managed processes first:

```bash
lean-ctx stop
```

This is critical on macOS: `KeepAlive` otherwise replaces a killed process
while its binary is being changed. For source-tree installs, use
`lean-ctx dev-install`, which performs the short stop and restart atomically.

### 4. Connect and manage agents

Supported integrations include Cursor, Claude Code, Codex, Windsurf, and
GitHub Copilot. Connect an agent with `wrap`, then restart that agent or editor
so it reloads its MCP configuration:

```bash
lean-ctx wrap cursor
lean-ctx wrap claude
lean-ctx wrap codex
lean-ctx wrap windsurf
lean-ctx unwrap cursor
```

Use the agent bus when an operator needs accountability for concurrent agents:

```bash
lean-ctx agent register --id "<agent-id>" --role coder --owner "<owner>"
lean-ctx agent list
```

Choose an identifier that is unique in the deployment. Registration records
agent identity and role; it is not a substitute for OS authentication or the
shell and path policies below.

### 5. Apply security and compression policies

Treat shell access and filesystem access as separate controls.

For shell policy, inspect the effective allowlist and add only a reviewed binary:

```bash
lean-ctx allow list
lean-ctx allow add <command>
lean-ctx allow remove <command>
```

`shell_allowlist_extra` is additive. Setting `shell_allowlist` directly
replaces the built-in list, so validate the full resulting list before rollout.

For filesystem policy, enable PathJail and specify only the project roots that
the agents require. Verify the effective policy before connecting an agent:

```bash
lean-ctx security status
lean-ctx config validate
lean-ctx doctor
```

Keep per-project compression overrides in version-controlled `.lean-ctx.toml`
files. Use global `config.toml` for workstation-wide defaults and security
boundaries. See [security and governance](../reference/13-security-and-governance.md)
for PathJail, allowlist semantics, secret handling, and hardening.

### 6. Collect and control logs

Local state, including logs, is stored under:

```text
~/.local/state/lean-ctx/
```

Set the log level for a foreground diagnostic run:

```bash
LEAN_CTX_LOG=debug lean-ctx proxy start
LEAN_CTX_LOG=info lean-ctx doctor
LEAN_CTX_LOG=warn lean-ctx status
LEAN_CTX_LOG=error lean-ctx proxy status
```

Use `debug` briefly and remove it after collecting evidence; it increases log
volume and may include operational detail unsuitable for broad retention.

Investigate logs by component:

| Component | What to look for |
| --- | --- |
| Proxy | Listener startup, upstream reachability, authentication, port binding. |
| Daemon | Session lifecycle, config reloads, cache initialization. |
| MCP server | Editor child-process startup, tool registration, client compatibility. |

Protect logs with the same retention, access-control, and redaction policy as
other developer tooling. Do not treat logs as a source of secret material.

## Verification

Run this compact acceptance sequence after installation or a policy change:

```bash
lean-ctx config validate
lean-ctx config show
lean-ctx status
lean-ctx doctor
lean-ctx proxy status
lean-ctx agent list
```

Confirm all of the following:

- The effective hook mode and compression setting match the approved policy.
- Proxy bind address and port match the local or gateway design.
- PathJail roots and shell allowlist are no broader than required.
- The expected editor starts an MCP server and exposes the intended tool profile.
- Logs show no repeating process restart, configuration, or permission errors.

## Troubleshooting

Use [Troubleshooting](../reference/12-troubleshooting.md) for complete diagnosis.
The most common administrator cases are:

| Symptom | First response |
| --- | --- |
| Process will not start | Run `lean-ctx doctor`, then start the scoped component with `LEAN_CTX_LOG=debug`. |
| Port conflict | Run `lean-ctx proxy status`; choose a free configured port, validate, then restart. |
| Config parse error | Run `lean-ctx config validate`; correct the reported key or value and apply the change. |
| Permission denied | Check PathJail roots and OS ownership of config, state, and project directories. |
| macOS code-signing failure | Run `lean-ctx doctor`; reinstall or use the documented signing setup before re-enabling the LaunchAgent. |

Do not use `kill` or `pkill` as a normal repair for managed macOS processes.
Use `lean-ctx stop` so the LaunchAgent is unloaded before process cleanup.
