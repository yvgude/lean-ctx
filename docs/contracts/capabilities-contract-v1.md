# Capabilities Contract (v1)

`GET /v1/capabilities` returns a discovery document so any client — in any
language — can learn at runtime what a lean-ctx instance supports, and branch on
real features instead of making trial calls. It is the entry point of the
Context OS "Open Door" (RFC `docs/context-os/rfc-v1.md`, EPIC 12.1).

- **Contract version:** `1` (`leanctx.contract.capabilities.contract_version`,
  constant `CAPABILITIES_CONTRACT_VERSION` in `rust/src/core/contracts.rs`).
- **Payload builder (SSOT):** `rust/src/core/server_capabilities.rs`
  (`capabilities_value()`).
- **Drift gate:** `rust/tests/capabilities_contract_up_to_date.rs` binds the key
  list below to `server_capabilities::TOP_LEVEL_KEYS`.
- **Auth:** same as the rest of `/v1` (Bearer token unless loopback). No secrets
  are ever included in the payload.

## Top-level keys

The capabilities document has exactly these top-level keys (machine-readable —
kept in sync with code by the drift test):

<!-- capabilities-top-level-keys -->
contract_version, server, plane, transports, presets, read_modes, tools, features, extensions, contracts
<!-- /capabilities-top-level-keys -->

| Key | Type | Meaning |
|-----|------|---------|
| `contract_version` | number | This contract's version (`1`). |
| `server` | object | `{ name, version, persona }` — `version` is the running `lean-ctx` release; `persona` is the active context persona (`persona-spec-v1`, EPIC 12.15). |
| `plane` | string | Deployment plane: `personal` (local), `team`, or `cloud`. The local default is `personal`. See RFC §6 (Local-Free Invariant). |
| `transports` | string[] | Wire transports this instance speaks: `stdio-mcp`, `http-mcp`, `rest`, `sse`. |
| `presets` | string[] | Built-in context personas (`persona-spec-v1`, EPIC 12.15/12.16). Today: `coding` (the historical default); non-coding presets land in 12.16. |
| `read_modes` | object | `{ count, modes }` — the `ctx_read` modes this build supports (mirrors the MCP manifest). |
| `tools` | object | `{ total, names }` — the granular tool surface available on this instance. |
| `features` | object | Capability flags. Always-on capabilities are `true`; feature-gated ones (`semantic_search`, `ast_compression`, `team_server`, `cloud_server`, `http_server`) mirror the compiled Cargo features. |
| `extensions` | object | Runtime-discovered extension surface: `plugins` (enabled plugins, `{ name, version, permissions }` — declared trust permissions per `extension-trust-v1`, EPIC 12.3), `tools` (manifest-declared plugin tools `{ name, plugin }`, EPIC 12.11), plus the registered `read_modes`, `compressors`, and `chunkers` names from the extension registry (EPIC 12.9). Built-ins are listed alongside extension-provided entries; the set grows with the sandboxed extension runtime (EPIC 12.8). |
| `contracts` | object | All machine-verified contract versions (`versions_kv()`), so a client can check every sub-contract at once. |

## Example

```json
{
  "contract_version": 1,
  "server": { "name": "lean-ctx", "version": "3.7.1", "persona": "coding" },
  "plane": "personal",
  "transports": ["stdio-mcp", "http-mcp", "rest", "sse"],
  "presets": ["coding", "data-analysis", "lead-gen", "research", "support"],
  "read_modes": { "count": 10, "modes": ["auto", "full", "map", "signatures", "diff", "aggressive", "entropy", "task", "reference", "lines:N-M"] },
  "tools": { "total": 42, "names": ["ctx_read", "ctx_search", "..."] },
  "features": {
    "compression": true, "caching": true, "knowledge": true, "session": true,
    "gateway": true, "sensitivity_floor": true, "savings_ledger": true, "audit_trail": true,
    "ast_compression": true, "semantic_search": true,
    "http_server": true, "team_server": true, "cloud_server": false
  },
  "extensions": {
    "plugins": [{ "name": "my-plugin", "version": "0.1.0", "permissions": ["network"] }],
    "tools": [],
    "read_modes": ["full"],
    "compressors": ["identity", "markdown", "prose", "whitespace"],
    "chunkers": ["csv", "eml", "html", "json", "lines", "paragraph"]
  },
  "contracts": { "leanctx.contract.http_mcp.contract_version": 1, "...": 1 }
}
```

## Versioning & Deprecation Policy (`/v1` surface)

This policy governs the whole `/v1` HTTP/MCP surface, not just this endpoint.

1. **Additive changes are non-breaking.** New top-level keys, new tools, new
   feature flags, new presets/extensions, and new enum *values* may be added
   within `v1`. Clients **must ignore unknown fields**.
2. **Breaking changes bump the path.** Removing/renaming a key, changing a
   type, or changing the meaning of a value requires a new version (`/v2`).
   `v1` does not break under our control.
3. **Discover, don't assume.** Clients should read `contract_version` and
   `contracts`, and gate behavior on `features`/`presets`/`extensions` rather
   than hardcoding assumptions about a given release.
4. **Deprecation window.** A surface marked deprecated stays available for at
   least **two minor releases** after the release that introduces its
   replacement. Deprecations are announced in `CHANGELOG.md` and, where a
   client touches the affected surface, surfaced via a `Warning` response
   header. Removal happens only on a major version bump (`/v2`).
5. **SSOT.** The payload shape is generated from
   `rust/src/core/server_capabilities.rs`; contract versions live in
   `rust/src/core/contracts.rs`. Documentation drift is a CI failure.
