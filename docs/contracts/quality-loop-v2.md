# Quality Loop v2 — Conservative Runtime Strategy Feedback

Status: experimental; successor to [Quality Loop v1](quality-loop-v1.md)
Owner: core engine
Consumers: `ctx_read`, `ctx_edit`, `auto_mode_resolver`, `ctx_metrics`

## Goal and guarantees

v2 combines only attributable negative runtime events into one bounded,
strategy-keyed estimator. It overlays the normal auto-mode result with a
pre-approved strategy from the ordering below. It never changes redaction or
security policy, never lowers a threshold, and never moves toward a riskier
strategy because of a clean outcome. As evidence decays, the resolver returns
to its configured default (or its ordinary auto result when no concrete default
is configured).

The resolver compares a candidate with the configured default through
`ctx_read::mode::more_conservative`. A negative event can only clamp the result
further toward `full`. Unknown and unresolved modes fall back to `full`; peers
in one tier do not replace one another.

## Strategy risk ordering

Lower rank preserves more usable source context. Tiers group strategies whose
renderers do not have a uniform, defensible order; the estimator never swaps
between peers. This ordering follows the actual read views and resolver
descriptions, rather than their compression ratios:

| Rank | Strategies | Basis in current behavior |
|---:|---|---|
| 0 | `full`, `raw`, whole-file `anchored` | Full source is delivered; `raw` is exact bytes and unwindowed anchors annotate the full file. |
| 1 | `full-compact` | The whole body is delivered, but trailing whitespace is removed. |
| 2 | `lines:*`, `diff`, windowed `anchored:*` | These return a selected line range or cached delta, not the complete current file. |
| 3 | `map`, `cognitive`, `task` | Structural, chunked, and task-focused views retain different subsets; they are peers. The resolver describes `map` as retaining dependency/export structure and task-relevant bodies. |
| 4 | `signatures` | API/signature surface omits implementation bodies that `map` can retain. |
| 5 | `mdl`, `aggressive`, `entropy`, `density:*` | These have extension-dependent summaries, lossy selection, or density limits; no uniform order is claimed within the tier. |
| 6 | `reference` | A pointer or short quote is the narrowest delivered view. |

The single comparison helper is `more_conservative(a, b)`: it returns the
lower-rank known strategy, retains `a` on a tie, and returns `full` if either
input is unknown or `auto`. The ordering and pairwise safety property are tested
in `ctx_read::mode`.

## Accepted signals and attribution

Every admitted event is represented by a typed signal kind, provenance,
confidence, path, delivered mode, and original token count. Only high-confidence
events enter the estimator; one event contributes one negative sample.

| Signal | Used? | Attribution rule |
|---|---|---|
| `EditFailureAfterCompressedRead` | Yes | `ctx_edit` old-string miss; cached `last_mode` must equal the bounce tracker's most recent read mode for the same path, and that mode must count as compressed. `ctx_patch` anchor misses do not produce this signal. |
| `FullRereadBounce` | Yes | Same path was read in a compressed mode, then read again as literal `full` within five sequence ticks. Reads forced by a recent edit are excluded. Other non-compressed views and shell accesses do not qualify. |
| `ExpandAfterCompressed` | No | `ctx_expand` may expand an archive/content handle without a source file and delivered mode link, so it cannot safely identify the strategy that caused the expansion. |
| `ModeOverride` | No | Overrides can come from an explicit caller, context gate, pressure policy, or other resolver rule; an override alone does not prove the previous strategy failed. |
| `RepeatedSearch` | No | Search queries are not reliably tied to a specific file and preceding read strategy. |

A bare command or test failure is never attributed: it carries no reliable
file, mode, or read-to-failure link. Low-confidence or missing provenance is
discarded rather than generalized to an extension-wide strategy.

## Estimator, hysteresis, and persistence

The estimator key is `(lowercase file extension, size bucket, delivered mode)`.
Size buckets use the auto resolver's token boundaries: unknown/zero, 1–500,
501–2,000, 2,001–8,000, and over 8,000 tokens. Line ranges normalize to the
`lines` family; density values normalize to `density`.

| Rule | Value |
|---|---:|
| Minimum effective samples to enter risk | 1.5 |
| Enter risk | At least 1.5 negative units and negative/effective samples >= 0.25 |
| Exit risk | below 0.5 negative evidence units |
| Exponential half-life | 15 days |
| Evidence retention | 30 days since last signal |
| Estimator entry cap | 200, least recently used evicted |

Same-path, same-mode, same-size clean `ctx_edit` replacements remain visible in
legacy metrics and count only in the pre-entry rate denominator, preserving the
v1 25% enter threshold. They do not subtract negative evidence or clear active
risk. Risk exits only through time decay or expiry, preventing alternating
clean and negative events from flapping the strategy. Bounces contribute only
negative samples.

New estimator state is an additive `estimator` field in the existing
`edit_quality.json` store, with the same data directory, atomic write, and
flush cadence. Existing v1 `(extension, mode)` pairs remain readable for
metrics. Active risky pairs are copied to an `Unknown` size bucket until their
original evidence window expires; the old state is not removed during
migration.

## Existing recovery paths

- Per-path edit recovery remains one-shot: an old-string miss can supply a full
  retry view on the next auto read, and an anchored `ctx_patch` miss can supply
  fresh anchors. These are immediate retry aids, not extra learned estimator
  inputs.
- `context_gate` keeps the short-lived `BounceTracker` pre-dispatch guard so a
  same-path bounce can be corrected immediately, before the estimator has its
  two-sample minimum. It uses the same `more_conservative` ordering.
- `path_mode_memory` remains a compatibility input while its old records age
  out. Its persisted path-only bounce counts contain neither source mode nor
  size, so projecting them onto an estimator key would invent attribution.
- The resolver's heatmap downgrade now compares through
  `more_conservative`; `resolve_adaptive` no longer owns a second bounce or
  path-memory escalation check.

## Observability and tests

`ctx_metrics` retains the legacy pair report and adds per-estimator-key sample,
event-kind, and risk counts. Tests cover the real strategy ordering, retained
v1 minimum and enter rate, minimum-sample behavior, hysteresis under mixed
negative events and clean outcomes, decay to default, additive migration,
bounded LRU state, and the configured-default risk ceiling.
