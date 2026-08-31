# Addon Bootstrap Engine — Phase 2 (removed in 3.9.20)

> Status: **removed**. `rust/src/core/addons/bootstrap.rs` and the rest of the
> stack this document describes were deleted in the 3.9.20 cleanup and did not
> come back when the addon channel reopened in 3.10.1. Nothing below is
> implemented: there is no `[install]` block, no package-manager bootstrap, no
> `addons.allow_bootstrap`, and no install receipts.
>
> **lean-ctx no longer installs anything on your behalf.** A manifest declares
> how to *run* a tool; fetching it stays the user's step, where their own package
> manager's trust model applies. That was the point of removing this — "add an
> addon" had become "execute a fetch-and-install pipeline you did not read".
>
> Kept as the design of record for the decision, not as documentation of a
> feature. For what 3.10.1 actually does, see
> [the addon guide](../guides/addons.md) and
> [`addon-manifest-v1`](../contracts/addon-manifest-v1.md) § What 3.10.1
> implements.

## Problem (as it stood before 3.9.20)

`lean-ctx addon add` was **declarative**: it appended a
`[[gateway.servers]]` entry to the global config and recorded the install — it
never fetched or installed a package. That was sufficient for **ephemeral
runners** (`npx`, `uvx`), which download and execute their package lazily on the
first tool call. So `repomix` and `serena` already installed on add.

It is **not** sufficient for tools that need a real, one-time bootstrap before a
runnable command exists:

| Tool      | Why a runner can't do it                                              |
|-----------|----------------------------------------------------------------------|
| Headroom  | Ships as `headroom-ai[all]`; `uvx --from` breaks on its entry points — needs `uv tool install`. |
| Graphify  | `uv tool install "graphifyy[mcp]"` **and** a pre-built `graph.json`.  |
| Cognee    | Clone + `uv sync`; no single-command runner.                         |
| Letta     | `npm i -g` plus a long-running server instance.                       |

For these the registry stays **listed** (homepage + manual instructions). The
bootstrap engine closes that gap: a declarative `[install]` block that lean-ctx
can execute idempotently, with a clean uninstall path and the same security bar
as the rest of the addon system.

## Goals / non-goals

**Goals**
- One declarative `[install]` block per addon, pinned and auditable.
- Idempotent install + reliable uninstall (no orphaned global packages).
- Reuse the existing trust/audit pipeline — no new ad-hoc shell-outs.
- Keep Phase-1 behaviour unchanged when no `[install]` block is present.

**Non-goals**
- No arbitrary script execution (no `curl | sh`, no inline shell).
- No secret provisioning (Mem0 / Claude-Context keys stay the user's job; the
  engine only documents and validates the required env names).
- No new network fetch of the registry itself (still compiled-in / signed override).

## The `[install]` manifest block

```toml
[install]
manager = "uv"                     # one of: pip | uv | cargo | npm | brew | dotnet
package = "headroom-ai[mcp]"       # the package spec the manager understands
version = "0.27.0"                 # MANDATORY exact pin (no ranges, no "latest")
bin     = "headroom"               # binary the [mcp] command expects (PATH idempotency)
# verify = ["headroom", "--version"]# optional argv probe; exit 0 ⇒ already installed
```

Rules (enforced by `AddonInstall::validate()`, called from `manifest.validate()`):
- `manager` ∈ a fixed allowlist (`uv`/`pip`/`cargo`/`npm`/`brew`/`dotnet`). Each manager
  maps to a **fixed argv template** the engine owns — the manifest never supplies
  raw shell.
- `version` is required and must be an exact pin (empty / `latest` / `*` are
  rejected).
- `package`, `version`, `bin` and every `verify` element are rejected if they
  contain shell metacharacters (`| ; & $ \` > <`, newlines) — defence-in-depth,
  since the engine never uses a shell.
- **Idempotency check** (shipped refinement): `verify` is *optional*. With no
  `verify`, the engine checks whether `bin` resolves on `PATH`; `verify` is an
  escape hatch (argv, exit 0 ⇒ installed) for tools whose presence needs a
  deeper probe.
- The block is only meaningful together with a runnable `[mcp]` block whose
  `command` is produced by the install (e.g. `headroom`).

### Manager → argv templates (engine-owned)

| `manager` | install argv                                     | uninstall argv                   |
|-----------|--------------------------------------------------|----------------------------------|
| `uv`      | `uv tool install {package}=={version}`           | `uv tool uninstall {base}`       |
| `pip`     | `pip install --user {package}=={version}`        | `pip uninstall -y {base}`        |
| `cargo`   | `cargo install {base} --version {version}`       | `cargo uninstall {base}`         |
| `npm`     | `npm install -g {package}@{version}`             | `npm rm -g {base}`               |
| `brew`    | `brew install {package}` (formula carries the pin, e.g. `node@22`) | `brew uninstall {base}` |
| `dotnet`  | `dotnet tool install --global {package} --version {version}`       | `dotnet tool uninstall --global {base}` |

`{base}` is `{package}` with extras and any inline version stripped
(`headroom-ai[mcp]` → `headroom-ai`), keeping an npm scope intact
(`@scope/pkg`). The manifest chooses a manager + package + pin; it **cannot**
influence the flags or inject extra argv. Every value is passed as a *discrete*
argv element via `std::process::Command` — no shell, no interpolation.

## Install lifecycle

The executor lives in `addons/bootstrap.rs` (`ensure_installed` / `uninstall`)
and is orchestrated by the CLI (`cli/addon_cmd.rs`) *after* consent and *before*
the health probe. The core `addons/install.rs` stays pure — it only persists the
receipt — so its unit tests never spawn a process.

```mermaid
flowchart TD
  add["addon add <name>"] --> has{has [install]?}
  has -- no --> wire["wire [[gateway.servers]] (Phase 1)"]
  has -- yes --> gate["bootstrap gate: validate + consent"]
  gate --> present{already present? (verify argv)}
  present -- yes --> wire
  present -- no --> run["run engine-owned install argv (pinned)"]
  run --> verify["run verify argv → must exit 0"]
  verify -- ok --> record["record install receipt (manager, package, version, bin)"]
  record --> wire
  verify -- fail --> rollback["best-effort uninstall + abort, no wiring"]
```

- **Idempotency**: check presence *first* (`verify` argv, else `bin` on `PATH`);
  if already satisfied, skip the manager entirely and just wire. Re-running `add`
  is safe and reports `Already installed — skipped`.
- **Pre-flight**: when an install *is* needed, verify the manager itself exists
  (`LEANCTX_BOOTSTRAP_<MGR>` override path, else the bare name on `PATH`) before
  spawning it. A missing manager fails with an install hint
  (`install uv → https://docs.astral.sh/uv/…`) instead of a raw spawn error, and
  the `add` disclosure shows a `requires: <manager> on PATH — ✓/✗` line up front
  so the gap is visible *before* consent.
- **Receipt**: `<data_dir>/addons/installed.json` carries an `install` record
  (manager, package, version, bin) — content-only, no timestamps, so it stays
  determinism-friendly (#498). `remove` reads it to uninstall.
- **Uninstall**: `addon remove` runs the manager's uninstall argv for packages
  *this engine installed* (tracked by receipt) — never something the user had
  already. It is best-effort: a failed uninstall logs a note but never blocks the
  unwire that already succeeded.
- **Failure**: a non-zero manager exit aborts `add` before anything is wired. A
  clean install whose `bin` is not yet on `PATH` is a non-fatal warning (a PATH
  setup issue, not a failed install), and the subsequent health probe still
  guards a truly broken wiring.

## Security gates

The bootstrap surface is gated at four layers (shipped):

- **Structural validation** (`AddonInstall::validate()`, hard error): unknown
  manager, missing/floating version, or shell metacharacters in
  `package`/`version`/`bin`/`verify` reject the manifest. Because it runs from
  `manifest.validate()`, every path is covered — `addon add`, `addon audit`,
  `from_path`, and the registry validator (so a bad block fails CI's
  `bundled_registry_passes_security_validator`).
- **Capability coherence**: a declared `[install]` block makes
  `trust::wiring_uses_network` return `true`, so an addon that *also* declares
  `[capabilities] network = "none"` trips the existing `cap_net_underdeclared`
  audit — same gate as the `npx`/`uvx` runner case.
- **Consent**: the `add` preview prints the **exact** install + uninstall argv,
  the manager, the package and the pin *before anything runs*, then requires the
  standard yes/no (`--yes` to skip in CI). `add` itself is the user's explicit,
  consented action.
- **Policy floor**: `addons.allow_bootstrap` (global-only). Default **on** — the
  whole point is that `add` installs — but a team that forbids local
  package-manager execution sets it to `false`, and `policy::gate` refuses any
  `[install]` addon before a single command runs.

## What this unlocks — and the honest migration status

The engine is generic across all five managers. Registry entries flip to
install-on-add **only when the tool actually ships a clean, pinned,
runnable-out-of-the-box MCP server** — never with fabricated wiring.

| Tool      | Status        | Why |
|-----------|---------------|-----|
| **Headroom** | ✅ migrated  | `uv tool install "headroom-ai[mcp]"` (pinned `0.27.0`) → `headroom mcp serve`; a local, secret-free stdio MCP server. The flagship install-on-add. |
| Graphify  | listed        | Package installs cleanly (`graphifyy[mcp]`), but its MCP server needs a **pre-built `graph.json`** (`python -m graphify.serve graph.json`) — no out-of-the-box server to probe. |
| Cognee    | listed        | MCP server needs a **repo clone + `uv sync`** (upstream issue #1815); no working pinned one-liner yet. |
| Letta     | listed        | A pinned `letta-mcp-server` package exists, but the server needs `LETTA_API_KEY` + a Letta backend to start — key-gated, not one-click. |

Mem0 and Claude-Context likewise remain key-gated: the engine *could* install
their package, but cannot provision `MEM0_API_KEY` / `OPENAI_API_KEY` + Milvus —
those stay documented prerequisites. Each tool above flips to installable with a
**one-line registry change** (an `[install]` + `[mcp]` block) the moment upstream
ships a clean server — no further engine work.

## Rollout — done

1. ✅ `[install]` parsing + validator gates.
2. ✅ Install/uninstall executor (`bootstrap.rs`), gated by `addons.allow_bootstrap`.
3. ✅ Headroom migrated; the bundled registry stays green on
   `bundled_registry_passes_security_validator`. Graphify/Cognee/Letta wait on a
   clean upstream MCP server (see table) rather than shipping broken wiring.

## Operational notes & open questions

- **Manager path override** (shipped): set `LEANCTX_BOOTSTRAP_<MANAGER>` (e.g.
  `LEANCTX_BOOTSTRAP_UV=/opt/uv`) to pin the exact manager binary for locked-down
  environments; otherwise the manager is resolved from `PATH`.
- Open: per-manager cache/location detection for richer "already present" checks
  (today: `verify` argv, else `bin` on `PATH`).
- Partly shipped: `add` now pre-flights the manager's existence (with an install
  hint); a full `doctor` sweep over *already-installed* addons is still open.
- Open: Windows support for the manager templates (the executable probe already
  falls back to "is a file" off-unix; argv templates assume POSIX managers).
