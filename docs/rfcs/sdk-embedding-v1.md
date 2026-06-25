# RFC: lean-ctx SDK / Embedding (v1)

Status: **accepted, increment 1 implemented**
Crate: `rust/crates/lean-ctx-sdk` (`lean_ctx_sdk`)
Related plans: *lean-ctx SDK Embedding*, *lean-ctx Developer Platform* (Track A)

## Problem

The Addon system lets the engine call *your* tool (out-of-process, no access to
internals). The opposite need — **consume lean-ctx as an embedded engine** — has
no supported surface. Lean-md is the driving case: it calls engine cores
in-process with a **shared `SessionCache`** so a read → re-read produces a token
delta. Going through `lean_ctx::core::…` directly couples every consumer to
internal churn.

## Goals

1. A small, **stable Rust façade** with its own types (`Engine`, `ReadMode`,
   `Output`, `Error`) — engine internals can change without breaking embedders.
2. **Shared session cache** so the in-process read→re-read delta works (the
   acceptance property).
3. **Safe by default**: PathJail on, scoped state dir, auto-update off,
   write/exec behind explicit opt-in, no forced global allocator.
4. **No new mechanism**: dispatch the *real* registered tools, exactly as the
   MCP server does — zero behavioural drift.

## Non-goals (v1)

- Async API. v1 is synchronous (owns a multi-thread runtime, dispatches via the
  blocking pool like the server). Async wrappers can come later.
- Re-exporting global mutations (`Config::update_global`, install/uninstall).
- A feature-minimal engine build. The engine references `proxy`/`http_server`/
  `ort` unconditionally today, so the SDK pins **default-minus-jemalloc** rather
  than a hand-cut feature set. `tree-sitter` stays on (AST read modes).

## Design

### The `Engine`

`Engine` owns: the resolved project root (the PathJail root), a shared
`Arc<RwLock<SessionCache>>`, a shared `Arc<RwLock<SessionState>>`, the full
`ToolRegistry` (`build_registry()`), and a multi-threaded Tokio runtime.

Each call builds a `ToolContext` wired to the shared cache/session and dispatches
the tool via `spawn_blocking(move || tool.handle(&args, &ctx))` — the exact path
`LeanCtxServer` uses, so `ctx_read`'s `Handle::block_on` and `ctx_search`'s
`block_in_place` are both legal.

### Own types

| Façade type | Wraps |
|-------------|-------|
| `Engine` / `EngineBuilder` | registry + shared cache/session + runtime |
| `ReadMode` | the engine `mode` string (`auto`/`full`/`signatures`/`lines:N-M`/…) |
| `Output` | `ToolOutput` (text + token accounting), derives `Debug`/`Clone` |
| `Error` | `rmcp::ErrorData` + jail/permission/init errors |

### Safe-by-default

`EngineBuilder::build()` resolves + validates the project root, sets the engine's
data/config/state/cache dirs to a scoped temp dir (unless `.data_dir(…)`),
disables the update check, and constructs the runtime. Write tools (`ctx_edit`,
`ctx_fill`) and exec tools (`ctx_shell`, `ctx_execute`, `shell`) return
`Error::NotPermitted` unless `.allow_write(true)` / `.allow_exec(true)`.

## Surface map (~26 capabilities)

dasTholo's Lean-md uses ~26 engine capabilities. v1 ships ergonomic typed
methods for the read-mostly core and an escape hatch (`Engine::call`) that
reaches **every** registered tool (write/exec gated). The table tracks how each
capability is served today.

| Capability | Engine tool | v1 surface |
|------------|-------------|-----------|
| read | `ctx_read` | **typed** `read()` |
| search | `ctx_search` | **typed** `search()` |
| symbol | `ctx_symbol` | **typed** `symbol()` |
| outline | `ctx_outline` | **typed** `outline()` |
| tree / repomap | `ctx_tree` / `ctx_repomap` | **typed** `tree()` · `call()` |
| find | `ctx_glob` | `call("ctx_glob", …)` |
| count | `ctx_cost` / `tokens` | `tokens::count` · `call()` |
| graph | `ctx_graph` | `call("ctx_graph", …)` |
| callgraph | `ctx_callgraph` | `call("ctx_callgraph", …)` |
| impact | `ctx_impact` | `call("ctx_impact", …)` |
| architecture | `ctx_architecture` | `call("ctx_architecture", …)` |
| smells | `ctx_smells` | `call("ctx_smells", …)` |
| refactor | `ctx_refactor` | `call("ctx_refactor", …)` |
| review | `ctx_review` | `call("ctx_review", …)` |
| recall / remember | `ctx_knowledge` | `call("ctx_knowledge", …)` |
| query (semantic) | `ctx_semantic_search` | `call("ctx_semantic_search", …)` |
| render / compose | `ctx_compose` / `ctx_overview` | `call(…)` |
| inspect / list (tools) | `ctx_tools` | `call("ctx_tools", …)` |
| include / addressing | `ctx_read` (`lines:`/paths) | `read()` |
| reformat / compress | shell pattern engine | `compress::shell_output` |
| date / env / routes | `ctx_routes` etc. | `call(…)` |
| edit | `ctx_edit` | `call()` **(needs `allow_write`)** |
| shell / exec | `ctx_shell` / `ctx_execute` | `call()` **(needs `allow_exec`)** |
| hash | engine hash | `hash::blake3_*` |
| addon authoring/audit | scaffold + audit gate | `addon::scaffold/audit` |

Promoting a `call()`-served capability to a typed method is additive and
semver-safe; the plan is to graduate them as the Lean-md port exercises them.

## Acceptance

- In-repo: `tests/engine_read.rs` proves read→re-read saves ≥ the first read,
  PathJail rejects escapes, search finds symbols, and the write/exec gate +
  unknown-tool paths error correctly. `examples/embed.rs` shows a live ~99%
  re-read delta on `Cargo.toml`.
- External: dasTholo ports Lean-md onto the `Engine` — the real acceptance test
  that the surface is sufficient (tracked on the GitLab epic).

## Distribution & trust

The SDK is a **build** substrate; **distribution stays the Addon system**. A
binary built with the SDK and shipped as an addon still runs under the gateway's
OS sandbox + output redaction + trust/signing — embedding does not weaken
distribution security. Two trust contexts: (1) distributed as an addon =
sandboxed; (2) run standalone = the embedder owns the boundary, like any binary.

## Build reality (honest)

Full parity keeps `tree-sitter` on (AST modes are intrinsically costly).
Embedders needing only read/search/knowledge can later get a lighter path once
the engine compiles under a minimal feature set (today it does not). A
`lean-ctx-crypto`/ML split is deferred until measured build numbers justify it.
