# Addons — community extensions for lean-ctx

Addons let anyone extend lean-ctx with an **external MCP server** and have it
show up through the gateway with one command — no fork, no recompile. This guide
covers using addons and **building & publishing your own**.

> Not sure an Addon is the right mechanism? See
> [Extending lean-ctx](extensions.md) for the one-decision guide (Addon vs
> Plugin vs Provider vs Pack vs SDK).

Contract: [`addon-manifest-v1`](../contracts/addon-manifest-v1.md).

## Use an addon

```bash
lean-ctx addon list               # installed addons + the registry
lean-ctx addon search markdown    # search the registry (empty = list all)
lean-ctx addon info <name>        # details + the MCP wiring it would add
lean-ctx addon add <name>         # install (asks for confirmation)
lean-ctx addon remove <name>      # uninstall
```

`add` prints the exact server it will run (transport, command, args, env) and
asks before changing anything. Pass `--yes` / `-y` to skip the prompt in
scripts. Installing an addon enables the MCP gateway (`gateway.enabled = true`);
its tools become reachable via `ctx_tools` (find/call) — restart your MCP client
to pick them up.

## Build your own addon

An addon is just an MCP server plus a manifest. Four steps:

### 1. Expose your tool as an MCP server

Ship a `stdio` server (an executable that speaks MCP over stdin/stdout) or an
`http` server (a streamable-HTTP endpoint). This is what lean-ctx will run or
connect to. If your project is currently a library or a fork, wrap its
capabilities behind a thin MCP server binary — that is what makes it a runtime
addon instead of a build-time fork.

### 2. Add `lean-ctx-addon.toml` to your repo

Scaffold one in seconds — `lean-ctx addon init` writes a valid,
secure-by-default manifest (slug taken from the directory name) you then edit:

```bash
lean-ctx addon init                 # stdio addon in ./lean-ctx-addon.toml
lean-ctx addon init my-addon --http # or name it + use an HTTP endpoint
```

…or write it by hand:

```toml
[addon]
name = "my-addon"                 # slug: [a-z0-9-]
display_name = "My Addon"
version = "0.1.0"
description = "What it does, in one line."
author = "you"
homepage = "https://github.com/you/my-addon"
license = "Apache-2.0"
categories = ["workflow"]
keywords = ["plans", "macros"]
min_lean_ctx = "3.8.0"

[mcp]
transport = "stdio"               # or "http"
command = "my-addon-mcp"          # stdio: executable to spawn
args = ["serve"]
# env = { MY_TOKEN = "..." }      # optional child-process env

# For an HTTP server instead of stdio:
# [mcp]
# transport = "http"
# url = "https://my-addon.example.com/mcp"
# headers = { Authorization = "Bearer ..." }
```

See the [contract](../contracts/addon-manifest-v1.md) for every field.

### Declare what your addon needs — `[capabilities]`

Add a `[capabilities]` block to opt your stdio addon into a **per-addon,
secure-by-default sandbox**. lean-ctx enforces the `network`/`filesystem` profile
you declare at the spawn point (`sandbox-exec` on macOS, `bwrap` on Linux — and
child processes inherit it), scrubs the environment to your `env` allowlist, and
shows the user the full list before they install:

```toml
[capabilities]
network = "full"          # "none" (default) blocks all outbound network
filesystem = "read_only"  # "read_write" if you write outside a scratch tmp
env = ["GITHUB_TOKEN"]    # only these host env vars reach your process
```

Declaring nothing is the safest: no network, read-only filesystem, and a
scrubbed environment (host secrets never leak to your child process). Omit the
block entirely to keep the legacy global `addons.sandbox` behaviour. Declaring
the minimum you need is what makes your addon trustworthy in the marketplace.

### 3. Test it live — locally, before publishing

```bash
lean-ctx addon audit ./lean-ctx-addon.toml   # the publish/list gate (#403)
lean-ctx addon add ./lean-ctx-addon.toml
lean-ctx addon list               # your addon, installed (source: local)
# … exercise it via ctx_tools …
lean-ctx addon remove my-addon
```

`addon add <path>` wires a local manifest exactly like a registry entry, so you
get the full install flow without touching the registry. `addon audit` runs the
same gate the registry validator does — wiring risk, **capability coherence**
(do your `[capabilities]` match what the wiring actually does?) and **malware
heuristics** — and exits non-zero on a `fail` verdict, so you can run it in CI.

#### Pin your binary (stdio) — `sha256`

For a `stdio` addon, pin the binary so a swapped executable can never run under
your addon's name:

```bash
shasum -a 256 my-addon-mcp        # → copy the hex digest
```

```toml
[mcp]
transport = "stdio"
command = "my-addon-mcp"
sha256 = "…the digest…"           # the gateway refuses a mismatch, fail-closed
```

A pinned binary is one of the requirements for the verified/paid tier (see the
audit gate below).

### 4. Get listed in the registry

Open a merge request adding your manifest as an entry to
`rust/data/addon_registry.json`:

```json
{
  "addon": {
    "name": "my-addon",
    "display_name": "My Addon",
    "description": "What it does, in one line.",
    "author": "you",
    "homepage": "https://github.com/you/my-addon",
    "license": "Apache-2.0",
    "categories": ["workflow"],
    "keywords": ["plans", "macros"],
    "min_lean_ctx": "3.8.0"
  },
  "mcp": {
    "transport": "stdio",
    "command": "my-addon-mcp",
    "args": ["serve"]
  }
}
```

Before opening the merge request, validate the registry locally — the same bar
CI enforces:

```bash
lean-ctx addon registry validate rust/data/addon_registry.json
```

Once merged, everyone can run `lean-ctx addon add my-addon`, and your addon
appears on the website's Addons page.

> **Not ready to publish an endpoint yet?** Submit a *listed* entry — the
> `[addon]` table without an `[mcp]` block. It shows up in the registry and on
> the website and links to your homepage; `addon add` points users there until
> you ship the endpoint, then adding the `mcp` block flips it to one-click
> installable.

### 5. Sell your addon (optional)

Add a `[pricing]` block to make your addon a paid artifact — the same commerce
rails that already sell context packs:

```toml
[pricing]
price_cents = 1900        # $19.00 one-time
currency = "usd"
# or usage-metered, billed per tool call:
# model = "usage"
# usage_price_per_1k_cents = 200   # $2.00 per 1,000 calls
```

A paid addon must clear the **paid-listing gate** before it can be sold — this is
deliberate: buyers of third-party code get App-Store-level assurance. The gate
requires:

- a **pass** audit that is **paid-eligible** (declared + coherent
  `[capabilities]`, and a pinned `sha256` for stdio addons),
- a **verified-publisher** entry, and
- well-formed pricing.

Check exactly where you stand any time:

```bash
lean-ctx addon audit ./lean-ctx-addon.toml   # shows pricing + paid-listing gate
```

If blocked, the audit lists the precise remaining steps (pin your binary, apply
for verification, declare capabilities). Free addons are unaffected — the gate
only governs paid artifacts.

## Build *on* lean-ctx from inside your addon (`lean-ctx call`)

Your addon can call lean-ctx's own tools — read, search, symbol/outline, refactor
and the rest — by shelling out to `lean-ctx call`. This is the simplest, most
robust integration path and works from **any language**:

```bash
lean-ctx call <tool> --project-root <root> --json '<args>'
```

- **Stateless** — each call is a fresh, short-lived process; one error = one exit
  code, trivially retryable. No server, no warm connection, no endpoint
  discovery — it only needs `lean-ctx` on `PATH`.
- **No `tool_profile` precondition** — `call` builds the tool registry itself and
  dispatches to *any* tool, independent of any running server's profile (unlike
  the MCP path, where the code-intel `ctx_*` tools require `tool_profile = power`).
- **Always pass `--project-root`** — `call` resolves a `path` argument against it
  (and pins `"."`/`""` to the root), so tools operate on your project, never the
  process CWD.

```jsonc
// example: ask lean-ctx to read a file, compressed
lean-ctx call ctx_read --project-root /repo --json '{"path":"src/main.rs","mode":"signatures"}'
```

### Declare it: the callback capability block

Spawning `lean-ctx` is subprocess execution, so a callback addon should declare
`exec` — it's how the audit and the install consent reflect what the addon does.
Recommended block:

```toml
[capabilities]
network = "none"            # local code-intel needs no internet
filesystem = "read_write"   # the lean-ctx child writes its session cache
exec = ["lean-ctx"]         # may spawn exactly lean-ctx
```

Two gotchas, because the spawned `lean-ctx call` **inherits your addon's
sandbox**:

- **Cache writes.** Under `filesystem = "read_only"`, the child's writes to its
  data dir are blocked (only a scratch tmp is writable) — output still returns,
  but caching degrades. Either declare `filesystem = "read_write"` **or** point
  the child at a writable tmp with `LEANCTX_DATA_DIR=/tmp/lean-ctx-<addon>`.
- **Write tools.** `ctx_refactor` and friends modify files; if your addon
  applies (not just previews) them, it needs `filesystem = "read_write"`.

`exec` is a **declared + audited** capability — not OS-enforced on any platform.
What's enforced is the network/filesystem sandbox, which the spawned `lean-ctx`
**inherits** (so the callback can't exfiltrate or tamper either). Declaring
`exec = ["lean-ctx"]` keeps the audit honest and shows the user exactly what the
addon does (see
[`addon-manifest-v1`](../contracts/addon-manifest-v1.md)).

## How it works

- Installing writes a `[[gateway.servers]]` entry to your global `config.toml`
  and records the addon in `<data_dir>/addons/installed.json`. The gateway is
  **global-only** and opt-in — an untrusted project can never wire a server.
- `remove` drops exactly the gateway server the addon installed. It leaves the
  gateway enabled; turn it off with `lean-ctx config set gateway.enabled false`.
- Everything is local and deterministic: no network calls or telemetry in the
  add/list/search/info/remove paths.

### Discover & measure

```bash
lean-ctx addon search plans     # full-text search; [verified] addons are badged
lean-ctx addon categories       # browse by category, with live counts
lean-ctx addon usage            # per-addon / per-tool call counters (local meter)
```

`addon usage` reads the local meter (`<data_dir>/addons/usage.json`): every
gateway tool call is attributed to its addon + tool, so you can see what you
actually rely on. It is local-only and a pure side-channel — it never changes a
tool's output. Turn it off with `lean-ctx config set addons.metering false`.

## Security & trust

An addon runs real code with your privileges (stdio) or sends context to a remote
endpoint (http), so lean-ctx makes installing one a disclosed, policy-gated
action. Full model: the [contract](../contracts/addon-manifest-v1.md#security-model).

- **Trust tier.** Catalog entries are **verified** (maintainer-audited) or
  **community** (installable, unaudited). The tier shows in `addon list`,
  `addon info` and the install preview.
- **Risk review.** Before install, lean-ctx prints a security review of the
  wiring — remote endpoints, shelling out, unpinned upstreams, secret-bearing env
  — so you see what an addon can do before you say yes.
- **Capabilities.** An addon that declares `[capabilities]` runs under a
  per-addon OS sandbox + environment allowlist derived from exactly those
  permissions — secure-by-default, shown to you before install.
- **Audit gate.** `lean-ctx addon audit` (and the registry validator) flags any
  addon whose declared capabilities don't match its wiring, and scans for malware
  patterns (pipe-to-shell, base64-decode→exec, persistence writes). A `fail`
  verdict bars a listing; verified/paid entries must pass cleanly, declare
  coherent capabilities, and pin their binary.
- **Binary pin.** A stdio addon can pin its binary's `sha256`; the gateway hashes
  the resolved executable before spawn and refuses a swap (fail-closed).
- **Untrusted output.** An addon's tool output is redacted for secrets and
  audit-tagged as untrusted before it reaches the model.
- **Kill-switch.** `lean-ctx addon revoke <name>` blocks an addon from running
  everywhere — install, the gateway catalog, and every call — without waiting for
  an uninstall. `unrevoke` lifts it; `revocations` lists active blocks.
- **Integrity lock.** Install pins a hash of the exact wiring. `lean-ctx addon
  verify` re-checks it against your live config and flags drift — a swapped
  command, an extra arg, or a widened capability after install.

### Lock it down (teams / enterprise)

The global-only `[addons]` block sets a floor an untrusted repo can't loosen:

```bash
# only install maintainer-verified addons
lean-ctx config set addons.policy verified_only

# or restrict to an explicit allowlist
lean-ctx config set addons.policy allowlist
lean-ctx config set addons.allowlist my-addon,other-addon

# refuse anything with a high-risk capability
lean-ctx config set addons.block_risky true

# sandbox spawned addon servers without a [capabilities] block
# (macOS sandbox-exec / Linux bwrap)
lean-ctx config set addons.sandbox strict

# fail closed if a declared-capability addon can't be sandboxed
lean-ctx config set addons.enforce_capabilities true

# require a signed user-override registry (trusted org key)
lean-ctx config set addons.require_signature true

lean-ctx config schema addons   # inspect every key
```

Distribute these via MDM / config-management, or pin them through the signed
org-policy floor (`policy org`) to make them un-bypassable.

## Troubleshooting

```bash
lean-ctx addon list               # is it installed? which gateway server?
lean-ctx config schema gateway    # inspect gateway config keys
lean-ctx status                   # MCP server / gateway status
```

If a freshly installed addon's tools do not appear, restart your MCP client so
it re-reads the gateway catalog.
