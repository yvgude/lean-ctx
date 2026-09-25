# Seeing what lean-ctx did

lean-ctx keeps tokens, secrets and risky commands out of your agent's context.
The dashboard shows all of it. This guide covers the smaller surfaces that
show it where you already are: your agent, your shell prompt, your commits.

Every one of them follows three rules:

- **Only you see it.** Nothing here is written into the model's context. The
  surfaces are status lines, prompt segments, `systemMessage` lines, desktop
  notifications and commit trailers.
- **Nothing when there is nothing to say.** No zeros, no placeholders. Data
  older than 12 hours or from another project shows nothing.
- **Every number can be proven.** `lean-ctx value` recomputes it from the
  hash-chained savings ledger and the signed audit trail (see
  [Proof](#proof)).

## Configuration

```toml
[value_display]
mode = "minimal"          # off | minimal | milestones | verbose
recap_every_turns = 10    # consider a turn recap every N turns
recap_min_tokens = 50000  # …and show it only above this (or on a security event)
notifications = true      # milestone notifications, in mode milestones/verbose
git_trailer = false       # set by `lean-ctx init --git-trailer`
```

| Mode | What you get |
| --- | --- |
| `off` | Nothing. The proof chains are still written. |
| `minimal` (default) | Status line, prompt segment, recaps above the threshold |
| `milestones` | `minimal` plus a desktop notification when a milestone is reached |
| `verbose` | `milestones`, and every turn recap regardless of the threshold |

`LEAN_CTX_VALUE_DISPLAY=off` overrides the mode for one shell or one run.

## Claude Code

`lean-ctx init --agent claude` sets up the
[status line](https://docs.anthropic.com/en/docs/claude-code/statusline):

```text
◆ lean-ctx −1.2M tok · 41 cached · ⛨ 3
```

`⛨` counts security events: secrets kept out of context, risky commands
blocked, paths outside the project blocked, injections flagged.

If you already have a status line, `init` leaves it alone and prints the
command that chains both:

```bash
lean-ctx statusline --wrap '<your command>'
```

Your command gets the same input, and lean-ctx's segment is appended to its
first line. `lean-ctx uninstall` gives your command back.

The Stop hook adds a one-line recap every 10 turns when it is worth one
(`◆ lean-ctx · last 10 turns: −312.0K tokens`). On a fresh start the last
session is summarised once, and once a week a digest covers all sessions.

## Shell prompt

```bash
lean-ctx init --prompt       # zsh, bash or fish, detected from $SHELL
lean-ctx init --prompt off   # remove it again
```

This adds a dim segment for the project you are in: on the right in zsh and
fish, in front of `PS1` in bash. `init --prompt` writes `prompt.<shell>` next
to the shell hook and sources it from its own block in your rc file. Your
existing prompt and `RPROMPT` stay as they are. `lean-ctx uninstall` removes
the block too.

The segment reads one small snapshot file and never the ledger, so it adds
no noticeable delay to your prompt.

### Starship

Starship draws the whole prompt, so `init --prompt` prints this module instead
of editing your rc file. Add it to `~/.config/starship.toml`:

```toml
[custom.lean_ctx]
command = "lean-ctx prompt-segment --shell plain"
when = true
style = "dimmed"
format = "[$output]($style) "
```

Starship hides the module whenever the command prints nothing. Add
`${custom.lean_ctx}` to your `format` if you list modules explicitly.

### Powerlevel10k

Define a custom segment in `~/.p10k.zsh` and add `lean_ctx` to
`POWERLEVEL9K_RIGHT_PROMPT_ELEMENTS`:

```zsh
function prompt_lean_ctx() {
  local seg="$(lean-ctx prompt-segment --shell plain)"
  [[ -n $seg ]] || return
  p10k segment -f 244 -t "$seg"
}
```

### Other prompts

`lean-ctx prompt-segment --shell plain` prints the segment without colour, or
nothing at all. Any prompt engine that can run a command can use it.
`--shell zsh|bash|fish` wraps the colour in the escapes each shell needs to
measure the prompt width correctly.

## Milestone notifications

With `mode = "milestones"`, lean-ctx sends a desktop notification the first
time you reach a milestone:

- 1M, 10M, 100M and 1B tokens kept out of your model's context
- the first secret kept out of context
- the first risky command blocked
- 7, 30 and 100 days in a row with lean-ctx

Rules:

- At most one notification a day. If several milestones are reached at once,
  the most significant one is shown and the rest wait for later days.
- Of the token and streak ladders, only the highest new rung is shown. After an
  upgrade with a large history you get one notification, not four.
- Milestones are computed from the verified chains. If a chain fails
  verification, no milestone is shown.
- Every notification ends with `Proof: lean-ctx value --all`.

The MCP server sends them with the platform's own mechanism: `osascript` on
macOS, `notify-send` on Linux desktops (skipped without `DISPLAY` or
`WAYLAND_DISPLAY`), a toast on Windows. No extra dependency is involved. Set
`notifications = false` to keep the mode but silence them.

## Commit trailer

```bash
lean-ctx init --git-trailer       # in the repository
lean-ctx init --git-trailer off
```

This installs a `prepare-commit-msg` hook that adds a trailer to your commit
message:

```text
lean-ctx: 840.0K tokens saved, 1 secret kept out of context
```

- The trailer counts only what happened in this project since the last
  commit that carried one, so no number is counted twice.
- Nothing is added to merge or squash commits, to `commit --amend` or
  `-c`/`-C` reuse, or when there is nothing new.
- The hook cannot fail a commit. If lean-ctx is missing or errors, the commit
  goes through unchanged.
- If the repository already has a `prepare-commit-msg` hook, `init` does not
  touch it. It prints the one line to add to your hook.

You can edit or delete the trailer in the editor like any other line.

## Wrapped

`lean-ctx gain --wrapped` (optionally with `--period=week|month`) adds a
security section when the period had security events. The counts come from
the audit trail, and they are shown only when the trail verifies:

```text
  ✓  2 secrets kept out of context
  ✓  1 risky command blocked
     measured · signed audit trail intact
```

## Speed

lean-ctx never estimates speed from your sessions: a session has no
comparison run without lean-ctx. Speed is shown only after you have measured
it:

```bash
lean-ctx eval init ./speed-suite     # a starter suite; add your own tasks
export LEAN_CTX_EVAL_MODEL_URL=http://localhost:11434/v1
export LEAN_CTX_EVAL_MODEL=qwen2.5-coder:7b
lean-ctx prove speed --suite ./speed-suite/suite.ndjson
```

Each task goes to the same live model twice per run: once with a raw context
dump (baseline) and once with lean-ctx's context, both within the same token
budget. The first request is a warm-up and is not counted. Which arm goes
first alternates. Each arm's latency is the median of `--runs` rounds
(default 3), and every answer is scored:

```text
◆ lean-ctx speed proof · suite.ndjson · qwen2.5-coder:7b
  5 tasks × 3 runs · budget 4000 tokens · warm-up request excluded

                      baseline    lean-ctx
  median latency     2.10 s      1.24 s     ✓ 41.0% faster
  context tokens     3.9K        2.1K
  correct answers    14/15       15/15
  faster on          5 of 5 tasks

  ✓ measured 2026-09-25 · signed
  Verify: lean-ctx prove speed --verify <data_dir>/value/speed/speed-proof-v1_20260925T101500Z.json
```

The proof is signed with your agent key. `lean-ctx prove speed --verify`
checks the signature and recomputes the summary from the raw timings, so an
edited number shows `TAMPERED` and exits 1.

`gain --wrapped` then shows the latest proof, with its date, size and model:

```text
  ⚡  41% faster model answers with lean-ctx
      measured 2026-09-25 · 5 tasks × 3 runs · qwen2.5-coder:7b
```

It is left out when the proof does not verify, when lean-ctx was not faster,
or when it answered fewer tasks correctly than the baseline.

## Proof

```bash
lean-ctx value            # the current session
lean-ctx value --all      # every session
lean-ctx value --session <id>
lean-ctx value --json
```

Each line names its source and label:

- `✓ measured`: counted from the savings ledger or the audit trail.
- `≈ derived`: arithmetic on measured numbers, such as the share of tool input
  that was saved.

Both chains are verified from their first entry, and the report names the first
and last entry hash behind each number, so you can find them in the JSONL
files. If a chain was modified, the report says `TAMPERED`, marks the numbers
as not proof, and exits with status 1.
