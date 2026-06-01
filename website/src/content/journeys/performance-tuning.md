LeanCTX is fast by default, but on a huge monorepo, a constrained CI runner, or a low-RAM laptop you may want to bound how much disk/RAM it uses or find what's slow. This journey covers the memory profile, the index/cache caps, and the slow-command log — with the exact knobs and their defaults.

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
memory_profile = "balanced"     # balanced | performance | conservative
```

```bash
# or per-process, no config edit:
LEAN_CTX_MEMORY_PROFILE=conservative lean-ctx serve --daemon
```

Reach for `conservative` on small CI runners or low-RAM machines; `performance`
trades disk for speed on a workstation.

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

When the cap is hit, LeanCTX tells you exactly how to react (raise the cap or
add ignore patterns). If an index is oversized or corrupt, reclaim it with
`lean-ctx cache prune` — the next read rebuilds a clean one.

---

## 3. Bounding the code graph

```toml
graph_index_max_files = 0        # 0 = unlimited; set a cap on giant monorepos
```

On a very large tree, capping `graph_index_max_files` keeps graph builds fast and
bounded; when the limit is reached, LeanCTX prints
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

LeanCTX records commands that exceed `slow_command_threshold_ms` (default
`5000`) so you can see where wall-clock time goes:

```toml
slow_command_threshold_ms = 5000
```

```bash
lean-ctx slow-log list      # show recorded slow commands
lean-ctx slow-log clear     # reset the log
```

Pair this with `lean-ctx gain --deep` (cost + heatmap) and `lean-ctx ghost`
(uncompressed-command waste) from [Journey 11](/docs/journeys/analytics-and-insights/) to
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
