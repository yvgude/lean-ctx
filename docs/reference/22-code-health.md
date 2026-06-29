# Journey 22 — Code Health: Clean Code as a Token-Cost Lever

> You ship with an AI agent every day and your provider bill keeps climbing. A
> big, quiet driver is code the agent must re-read to understand — tangled,
> cryptically named, tightly coupled functions that get loaded and reasoned about
> on every turn that touches them. This journey covers the Code Health Engine:
> the navigability score, the quality tax in USD, and every surface that turns
> code health into a measurable token-cost lever.

Source files referenced here:
- `rust/src/core/code_health/` — the engine: `score.rs` (navigability + USD tax),
  `cognitive.rs` (S3776), `naming.rs`, `coupling.rs`, `analyze.rs`, `annotate.rs`,
  `delta.rs`, `gate.rs`, `scan.rs`, `persist.rs` (`health.json`), `fabric.rs`
  (BM25 / graph / knowledge fan-out + pruning)
- `rust/src/tools/ctx_quality.rs`, `rust/src/tools/registered/ctx_quality.rs` — the MCP tool
- `rust/src/cli/health_cmd.rs` — the `lean-ctx health` command
- `rust/src/core/gain/gain_score.rs` — the `navigability` gain component
- `rust/src/hook_handlers/edit_health.rs` — the native-edit PostToolUse notice
- `rust/src/core/config/sections.rs` — `CodeHealthConfig`

---

## 0. The principle

Clean code is cheaper for a model to read for the same reason it is cheaper for a
human: less to hold in working memory. LeanCTX already compresses *how* code
reaches the model; the Code Health Engine attacks the *intrinsic* cost of the
code itself. It reuses the same tree-sitter AST (18 languages) as the rest of
code intelligence, so the score is computed **once per index build** and read
(never recomputed) everywhere else.

---

## 1. The signals → one navigability score

The navigability score (0–100) rolls up three AST-grounded signals:

| Signal | What it measures |
|--------|------------------|
| **Cognitive complexity** | How hard a function is to *follow* — nesting, breaks in linear flow, boolean tangles. SonarSource's `S3776`, not cyclomatic count. |
| **Naming quality** | Cryptic / single-letter / meaningless identifiers that force re-reads to infer intent. |
| **Module coupling** | Afferent / efferent coupling and instability — how entangled a file is with the repo. |

A function whose cognitive complexity crosses the threshold (default `15`) is a
**hotspot**. The engine also estimates a **quality tax in USD** — the recurring
token cost of the hotspots, priced with the same model-pricing table the gain
report uses.

---

## 2. Compute once, fan out everywhere

`persist.rs` writes `health.json` next to the graph index and recomputes only
when the indexed source set changed (fingerprint-gated — a no-op touch never
rescans). `fabric.rs` then weaves the result into the long-term stores as a
**replace-source**:

```
top hotspots  → BM25 chunks + knowledge facts (searchable, recallable)
every over-threshold fn → property-graph `health_hotspot` edge (cc = edge weight)
```

Stale signals are pruned on every refresh, so a fixed hotspot disappears instead
of lingering. This is what lets `ctx_semantic_search` find hotspots and
`ctx_callgraph` annotate a risky symbol with its complexity.

---

## 3. `ctx_quality` — the on-demand report (MCP)

```
ctx_quality action=report                  # whole-project score + hotspots + USD tax
ctx_quality action=file path=src/auth.rs   # one file, function by function
ctx_quality action=delta                   # health change vs the last baseline
ctx_quality action=report format=json      # machine-readable
```

Read-only, in the **standard** profile — it never costs a write-permission prompt.

---

## 4. `lean-ctx health` — the terminal & CI command

```bash
lean-ctx health                 # navigability score + top hotspots + quality tax
lean-ctx health src/            # scope to a path
lean-ctx health --json          # machine-readable
lean-ctx health --gate          # non-zero exit if the project is over its floor
```

`--gate` makes "don't let the codebase get less navigable" a scriptable CI line,
the same way `doctor overhead --gate` guards the context budget.

---

## 5. Read annotations & the edit-gate

In `signatures` / `map` reads, over-threshold functions are annotated inline
(sparse, deterministic):

```
fn process_request(...)            · cc=23 (over)
```

Both `ctx_edit` and `ctx_patch` run a complexity-delta check before writing:

```
⚠ code-health: process_request cognitive complexity 18 → 27 (+9, over threshold 15)
```

The same advisory notice fires from the PostToolUse hook for the host's native
`Edit` / `MultiEdit`, so the signal follows the agent regardless of edit path.
The top hotspots are surfaced once in a compact **session-start block**.

---

## 6. It feeds the gain score

The gain score gains a fifth component, `navigability`, so a cleaner codebase
lifts the headline number:

```
Score: 84/100  (compression 71, cost 90, quality 76, consistency 80, navigability 84)
```

When no health data exists the score falls back to its original four-component
weighting — you are never penalised for a signal that has not been computed.

---

## 7. Configuration

All knobs live under `[code_health]`:

| Key | Default | Meaning |
|-----|---------|---------|
| `cognitive_threshold` | `15` | Complexity above which a function is a hotspot. |
| `gate` | `"warn"` | Edit-gate behaviour: `"warn"` (annotate), `"block"` (refuse clean→over-threshold), `"off"`. |
| `annotate_reads` | `true` | Inline `cc=` annotations in `signatures` / `map` reads. |
| `naming` | `true` | Run the naming-quality heuristic. |

---

## 8. Determinism

Every health output is a deterministic function of (file content, mode,
threshold) — no timestamps, counters or random elements in tool-output bodies.
That byte-stability keeps provider prompt caching (Anthropic up to 90%, OpenAI
50%) applying, so the signal adds insight without breaking the cache discount it
is meant to protect (#498).

---

## See also

- Journey 11 — Analytics, Insights & Reporting (the gain score and quality tax)
- Journey 4 — Code Intelligence (the graph/impact tools the engine shares its AST with)
- Journey 19 — Customization & Governance (`[code_health]` knobs and enforcement)
