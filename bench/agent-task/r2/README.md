# Faithful R2 lean-ctx arm

The R2 "Who Owns the Context Window?" round (Entelligentsia / tokbench, on the
Forge/forge-cli + pi runtime) is an oracle-free **planted-bug fix** task judged by
rebuild + reproduce. This directory holds the lean-ctx arm config so that
**"installed = running as designed"** — the R1 round ran lean-ctx with its
overhead defaults on, which cost a per-turn injected-prefix tax.

## What the faithful arm changes (vs R1 defaults)

| Lever | Setting | Why it matters on a phase-isolated harness |
|------|---------|--------------------------------------------|
| Zero injection | `rules_injection = off` | Drops the rule-file half of the ~3K-token per-turn prefix that R1 re-billed every turn. |
| Minimal surface | `minimal_overhead = true`, `tool_profile = minimal` | Drops the tool-schema half of that prefix (6-tool core, not the full surface). |
| Cold reads | `structure_first = true` | Biases `auto` → `map` for medium source files on a cold read (the only read saving that survives a fresh process), while capability guards keep suspect files full. |
| Shell routing | pi `mode = replace` / `routeShell = true` | Forces build/test/make output through `ctx_shell` (R1 saw 102 native bash / 0 ctx_shell — uncompressed). |
| Surface reach | `proxy_enabled = true`, `history_mode = cache-aware` | The proxy compresses the *whole* request body (incl. `forge_*` store output and native shell), with a byte-stable prefix so a cached rail keeps hitting. |

These are the three dominance vectors: **capability** (localize the defect in
fewer turns, never compress the suspect away), **surface** (proxy + shell), and
**honesty** (`lean-ctx gain` reports net-of-injection — see `meter-honest`).

## Files

- `lean-ctx.toml` — engine config. Copy to `$XDG_CONFIG_HOME/lean-ctx/config.toml`, or drop into the repo workspace as `.lean-ctx.toml`.
- `faithful-arm.env` — the same settings as env vars, plus the proxy base-URL wiring. Source it for harnesses that prefer env over files.
- `pi-config.json` — pi extension config (`~/.pi/agent/extensions/pi-lean-ctx/config.json`) for the pi/forge runtime.

## Run it (pi / forge-cli runtime — the R2 rail)

```bash
# 1. install config
mkdir -p ~/.pi/agent/extensions/pi-lean-ctx
cp bench/agent-task/r2/pi-config.json ~/.pi/agent/extensions/pi-lean-ctx/config.json
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/lean-ctx"
cp bench/agent-task/r2/lean-ctx.toml "${XDG_CONFIG_HOME:-$HOME/.config}/lean-ctx/config.toml"

# 2. start the wire-level proxy (foreground; background it for a run)
lean-ctx proxy start --port=4444 &

# 3. point the agent at the proxy
set -a; source bench/agent-task/r2/faithful-arm.env; set +a
```

## Run it (this repo's Claude harness)

`bench/agent-task` wires the `leanctx` arm purely via `lean-ctx init`
(`swebench_harness/run_arm.py`). To run it faithfully, apply the engine config
and proxy into the arm's fresh `HOME` and source `faithful-arm.env` before the
agent launches — the harness's own protocol stays unchanged.

## Verify the arm is actually faithful (preflight)

Run **before any priced run** — a green preflight is the gate devasur asked for:
it proves shell routes through `ctx_shell` (native `bash` suppressed) and is
actually compressed, not the R1 `102 native bash / 0 ctx_shell`:

```bash
node bench/agent-task/r2/preflight.mjs            # resolves the installed pi config
node bench/agent-task/r2/preflight.mjs --config bench/agent-task/r2/pi-config.json
```

It checks the binary + version, that the config suppresses native `bash`
(`mode=replace` / `routeShell` — the unit-tested `resolveSuppressedBuiltins`
invariant), the embedded bridge, the faithful overhead levers, and measures real
shell compression. Exit code 1 if any hard gate fails.

Manual spot-checks (what the preflight automates):

```bash
lean-ctx config get rules_injection   # -> off
lean-ctx config get tool_profile      # -> minimal
lean-ctx proxy status                 # -> running on :4444, compression stats
lean-ctx gain                         # -> net_tokens_saved (net of injected overhead)
```

## tokbench PR offer

devasur invited patches on #361 ("we would appreciate your patch offer if you
can send us a PR"). The integration PR to tokbench is exactly this arm:

1. add the lean-ctx arm using `pi-config.json` + `lean-ctx.toml` above,
2. start `lean-ctx proxy start` for the arm and export `faithful-arm.env`,
3. the pi-extension `routeShell` / `mode=replace` fix already lives in
   `packages/pi-lean-ctx` (it suppresses native `bash` so shell output reaches
   the compressor without the agent having to choose `ctx_shell`); ship
   `preflight.mjs` as the pre-run gate that proves it on the bench (the
   green-preflight = "running as designed" devasur asked for).

Right-of-reply framing: lean-ctx is the only **code-aware** arm (localizes +
compresses without hiding the defect), the **broadest reach** via the proxy, and
the **only meter that reconciles to the provider bill**. rtk is shell-only and
architecturally capped; headroom is a blind wire compressor that under-compresses
code/prose by default and can compress bug-relevant content away.

## Version caveat + self-verify (ready to upstream to the tokbench README)

R1 measured lean-ctx with its **overhead defaults ON** (rules + full tool surface
injected every turn) on a phase-isolated harness — the single architecture–surface
combination where an injected per-turn prefix is pure re-billed cost, and *not*
the configuration lean-ctx recommends for that workload. We propose a short
version caveat at the top of the tokbench README plus a link to this faithful R2
arm:

> **Version caveat.** The lean-ctx figures below are from R1, run with the
> round's pinned lean-ctx version and default overhead injection **on**. On a
> phase-isolated harness that re-bills the per-turn prefix every turn. The
> maintainer-supplied **faithful R2 arm** (`rules_injection=off`,
> `tool_profile=minimal`, proxy + pi `mode=replace`) targets break-even there
> and a strict win on cache-priced / long-lived rails. See: `bench/agent-task/r2`.

### Self-verify (reproducible, local, no live model)

The advantage is provable **before** the priced R2 run — deterministically, with
no live model or secrets:

```bash
lean-ctx benchmark dual-arm          # human table
lean-ctx benchmark dual-arm --json   # machine-readable + BLAKE3 digest
```

Arm A = a stateless, phase-isolated session (the R1 shape); Arm B = the
long-lived proxy rail with cache-aware, byte-stable pruning. Costs come from real
token measurements (`o200k_base`) and published per-model pricing; the run is
fingerprinted by a BLAKE3 digest, so anyone can reproduce the exact figures.

Latest local self-verify — `medium` scenario, 15 turns, digest `f5ed145e61ce3689`,
150,574 raw input tok (phase-isolated) vs 4,164 lean prefix tok (long-lived):

| Model | Cache-priced? | Input-side saved |
|-------|---------------|------------------|
| claude-opus-4.5 | yes | 99.4 % |
| claude-sonnet-4.5 | yes | 99.4 % |
| gpt-5.4 | yes | 99.5 % |
| gemini-2.5-pro | no | 97.2 % |
| fallback-blended | no | 97.2 % |

Input-side, output held equal. Cache-priced rails show a strict win; non-caching
rails still net ahead on compression + the read-cache and are never worse than
break-even.

### Refreshed maintainer statement

> lean-ctx's R1 placement reflected its *default* overhead on a phase-isolated
> harness — the one architecture–surface combination where an injected per-turn
> prefix is pure re-billed cost. The faithful R2 arm removes that prefix
> (`rules_injection=off` + minimal surface) and routes the **whole** request body
> through the proxy with a byte-stable, cache-aware prefix. Our local dual-arm
> self-verify (deterministic, digest `f5ed145e61ce3689`) shows the result: a
> strict input-side win on cache-priced, long-lived rails and at least
> break-even everywhere else. lean-ctx remains the only code-aware arm and the
> only meter that reconciles to the provider bill (net-of-injection).
