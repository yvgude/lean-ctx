# Adaptive Learning Layers

lean-ctx tunes itself from outcomes. Seven research-driven layers (GL #538–#544)
observe how compression, context placement and multi-agent coordination actually
perform on *your* machine — and adapt. This page explains what each layer learns,
where its data lives and how to inspect or share it.

All learning is **local-first**, bounded and clamped: research-tuned defaults stay
the anchor; learned adjustments decay back toward them when the evidence ages.

## The layers at a glance

| Layer | Learns | Store (`~/.lean-ctx/`) | Inspect |
|---|---|---|---|
| Learned thresholds (#538) | Per-file-type compression aggressiveness | `thresholds_learned.json` | `lean-ctx learning`, `ctx_metrics` |
| LITM calibration (#539) | Where wakeup facts are actually recalled from (begin vs end) | `litm_calibration.json` | `lean-ctx learning`, `ctx_metrics` |
| Stigmergy scent field (#540) | What parallel agents work on, where they got stuck | `scent_field.json` | Dashboard → Trends, `ctx_agent sync` |
| Delta playbook (#541) | Strategies, pitfalls, key files that survive checkpoints | session state | `ctx_compress` output, Dashboard |
| Query-conditioned IB (#542) | Nothing persistent — biases compression toward your active query | — | `ctx_read` entropy mode |
| Theta-gamma chunking (#543) | Nothing persistent — clusters wakeup facts into topic chunks | — | wakeup output |
| Semantic likelihood scorer (#544) | Nothing persistent — drops semantically redundant lines | — | entropy mode (needs embeddings) |

## 1. Learned compression thresholds (#538)

Every compressed read is an implicit experiment. Four outcome signals adjust a
per-extension entropy-threshold delta:

- **Bounce** (compressed read → full re-read within 5 reads): strong *back off*.
- **Edit failure** after a compressed read: strongest *back off*.
- **Clean compressed read**: gentle *compress more*.
- **Wasted full read** (large full read of a never-bouncing type): *compress more*.

Deltas are clamped to ±0.15, decay 2% daily toward zero and only apply after 10
observations per extension. Result: `.md` files that keep bouncing get gentler
compression on *your* machine; generated `.json` that nobody re-reads gets more.

```
$ lean-ctx learning
Learned compression thresholds:
  .rs: delta +0.041 (27 signals) — compresses more
  .md: delta -0.060 (11 signals) — backs off
```

## 2. LITM placement calibration (#539)

"Lost in the middle" placement (task at the end, anchors at the begin) ships with
research defaults. The calibration layer measures where *your* client's recalls
actually hit — every explicit `ctx_knowledge recall` that matches a wakeup
manifest entry scores its position — and shifts the begin/end budget share
accordingly (clamped to 35–85%).

## 3. Stigmergy scent field (#540)

Parallel agents coordinate indirectly, like ant pheromones: deposits of
`CLAIMED`, `DONE`, `STUCK`, `HOT`, `AVOID` on files/tasks, with per-kind
exponential decay (10–60 min half-life).

- `ctx_agent claim <path>` — claim a work target; second agent gets a rejection
  with holder + age. Rejected claims are counted as **prevented duplicate work**.
- `ctx_agent release <path>` — release early.
- `ctx_agent sync` — see the live field.
- `ctx_read` shows `[scent: claimed by …]` hints on foreign-claimed files.

Identity: explicitly registered agents use their registered ID; unconfigured
processes get a PID-distinct identity (`local-12345`), so two Cursor windows on
the same machine genuinely see each other (#547).

## 4. Delta playbook (#541)

Checkpoints (`ctx_compress`) no longer re-summarize prior summaries (the ACE
"context collapse" failure mode). Instead the session distills into itemized
entries with stable IDs — `Strategy`, `Pitfall`, `Fact`, `FileRef` — that are
only appended, confirmed (dedup by token-Jaccard), voted and locally evicted.
Resumed sessions replay the playbook instead of a lossy prose summary.

## 5–7. Query-aware compression (#542, #543, #544)

- **#542**: entropy-mode compression fuses token entropy with an IDF-weighted
  relevance score against your active task / latest semantic query.
- **#543**: wakeup facts render as topic-clustered chunks (theta–gamma model:
  ~4 items per chunk), saving tokens and improving recall structure.
- **#544**: with the embedding engine active, near-duplicate lines are dropped
  by cosine similarity against a sliding window of kept lines (MMR-style).

## Embeddings: self-activating (#551)

Semantic features need a local ONNX embedding model (~30–90 MB). On the first
semantic need lean-ctx downloads it **in the background** (TOFU SHA-256 pinned,
see `docs/guides/custom-embeddings.md`) and warms the engine — no hot path ever
blocks. Opt out for air-gapped machines:

```toml
[embedding]
auto_download = false
```

or `LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD=0` (env wins in both directions).
`ctx_metrics` always shows the engine status and the reason if it is off.

## Sharing learning with your team (#550)

Learning state is shareable as a secret-free JSON bundle (file extensions,
client profiles and aggregate numbers only — no paths, no content):

```
$ lean-ctx learning export team.json     # on the experienced machine
$ lean-ctx learning import team.json     # on the new machine
```

Merge semantics are double-count-safe and idempotent:

- threshold deltas: **sample-weighted average**, clamps enforced;
- LITM counters: **element-wise maximum**.

Re-importing the same bundle is a no-op, so bundles can be committed to a repo
or distributed via CI without drift.

## Proving it works (#549)

`ctx_metrics` carries a **Learning Efficacy** section, and the dashboard
(Trends page) shows the same evidence:

- bounce rate week-over-week (from the signed savings ledger),
- LITM placement hit-rate movement (30-day snapshot ring),
- playbook survival (aged entries still net-helpful),
- duplicate work prevented (rejected claims).

If a learning layer does not move its metric, it gets retuned or removed — the
layers earn their place with evidence, not theory.

## Cognition v2 — science-grounded subsystems

A second wave of layers models the *context lifecycle itself* on neuroscience and
physics. Unlike the adaptive layers above (which tune compression), these govern
what stays in working context, how salience decays, and what is admitted from
external sources. **All are deterministic by default** so tool output stays
byte-stable (prompt-cache contract / Rule #498); probabilistic exploration is
opt-in via `LEAN_CTX_STOCHASTIC=1`.

| Subsystem | Science | What it does | Key config |
|---|---|---|---|
| Time-variant Φ | Attention salience | Recomputes + EMA-blends context Φ on every re-read instead of freezing it | — |
| Power-law decay | Ebbinghaus + spacing | Knowledge confidence decays `R = exp(-Δt/S)`, `S` grows per retrieval | `forgetting_model`, `base_stability_days`, `LEAN_CTX_LIFECYCLE_FORGETTING` |
| Hebbian eviction | "Fire together, wire together" | Co-accessed cache entries protect each other from eviction | — |
| CLS consolidation | Complementary learning systems | Replay lifts confidence of related, frequently-retrieved facts | — |
| Integration-aware Φ | IIT non-redundancy (MMR) | Greedy MMR selection + **content**-based dedup (not paths) | — |
| Global-workspace ignition | Global Workspace Theory | High-Φ outliers are broadcast/pinned, resist downgrade | `LEAN_CTX_GWT_IGNITION_Z` |
| Learned field weights | Reinforcement learning | Bandit picks Φ weights — argmax-of-mean by default, Thompson under flag | `LEAN_CTX_STOCHASTIC` |
| Idle replay | Sharp-wave-ripple replay | A quiet gap triggers a deeper background consolidation pass | `LEAN_CTX_COGNITION_IDLE_SECS` |
| FEP prefetch | Active inference / free energy | Surfaces likely-next co-accessed files as a warmup hint (never auto-reads) | — |
| Immune detector | Artificial immune system | Screens external provider data for injection/poisoning before ingest; stricter for untrusted workspaces | coupled to Workspace Trust |
| Observation synthesis | Entity-summary memory (Hindsight) | Distils per-entity fact clusters into deterministic, recall-prioritized observation summaries | `cognition_synthesis_min_cluster`, `cognition_loop_max_steps` |

### Proving they are active

Every subsystem ticks a shared activity registry at its real call site. Inspect
what is wired and what has actually fired this session:

```
$ lean-ctx introspect cognition
Cognition subsystems: 8/12 active (12 wired)

  [active] Sticky-Phi fix             count=42   last=3s ago   time-variant salience (attention)
  [active] Immune detector            count=2    last=1m ago   artificial immune system
  [idle  ] QUBO selection (spike)     count=0    last=never    quantum-inspired optimization
  ...
```

`lean-ctx doctor` summarizes the same (`Cognition  8/12 subsystems active`).
Add `--json` for machine-readable output.

### Observation synthesis (entity summaries)

Inspired by [Hindsight](https://github.com/vectorize-io/hindsight)'s *observation
network*, the loop's 9th step distils clusters of related facts into compact,
per-entity **observations** — a synthesized orientation layer over the raw store.

- **Epistemic typing (evidence vs. inference).** Every fact is typed by archetype
  on write (`infer_from_category`), separating objective *evidence* (architecture,
  dependency, convention, gotcha, fact) from *inference* (decision, preference,
  observation). Typing already feeds salience ranking, and — opt-in via
  `archetype_aware_decay` — lets structural evidence decay slower than inference on
  the Ebbinghaus curve.
- **Deterministic synthesis.** Facts are grouped by an entity anchor (a file path
  in the key/value, else the category); each cluster of ≥
  `cognition_synthesis_min_cluster` (default 3) facts becomes one observation
  written through the normal `remember()` path — so versioning, persistence, and
  idempotency come for free (unchanged facts → confirmation; changed → supersede).
  The value is a stable function of the source content (no timestamps/counters), so
  hot-path recall stays byte-stable (#498). An optional LLM refinement sits behind
  `llm.enabled`; the deterministic digest is always the fallback.
- **Recall priority.** A relevant synthesized observation gets a *balanced* recall
  boost — above incidental matches, but below an exact key hit — so a stale summary
  never buries a precise raw fact.

Synthesis runs as step 9, active only when `cognition_loop_max_steps >= 9` (the new
default; set 8 to disable). Activity shows as `observation_synthesis` in
`lean-ctx introspect cognition`.

### QUBO selection (research spike)

Context selection under a token budget is a quadratic optimization (maximize Φ,
penalize redundancy, respect budget) — i.e. a QUBO, the form solved by quantum
annealers. A deterministic simulated-annealing solver and a benchmark harness ship
behind `LEAN_CTX_EXPERIMENTAL_QUBO`:

```
$ lean-ctx introspect qubo
QUBO spike (experimental, greedy stays default)
items=13  budget=1500
greedy: phi=3.800 tokens=1500
qubo:   phi=3.800 tokens=1500
phi gain: +0.0%
```

On clean problems QUBO reaches parity with the greedy knapsack — **no measurable
win, so greedy remains the default.** The spike exists to *measure*; promotion is
conditional on a future, reproducible gain.

## Research references

- LLMLingua / LLMLingua-2 (2403.12968) — perplexity/classifier token pruning
- ACE: Agentic Context Engineering (2510.04618) — delta contexts, anti-collapse
- Lost in the Middle (2307.03172) — U-shaped attention
- StreamingLLM / H2O (2309.17453, 2306.14048) — attention sinks, KV eviction
- Theta–gamma coupling (Lisman & Jensen 2013) — working-memory chunking
- Information Bottleneck (Tishby et al.) — relevance-conditioned compression
- Stigmergy (Theraulaz & Bonabeau 1999) — indirect coordination
- Ebbinghaus (1885), SM-2 spacing — forgetting curve, spacing effect
- Hebb (1949), McClelland CLS (1995) — associative learning, consolidation
- Integrated Information Theory (Tononi 2004) — integration / non-redundancy
- Global Workspace Theory (Baars 1988; Dehaene) — ignition / broadcast
- Free-Energy Principle (Friston 2010) — active inference, prefetch
- Artificial Immune Systems (de Castro & Timmis 2002) — anomaly/self-nonself
- QUBO / simulated bifurcation (Goto et al. 2019) — quantum-inspired optimization
- Hindsight (Vectorize, 2025) — agent observation networks, evidence vs. inference
