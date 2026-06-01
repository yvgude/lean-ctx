You're connected. Now you (and your AI) work in the codebase every day. This journey covers the commands and MCP tools you'll touch constantly: reading files, running commands, searching, and seeing what you saved.

## The two ways LeanCTX helps you every day

| Path | When it fires | What you do |
|------|---------------|-------------|
| **MCP tools** | Your AI reads/searches files | Nothing — your editor calls `ctx_*` automatically |
| **Shell hook** | A command runs in a hooked shell | Nothing — output is compressed automatically |

You rarely call the CLI by hand. The CLI commands below exist so you *can* (for
scripts, for inspection, and to understand what your AI is doing).

## Reading files

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
| `auto` | LeanCTX picks the best mode | you're unsure (default) |
| `full` | whole file, cached | you'll edit it |
| `map` | imports + API surface | context-only file |
| `signatures` | function/type signatures only | you need the API |
| `aggressive` | heavy compression | very large file |
| `entropy` | entropy-ranked lines | huge file, want the dense parts |
| `task` | lines relevant to a task | task-focused read |
| `reference` | reference handle, not content | output too big to inline |
| `diff` | lines changed since last read | re-checking a file |
| `lines:N-M` | a specific range | you know where to look |

**Under the hood:** `ctx_read` consults the session cache; a cache hit returns a
file reference instead of content. A mode predictor learns which mode works best
for which file over time.

**Golden output — the same file in three modes.** Reading a 66-line file:

`mode = map` — imports + API surface only:

```text
jetbrains.rs 66L
  deps: super::super::resolve_binary_path
  API:
    λ+install_jetbrains_hook()
    λ-print_jetbrains_manual_step(display_path:s)
```

`mode = signatures` — the same API as a flat signature list:

```text
jetbrains.rs 66L
 deps super::super::resolve_binary_path
λ+install_jetbrains_hook()
λ-print_jetbrains_manual_step(display_path:s)
```

`mode = full` returns all 66 lines verbatim. The `λ+` / `λ-` markers encode
visibility (`+` public, `-` private), so `map` and `signatures` convey the file's
shape in ~5 lines instead of 66 — and the first read in a session may also prepend
an `--- AUTO CONTEXT ---` block with related files and graph edges.

### `lean-ctx diff <a> <b>` / `ctx_delta`

Compressed diff between two files (CLI) or incremental diff since the last read
of one file (`ctx_delta`, the MCP tool — returns only changed lines).

## Running commands

### `lean-ctx -c "cmd"` / `ctx_shell`

**What it does:** Runs a shell command and compresses noisy output (test runners,
builds, package managers) while keeping the signal.

```bash
lean-ctx -c "cargo test"             # compressed
lean-ctx -c "cargo test" --raw       # full output
lean-ctx -t "cargo build"            # tracked: full output + recorded stats
lean-ctx bypass "cmd"                # zero compression (= LEAN_CTX_RAW=1)
```

When the shell hook is installed, your AI's terminal commands route through this
automatically — you don't type `lean-ctx -c` yourself. The hook respects an
allowlist (`shell_allowlist`, ~200 binaries) and skips `excluded_commands`.

**Safety:** commands run under PathJail and the shell allowlist. Secrets in
output are redacted when `[secret_detection]` is on (default). Set
`shell_strict_mode = true` to block `$()` / backticks.

## Searching & navigating

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
> [Context Engineering](/docs/journeys/context-engineering/) for details.

## Seeing what you saved — `lean-ctx gain`

**What it does:** The token-savings dashboard. This is where savings live — by
design, LeanCTX does **not** print "↓80% saved" footers inline (that would cost
tokens). You check `gain` when you want the numbers.

```bash
lean-ctx gain                # summary dashboard
lean-ctx gain --live         # live-updating
lean-ctx gain --graph        # trend graph
lean-ctx gain --daily        # per-day breakdown
lean-ctx gain --wrapped      # "year in review" summary
lean-ctx gain --json         # machine-readable
```

**Empty state:** a fresh install shows "No savings recorded yet — and that's
expected," with next steps. Savings accrue as your AI uses the `ctx_*` tools;
the first real numbers appear after a few file reads or commands.

**Golden output — a populated dashboard** (real numbers from a long-running
install):

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

## Choosing how much LeanCTX exposes — `lean-ctx tools`

**What it does:** Sets the **tool profile** — how many MCP tools your AI sees.
Fewer tools = less per-call overhead.

```bash
lean-ctx tools minimal       # 5 essential tools
lean-ctx tools standard      # 20 tools (balanced)
lean-ctx tools power         # all tools (default for existing installs)
lean-ctx tools show          # current profile
lean-ctx tools list          # what each profile contains
```

> **`tools` vs. `profile`:** `tools` controls *which MCP tools* are exposed.
> `profile` (see [Advanced & Integrations](/docs/journeys/advanced-and-integrations/))
> controls *context profiles* — compression and read-mode behavior. They sound
> similar but do different things; `lean-ctx tools` is the canonical entry point
> for tool profiles.

After changing the profile, restart your AI tool so it re-reads the tool list.

## Output verbosity

```bash
lean-ctx compression standard   # off | lite | standard | max
```

Controls how aggressively shell/tool output is compressed (`terse` is an alias).
`max` is the densest; `off` disables it for a session. Default is `lite`.
