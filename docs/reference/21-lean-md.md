# Journey 21 — lean-md (Addon Integration)

> lean-md is an **external lean-ctx addon** — a macro/directive Markdown renderer.
> It lives in its own repository (`dasTholo/lean-md`) with its own release cycle.
> This page documents how lean-ctx **integrates** the addon. The full `@directive`
> catalog, engine spec, and E-constructs live in the addon repo, not here.

---

## 1. What lean-md is

lean-md renders `.lmd.md` / `.lean-md` files: `@directive` calls plus a macro
engine (`@define`/`@call`), container gating (`@if`/`@consumer`), and pipes
(`@render`). Code-intel directives (`@read`/`@refactor`/`@search`/…) call lean-ctx
`ctx_*` tools **over the wire** (CLI/MCP); the renderer itself is standalone
(`rushdown` + `evalexpr`) with **no** lean-ctx crate dependency.

Engine, full directive catalog, and spec: **https://github.com/dasTholo/lean-md**.

## 2. Installation

```bash
lean-ctx addon add @dasTholo/lean-md        # hosted pack (ctxpkg.com)
lean-ctx addon add ./lean-md-2.0.0.ctxpkg   # a signed package you already have
```

`addon add` prefers a path that exists on disk and otherwise resolves
`ns/name[@version]` against a registry. Either way the package is verified
locally and its `[mcp]` command shown before you are asked. There is no
`addon search`, and no install-by-name from the bundled list — that list is a
directory, not a package source.

lean-ctx does **not** install the `lean-md` binary for you; the manifest records
how to run it once you have it.

After install, restart the MCP client so the gateway catalog is re-read. The addon
is spawned as a stdio gateway child; its tools (`ctx_md_render`, `ctx_md_check`)
become reachable through the lean-ctx server.

## 3. Integration points in lean-ctx

lean-ctx keeps its lmd surface deliberately small: `.lmd.md` is read **raw** (§3.1),
the addon ships as a registry entry (§3.2), and the addon calls back through the
stable `ctx_*` surface (§3.3). Everything else is the addon's.

### 3.1 Raw `.lmd.md` read (no in-tree rendering)

`ctx_read` treats `.lmd.md` like any other file: it returns the **raw** bytes and
never renders (a half-rendered body would be worse than none). Rendering is the
addon's job, reached explicitly through its `ctx_md_render` / `ctx_md_check` tools
once installed. lean-ctx carries **no** `.lmd.md` special-casing in `ctx_read`; the
earlier auto-render delegation hook was reverse-cut before merge.

Source: `rust/src/tools/registered/ctx_read.rs` (no lmd branch),
gate test `rust/tests/ctx_read_lmd_md_raw.rs`.

### 3.2 Addon registry entry

`rust/data/addon_registry.json` carries the **listed** `lean-md` entry (no runnable
`[mcp]` command, no `[install]` block), so `core::addons::manifest::is_installable`
reports `false` and the entry serves discovery only. The validator
(`core::addons::registry::validate_entries`) requires a homepage for a listed entry.

### 3.3 ctx_* outbound surface = addon contract

Every lean-md code-intel directive calls back into lean-ctx via
`backend.call("ctx_*", …)`. That tool set (`ctx_read`, `ctx_refactor`,
`ctx_search`, `ctx_outline`, `ctx_impact`, `ctx_repomap`, `ctx_review`,
`ctx_routes`, `ctx_smells`, `ctx_architecture`, `ctx_graph`, `ctx_callgraph`,
`ctx_knowledge`, `ctx_handoff`, `ctx_agent`, …) is a stable **outbound contract**
and must stay registered. Only `ctx_md_render` / `ctx_md_check` are addon-provided
and absent from lean-ctx.

## 4. Decoupling rationale (vs. main)

lean-md was developed in-tree (phases 1–9) and then **reverse-cut** before merge:
the in-tree engine never reaches `main`. The lmd-related deltas this branch lands
in lean-ctx are integration-only.

| Class   | Change (vs. main)                                                       | Why                                                |
|---------|-------------------------------------------------------------------------|--------------------------------------------------|
| removed | `.lmd.md` auto-render delegation in `ctx_read.rs` → **raw read**         | no in-tree engine renders; the addon renders on request |
| changed | `addon_registry.json`: `lmd` placeholder → **listed** `lean-md` entry    | discoverability; install goes through the hosted pack   |
| added   | generic `extension_registry::RenderTransform` trait + registry           | infra for `@render type=<name>`, not lmd-exclusive      |
| kept    | ctx_* outbound tool surface                                             | the addon calls them over the wire                 |
| added   | gate tests `reverse_cut_gate.rs`, `ctx_read_lmd_md_raw.rs`              | enforce the cut invariant + raw read               |

The engine, full `@directive` catalog, E-constructs, and spec now live in
`dasTholo/lean-md` and are **not** mirrored here.

## 5. See also

- Addon repo (engine + full directive reference): https://github.com/dasTholo/lean-md
- Addon manifest contract: `docs/contracts/addon-manifest-v1.md` (upstream)
- MCP tool catalog: [`appendix-mcp-tools.md`](appendix-mcp-tools.md)
- Decoupling design: https://github.com/dasTholo/lean-md (addon repo — hosts engine, spec & decoupling design)
