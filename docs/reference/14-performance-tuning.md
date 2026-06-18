# Journey 14 — Performance Tuning

> lean-ctx is fast by default, but on a huge monorepo, a constrained CI runner,
> or a low-RAM laptop you may want to bound how much disk/RAM it uses or find
> what's slow. This journey covers the memory profile, the index/cache caps, and
> the slow-command log — with the exact knobs and their defaults.

Source files:
- `rust/src/core/config/mod.rs` — `memory_profile`, `bm25_max_cache_mb`, `graph_index_max_files`
- `rust/src/cli/config_cmd.rs` — `config show` (effective limits)
- `rust/src/core/bm25_index.rs`, `graph_index.rs` — index caps
- `rust/src/shell/exec.rs` — `slow_command_threshold_ms`

---

## 0. See your effective limits first

Before tuning, look at what's actually in effect. `lean-ctx config show` resolves
config + env + defaults into one view and tags the **source** of each value:

```text
╭─── Simplified (high-level) ───────────────────────────────╮
│ compression_level   = Max         ← config
│ max_disk_mb         =          0  ← default
│ max_ram_percent     =          5  ← default
│ max_staleness_days  =          0  ← default
│ memory_profile      = Performance  ← default
╰────────────────────────────────────────────────────────────╯

╭─── Derived effective limits ────────────────────────────────╮
│ archive_max_disk_mb    =    500 MB
│ bm25_max_cache_mb      =    512 MB
│ archive_max_age_hours  =     48 h
│ graph_index_max_files  =      0
╰────────────────────────────────────────────────────────────╯
```

`← config` vs `← default` tells you whether a value is yours or the built-in.
`0` means "unbounded / use the derived default" (see each knob below).

---

## 1. The memory profile — one dial for the footprint

`memory_profile` sets the overall disk/RAM posture; the *derived* limits
(archive size, BM25 cache, staleness) follow from it unless you override them
individually.

```toml
memory_profile = "balanced"     # low | balanced | performance
```

```bash
# or per-process, no config edit:
LEAN_CTX_MEMORY_PROFILE="low" lean-ctx serve --daemon
```

Reach for `low` on small CI runners or low-RAM machines; `performance`
trades disk for speed on a workstation. `balanced` is in between.

---

## 2. Bounding the search index (BM25)

The BM25 full-text index is the biggest disk consumer on large repos.

```toml
bm25_max_cache_mb = 512          # cap the BM25 cache (derived from profile if unset)
extra_ignore_patterns = ["vendor/**", "*.min.js"]   # never index these
```

```bash
LEAN_CTX_BM25_MAX_CACHE_MB=256 lean-ctx index build
```

When the cap is hit, lean-ctx tells you exactly how to react (raise the cap or
add ignore patterns). If an index is oversized or corrupt, reclaim it with
`lean-ctx cache prune` — the next read rebuilds a clean one.

---

## 3. Bounding the code graph

```toml
graph_index_max_files = 0        # 0 = unlimited; set a cap on giant monorepos
```

On a very large tree, capping `graph_index_max_files` keeps graph builds fast and
bounded; when the limit is reached, lean-ctx prints
`[graph_index: reached configured limit of N files. Set graph_index_max_files = 0 for unlimited.]`
so the truncation is never silent.

To skip indexing entirely (e.g. an ephemeral CI job that only needs reads):

```bash
LEAN_CTX_NO_INDEX=1 lean-ctx <cmd>          # or LEAN_CTX_DISABLE_SEARCH_INDEX=1
```

---

## 4. Disk / RAM / staleness budgets

These cross-cutting budgets apply across caches and indexes; `0` means "use the
profile-derived default":

| Knob | Env override | Meaning |
|------|--------------|---------|
| `max_disk_mb` | `LEAN_CTX_MAX_DISK_MB` | total on-disk budget across caches/indexes |
| `max_ram_percent` | `LEAN_CTX_MAX_RAM_PERCENT` | RAM ceiling as % of system memory (default 5) |
| `max_staleness_days` | `LEAN_CTX_MAX_STALENESS_DAYS` | auto-prune entries older than N days |

`config show` warns if `max_disk_mb` is set lower than
`archive.max_disk_mb + bm25_max_cache_mb`, so your sub-budgets can't quietly
exceed the global cap.

---

## 5. Finding what's slow — `slow-log`

lean-ctx records commands that exceed `slow_command_threshold_ms` (default
`5000`) so you can see where wall-clock time goes:

```toml
slow_command_threshold_ms = 5000
```

```bash
lean-ctx slow-log list      # show recorded slow commands
lean-ctx slow-log clear     # reset the log
```

Pair this with `lean-ctx gain --deep` (cost + heatmap) and `lean-ctx ghost`
(uncompressed-command waste) from [Journey 11](11-analytics-and-insights.md) to
turn "it feels slow" into a concrete list.

---

## 6. Keeping caches lean

```bash
lean-ctx cache stats              # size + hit rate
lean-ctx cache prune              # drop oversized/quarantined/orphaned indexes
lean-ctx cache reset --project    # wipe just this project's cache
```

A healthy cache has a high hit rate (each hit is a ~13-token re-read).
`cache prune` is the safe periodic maintenance command; it never touches valid,
in-budget entries.

---

## 7. Workload fit — where lean-ctx nets out (and where it doesn't)

lean-ctx saves tokens two ways: **cold-read compression** (a single read sent
smaller) and **cached re-reads** (an unchanged file re-read collapses to a
~13-token back-reference). It also *adds* a fixed per-turn prefix — the MCP tool
schemas, the server instructions, and the rules block. Whether you net out ahead
depends on the harness and the provider:

| Factor | Nets ahead | Can cost tokens |
|--------|-----------|-----------------|
| Context lifetime | one long-lived context (agent loop, interactive session) — re-reads land in the same window, so the ~13-token stub is usable | **phase-isolated** harness (a fresh process/context per phase) — the back-reference can't resolve in a cold context, so there is no re-read dividend |
| Provider pricing | **prompt-cache-priced** (the injected prefix rides the provider cache and is billed once) | **non-caching / request-metered** — the prefix is re-sent and re-billed *every turn* |

On a phase-isolated **and** non-caching workload the cached-re-read lever has no
surface and the injected prefix is pure re-billed overhead. That is an
architecture–surface fit, not a failure mode — but you should tune for it.

### Win vs. break-even at a glance

Three independent levers decide the outcome. The savings stack when they line
up and cancel when they don't:

- **Reach** — how much of the request body lean-ctx can shrink. As a *tool layer*
  (`ctx_*` MCP tools) it only compresses its own outputs (~5 % of the window). As
  the *wire-layer proxy* (or a true context **engine** like Hermes) it compresses
  **every** `tool_result` and prunes history cache-stably (~95 %).
- **Lifetime** — *long-lived* (one agent loop / interactive context, so re-reads
  land in the same window and collapse to a ~13-token stub) vs. *phase-isolated*
  (a fresh process/context per phase, where the back-reference can't resolve).
- **Pricing** — *prompt-cache-priced* (the injected prefix rides the provider
  cache and is billed once) vs. *non-caching / request-metered* (the prefix is
  re-sent and re-billed every turn).

| Reach | Lifetime | Pricing | Verdict |
|-------|----------|---------|---------|
| engine / proxy | long-lived | cache-priced | **Clear win** — re-reads collapse, the 95 % surface is compressed, the prefix is cached once |
| engine / proxy | long-lived | non-caching | **Win** — re-read dividend + full-body compression outweigh the re-billed prefix |
| tool-only | long-lived | cache-priced | **Modest win** — cached prefix + warm `ctx_*` re-reads, but only ~5 % reach |
| tool-only | phase-isolated | non-caching | **Break-even** — no warm re-reads, ~5 % reach, prefix re-billed each turn; tune with the row below |

The honest target is therefore: **own the window** (route through `proxy enable`
or the engine), keep **one long-lived context**, and prefer a **cache-priced**
rail. Where you can't, the goal is *break-even, not a loss* — which is exactly
what the next sections tune for.

### The meter's denominator (read this before quoting `gain`)

`lean-ctx gain` measures compression on **lean-ctx-touched traffic** (the reads
and shell output it actually processed) — its denominator is that traffic, **not
your full provider bill**. It does not subtract the per-turn prefix lean-ctx
injects. So on a non-caching rail the dashboard can read net-positive while the
billed input moved net-negative. To keep this honest, `gain` now prints a
**Methodology** line and `gain --json` carries `injected_overhead_tokens_per_turn`:

```text
net bill impact ≈ tokens_saved − injected_overhead_tokens_per_turn × turns
```

### Reaching tool output the `ctx_*` tools can't wrap

The `ctx_*` tools only compress their own results — they cannot wrap the output
of *another* MCP server's tools (e.g. a host's `store`/`artifact` tools). The
**proxy** can: it sits at the provider API and compresses **every** `tool_result`
in the request body regardless of which tool produced it.

```bash
lean-ctx proxy enable        # redirect the provider base URL through lean-ctx
```

So if the heaviest token sink arrives via another server's MCP tools, route the
provider through the proxy rather than relying on the tool layer. (On a
non-caching provider the proxy shrinks what is *sent*; it cannot un-bill a prefix
the client re-sends each turn.)

### Recommended config for a phase-isolated / non-caching harness

```toml
rules_injection = "off"       # host supplies its own steering — write no rules file (#361)
```

```bash
LEAN_CTX_MINIMAL=1 lean-ctx serve --daemon     # trim the tool surface to the core set
```

Make every cold read carry its weight by defaulting to a compressing read mode
via a [persona](10-customization-and-governance.md) (`default_read_mode = "map"`
or `"signatures"`), since there are no warm re-reads to harvest. The cold-read
modes are the right lever here.

---

## Tuning checklist

| Constraint | Knob |
|------------|------|
| Low-RAM / small CI runner | `memory_profile = "conservative"` |
| Index eats too much disk | `bm25_max_cache_mb` + `extra_ignore_patterns` |
| Giant monorepo, slow graph | `graph_index_max_files = <N>` |
| No index needed at all | `LEAN_CTX_NO_INDEX=1` |
| Hard disk/RAM ceiling | `LEAN_CTX_MAX_DISK_MB` / `LEAN_CTX_MAX_RAM_PERCENT` |
| "What's slow?" | `slow-log list` + `gain --deep` |
| Reclaim space now | `cache prune` |
| Host supplies its own steering | `rules_injection = "off"` |
| Phase-isolated / non-caching harness | `LEAN_CTX_MINIMAL=1` + persona `default_read_mode = "map"` + `proxy enable` |
| Reach another server's tool output | `lean-ctx proxy enable` |
