# Journey 2 — Daily Use

> You're connected. Now you (and your AI) work in the codebase every day. This
> journey covers the commands and MCP tools you'll touch constantly: reading
> files, running commands, searching, and seeing what you saved.

Source files referenced here:
- `rust/src/cli/read_cmd.rs` — read / diff / grep / find / ls / deps
- `rust/src/shell/` — command execution + compression
- `rust/src/tools/ctx_read.rs`, `ctx_shell.rs`, `ctx_search.rs` — MCP equivalents
- `rust/src/core/stats/format.rs` — the `gain` dashboard
- `rust/src/cli/profile_cmd.rs` — `tools` / `profile`

---

## 0. The two ways lean-ctx helps you every day

| Path | When it fires | What you do |
|------|---------------|-------------|
| **MCP tools** | Your AI reads/searches files | Nothing — your editor calls `ctx_*` automatically |
| **Shell hook** | A command runs in a hooked shell | Nothing — output is compressed automatically |

You rarely call the CLI by hand. The CLI commands below exist so you *can* (for
scripts, for inspection, and to understand what your AI is doing).

---

## 1. Reading files

### `lean-ctx read <file>` / `ctx_read`

**What it does:** Reads a file with compression and a session cache. The first
read compresses; an unchanged re-read costs ~13 tokens instead of the whole file.

```bash
lean-ctx read src/main.rs            # auto mode
lean-ctx read src/main.rs -m signatures
lean-ctx read src/main.rs --fresh    # bypass cache
```

**The 10 read modes** (`mode` param):

| Mode | Returns | Use when |
|------|---------|----------|
| `auto` | lean-ctx picks the best mode | you're unsure (default) |
| `full` | whole file, cached | you'll edit it |
| `map` | imports + API surface | context-only file |
| `signatures` | function/type signatures only | you need the API |
| `aggressive` | heavy compression | very large file |
| `entropy` | entropy-ranked lines | huge file, want the dense parts |
| `task` | lines relevant to a task | task-focused read |
| `reference` | reference handle, not content | output too big to inline |
| `diff` | lines changed since last read | re-checking a file |
| `lines:N-M` | a specific range | you know where to look |

**Under the hood:** `ctx_read` consults the `SessionCache`; a cache hit returns a
file reference instead of content. The mode predictor (`mode_stats.json`) learns
which mode works best for which file over time.

**Golden output — the same file in three modes.** Reading
`rust/src/hooks/agents/jetbrains.rs` (66 lines):

`mode = map` — imports + API surface only:

```text
jetbrains.rs [66L]
  deps: super::super::resolve_binary_path
  API:
    fn ⊛ install_jetbrains_hook() @L3-55
    fn print_jetbrains_manual_step(display_path:s) @L60-66
```

`mode = signatures` — the same API as a flat signature list:

```text
jetbrains.rs [66L]
fn ⊛ install_jetbrains_hook() @L3-55
fn print_jetbrains_manual_step(display_path:s) @L60-66
```

`mode = full` returns all 66 lines verbatim. The `⊛` marks a public/exported
symbol (private items carry no marker), and the trailing `@Lstart-end` is the
symbol's exact line span — so `map` and `signatures` convey both the file's
*shape* and *where each symbol lives* in ~5 lines instead of 66, letting an agent
jump straight to a function instead of issuing a follow-up search. The line-range
suffix is emitted only in these navigation modes; compression-first modes
(`aggressive`, `entropy`, full reads) stay byte-identical. The first read in a
session may also prepend an `--- AUTO CONTEXT ---` block with related files and
graph edges.

### `lean-ctx diff <a> <b>` / `ctx_delta`

Compressed diff between two files (CLI) or incremental diff since the last read
of one file (`ctx_delta`, the MCP tool — returns only changed lines).

---

## 2. Running commands

### `lean-ctx -c "cmd"` / `ctx_shell`

**What it does:** Runs a shell command and compresses noisy output (test runners,
builds, package managers) while keeping the signal.

```bash
lean-ctx -c "cargo test"             # compressed
lean-ctx -c "cargo test" --raw       # full output
lean-ctx -t "cargo build"            # tracked: full output + recorded stats
lean-ctx raw "cmd"                   # skip compression (= LEAN_CTX_RAW=1; allowlist still applies)
```

When the shell hook is installed, your AI's terminal commands route through this
automatically — you don't type `lean-ctx -c` yourself. The hook respects an
allowlist (`shell_allowlist`, ~200 binaries) and skips `excluded_commands`. Need
one more binary? `lean-ctx allow <cmd>` adds it (and `lean-ctx allow --list`
shows the effective allowlist). Output that is already token-dense — JSON or
TOON — is detected and passed through instead of being re-compressed.

**Safety:** commands run under PathJail and the shell allowlist. Secrets in
output are redacted when `[secret_detection]` is on (default). Set
`shell_strict_mode = true` to block `$()` / backticks.

---

## 3. Searching & navigating

| Command | MCP tool | What it does |
|---------|----------|--------------|
| `lean-ctx grep <pat> [path]` | `ctx_search` | Regex search, compressed results |
| `lean-ctx find <pat> [path]` | — | Find files by glob/substring |
| `lean-ctx ls [path]` | `ctx_tree` | Compact directory map with counts |
| `lean-ctx deps [path]` | — | Project dependencies |
| — | `ctx_semantic_search` | Meaning-based search (BM25 + embeddings) |

**Regex vs. semantic:** use `ctx_search`/`grep` when you know the string; use
`ctx_semantic_search` when you know the *concept* ("where do we validate auth
tokens?"). Semantic search needs an index — it builds on first use and updates
in the background.

> **One call instead of three:** when you're exploring ("where is X handled?"),
> `ctx_compose` answers in a single call — keywords + ranked files + matches +
> the top symbol inline — instead of a separate search → read → search loop.
> It's the highest-leverage everyday power tool; see
> [Journey 7 — Context Engineering](07-context-engineering.md) for details.

---

## 4. Seeing what you saved — `lean-ctx gain`

**What it does:** The token-savings dashboard. This is where savings live — by
design, lean-ctx does **not** print "↓80% saved" footers inline (that would cost
tokens). You check `gain` when you want the numbers.

```bash
lean-ctx gain                # summary dashboard
lean-ctx gain --live         # live-updating
lean-ctx gain --graph        # trend graph
lean-ctx gain --daily        # per-day breakdown
lean-ctx gain --wrapped      # "year in review" summary
lean-ctx gain --svg          # shareable SVG card (social/OG image)
lean-ctx gain --share        # self-hostable HTML share page (opt-in permalink)
lean-ctx gain --json         # machine-readable
```

For an **auditable, per-event** record behind these aggregates — with tokenizer
transparency, bounce-netting, and a tamper-evident SHA-256 chain — use
`lean-ctx savings` (and `lean-ctx savings verify`). It's local-only and on by
default; see Journey 11 §2.3.

**Empty state:** a fresh install shows "No savings recorded yet — and that's
expected," with next steps. Savings accrue as your AI uses the `ctx_*` tools;
the first real numbers appear after a few file reads or commands.

**Golden output — a populated dashboard** (real numbers from a long-running
install; the "Cosmic Orbit" mascot levels up as savings grow):

<details>
<summary><code>lean-ctx gain</code> — token savings dashboard</summary>

```text
  ╭──────────────────────────────────────────────────────────────╮
  │    ◆  lean-ctx   Token Savings Dashboard                     │
  ├──────────────────────────────────────────────────────────────┤
  │    388.8M        62.6%         18,707        $983.19         │
  │    tokens saved  compression   commands      USD saved       │
  ╰──────────────────────────────────────────────────────────────╯
    past 30 days:  $971.96 saved

  Cost Breakdown  @ $2.50/M input · $10.00/M output
  ──────────────────────────────────────────────────────────────
    Without lean-ctx     $1585.68   $1552.01 input + $33.67 output
    With lean-ctx         $602.50   $580.05 input + $22.45 output
    You saved             $983.19   input $971.96 + output $11.22
```

</details>

Related: `lean-ctx token-report` (token + memory report), `lean-ctx ghost`
(hidden token waste from uncompressed commands), `lean-ctx discover` (missed
compression opportunities in your shell history).

---

## 5. Choosing how much lean-ctx exposes — `lean-ctx tools`

**What it does:** Sets the **tool profile** — how many of the 77 MCP tools your
AI sees. Fewer tools = less per-call overhead.

```bash
lean-ctx tools minimal       # 10 essential tools
lean-ctx tools standard      # 19 tools (balanced)
lean-ctx tools power         # all 69 (default for existing installs)
lean-ctx tools show          # current profile
lean-ctx tools list          # what each profile contains
```

> **`tools` vs. `profile`:** `tools` controls *which MCP tools* are exposed.
> `profile` (Journey 5) controls *context profiles* — compression and read-mode
> behavior. They sound similar but do different things; `lean-ctx tools` is the
> canonical entry point for tool profiles.

After changing the profile, restart your AI tool so it re-reads the tool list.

---

## 6. Output verbosity

```bash
lean-ctx compression standard   # off | lite | standard | max
```

Controls how aggressively shell/tool output is compressed (`terse` is an alias).
`max` is the densest; `off` disables it for a session. Default is `lite`.

---

## UX notes captured during this walkthrough

- The split between `tools` (MCP tool count) and `profile` (context behavior) is
  the single most confused pair of commands. The help text now states the
  distinction; `lean-ctx tools` is documented as canonical.
- `gain` is the *only* place savings are shown, intentionally. New users
  sometimes expect inline footers; the empty-state message now sets that
  expectation.
