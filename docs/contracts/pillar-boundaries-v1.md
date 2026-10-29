# Pillar Boundaries Contract v1

> Architectural contract defining the three lean-ctx pillars and their
> dependency rules. CI-enforced via `contracts_frozen.rs`.

The names below describe implementation modules, not competing product categories.
LeanCTX is the **Context Gateway for AI Systems**; see the
[canonical product definition](../POSITIONING_CANONICAL.md) for edition and release scope.

## Pillars

### Engine (always compiled)

LeanCTX Engine implements local context selection, supported controls, and evidence.
Local operations can run without a model call. Enabled providers, model/proxy
routes, updates, and opted-in telemetry have separate network paths.

**Top-level modules:** `core`, `tools`, `server`, `engine`, `tool_defs`,
`instructions`, `mcp_stdio`, `hooks`, `hook_handlers`, `rules_inject`,
`rewrite_registry`, `shell`, `shell_hook`, `dashboard`, `tui`, `terminal_ui`,
`lsp`, `compound_lexer`, `marked_block`, `dropin`, `heatmap`, `token_report`,
`daemon`, `daemon_autostart`, `daemon_client`.

**Feature flags:** none (always compiled).

### Gateway (lean-ctx-enterprise)

The org-wide LLM reverse proxy with usage tracking, budget enforcement, and
FinOps dashboards.

**Top-level modules:** `proxy`, `proxy_autostart`, `proxy_setup` (OSS);
`gateway_server` (enterprise).

The self-hosted enterprise gateway process (`gateway_server`) and its
`engine-integration` feature were retired on 2026-10-16; the proxy runs alone.

### Cloud (lean-ctx-enterprise)

Hosted coordination: accounts, team provisioning, knowledge sync, billing
edge, context package registry.

**Top-level modules:** `cloud_server`, `cloud_client`, `cloud_sync`,
`http_server` (team/billing surfaces) — all in `lean-ctx-enterprise`.

**Feature flags:** enterprise build only (ADR-023).

### Shared (always compiled)

CLI, IPC, config, diagnostics — consumed by all three pillars.

**Top-level modules:** `cli`, `config_io`, `ipc`, `doctor`, `setup`,
`status`, `report`, `uninstall`.

## Dependency rules

1. **Engine depends on nothing** — it is the foundation.
2. **Gateway depends on Engine** — the proxy compresses prompts using
   `core::compressor`.
3. **Cloud depends on Engine** — sync and billing reference `core::config`,
   `core::savings_ledger`.
4. **Gateway ↔ Cloud are independent** — no direct imports between
   OSS `proxy` and enterprise `gateway_server` / `cloud_server` / `cloud_sync`.
5. **`http_server` (OSS)** — Engine HTTP MCP transport only. Team/Cloud
   HTTP surfaces live in `lean-ctx-enterprise`.

## Local-Free Invariant

The open-source Engine's local capability set is ungated by a commercial account
or plan. Separately licensed SDK and Enterprise capabilities follow their own
agreements; the local invariant does not grant their production or self-hosting
rights. Commercial features remain additive to the open Engine. CI checks the
Engine capability classification via `local_free_invariant`.

## Naming convention

| Old name | New name | Reason |
|----------|----------|--------|
| `core::gateway` | `core::mcp_catalog` | Avoid collision with `gateway_server` (the LLM Gateway) |
