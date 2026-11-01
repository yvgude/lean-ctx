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

A terminal `ctx_shell` background status archives every non-empty output,
regardless of the generic archive threshold. When status returns an `archiveId`,
lean-ctx protects that archive from its TTL and disk-budget cleanup for at least
one hour from that response; the configured lifetime (48 hours by default)
applies afterward. If the disk budget cannot admit that guarantee, status omits
the archive identifier and reports `output archive unavailable` without a stale
size claim. `ctx_expand` accepts either the `archiveId` or the corresponding
`shell_*` job identifier.

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

The resident `ctx_search` trigram index verifies freshness against the live
filesystem on **every** lookup via a cheap corpus signature, so an edit — even
through a tool lean-ctx never sees (native editors, `git checkout`) — is
reflected on the next search. On very large *indexed* trees you can coalesce that
per-lookup stat-walk under bursty search load, trading a bounded staleness window
for fewer walks:

```bash
LEAN_CTX_SEARCH_INDEX_COALESCE_MS=1000 lean-ctx <cmd>   # default 0 = always verify
```

---

## 4. Disk / RAM / staleness budgets

These cross-cutting budgets apply across caches and indexes; `0` means "use the
profile-derived default":

| Knob | Env override | Meaning |
|------|--------------|---------|
| `max_disk_mb` | `LEAN_CTX_MAX_DISK_MB` | total on-disk budget across caches/indexes |
| `max_ram_percent` | `LEAN_CTX_MAX_RAM_PERCENT` | soft process-RSS target as % of system memory (default 5) |
| `mcp_max_rss_mb` | `LEAN_CTX_MCP_MAX_RSS_MB` | per-process RSS target of each stdio MCP server; the lower of this and `max_ram_percent` applies (default 512, `0` = percent only) |
| `daemon_max_rss_mb` | `LEAN_CTX_DAEMON_MAX_RSS_MB` | RSS target of the background daemon; the lower of this and `max_ram_percent` applies (default 2048, `0` = percent only) |
| `mcp_idle_exit_minutes` | `LEAN_CTX_MCP_IDLE_EXIT_MINUTES` | exit a stdio MCP server after N minutes without a tool call (default `0` = never) |
| `max_staleness_days` | `LEAN_CTX_MAX_STALENESS_DAYS` | auto-prune entries older than N days |

`config show` warns if `max_disk_mb` is set lower than
`archive.max_disk_mb + bm25_max_cache_mb`, so your sub-budgets can't quietly
exceed the global cap.

`max_ram_percent` drives userspace throttling and cache eviction; it is not a
kernel-enforced ceiling and RSS can temporarily exceed it. For a strict
per-process boundary, run lean-ctx in a cgroup/container with `MemoryMax` (or
equivalent). At sustained pressure, reduce `memory_profile` or provision more
RAM.

### Long-lived MCP servers

A stdio MCP server ends when its host closes stdin or exits. Some hosts do
neither: the Codex app-server keeps one `lean-ctx mcp` per loaded thread alive
for days. Two mechanisms bound what such a fleet holds:

- **Per-process ceiling.** `mcp_max_rss_mb` (512 MB) lowers the guardian's
  target for each server. The usual ladder then applies: output trimming above
  1×, index unloading above 1.5× (768 MB) and an emergency cache drop above
  2× (1 GB).
- **Idle release.** After the `memory_cleanup` TTL without a tool call (1 h
  `shared`, 5 min `aggressive`) the server drops its read cache and resident
  indexes and returns free pages to the OS. Both reload on the next call. A
  running call or background shell job keeps the server busy.

On macOS the guardian measures physical footprint, the "Memory" column in
Activity Monitor, rather than resident size, which compressed pages leave.
A live heap of roughly 65–90 MB (tokenizer tables, parsers, the tool
registry) stays after every eviction, so a cap below ~150 MB only keeps the
guardian busy. Each release logs its before and after figures
(`[mcp-idle] released: RSS … → … MB, heap … → … MB`). On macOS the footprint
falls more slowly than the heap: the allocator hands freed pages back with
`MADV_FREE`, and the kernel takes them only when it needs memory. They count
until then, but they are reclaimable. One measured release: heap 163 → 89 MB,
footprint 275 MB, falling to 225 MB over the following minute.

`mcp_idle_exit_minutes` additionally ends idle servers. It is off by default
because Codex does not restart an exited server: the resumed thread then loses
lean-ctx until the Codex app restarts. Enable it for hosts that respawn their
servers, or when you accept that trade-off.

### The background daemon

One daemon (`lean-ctx serve --_foreground-daemon`) builds the indexes for
every session and project root on the machine and runs for days. It keeps the
search index of at most eight roots resident (least recently used first out),
like the graph cache, and `daemon_max_rss_mb` (2048 MB) caps its guardian
target on large machines. After each build phase it returns freed allocator
pages to the OS; tree-sitter's parser memory goes through the same allocator,
so it is returned as well.

---

## 5. Reproducing session-store startup latency

Session lookup for a safe project root reads its bounded `project-index` first.
An absent, malformed, or stale index is repaired once by scanning the store;
the normal command path never scans unrelated session JSON.

Build the release binary, then run the isolated benchmark. It creates a temporary
Git project and data directory, seeds one indexed session plus 10,000 unrelated
JSON sessions, warms the command, and reports p50 without touching user data:

```bash
cd rust && cargo build --release
cd .. && scripts/benchmark/session-store.sh 10000
```

On the 2026-09-04 development Mac, assess `p50_ms` after warm-up against the
MES-2189 target of 200 ms or less. Set `LEAN_CTX_BIN`, `RUNS`, or `WARMUP` to
measure another built binary or sampling regime.

Session retention is separate from archive retention: `session_retention_days`
(or `LEAN_CTX_SESSION_RETENTION_DAYS`) supplies the default for explicit
`lean-ctx sessions cleanup` and `ctx_session cleanup`. Cleanup preserves the
newest session for every project root.

---

## 6. Finding what's slow — `slow-log`

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

## 7. Keeping caches lean

```bash
lean-ctx cache stats              # size + hit rate
lean-ctx cache prune              # drop oversized/quarantined/orphaned indexes
lean-ctx cache reset --project    # wipe just this project's cache
```

A healthy cache has a high hit rate (each hit is a ~13-token re-read).
`cache prune` is the safe periodic maintenance command; it never touches valid,
in-budget entries.

---

## 8. Workload fit — where lean-ctx nets out (and where it doesn't)

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

### Claude Code: re-read stubs are withheld by default

Claude Code sub-agents reuse their parent's MCP connection, so the parent and
every sub-agent talk to the **same** lean-ctx process. The only scope lean-ctx
can derive there is process-wide (`proc:`), and a match on it cannot prove the
content is in the *asking* agent's window. lean-ctx therefore fails closed:
unchanged re-reads, same-variant `map`/`signatures` re-reads and cross-agent
delivery stubs are **not** collapsed under Claude Code — each re-read returns
the (compressed) content again. Cold-read compression is unaffected.

`LEAN_CTX_SCOPE=<name>` opts back in for an integration that can guarantee one
agent per lean-ctx process (e.g. a headless loop that never spawns
sub-agents). Do **not** set it for interactive Claude Code sessions that use
sub-agents: every sub-agent would inherit the same scope and could receive a
back-reference to content it never saw.

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
