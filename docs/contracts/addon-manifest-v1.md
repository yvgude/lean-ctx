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
| `verified` | bool | `false` | **Registry-controlled** trust tier. `true` only for entries a maintainer has audited and vouched for. Setting it in a hand-written manifest is meaningless — trust is conferred by the registry an entry ships in, not by the entry claiming it. |

### `[mcp]`

Mirrors a `[[gateway.servers]]` entry — installation is a direct translation.

| Field | Type | Default | Transport | Meaning |
|-------|------|---------|-----------|---------|
| `transport` | `stdio` \| `http` | `stdio` | both | Wire protocol. |
| `command` | string | `""` | stdio | Executable to spawn. |
| `args` | string[] | `[]` | stdio | Arguments passed to `command`. |
| `env` | table | `{}` | stdio | Extra environment variables for the child process. |
| `sha256` | string | `""` | stdio | Optional SHA-256 pin of the `command` binary (the value `shasum -a 256` prints). When set, the gateway hashes the resolved binary before spawn and refuses a mismatch (fail-closed). Empty = unpinned. |
| `url` | string | `""` | http | Streamable-HTTP endpoint (must be `http(s)://`). |
| `headers` | table | `{}` | http | Extra request headers (e.g. auth). |

### `[capabilities]` (optional, additive in v1)

Declares the permissions a `stdio` addon needs. The declaration is
**secure-by-default** and *enforced* per-addon at the spawn point — it is not a
disclosure-only hint. Absent (`None`) → the addon keeps the legacy global
`addons.sandbox` behaviour, so existing manifests are unaffected.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `network` | `none` \| `full` | `none` | Outbound network. `none` → the OS sandbox blocks egress. |
| `filesystem` | `read_only` \| `read_write` | `read_only` | `read_only` → writes denied except a scratch tmp. |
| `env` | string[] | `[]` | Host environment variable names the child may receive, on top of a minimal base allowlist. Names must match `[A-Za-z0-9_]`. |
| `exec` | `none` \| `full` \| string[] | `none` | **Declared** child-process execution (disclosure + audit, *not* OS-enforced — see below). `none` → declares it spawns nothing; `full` → any binary; a string array → an allowlist of binary names/paths it may spawn (e.g. `["lean-ctx"]`). |

A present-but-empty `[capabilities]` block resolves to the strictest profile (no
network, read-only filesystem, scrubbed env, no exec). The declared block drives
two **OS-enforced** controls at `core::gateway::client`, plus one **declared +
audited** control:

1. a **per-addon OS sandbox** (`sandbox-exec` / `bwrap`) derived from
   `network` + `filesystem` — and **inherited by child processes**, so a
   subprocess the addon spawns is bound by the same egress/write limits,
2. an **environment allowlist** — the child's env is cleared and re-populated
   with the base allowlist + the declared `env` names + the addon's own
   `[mcp.env]`, so ambient host secrets never leak, and
3. `exec` — **declared, surfaced for consent, and audited** (see below); not an
   OS control.

```toml
[capabilities]
network = "full"          # talks to a remote API
filesystem = "read_only"  # never writes outside tmp
env = ["GITHUB_TOKEN"]    # may read this one host variable
exec = ["lean-ctx"]       # may spawn only `lean-ctx` (e.g. callback addons)
```

#### `exec` is declared + audited, not OS-enforced

`exec` is **declared, surfaced for consent, and audited on every platform** — but
it is deliberately **not** an OS-sandbox control, for two reasons:

- **It isn't portable.** Linux `bwrap`/seccomp cannot allowlist `execve` by path
  (the filename is a pointer it can't dereference), so any "macOS-only" exec
  gating would be a guarantee we can't keep cross-platform.
- **It breaks real servers.** Path-denying `process-exec` blocks the very
  interpreter chain an addon needs to *start*: a Python/Node MCP server execs
  `env` → the interpreter (often a re-exec'd stub) before any of its own code
  runs, and a deny-all `process-exec` profile rejects all of it — so the addon
  never launches (verified: `execvp … Operation not permitted`).

Crucially, the data-safety guarantees don't depend on exec gating: the OS
sandbox **network** and **filesystem** profiles are **inherited by child
processes**, so any subprocess an addon spawns is bound by the same egress and
write restrictions — it still cannot exfiltrate or tamper. `exec` therefore earns
its keep as **honest disclosure**: an addon whose wiring shells out must declare
it (`cap_exec_underdeclared` blocks a listing that doesn't), and the user sees
the declaration at install. The audit (below) reasons about `exec` identically on
all platforms.

The capabilities the user consents to at install are recorded in
`installed.json` as `granted_capabilities`.

### `[pricing]` (optional — sellable addons, Track B)

Generalises the ctxpkg paid-artifact model to addons. Absent (`None`) → the
addon is **free**. A paid entry must clear the [paid-listing gate](#paid-listing-gate-track-b)
before it can be listed or sold.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `price_cents` | int | `0` | One-time price in the smallest currency unit. `0` = free under the one-time model. |
| `currency` | string | `usd` | 3-letter lowercase ISO-4217 code. |
| `model` | `one_time` \| `usage` | `one_time` | Billing model. `usage` is metered per tool call via the P5 usage meter. |
| `usage_price_per_1k_cents` | int | `0` | Usage model only: price per 1,000 tool calls. Required (non-zero) when `model = usage`. |

```toml
[pricing]
price_cents = 1900        # $19.00 one-time
currency = "usd"
# or, usage-metered:
# model = "usage"
# usage_price_per_1k_cents = 200   # $2.00 per 1,000 tool calls
```

The artifact-side model lives in `core::addons::commerce`. Payment **execution**
(checkout, 402 download gating, Stripe Connect publisher payouts — GL #532)
reuses the live ctxpkg billing rails, generalised to `artifact_type = addon` in
the billing service.

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
3. **Assess + disclose** — statically review the `[mcp]` wiring for risk signals
   (remote endpoint, shelling out, unpinned upstream, secret-bearing env), print
   the trust tier, the exact transport/command/args/env (or url/headers), and any
   findings.
4. **Gate** — enforce the global-only `[addons]` install policy (see below).
   A blocked addon never reaches the next step.
5. **Confirm** — require confirmation (`--yes`/`-y` to skip; refuses
   non-interactively without it, per [`cli::prompt`]).
6. **Wire** via `Config::update_global` (the safe, global-only persistence path):
   set `gateway.enabled = true` if it was off, then upsert a `[[gateway.servers]]`
   entry named after the addon (idempotent — replaces any same-named entry).
7. **Record** in `<data_dir>/addons/installed.json` (`name`, `version`, `source`,
   `gateway_server`, `granted_capabilities` when the manifest declared a
   `[capabilities]` block, and `content_hash` — the integrity lock over the
   installed wiring) and invalidate the gateway catalog cache.

`lean-ctx addon remove <name>` reverses 4–5: drop the gateway server it owns and
the store entry. It leaves `gateway.enabled` untouched (disable explicitly with
`lean-ctx config set gateway.enabled false`).

### State vs. config

The live `[[gateway.servers]]` block in `config.toml` is the single source of
truth for what actually runs. `installed.json` is bookkeeping only — it maps an
addon to the gateway server it installed so `remove` unwinds exactly what `add`
wired. Deleting it never affects running servers.

## Security model

An addon is **executable trust**: a `stdio` addon spawns a child process with
your privileges; an `http` addon sends context to a remote endpoint; and every
addon's tool output flows into the model context (a prompt-injection surface). An
addon is as powerful as a VS Code extension or an npm package, so lean-ctx treats
installing one as a consequential, disclosed, policy-gated action.

### Baseline (always on)

- The gateway is **global-only** and **opt-in**; a project-local config can never
  point it at arbitrary commands.
- `add`/`remove` are consequential writes: they disclose the wiring and require
  confirmation — never silent.
- The bundled registry is **curated** and compiled into the binary (no live
  fetch). `addon add <path>` on a local manifest is explicit and operator-driven.
- Output is deterministic and local-only: no network calls, no telemetry in the
  add/list/search/info/remove paths.

### Trust tier

`addon.verified` splits the catalog into **verified** (maintainer-audited) and
**community** (installable, unaudited). The tier is shown in `addon list`,
`addon info` and the install preview, and on the website. It is set by the
registry, never self-asserted (see the field table).

### Static risk assessment

Before install, `core::addons::trust::assess` inspects the `[mcp]` wiring and
surfaces findings at three severities:

| Severity | Examples |
|----------|----------|
| `danger` | HTTP/remote endpoint, non-HTTPS url, inline shell (`sh -c`), fetch-and-exec (`curl`) |
| `warn` | shell metacharacters in args, unpinned package runner (`npx`/`uvx` without a version), `latest` tag |
| `info` | passes environment variables / request headers |

The same function backs the **registry CI validator**
(`core::addons::registry::validate_entries`): every bundled entry must have a
unique slug, installable entries need author/homepage/license/description and
must not shell out, fetch-and-exec, use a non-HTTPS endpoint or pull an unpinned
upstream, and **verified** entries must be free of any `warn`/`danger` finding.

### Install policy floor — `[addons]`

A **global-only** config block (never merged from a project-local file; ship it
via MDM / config-management or pin it through the signed org-policy floor). Fully
permissive by default.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `policy` | `open` \| `verified_only` \| `allowlist` \| `locked` | `open` | What may be installed. `verified_only` requires the verified tier; `allowlist` restricts to `addons.allowlist`; `locked` disables installs. |
| `allowlist` | string[] | `[]` | Permitted slugs when `policy = allowlist`. |
| `require_signature` | bool | `false` | Honour a user-override registry only if signed by a trusted org key. |
| `sandbox` | `off` \| `auto` \| `strict` | `off` | Legacy global sandbox for addons **without** a `[capabilities]` block (see below). |
| `block_risky` | bool | `false` | Refuse to install an addon that has a `danger` finding. |
| `enforce_capabilities` | bool | `false` | Fail closed when an addon declares restricted `[capabilities]` but no OS sandbox launcher is available to honour them. Off → best-effort (warn + run). |
| `metering` | bool | `true` | Record per-addon / per-tool gateway usage to `<data_dir>/addons/usage.json` (local; analytics + billing base). |

`core::addons::policy::gate` enforces this in `install` before any gateway
mutation, so a blocked addon never touches `config.toml`.

### Registry signing

The bundled registry is trusted by construction. The risk surface is a
**user-override** registry (`<data_dir>/addon_registry.json`), which can shadow
trusted names. With `require_signature = true`, the override is honoured only if a
sidecar `addon_registry.json.sig` carries a valid Ed25519 signature **by a
trusted org key** — the same pinned-key anchor as the signed org-policy floor
(`policy org trust`). An unsigned/invalid/untrusted override is ignored (warned),
falling back to the bundled catalog.

### Sandboxing

lean-ctx wraps each spawned stdio server in an OS-native sandbox at the single
spawn point (`core::gateway::client`): `sandbox-exec` (macOS) or `bwrap` (Linux).
Two paths, one mechanism:

- **Per-addon capabilities (preferred).** When the manifest declares a
  `[capabilities]` block, the sandbox profile + environment allowlist are derived
  from exactly that declaration (secure-by-default). `network = none` blocks
  egress; `filesystem = read_only` makes the filesystem read-only except a
  scratch tmp; the env is scrubbed to the base allowlist + declared `env`. If a
  restrictive profile cannot be enforced because no launcher is available, the
  spawn fails closed only when `addons.enforce_capabilities = true`, otherwise it
  warns and runs.
- **Legacy global mode.** For addons **without** a `[capabilities]` block,
  `addons.sandbox = auto|strict` applies the historical global control: `auto`
  blocks outbound network; `strict` also makes the filesystem read-only and
  **refuses to spawn** if no launcher exists. Off by default — zero behavioural
  change unless enabled.

Both paths share the plugin environment allowlist, so addon and plugin
subprocesses converge on one trust model.

### Capability audit + publish gate (`core::addons::audit`)

`assess` answers *what does the wiring do?*; `audit` answers the two questions
that gate **listing** and **paid** entries (the mandatory gate before any paid
listing). It composes three checks into one deterministic report:

1. **Wiring risk** — every `assess` finding (remote endpoint, shell-exec,
   unpinned upstream, …).
2. **Capability coherence** — does the declared `[capabilities]` match the
   wiring? An addon that performs network I/O (HTTP transport, or a stdio
   `command` that fetches/runs remote code) but declares `network = none` is
   **under-declaring** (`cap_net_underdeclared`, blocking). Declaring `full`
   when the wiring shows no network use is a least-privilege hint
   (`cap_net_overdeclared`, info). The same applies to `exec`: a wiring that
   shells out / fetch-execs but grants no `exec` capability is
   `cap_exec_underdeclared` (blocking); a blanket `exec = full` with no static
   subprocess evidence is `cap_exec_overdeclared` (info — an explicit allowlist
   is never flagged, since runtime spawning such as a callback into
   `lean-ctx call` is invisible to a static check).
3. **Malware heuristics** — content scan of command/args/env-values/url for
   pipe-to-shell (`… | sh`), base64-decode→exec, persistence writes (shell rc /
   launch-agent / cron paths), and embedded encoded blobs. This is the check the
   ctxpkg `trust_report` lists as `skipped` today.

The findings fold into one **verdict**:

| Verdict | Meaning |
|---------|---------|
| `pass` | No risk findings — eligible for the verified/paid tier. |
| `review` | Legitimate but high-capability (remote endpoint, unpinned) — installable, needs human review before verified/paid. |
| `fail` | A blocking finding — malware heuristic, under-declared capability, shell/fetch-exec, or non-HTTPS. Must not be listed. |

**Paid/verified eligibility** (`paid_eligible`) requires *all* of: a `pass`
verdict, a declared `[capabilities]` block, capabilities coherent with the
wiring, and — for stdio — a pinned `sha256` binary. The registry validator
(`validate_entries`) enforces the blocking subset for every installable entry and
requires a `verified` entry to be finding-free. Run it ad hoc with
`lean-ctx addon audit <name|path>` (non-zero exit on `fail`).

### Paid-listing gate (Track B)

The mandatory gate before an addon may be **listed or sold for money**
(`core::addons::commerce::paid_listing_gate`). It is a no-op for free addons; for
a `[pricing]` entry that charges, it requires *all* of:

1. **Audit paid-eligible** — the `paid_eligible` conditions above (clean audit,
   declared + coherent capabilities, pinned stdio binary).
2. **Verified publisher** — `addon.verified` (the curated, vouched tier, #516).
3. **Well-formed pricing** — valid ISO-4217 currency; a `usage` entry sets a
   non-zero per-1k rate.

`validate_entries` enforces this gate, so the registry can never carry a paid
listing that has not cleared it. `lean-ctx addon audit` prints the gate result
and, when blocked, the exact remaining blockers. This is the artifact-side half
of paid distribution; payment execution + Connect payouts (#532) live in the
billing service.

### Integrity lock (lockfile + re-verify)

`installed.json` doubles as a lockfile: install pins a `content_hash` of the
exact gateway wiring (transport/command/args/env/url/headers/capabilities).
`lean-ctx addon verify` (`core::addons::integrity`) re-computes the hash from the
live `[[gateway.servers]]` config and reports drift — a swapped command, an extra
arg, or a widened capability after install is caught (`DRIFT`, non-zero exit).
This is the positive-integrity counterpart to the revocation deny-list. Pulling a
newer signed version (the "updater") reuses the ctxpkg remote rails.

### Revocation / kill-switch

A revocation immediately **blocks an addon from running**, without waiting for the
user to uninstall it. `core::addons::revocation` enforces it at three points:

1. **install** — a revoked addon refuses to install,
2. **gateway catalog build** — a revoked server is dropped (its tools disappear,
   with a surfaced `revoked — <reason>` error),
3. **every proxy call** — a call to a revoked server is refused.

The local list lives at `<data_dir>/addons/revocations.json` (managed by
`lean-ctx addon revoke <name> [--reason …] [--version X]`). An unpinned entry
blocks all versions; a `--version`-pinned entry blocks only that version. An org
revocation feed layers in through the same signed-override trust anchor as the
registry (verified before it can block). `lean-ctx addon unrevoke <name>` lifts a
local revocation.

### Usage metering (`core::addons::meter`)

Every gateway proxy call ([`crate::core::gateway::proxy`]) is attributed to its
owning server and tool and counted in `<data_dir>/addons/usage.json`
(`{ servers: { <name>: { calls, errors, tools: { <tool>: { calls, errors } } } } }`).
A transport failure or a downstream `is_error` counts as an error. Metering is a
**side-channel** — it never alters the proxied tool output, so output determinism
(#498) holds — and is local-only, controlled by `addons.metering` (default on).
It is the honest basis for marketplace "most-used" discovery, builder analytics
and usage-metered billing (Track B). Surfaced via `lean-ctx addon usage`.

### Runtime redaction + audit

Downstream tool output is untrusted content. Before it reaches the model,
`core::addons::runtime::scrub_output` runs it through the same secret redaction as
the shell layer and records an audit trace tagging the bytes as untrusted,
attributed to the originating server.

### Reporting a malicious addon

Open a confidential issue on the tracker or email the maintainers. We can pull an
entry from the registry (a release ships the curated catalog) and, for a
published endpoint, advise affected users to `lean-ctx addon remove <name>`.

## CLI surface

| Command | Effect |
|---------|--------|
| `lean-ctx addon list` | Installed addons + the registry. |
| `lean-ctx addon init [name] [--http] [--force]` | Scaffold a `lean-ctx-addon.toml` in the cwd. |
| `lean-ctx addon registry validate [path]` | Validate a registry file (or the installed registry) against the security + quality bar. |
| `lean-ctx addon search [query]` | Search the registry (empty = all). |
| `lean-ctx addon categories` | Browse the registry by category (with counts). |
| `lean-ctx addon usage` | Per-addon / per-tool call counters from the meter. |
| `lean-ctx addon info <name\|path>` | Details + MCP wiring for one addon. |
| `lean-ctx addon add <name\|path> [-y]` | Install (registry or local manifest). |
| `lean-ctx addon remove <name> [-y]` | Uninstall. |
| `lean-ctx addon audit <name\|path>` | Publish/list gate: wiring risk + capability coherence + malware heuristics (non-zero exit on `fail`). |
| `lean-ctx addon verify` | Re-check installed wiring against its integrity lock. |
| `lean-ctx addon revoke <name> [--reason …] [--version X]` | Kill-switch: block an addon from running. |
| `lean-ctx addon unrevoke <name> [-y]` | Lift a revocation. |
| `lean-ctx addon revocations` | List active revocations. |
