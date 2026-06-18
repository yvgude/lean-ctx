# Journey 7 — Context Engineering & Observability

> You want to actively *manage the context window itself*: see what's in it,
> measure cost, decide what to keep or evict, plan a budget, and reach for the
> deeper "power" tools. This journey documents the advanced and meta tools that
> don't belong to a single everyday flow — the ones that make lean-ctx a context
> *runtime*, not just a compressor.

Most tools here are in the **power** profile. Enable them with
`lean-ctx tools power`, or load just one category at runtime with
`ctx_load_tools` (see §6).

Source files referenced here:
- `rust/src/tools/registered/ctx_radar.rs`, `ctx_metrics.rs`, `ctx_cost.rs`,
  `ctx_feedback.rs`, `ctx_verify.rs`, `ctx_proof.rs`
- `rust/src/tools/ctx_control.rs`, `ctx_plan` / `ctx_compile` / `ctx_ledger`
- `rust/src/tools/ctx_preload.rs`, `ctx_prefetch.rs`, `ctx_dedup.rs`,
  `ctx_compose.rs`, `ctx_fill`
- `rust/src/cli/context_cmd.rs`, `ledger_cmd.rs`

---

## 1. See what's in the context — observability

Before managing context, you measure it.

| Tool | CLI | What it answers |
|------|-----|-----------------|
| `ctx_radar` | — | Full budget breakdown: prompt, messages, tools, reads, shell |
| `ctx_metrics` | `lean-ctx stats` | Session token stats, cache hit-rates, per-tool savings |
| `ctx_context` | — | Session-context overview: cache, files seen, current state |
| `ctx_cost` | `lean-ctx gain --cost` | Local cost attribution per agent / per tool |
| `ctx_heatmap` | `lean-ctx heatmap` | File-access heatmap (hot vs. cold files) |

```text
ctx_radar format=display      # human-readable context budget
ctx_radar format=json         # machine-readable, for dashboards
```

`ctx_radar` is the single best "where are my tokens going?" view: it attributes
the live context window across system prompt, message history, tool schemas, file
reads, and shell output. Pair it with `ctx_metrics` for cumulative savings.

---

## 2. Context Field Theory — actively shape the window

lean-ctx models the context window as a *field* you can manipulate with overlays
(exclude, pin, prioritize) rather than only react to.

### `ctx_control` / `lean-ctx control`

Overlay-based manipulation. Overlays apply at a `scope` (`call`, `session`, or
`project`) and are reversible.

```bash
lean-ctx control pin src/auth.rs --reason "active task"
lean-ctx control exclude vendor/ --scope session
lean-ctx control set_priority src/main.rs --value high
lean-ctx control list            # current overlays
lean-ctx control history         # what changed and why
lean-ctx control reset           # drop overlays
```

Actions: `exclude`, `include`, `pin`, `unpin`, `set_view`, `set_priority`,
`mark_outdated`, `reset`, `list`, `history`.

### `ctx_ledger` / `lean-ctx ledger` — pressure management

The ledger tracks per-file context "pressure" (token cost vs. recency vs. use)
and lets you evict the expensive, stale entries.

```bash
lean-ctx ledger status           # pressure table
lean-ctx ledger evict big.json large.log
lean-ctx ledger prune            # drop low-value entries
lean-ctx ledger reset
```

### `ctx_plan` / `lean-ctx plan` — budget a task up front

Phi-scored planning: given a task and a token budget, it allocates the budget
across the files/symbols most worth loading.

```bash
lean-ctx plan "add OAuth login" --budget=4000
```

MCP `profile`: `ultra_lean` | `balanced` | `forensic`.

### `ctx_compile` / `lean-ctx compile` — fill the budget optimally

Knapsack + Boltzmann view-selection: compiles the actual context to send under a
budget, choosing per-file *views* (handles, compressed, or full).

```bash
lean-ctx compile --mode=compressed --budget=6000
```

Together these form a pipeline: **radar** (measure) → **plan** (allocate) →
**compile** (materialize) → **control/ledger** (adjust).

---

## 3. Proactive context — load before you're asked

| Tool | What it does |
|------|--------------|
| `ctx_overview` | Task-relevant project map (Journey 3) |
| `ctx_preload` | Load task-relevant files now; compact L-curve summary (~50–100 tok) |
| `ctx_prefetch` | Predictive prefetch of blast-radius files for changed files |
| `ctx_compose` | One call: keywords + ranked files + matches + top symbol inline |
| `ctx_fill` | Fill remaining budget with the most coverage-effective files |
| `ctx_dedup` | Detect (and optionally remove) duplicated content across files |

```text
ctx_preload task="refactor the auth module"
ctx_compose task="where is rate limiting enforced?"
ctx_prefetch changed_files=["src/auth.rs"] budget_tokens=3000
ctx_dedup action=analyze        # then action=apply to reclaim
```

`ctx_compose` is the highest-leverage everyday power tool: it replaces the
typical search → read → outline → read chain (3–5 calls) with one rich response.

**Golden output — the compact search primitive.** `ctx_compose` builds on
`ctx_search`, whose results are deliberately terse — a header plus one line per
hit (`path:line code`), so a match costs a handful of tokens instead of pages of
grep context:

```text
1 matches in 805 files:
hooks/mod.rs:153 pub fn refresh_installed_hooks() {
```

`ctx_compose` then ranks the surrounding files and inlines the top symbol, so the
agent gets the answer — not just the location — in the same call.

---

## 4. Advanced reads & symbols

Beyond `ctx_read` (Journey 2):

| Tool | What it does |
|------|--------------|
| `ctx_smart_read` | Auto-pick the optimal read mode for a file |
| `ctx_symbol` | Read just one named symbol block (fn/struct/class) |
| `ctx_outline` | List all symbols of a file with signatures |
| `ctx_retrieve` | Fetch the uncompressed original from cache (CCR) |
| `ctx_compress_memory` | Compress memory/config files (CLAUDE.md, .cursorrules) |
| `ctx_expand` | Zero-loss retrieval of an archived tool output by id |

`ctx_expand` is the escape hatch: any large tool output that was archived can be
fully recovered later (`retrieve`, `list`, `search_all`) — nothing is ever lost,
only deferred.

---

## 5. Execution, workflows & intent

| Tool | What it does |
|------|--------------|
| `ctx_execute` | Sandboxed code execution (11 languages); only stdout enters context |
| `ctx_workflow` | Workflow state machine with evidence tracking |
| `ctx_intent` | Structured intent input with a routing policy |
| `ctx_response` | Compress LLM response text (strip filler, TDD) |

```text
ctx_execute language=python code="print(sum(range(100)))"
ctx_workflow action=start name=release spec=...
ctx_workflow action=transition to=verify
```

`ctx_workflow` enforces an evidence-tracked state machine (e.g. plan → implement
→ verify → ship), so an agent can't claim "done" without recorded evidence.

---

## 6. Dynamic tool loading — keep the surface small

You don't need all 77 tools loaded to use one. Lazy clients (and `minimal`/
`standard` profiles) reach deeper tools on demand:

```text
ctx_discover_tools query="impact analysis"   # find tools by keyword
ctx_call name=ctx_impact arguments={...}      # call any tool by name
ctx_load_tools action=load category=arch      # load a category at runtime
ctx_load_tools action=list                    # what's loaded
```

Categories: `arch`, `debug`, `memory`, `metrics`, `session`. This is how you keep
per-call overhead low (small visible tool list) without losing access to the full
runtime.

---

## 7. Verification & proofs (CI / audit)

| Tool | CLI | What it does |
|------|-----|--------------|
| `ctx_verify` | `lean-ctx verify` | Verification observability + ContextProofV2 |
| `ctx_proof` | `lean-ctx proof` | Export machine-readable ContextProofV1 |
| `ctx_feedback` | — | Record LLM output tokens + latency for harness feedback |
| `ctx_benchmark` | `lean-ctx benchmark` | Benchmark compression modes for a file/project |
| `ctx_analyze` | — | Entropy analysis → recommends the optimal compression mode |

```bash
lean-ctx benchmark run            # compare read modes on this repo
lean-ctx benchmark compare        # vs. naive baseline, write a report
lean-ctx verify --format both
lean-ctx proof export             # ContextProof artifact for audit
```

These exist so the savings are **provable**, not just claimed — useful in CI to
assert a context budget, or for an audit trail (`lean-ctx audit`).

---

## UX notes captured during this walkthrough

- This layer is genuinely advanced; it's gated behind the `power` profile on
  purpose so new users aren't overwhelmed. The `radar → plan → compile` pipeline
  is the through-line that makes the CFT tools coherent rather than a grab-bag.
- `ctx_compose` deserves promotion: it's the one power tool worth using daily, so
  it's also cross-linked from Journey 2.
