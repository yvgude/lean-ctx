# Addons — extending lean-ctx

> **Status: Preview — local addon channel.** Building, installing and running an
> addon on your own machine is a supported path. LeanCTX does **not** offer
> a marketplace, a curated registry, ranking or recommendation, managed binary
> distribution, or a commercial catalog, and this document is not a promise of
> one.

An addon is a package you install with one command. It carries one or both of:

- **a sandboxed WebAssembly module** that runs *inside* the context pipeline —
  today that means a **compressor**; context providers use the same ABI and are
  wired the same way. Every other extension point (chunkers, read modes, render
  transforms) exists as a Rust trait and is not reachable over the WASM ABI yet.
- **an MCP server declaration**, wired into lean-ctx's gateway so a third-party
  tool plugs in with no fork and no recompile.

Which one you write follows from where your code has to run. A compressor has
to run inside the pipeline, so it is WASM. A tool that speaks MCP already has a
process model of its own, so it is declared rather than embedded — and it stays
yours: you publish it, and lean-ctx audits foreign MCP servers in `tools health`
either way. The two halves are described separately below.

## Using an addon

```bash
lean-ctx addon add ./my-addon-1.0.0.ctxpkg   # verifies, shows what it is, asks
lean-ctx addon add @ns/my-addon              # same, from a registry you name
lean-ctx addon list                          # what is installed and what it loads
lean-ctx addon info @ns/my-addon             # the author's manifest, module digests
lean-ctx addon remove @ns/my-addon
```

`add` is the only verb that stores executable code, so it is the only one that
asks — and it refuses to proceed non-interactively unless you pass `--yes`.

Versions sit side by side in the store, and only the most recently installed one
loads. `addon list` marks the others `(superseded)` and says their modules are
on disk but not running, rather than listing code that never executes;
`addon remove <name>` clears every version.

`lean-ctx doctor` reports what the engine actually loaded, which is a different
question: install checks a module's four magic bytes, not a full parse, so a
truncated or corrupt module installs cleanly and is then refused by the loader.
`addon list` would still show it. Doctor names it.

A registry reference is downloaded to a temp file and then goes through exactly
the same preview, prompt and verification as a file you already had: there is
one consent path, not a shorter one for remote installs. An argument that names
something on disk always wins over a registry lookup, so `acme/widget` cannot
quietly fetch from the network when a file by that name is sitting there.
`--registry <url>` selects where to look.

## Writing one: a WASM module

A directory with a manifest and at least one module:

```
my-addon/
  lean-ctx-addon.toml
  my_addon.wasm
```

```toml
[addon]
name = "@ns/my-addon"
version = "1.0.0"
description = "What it does, in one line"
```

The module exports the ABI v1 entrypoints — see
[`wasm-abi-v1`](../contracts/wasm-abi-v1.md) for the exact signatures. Any
language that compiles to `wasm32-unknown-unknown` works. That file is a frozen
artifact: its own header predates this channel reopening and still says
"Research contract", which is why the status lives here and in the stability
matrix instead. The signatures in it are current and will not move — a breaking
ABI change would ship as `wasm-abi-v2`, with both accepted during an overlap.

```bash
lean-ctx addon release ./my-addon
```

That produces a signed `.ctxpkg` with the modules **embedded**. No artifact
host, no checksum files, no CI: the package signature covers the executable
bytes, so there is nothing external to hash or serve. Publish it wherever you
like, or with `lean-ctx pack publish` to a registry you name.

The first `release` creates an ed25519 signing key under your data directory.
It identifies you as the publisher across releases — back it up.

## Writing one: an MCP server

Not every extension belongs inside the pipeline. A tool that already speaks MCP
has a process model of its own, and wrapping it in WASM would buy nothing. Such
an addon carries no module — it declares the server instead:

```toml
[addon]
name = "lean-md"
version = "2.0.0"
description = "Macro/directive markdown renderer"

[mcp]
transport = "stdio"
command = "lean-md"
args = ["mcp", "serve"]
sha256 = "…"          # optional; when set it is checked before every spawn
integration = "memory"  # optional; fold results into a lean-ctx surface
```

`integration` picks a **typed adapter** so the server's output lands in the
`ctx_*` tool your agent already uses instead of arriving as opaque text:
`codebase-pack` → `ctx_expand`, `code-graph` / `code-symbols` → `ctx_callgraph`,
`memory` → `ctx_knowledge`, `compression` → the compressor pipeline, `none` for
passthrough. It may sit in `[addon]` (where it has always been documented) or in
`[mcp]`, which mirrors the gateway entry and wins if both are set; with neither,
a recognised `[addon] categories` entry derives one. Common aliases (`repomix`,
`callgraph`, `compressor`) are accepted and stored canonically. A slug written
as an integration but not recognised is an error rather than a silent fallback —
a typo that installed cleanly and quietly did less would look exactly like
working software. Categories stay lenient, because they are free-form browsing
labels rather than a vocabulary.

`addon add` turns that into a `[[gateway.servers]]` entry. Three limits, all of
them deliberate, and all of them things the pre-3.9.20 channel did differently:

- **lean-ctx never installs the server.** No `uv tool install`, no `npx`, no
  download. Putting `lean-md` on your machine stays your step, where your own
  package manager's trust model applies. The manifest only says how to run it.
- **The exact command is shown before you consent** — printed in full, not
  summarised, together with whether it is pinned. A `stdio` server runs as a
  **normal process with your privileges**: it is not sandboxed, the WASM
  guarantees above do not apply to it, and the prompt says so. An `http` server
  gets the disclosure that fits it instead — nothing runs on your machine, but
  lean-ctx sends that endpoint requests and treats its replies as untrusted
  input. Read the argv, or the URL.
- **Adding a server does not switch the gateway on.** `[gateway]` stays
  global-only and opt-in; `lean-ctx addon list` tells you when an addon is
  wired but the gateway is off, rather than letting you assume it is running.

Installing a newer version replaces what the *author* declares — transport,
command, args, URL, pin, integration — and keeps what *you* configured: the
credentials in `secret_env` / `secret_headers`, which a manifest cannot carry by
design, and the per-server `enabled` switch, so an addon you deliberately turned
off does not come back on behind an upgrade.

An `http` server that only accepts a browser login declares it:

```toml
[mcp]
transport = "http"
url = "https://api.example.com/mcp"
auth = "oauth"
scopes = ["mcp:read"]   # optional
```

Run `lean-ctx addon auth <name>` once. lean-ctx discovers the server's OAuth
endpoints, registers itself as a public client, opens the login page (PKCE,
redirect to a one-shot listener on `127.0.0.1`) and stores the result. From
then on the gateway attaches the token and refreshes it when it expires. The
command also works for a server whose manifest does not declare `auth` — a
successful login marks the entry. `--status` shows whether you are logged in,
`--logout` deletes the credentials, `--no-browser` only prints the URL.

The credentials are encrypted (XChaCha20-Poly1305, bound to the server URL) in
`<data_dir>/mcp-oauth/`. The key lives in the macOS Keychain or the Windows
Credential Manager; on Linux it is a `0600` file next to the credentials, so
there it protects against copying the credential files alone, not against a
process running as you. `addon remove` deletes the credentials, and an upgrade
keeps a login you completed.

When `sha256` is set, the gateway resolves `command` against the `PATH` the
child will see, hashes it, and refuses to spawn on a mismatch. An unset pin is
a documented no-op, not a silent pass.

Nothing stops one addon from being both: modules run in the pipeline, `[mcp]`
wires the server, and `addon remove` undoes both.

## What the sandbox does and does not do

This section is about **WASM modules**. A declared `[mcp]` server is an ordinary
process with your privileges and none of it applies to one — that is why its
command is printed in full before you consent.

**Enforced**, on every call:

- No ambient environment. A module sees nothing of your shell.
- A fresh WASM store per call, so nothing leaks between invocations.
- The host truncates the output to the byte budget *after* decoding it, so a
  module cannot exceed the budget by ignoring it.
- Modules are stored read-only and never marked executable — they are fed to an
  interpreter, never to the OS loader.

**Verified at install:**

- The package signature, re-checked locally rather than trusted from wherever
  it came ("registry compromise ≠ client compromise").
- The signer: the key travels inside the package, so `addon add` requires a
  publisher key pinned in `trusted-signers.toml`, a local identity or this
  machine's own key (your own `addon release` output installs as is). Pass
  `--allow-untrusted-signer` to accept a valid signature from an unknown key.
- Each module against its pinned SHA-256, before anything is written.
- WebAssembly magic bytes, so a mislabelled payload is refused early.

**Not claimed:** a module can compute anything it likes within those bounds.
The sandbox limits reach, not intent. Install addons from publishers you have
reason to trust, and read `addon info` before you do.

## Development override

`LEAN_CTX_WASM_DIR=<dir>` loads unsigned `.wasm` files straight from a
directory, for authoring a module before packaging it. It is a developer
convenience with none of the verification above — not an install path.

See [`local-free-invariant-v1`](../contracts/local-free-invariant-v1.md) for the
local-first guardrails this channel sits inside.
