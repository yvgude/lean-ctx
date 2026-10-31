# Seeing what lean-ctx did

LeanCTX selects context and applies configured controls on supported paths; it
records the measurements and events those paths expose. The local web view
(`lean-ctx dashboard`) shows those records. This guide covers
the smaller surfaces that show it where you already are: your agent, your
shell prompt, your editor, your commits.

Every one of them follows three rules:

- **Only you see it.** Nothing here is written into the model's context. The
  surfaces are status lines, prompt segments, `systemMessage` lines, desktop
  notifications and commit trailers.
- **Nothing when there is nothing to say.** No zeros, no placeholders. Data
  older than 12 hours or from another project shows nothing.
- **Every number has a source and label.** `lean-ctx value` derives displayed
  values from the local savings ledger and audit trail (see [Proof](#proof)).
  Hashes and signatures establish record integrity; they do not turn a local
  estimate into provider usage, invoice savings or answer-quality evidence.

## Configuration

```toml
[value_display]
mode = "minimal"          # off | minimal | milestones | verbose
statusline = "brand"      # Claude Code status line: brand | subtle
recap_every_turns = 5     # consider a turn recap every N turns
recap_min_tokens = 10000  # …and show it only above this (or on a security event)
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
[status line](https://docs.anthropic.com/en/docs/claude-code/statusline).
Existing installs get it too: `lean-ctx update` and the MCP server's start
refresh the Claude hooks and add the status line when there is none (a
`"statusLine": null` entry counts as none):

```text
◆ LeanCTX │ SELECT 12 files · 30 commands │ CONTROL ⛨ 3 enforced │ TOKENS −1.2M kept out · 60% leaner · 41 cached
```

The line reads like the Context Gateway it reports on, in LeanCTX colours:

- **SELECT**: what reached the model through lean-ctx: files read and
  commands run.
- **CONTROL** (`⛨`): rules enforced before the handoff: secrets kept out of
  context, risky commands blocked, paths outside the project blocked,
  injections flagged.
- **TOKENS**: tokens kept out of the model's context, their share of the tool
  output, and re-reads served from cache.

A group with nothing measured is left out. `NO_COLOR` drops the colours and
`LEAN_CTX_ASCII` the glyphs. `statusline = "subtle"` restores the single dim
line the other channels use (`◆ lean-ctx −1.2M tok · 41 cached · ⛨ 3`).

The numbers are those of the lean-ctx session serving this conversation.
When several agents work in the same project (two Claude windows, a Codex
task), each starts its own lean-ctx server; the server records which agent
process started it, and the status line picks the session whose agent is one
of its own ancestors. Only when none matches, for example a server from an
older lean-ctx, does it fall back to the project's most recent session. The
Stop hook's recaps pick their session the same way.

If you already have a status line, `init` leaves it alone and prints the
command that chains both:

```bash
lean-ctx statusline --wrap '<your command>'
```

Your command gets the same input, and lean-ctx's segment is appended to its
first line. `lean-ctx uninstall` gives your command back.

The Stop hook adds a one-line recap every 5 turns when it is worth one, that
is when the window saved at least 10K tokens or had a security event
(`◆ lean-ctx · last 5 turns: −48.2K tokens`). On a fresh start the last
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

lean-ctx does not report speed. A session has no comparison run without
lean-ctx, and the signed A/B speed proof (`prove speed`) was removed with
the engine's evaluation tooling.

## VS Code, Cursor and Windsurf

The lean-ctx extension (`packages/vscode-lean-ctx`) adds one status bar item
with the same segment as the shell prompt, for the workspace folder of the
file you are editing. In 3.10.3 it ships as source only and is not yet
published to an extension registry:

```text
◆ −1.2M tok ⛨ 3
```

Hovering it shows the breakdown, with each line labelled `✓` (counted) or `≈`
(derived). Clicking it runs `lean-ctx value` in a terminal.

The extension computes nothing itself. It calls:

```bash
lean-ctx prompt-segment --json --dir <workspace folder>
```

and refreshes when the snapshot directory named in the `watch` field changes,
at most every 1.5 seconds, plus once a minute. The item is hidden when:

- nothing was measured,
- the numbers are older than 12 hours,
- `mode = off` is set, or
- the binary is missing or older than 3.10.3.

The payload is also a stable contract (`schema: 1`) for other editors. The
values below are illustrative; actual values depend on the local activity that
was recorded and are not expected savings:

```json
{
  "schema": 1,
  "display": "minimal",
  "segment": "◆ −1.2M tok ⛨ 3",
  "tooltip": ["✓ 1.2M tokens kept out of context", "≈ 60% of 2.0M tokens of tool output", "✓ 3 secrets kept out of context"],
  "speed": null,
  "watch": "<data_dir>/value/projects",
  "verify": "lean-ctx value"
}
```

## Local Protection view and signed batches

The **Protection** page that `lean-ctx dashboard` serves on your machine
starts with **Guards that fired**. It shows the lifetime security counts from
`lean-ctx value --all` next to the audit-trail status:
`✓ audit trail intact · N entries`, or where the chain breaks.

Each signed savings batch (`lean-ctx savings sign`) carries a separately
signed security tally:

- the counts
- the audit trail's entry count, plus its first and last hash
- the batch's last entry hash, which binds the tally to that batch

`lean-ctx savings verify-batch` checks both signatures and lists the tally.
A copied or edited tally fails verification. A verifier that does not know
the tally still verifies the batch as before.

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
