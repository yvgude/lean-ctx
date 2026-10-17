# Quality Loop v1 — Edit-Outcome Feedback into Mode Selection

Status: experimental (GL #494)
Owner: core engine
Consumers: `auto_mode_resolver`, `ctx_edit`, `ctx_metrics`

## Problem

`BounceTracker` and `path_mode_memory` close the feedback loop for *re-read*
bounces (compressed read → full re-read), but an edit that fails because the
file was last read in a compressed mode taught the system nothing: the agent
quoted an `old_string` from a `map`/`signatures` rendering whose body was
never in context, the edit missed, and the next read of a similar file was
compressed exactly the same way.

### What counts as a re-read bounce (correction)

Every read is classified by what it showed of the file (`ReadView`):

| View | `ctx_read` modes | Shell (`ctx_shell`) |
|---|---|---|
| Compressed (lossy) | `map`, `signatures`, `aggressive`, `entropy`, `task`, `reference`, `cognitive`, `mdl`, `density:`, `auto` resolved to one of these | — |
| Full content | `full`, `raw`, `full-compact`, `anchored` (whole file) | `cat`, `bat`/`batcat` without `-r`, `less`, `more` — unpiped, unredirected |
| Partial | `lines:` (ranges, multi-select, `lines:-N`), `anchored:N-M`, `diff`, any unrecognised mode | `head`, `tail`, `sed` without `-i`, `bat -r`, a full reader piped onward |

A bounce is detected only when a **full-content** read immediately follows a
**compressed** read of the same path within 5 read ticks. Partial reads never
start or complete one, and a partial read between the two ends the chain: the
compressed view did its job by pointing at the window. Reading a file window
by window and then in full is therefore no correction. `anchored` is verbatim
plus line anchors, so it never starts a correction; as full content it
completes one, which the `ctx_patch` that normally follows cancels.

A re-read bounce is provisional: a compressed read, a full read and then an
edit of the same file is the read-before-edit flow, not a compression
failure. `BounceTracker` books a detected bounce only when no edit of the path
follows within 10 read ticks, and at process shutdown at the latest. Edits
that cancel it:

- `ctx_patch`, `ctx_edit`;
- `ctx_shell` commands that rewrite the file: `sed -i`, `perl -i`, output
  redirection (`>`, `>>`, `>|`, `N>`, `&>`), `tee [-a]`, `cp`/`mv`/`install`
  onto the path (including `-t DIR`), `apply_patch` bodies, also inside
  `&&`/`;`/`||` chains, after `cd DIR`, and inside `sh -c`/`lean-ctx -c`.
  Duplications (`2>&1`, `>&2`), `/dev/*` and targets built from `$VAR` or
  command substitution are never edits;
- native host edits reported by the hook through the `edit_beacon.log` file
  in the data directory: Claude Code `Edit`/`Write`, Cursor `StrReplace`, …,
  native shell commands of the forms above seen by the Bash `PreToolUse`
  hooks (Claude Code, Cursor, Codex; not for commands the hook denies), and
  Codex `apply_patch` calls seen by the Codex `PostToolUse` observer
  (matcher includes `apply_patch`; existing `hooks.json` files are updated by
  the installed-hook refresh at MCP server start or by `lean-ctx init`).

Only booked bounces reach
the savings ledger, `path_mode_memory`, the quality signal and the scent
field. A path that memory marks as always re-read in full is delivered in full
for explicit compressing modes as well (`reason=learned-full`); `lines:`,
`diff`, `raw` and `anchored` reads are never rewritten.

## Signals

Edit outcomes are recorded by `ctx_edit` (both the MCP tool and the in-process
`tools::ctx_edit::handle` path) via `core::edit_quality::record_edit_outcome`,
keyed by the **mode of the last lean-ctx read of that file** (session cache
`last_mode`). Only two outcomes carry signal:

| Outcome | Condition | Recorded as |
|---|---|---|
| Success | replacement applied (`CacheEffect::Invalidate`) | success for `(ext, last_mode)` |
| Compression-correlated failure | `old_string` not found (auto-escalation `CacheEffect::StoreFull`, or plain miss after a `full` read as baseline) | failure for `(ext, last_mode)` |

Explicitly **not** recorded (no compression signal): `create=true`, empty or
identical `old_string`/`new_string`, preimage/TOCTOU mismatches, missing
files, already-applied edits, and files never read through lean-ctx
(`last_mode` empty).

## Feedback rules

### 1. Per-path one-shot escalation

A compression-correlated failure (last mode ≠ `full`) arms a pending
escalation for that path. The **next** `mode=auto` resolution of the same
path returns `full` (resolver source: `edit_fail_escalation`), then the
escalation is consumed. Pending escalations expire after **1 hour**.

This complements the immediate in-response escalation that `ctx_edit` already
appends (full content in the error message): the in-response copy serves the
retry, the pending escalation serves the next independent read.

### 2. Per-(extension × mode) risky penalty

Aggregated per `(file extension, read mode)` pair with hysteresis:

```
fail_rate = fails / (fails + successes)

enter risky:  fails >= 2  AND  fail_rate >= 0.25
exit  risky:  fail_rate < 0.15
```

While a pair is risky, `mode=auto` resolutions that would pick that mode for
that file type return `full` instead (resolver source:
`edit_quality_penalty`). The two thresholds prevent flapping: a single lucky
edit cannot immediately re-enable a mode that keeps breaking edits.

The penalty is **per extension**, never global: `rs|map` being risky does not
affect `py|map` or `rs|signatures`.

## Persistence

`~/.lean-ctx/edit_quality.json` (respects `LEAN_CTX_DATA_DIR`), atomic
tmp+rename writes, flushed every 10 recordings and on server shutdown.

Bounds:

| Limit | Value |
|---|---|
| Pair decay (no failure) | 30 days |
| Escalation TTL | 1 hour |
| Max pairs | 200 (oldest-failure evicted) |
| Max pending escalations | 100 (oldest evicted) |

## Observability

`ctx_metrics` prints an `Edit quality (compression-correlated)` section: per
pair fails/successes/fail-rate, the `[risky -> full]` marker, plus served and
pending escalation counts. Auto-mode resolutions show up in the existing
`Auto-mode sources` line as `edit_fail_escalation` and `edit_quality_penalty`.

## Invariants

- Recording an outcome never blocks an edit: store access is lock-guarded and
  failures to lock are silently skipped.
- The penalty only ever *escalates* toward `full`; it never picks a lossier
  mode than the resolver would have chosen.
- No regression of bounce semantics: `BounceTracker` / `path_mode_memory`
  remain independent signals evaluated before the quality loop's penalty.
