# Addon Manifest — v1

Status: **stable (v1)** · Module: `core::addons` · CLI: `lean-ctx addon`

An **addon** packages an external MCP server (plus metadata) behind a small
`lean-ctx-addon.toml` manifest, so a third-party tool plugs into lean-ctx's MCP
gateway with one `lean-ctx addon add` — no fork, no recompile. Addons are
user-global and reuse the gateway trust model: `[gateway]` is global-only (never
merged from an untrusted project-local config) and a full no-op until enabled.

This contract defines the manifest shape, the registry shape, and the install
semantics. The how-to lives in [`docs/guides/addons.md`](../guides/addons.md).

## Manifest: `lean-ctx-addon.toml`

Two tables: `[addon]` (metadata) and `[mcp]` (how lean-ctx runs the server).

### `[addon]`

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `name` | string | — (required) | Stable slug `[a-z0-9-]` (no leading/trailing dash). Becomes the gateway server name. |
| `display_name` | string | `""` | Human-friendly name (falls back to `name`). |
| `version` | string | `""` | Author-declared version (free-form). |
| `description` | string | `""` | One-line summary shown in `addon list` and on the website. |
| `author` | string | `""` | Maintainer or org. |
| `homepage` | string | `""` | Project homepage / repository URL. |
| `license` | string | `""` | SPDX id (e.g. `Apache-2.0`). |
| `categories` | string[] | `[]` | Coarse buckets for browsing (e.g. `plans`, `workflow`, `search`). |
| `keywords` | string[] | `[]` | Free-form search terms. |
| `min_lean_ctx` | string | `""` | Minimum lean-ctx version targeted (informational). |

### `[mcp]`

Mirrors a `[[gateway.servers]]` entry — installation is a direct translation.

| Field | Type | Default | Transport | Meaning |
|-------|------|---------|-----------|---------|
| `transport` | `stdio` \| `http` | `stdio` | both | Wire protocol. |
| `command` | string | `""` | stdio | Executable to spawn. |
| `args` | string[] | `[]` | stdio | Arguments passed to `command`. |
| `env` | table | `{}` | stdio | Extra environment variables for the child process. |
| `url` | string | `""` | http | Streamable-HTTP endpoint (must be `http(s)://`). |
| `headers` | table | `{}` | http | Extra request headers (e.g. auth). |

### Installable vs. listed

- **Installable** — the `[mcp]` block resolves: `stdio` has a non-empty
  `command`, or `http` has an `http(s)` `url`. `lean-ctx addon add` wires it.
- **Listed** — a registry entry **without** a runnable `[mcp]` block. It appears
  in `addon list` / `search` / the website and links to its homepage, but
  `addon add` refuses (no fabricated wiring). Used for announced addons that have
  not published an MCP endpoint yet.

## Registry

The curated catalog. Layered like the model registry:

1. **Bundled** — `rust/data/addon_registry.json`, compiled into the binary.
2. **User override** — `<data_dir>/addon_registry.json` (optional). An entry with
   the same `name` replaces the bundled one.

Shape:

```json
{
  "registry_version": 1,
  "addons": [
    { "addon": { "name": "…", "description": "…", … }, "mcp": { … } }
  ]
}
```

Each array element is exactly one manifest (the `[mcp]` table may be omitted for
listed-only entries). Getting listed = a merge request adding an entry here.

## Install semantics

`lean-ctx addon add <name|path>`:

1. **Resolve** the manifest — by registry `name`, or from a local
   `lean-ctx-addon.toml` path (a path ends in `.toml`, contains `/`, starts with
   `.`, or is an existing file).
2. **Validate** metadata; require an installable `[mcp]` block (else refuse with
   a homepage pointer).
3. **Disclose + confirm** — print the exact transport/command/args/env (or
   url/headers) that will run, then require confirmation (`--yes`/`-y` to skip;
   refuses non-interactively without it, per [`cli::prompt`]).
4. **Wire** via `Config::update_global` (the safe, global-only persistence path):
   set `gateway.enabled = true` if it was off, then upsert a `[[gateway.servers]]`
   entry named after the addon (idempotent — replaces any same-named entry).
5. **Record** in `<data_dir>/addons/installed.json` (`name`, `version`, `source`,
   `gateway_server`) and invalidate the gateway catalog cache.

`lean-ctx addon remove <name>` reverses 4–5: drop the gateway server it owns and
the store entry. It leaves `gateway.enabled` untouched (disable explicitly with
`lean-ctx config set gateway.enabled false`).

### State vs. config

The live `[[gateway.servers]]` block in `config.toml` is the single source of
truth for what actually runs. `installed.json` is bookkeeping only — it maps an
addon to the gateway server it installed so `remove` unwinds exactly what `add`
wired. Deleting it never affects running servers.

## Security model

- The gateway is **global-only** and **opt-in**; a project-local config can never
  point it at arbitrary commands.
- `add`/`remove` are consequential writes: they disclose the wiring and require
  confirmation — never silent.
- The bundled registry is **curated** (review at merge time). `addon add <path>`
  on a local manifest is explicit and operator-driven.
- Output is deterministic and local-only: no network calls, no telemetry in the
  add/list/search/info/remove paths.

## CLI surface

| Command | Effect |
|---------|--------|
| `lean-ctx addon list` | Installed addons + the registry. |
| `lean-ctx addon search [query]` | Search the registry (empty = all). |
| `lean-ctx addon info <name\|path>` | Details + MCP wiring for one addon. |
| `lean-ctx addon add <name\|path> [-y]` | Install (registry or local manifest). |
| `lean-ctx addon remove <name> [-y]` | Uninstall. |
