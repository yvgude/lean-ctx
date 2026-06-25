# Journey 11 — Analytics, Insights & Reporting

> How much is lean-ctx actually saving you? Where is context being wasted? Which
> commands are slow? This journey covers every reporting, measurement, and
> "show me the numbers" surface — without ever costing the agent extra tokens
> (all of this is CLI / dashboard, not injected context).

Source files referenced here:
- `rust/src/cli/dispatch/analytics.rs` — `gain` (all modes)
- `rust/src/tools/ctx_gain.rs`, `core/stats/` — savings engine
- `rust/src/cli/session_cmd.rs` — `wrapped`
- `rust/src/cli/tee_cmd.rs` — `tee`, `filter`, `slow-log`
- `rust/src/cli/dispatch/network.rs` — `dashboard`, `watch`

---

## 0. The principle

> Per the project's own rule: lean-ctx never prints "↓80% saved" into agent
> context — that would burn tokens. Savings live **here**, in the CLI and
> dashboard, where a human looks at them.

So analytics is a pull model: nothing is added to your agent's window; you run a
command when you want the numbers.

---

## 1. `gain` — the savings dashboard

`lean-ctx gain` is the single entry point, with one mode per question:

```bash
lean-ctx gain                      # headline savings summary
```

| Flag | Answers |
|------|---------|
| `--live` (`--watch`) | live-updating savings as you work |
| `--graph` | savings over time, sparkline |
| `--daily` | per-day breakdown |
| `--cost` | dollar cost saved (model-priced) |
| `--score` | efficiency score |
| `--tasks` | savings grouped by task |
| `--agents` | savings grouped by agent (see Journey 8) |
| `--heatmap` | which files/commands save the most |
| `--wrapped` | "Spotify Wrapped"-style recap (terminal) |
| `--svg` (`--card`) | the Wrapped recap as a shareable SVG (social/OG image) → `lean-ctx-wrapped.svg` |
| `--share` (`--page`) | a self-hostable HTML share page (SVG embedded inline) → `lean-ctx-wrapped.html` |
| `--pipeline` | provider-pipeline processing stats |
| `--deep` | everything: report + tasks + cost + agents + heatmap |
| `--json` | machine-readable (for scripts/CI) |
| `--reset` | clear all savings data |

`--svg`/`--share` accept an optional path (`--svg=card.svg`, `--share=out.html`) and
respect `--period`. `--share` also takes `--base-url=https://…` to emit absolute
Open Graph / Twitter image meta for link unfurling (see §2).

Refinements: `--model <name>` (price against a specific model), `--period <p>`
(time window, default `all`), `--limit <n>` (rows, default 10).

> Start with `lean-ctx gain`; reach for `--deep` when you want the full picture
> in one shot, or `--cost --model gpt-4o` to put a dollar figure on it.

### 1.1 Savings-faithful measurement (why `saved` may read 0)

`gain` only counts savings the **bridge** actually realised: the proxy must be
running *and* intercepting your editor's LLM requests. The summary's first line
makes this precondition explicit:

```
Bridge: connected — 69 tools, 142 requests intercepted   # engaged, numbers are real
Bridge: proxy up, 0 requests intercepted — 69 tools exposed (route the editor through lean-ctx)
Bridge: OFF — proxy not reachable; savings cannot be measured (69 tools registered)
```

When `saved` is `0`, `gain` distinguishes **bridge off** from a **genuine zero**:

| Bridge state | Meaning | What to do |
|--------------|---------|------------|
| `OFF` (proxy down) | No requests intercepted — savings are unmeasured, not zero | Start the proxy (`lean-ctx serve`); confirm `/lean-ctx` shows *connected* |
| proxy up, 0 requests | Bridge reachable but your editor is not routed through it | Verify the editor's `mcp.json` points at lean-ctx (`/lean-ctx` → connected) |
| connected | Bridge engaged; `0` is a real zero for this window | Re-run a read to warm the cache — cold first reads have nothing to save yet |

To measure savings faithfully: enable the bridge, verify `/lean-ctx` reports
*connected* with the expected tool-count, then perform a few reads/commands. The
same engagement state is available machine-readably under the `bridge` key of
`lean-ctx gain --json`.

---

## 2. Sharing & proof — `wrapped`, share cards, and the verified `savings` ledger

### 2.1 `wrapped` — the shareable recap

```bash
lean-ctx wrapped                   # (also: lean-ctx gain --wrapped)
lean-ctx gain --wrapped --period=month
```

A celebratory, screenshot-friendly summary of tokens/cost saved over a period —
good for sharing with your team or justifying the tool to a lead.

### 2.2 Share cards — `--svg` and `--share`

```bash
lean-ctx gain --svg                                  # -> lean-ctx-wrapped.svg
lean-ctx gain --share                                # -> lean-ctx-wrapped.html
lean-ctx gain --share --base-url=https://you.dev/w   # + social preview meta
```

- `--svg` renders the recap as a dependency-free 1200×630 SVG (perfect as a social
  / OpenGraph image; convert to PNG with any SVG tool).
- `--share` emits a **self-contained, self-hostable** HTML page with the SVG
  embedded inline (renders offline, anywhere). Host it wherever you like — your
  site, a gist, GitHub Pages — and that URL *is* the permalink. lean-ctx uploads
  nothing; this is an opt-in artifact, consistent with the zero-telemetry default.
- With `--base-url`, the page gains Open Graph / Twitter meta so the link unfurls
  into a rich card (point it at a hosted PNG render of the SVG, since networks
  don't render SVG `og:image`).

### 2.3 `savings` — the verified savings ledger (auditable)

```bash
lean-ctx savings                   # summary: gross, bounce, net, tokenizer, integrity
lean-ctx savings verify            # re-walk the SHA-256 hash chain (tamper-evidence)
lean-ctx savings export            # every event as JSON
```

Where `gain`/`wrapped` show **aggregate** savings, `savings` is the **per-event,
auditable** record behind them. Every value-producing read appends one append-only
event to `~/.lean-ctx/savings/ledger.jsonl` capturing the counterfactual
(`baseline` vs `actual` tokens), the resolved pricing model, the **tokenizer** that
produced the counts (`o200k_base`), a privacy-preserving repo hash, and a SHA-256
`prev → entry` hash chain. It is **local-only and on by default** (opt out with
`LEAN_CTX_SAVINGS_LEDGER=off`).

Honesty is the point of the ledger:

- **Tokenizer transparency** — counts use `o200k_base` as a proxy; your model's own
  tokenizer may differ a few percent, so the tokenizer is recorded explicitly
  rather than assumed.
- **Bounce-netting** — when a compressed read is later invalidated by a full
  re-read ("bounce"), a negative adjustment is recorded so totals show the
  **realized** saving, not a gross upper bound. `gain --wrapped` nets the same
  bounce out of its headline, and the ledger summary shows gross → bounce → net.
- **Tamper-evidence** — `savings verify` recomputes the chain end to end; any
  edited, reordered, inserted, or removed entry is detected.

> Why a separate ledger? It is the trusted substrate for value-based reporting:
> a number you can hand to a finance team and have it survive scrutiny. See
> `docs/business/03-verified-savings-ledger.md`.

---

## 3. `token-report` — tokens + memory

```bash
lean-ctx token-report              # tokens saved + memory footprint
lean-ctx token-report --json
```

Where `gain` focuses on savings, `token-report` (alias `report-tokens`) adds the
memory side: how much session/knowledge/cache state lean-ctx is holding.

**Golden output — `lean-ctx token-report`** combines the knowledge store, the
live session, and the latest CEP scorecard in one view:

```text
lean-ctx token-report  v3.6.26
  project: /Users/you/dev/lean-ctx
  data:    /Users/you/.lean-ctx
  knowledge: 105 active, 97 archived, 0 patterns, 91 history
  session: 1953 calls, 90710600 tok saved, 333 files read (17 repeated)
  cep(last): score=66 cache_hit_rate=18 mode_diversity=100 compression_rate=82 tok_saved=284748
  report saved: /Users/you/.lean-ctx/report/latest.json
```

The `cep(last)` line is the most recent Context Engineering Protocol scorecard
(see §9); `17 repeated` reads are the cache wins that cost ~13 tokens each.

---

## 4. Finding waste — `discover` and `ghost`

```bash
lean-ctx discover                  # commands in your shell history that ran uncompressed
lean-ctx ghost                     # "ghost tokens": hidden waste lean-ctx could catch
lean-ctx ghost --json
```

- `discover` scans shell history for commands you ran *without* lean-ctx — your
  "you could have saved more here" list.
- `ghost` quantifies waste that's currently slipping through, so you know
  whether tightening compression (Journey 10) is worth it.

---

## 5. Performance — `slow-log`

```bash
lean-ctx slow-log list             # slowest commands lean-ctx wrapped
lean-ctx slow-log clear
```

If lean-ctx ever feels like it's adding latency, this tells you exactly which
commands were slow to compress, so you can exclude or filter them.

---

## 6. Output logs — `tee`

```bash
lean-ctx tee list                  # captured output logs
lean-ctx tee last                  # the most recent
lean-ctx tee show <id>
lean-ctx tee clear
```

`tee` keeps a log of compressed command outputs so you can recover the *full*
output of something you ran earlier without re-running it.

---

## 7. The web dashboard — `dashboard`

```bash
lean-ctx dashboard                 # http://localhost:3333
lean-ctx dashboard --port 4000 --host 0.0.0.0
```

A browser UI over everything in this journey: live savings, heatmaps, sessions,
knowledge, agents. The richest way to explore; ideal for a second monitor.

> This dashboard is the home for the UX feedback in issue #249 — it's where
> context-management visualization lives, distinct from the CLI numbers.

### Open it inside your editor

```bash
lean-ctx dashboard --vscode        # open as a native editor tab (no browser)
```

With the [lean-ctx editor extension](https://marketplace.visualstudio.com/items?itemName=LeanCTX.lean-ctx)
installed, `--vscode` opens the dashboard as a real editor tab instead of a
browser window. The CLI detects the editor that launched your terminal — VS
Code, Cursor, VSCodium, Windsurf or VS Code Insiders — and hands off to the
extension, which runs the dashboard on a private loopback port and tears it down
when you close the tab. You can also open it from the command palette
(`lean-ctx: Open Web Dashboard`) or the deep link
`vscode://LeanCTX.lean-ctx/dashboard`.

If no editor (or the extension) is found, `--vscode` falls back to the browser,
so the command is never a no-op. `--open=vscode` and
`LEAN_CTX_DASHBOARD_OPEN=vscode` behave the same way.

---

## 8. The live TUI — `watch`

```bash
lean-ctx watch                     # real-time event stream in the terminal
```

A terminal dashboard (no browser) showing the live event stream — reads,
compressions, cache hits — as they happen. Great for confirming "is lean-ctx
actually intercepting this?" in real time.

---

## 9. Quality scoring — `cep` and `benchmark`

```bash
lean-ctx cep                       # CEP score trends (Context Engineering Protocol)
lean-ctx benchmark run             # run the benchmark suite
lean-ctx benchmark report          # results
lean-ctx benchmark eval / compare  # evaluate / compare runs
lean-ctx benchmark scorecard       # reproducible savings + recall/MRR + latency
```

- `cep` tracks the Context Engineering Protocol score over time — a measure of
  how well-structured the agent's context has been.
- `benchmark` measures compression quality/throughput so regressions are caught
  (also used in CI, Journey 9).

### Reproducible scorecard — `benchmark scorecard`

One command runs a **fixed, committed scenario matrix** (`small` / `medium` /
`large`) and reports the three numbers that matter together: compression
**savings**, retrieval **recall@5/@10 + MRR**, and search **latency**.

```bash
lean-ctx benchmark scorecard                       # human-readable table
lean-ctx benchmark scorecard --json                # structured JSON
lean-ctx benchmark scorecard --json --output sc.json
```

The corpus is generated deterministically (content derived purely from the file
index — no RNG) and retrieval is pure BM25, so the **quality metrics are
reproducible** run-to-run and machine-to-machine. Each report embeds a
`determinism_digest` (a fingerprint of the latency-free metrics) in both the JSON
and the human table, so two artifacts are **self-verifying** — compare the
digests to confirm identical quality without diffing every number. Latency is
wall-clock and therefore reported but excluded from the digest. CI runs the
scorecard on every push and uploads `scorecard.json` as a build artifact, and a
test (`scorecard_determinism`) asserts the digest is stable.

---

## 10. Learning loops — `learn` and `gotchas`

These turn observed history into durable insight:

```bash
lean-ctx gotchas list              # recorded bugs/footguns ("bug memory")
lean-ctx gotchas stats / export / clear
lean-ctx learn                     # learned gotchas
lean-ctx learn --apply             # promote them into AGENTS.md
```

- `gotchas` (alias `bugs`) is a memory of mistakes/footguns hit in this project.
- `learn --apply` promotes high-value lessons into your agent rules — the
  analytics-to-governance bridge (pairs with `export-rules`, Journey 10).

---

## 11. Raw stats & transcript compaction

```bash
lean-ctx stats                     # raw stats store summary
lean-ctx stats json                # raw JSON
lean-ctx stats reset-cep           # reset CEP scores only
lean-ctx compact [path]            # compress stored agent transcripts
```

`stats` is the low-level store behind `gain`; `compact` shrinks saved agent
transcripts so long histories don't bloat the data dir.

---

## 12. Decision guide

| You want… | Reach for |
|-----------|-----------|
| Headline savings | `gain` (§1) |
| A shareable recap | `wrapped` (§2.1) |
| A social/OG image or hostable page | `gain --svg` / `gain --share` (§2.2) |
| An auditable, per-event savings record | `savings` (§2.3) |
| Tokens **and** memory footprint | `token-report` (§3) |
| Where am I still wasting tokens? | `discover`, `ghost` (§4) |
| Is lean-ctx slowing me down? | `slow-log` (§5) |
| Recover an earlier full output | `tee` (§6) |
| Rich visual exploration | `dashboard` (§7) |
| Watch it work live | `watch` (§8) |
| Context-quality / regression tracking | `cep`, `benchmark` (§9) |
| Turn history into rules | `learn`, `gotchas` (§10) |
| Raw numbers / shrink transcripts | `stats`, `compact` (§11) |

---

## Storage & data (analytics)

| Path | Contents |
|------|----------|
| `~/.lean-ctx/` stats store | savings/usage that `gain`/`stats` read |
| `~/.lean-ctx/savings/ledger.jsonl` | verified per-event savings ledger (`savings`) |
| `~/.lean-ctx/pipeline_stats.json` | provider-pipeline stats (`gain --pipeline`) |
| tee logs | captured full command outputs |
| gotchas/bug memory | recorded footguns |

---

## UX notes captured during this walkthrough

- `gain` has 12+ modes that aren't discoverable from `gain` alone; §1 tabulates
  every one so users stop guessing flag names.
- The deliberate "no savings text in agent context" rule is stated up front (§0)
  so users understand *why* the numbers only live in the CLI/dashboard.
- `discover`/`ghost` (waste finders) and `learn`/`gotchas` (learning loops) are
  powerful but obscure; grouped here by intent so they're actually found.
