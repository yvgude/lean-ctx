# LeanCTX — Context Gateway for VS Code, Cursor and Windsurf

Inspect **LeanCTX Engine** activity in the project you have open. The extension
is part of LeanCTX, the **Context Gateway for AI Systems**.
**Control what your AI can see.** Its status item reports recorded activity:

```
◆ −1.2M tok ⛨ 3
```

- **◆ −1.2M tok**: reported context-token reduction
- **⛨ 3**: recorded guard events, such as redactions, blocked commands or paths, and flagged patterns

Hover the item for the breakdown. Each line is labelled by where its number comes from:

- `✓` counted from the savings ledger or the audit trail
- `≈` derived from counted values, such as a percentage

If you ran `lean-ctx prove speed`, the tooltip also shows the signed A/B result. The extension never shows a speed claim you haven't measured.

## Inspect the recorded evidence

The extension computes nothing. It asks the lean-ctx binary (`lean-ctx prompt-segment --json`), which reads the same snapshot as your shell prompt.

To inspect a number's source, click the item (or run **lean-ctx: Show proof**).
It runs `lean-ctx value`, which recomputes recorded values from the ledger and
audit trail and checks their integrity. A valid chain does not establish complete
detector coverage, a complete model prompt, answer quality, or paid-invoice savings.

## Quiet by design

- Nothing is shown until something was measured. There is never a `0`.
- The numbers are hidden once they are older than 12 hours.
- There are no pop-ups or notifications, and nothing is ever sent to your agent's context.
- The item is hidden when `value_display.mode = "off"` is set in the lean-ctx config, or when `leanctx.statusBar.enabled` is `false`.

## Semantic bridge

lean-ctx keeps a code graph of your project (who calls whom, who implements
what). Names alone are ambiguous — five `save()` methods look the same — so
lean-ctx can ask this editor where a call really goes. The extension answers
with the editor's own language features (go to definition, references,
implementations, type hierarchy), from whatever language extensions you have
installed; TypeScript and JavaScript work out of the box.

- **Local and read-only.** One server per workspace folder on `127.0.0.1`,
  behind a random token that changes with every window; only lean-ctx on
  this machine can find it. It never edits anything.
- **Confined.** Requests may only name files inside the workspace folder.
- **Visible.** The **lean-ctx** output channel shows which folders are served.
- **Off switch.** `leanctx.semanticBridge.enabled = false`.

The bridge announces itself where `lean-ctx editor-bridge dir` says. Details:
[semantic code intelligence guide](../../docs/guides/semantic-intelligence.md).

## Requirements

lean-ctx on your machine: **3.10.3 or newer** for the status bar; the
semantic bridge needs a lean-ctx with `lean-ctx editor-bridge` (newer than
3.10.5) — with an older binary it stays off and says so in the **lean-ctx**
output channel. The extension finds the binary in this order:

1. `leanctx.binaryPath`
2. your `PATH`
3. `~/.local/bin`
4. `~/.cargo/bin`
5. `/opt/homebrew/bin`
6. `/usr/local/bin`

## Commands

| Command | What it does |
|---|---|
| lean-ctx: Show proof | `lean-ctx value` in a terminal |
| lean-ctx: Open dashboard | `lean-ctx dashboard` in a terminal |
| lean-ctx: Refresh status bar | re-reads the numbers now |

## Development

```bash
npm ci
npm test          # typecheck + node --test
npx @vscode/vsce package
```
