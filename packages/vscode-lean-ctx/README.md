# lean-ctx for VS Code, Cursor and Windsurf

One quiet status bar item that shows what lean-ctx did in the project you have open:

```
◆ −1.2M tok ⛨ 3
```

- **◆ −1.2M tok**: tokens lean-ctx kept out of your agent's context
- **⛨ 3**: guards that fired (secrets kept out of context, risky commands blocked, paths outside the project blocked, prompt-injection patterns flagged)

Hover the item for the breakdown. Each line is labelled by where its number comes from:

- `✓` counted from the savings ledger or the audit trail
- `≈` derived from counted values, such as a percentage

If you ran `lean-ctx prove speed`, the tooltip also shows the signed A/B result. The extension never shows a speed claim you haven't measured.

## Every number is provable

The extension computes nothing. It asks the lean-ctx binary (`lean-ctx prompt-segment --json`), which reads the same snapshot as your shell prompt.

To re-check any number, click the item (or run **lean-ctx: Show proof**). It runs `lean-ctx value`, which recomputes everything from the hash-chained ledger and audit trail and reports `TAMPERED` if anything was edited.

## Quiet by design

- Nothing is shown until something was measured. There is never a `0`.
- The numbers are hidden once they are older than 12 hours.
- There are no pop-ups or notifications, and nothing is ever sent to your agent's context.
- The item is hidden when `value_display.mode = "off"` is set in the lean-ctx config, or when `leanctx.statusBar.enabled` is `false`.

## Requirements

lean-ctx **3.10.3 or newer** on your machine. The extension finds the binary in this order:

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
