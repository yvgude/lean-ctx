You're set up and using LeanCTX. Now you need to update it, fix something, or
remove it cleanly. This journey covers the whole lifecycle.

> **Just need to fix a specific symptom?** Jump to the central
> [Journey 12 — Troubleshooting Playbook](/docs/journeys/troubleshooting/)
> (symptom → diagnosis → fix). This journey covers the lifecycle *commands*
> themselves.

## 1. `lean-ctx update` — self-update from GitHub Releases

```bash
lean-ctx update             # check + install latest
lean-ctx update --check     # only report whether an update exists
lean-ctx update --insecure  # skip checksum verification (not recommended)
lean-ctx update --skip-rules # update without touching your rules files
```

**Under the hood:**

1. Fetches `releases/latest` from the GitHub API; compares the tag to your
   currently installed version.
2. If already current: prints "Already up to date", then still runs a **setup
   refresh** so your wiring stays correct after an editor update — unless
   `--check`.
3. If newer: downloads the platform asset (resolving your os/arch, including
   glibc vs musl on Linux), **verifies the SHA256 checksum** (refuses to install
   an unverifiable binary unless `--insecure`), then replaces the running binary
   safely:
   - macOS: unlink-then-rename (avoids SIGKILL from code-page revalidation),
     then re-`codesign`.
   - Windows: rename-out / rename-in, with a deferred `.bat` updater if the
     binary is locked by a running editor MCP server.
4. Re-runs setup to keep your wiring correct (see below).

### The post-update setup refresh — why your settings are safe

This is the step behind the old "update changed my settings" complaint. It:

- Re-enables the proxy **only if it was already active**.
- Decides whether to touch rules files: CLI `--skip-rules` always wins;
  otherwise it respects your `config.toml` rules opt-in. **If you never opted
  into rules, update will not write rules files.**
- Re-runs setup, which always refreshes MCP configs (so the editor reconnects to
  the new binary) but only touches rules when allowed.

> The unchanged version of any file LeanCTX edits is always in a sibling
> `*.lean-ctx.bak`. Rules edits only ever change content between
> `<!-- lean-ctx -->` markers.

### Auto-update scheduling

```bash
lean-ctx update --schedule        # enable 6-hourly auto-update
lean-ctx update --schedule 12h    # custom interval (1–168h)
lean-ctx update --schedule notify # check + notify, don't auto-install
lean-ctx update --schedule off    # disable
lean-ctx update --schedule status # show current schedule
```

Backed by a LaunchAgent (macOS) / systemd user timer (Linux). No mid-session
restarts — updates install in the background and take effect on next launch.

## 2. `lean-ctx uninstall` — clean removal

```bash
lean-ctx uninstall                 # remove everything lean-ctx wrote
lean-ctx uninstall --keep-config   # keep MCP configs + rules (for reinstall)
lean-ctx uninstall --dry-run       # preview every change, write nothing
```

**Under the hood** — removes, in order:

1. Shell hook + proxy env exports (RC files cleaned surgically).
2. MCP configs + rules files (unless `--keep-config`).
3. Agent hook files, plan-mode settings, skill dirs, project agent files.
4. Proxy autostart + daemon autostart (LaunchAgent/systemd).
5. Orphaned `.lean-ctx.bak` / `.tmp` backups across all known editor dirs.
6. The data directory (`~/.lean-ctx`, `~/.config/lean-ctx`) + project-local
   `.lean-ctx/` and `.lean-ctx-id`.

Every edit backs up first; successful surgical edits then clean their backups.

**The binary is not auto-deleted** (it may be running). It prints the right
removal command for your install method:

- cargo install → `cargo uninstall lean-ctx`
- Homebrew → `brew uninstall lean-ctx`
- everything else → `rm <path>`

…then: `command -v lean-ctx  # should print nothing once removed`.

## 3. `lean-ctx doctor [--fix]` — diagnose & repair

See [Journey 1 §6](/docs/journeys/setup-and-onboarding/).
For troubleshooting specifically:

- `doctor` shows what's wrong with an action-oriented footer.
- `doctor --fix` re-runs merge-based setup and repairs MCP/rules/hook drift.
- `doctor integrations` does deep per-editor checks (Cursor/Claude Code).

## 4. Process control — `stop`, `restart`, `dev-install`

```bash
lean-ctx stop          # stop ALL lean-ctx processes (daemon, proxy, orphans)
lean-ctx restart       # restart the daemon (applies config.toml changes)
lean-ctx dev-install   # build release + atomic install + restart (dev only)
```

> Important (macOS): the proxy runs as a LaunchAgent with `KeepAlive=true`. A
> plain `kill`/`pkill` will be respawned. `lean-ctx stop` unloads the LaunchAgent
> first, then terminates everything. Always `lean-ctx stop` before manually
> replacing the binary.

## 5. Emergency / "my shell is broken"

If a shell alias misbehaves:

```bash
lean-ctx-off           # disable all aliases for the current session
lean-ctx uninstall     # permanent: remove all hooks
```

Aliases are designed to fall back to the original command if the binary is
missing, so a broken/removed binary never bricks your shell. The
`LEAN_CTX_DISABLED=1` env var bypasses all compression and prevents the hook
from loading at all.

## 6. Cache & storage maintenance

```bash
lean-ctx cache list          # show file-read cache entries
lean-ctx cache stats         # cache size + hit stats
lean-ctx cache invalidate <file>  # drop one file from the read cache
lean-ctx cache clear         # clear the read cache
lean-ctx cache reset [--project]  # reset all cache (or just this project)
lean-ctx cache prune         # remove oversized/quarantined/orphaned indexes (BM25 + graphs)
```

Use `cache invalidate <file>` for surgical eviction (e.g. a file changed outside
the watcher); `cache reset --project` wipes only the current project's cache,
while `cache reset` wipes everything.

**Golden output — `lean-ctx cache stats`** reports the read cache size and how
often re-reads were served from it (each hit is a ~13-token read instead of a
full file):

```text
CLI Cache Stats:
  Entries:   1
  Reads:     3
  Hits:      1
  Hit Rate:  33%
```

The doctor warns when the BM25 cache has quarantined indexes or when the archive
FTS approaches its size cap — both are resolved by the commands above.

## 7. Platform notes (Windows / cross-platform)

LeanCTX runs on macOS, Linux, and Windows. A few behaviors are platform-specific:

**Path display.** All file paths in tool output are normalized to forward
slashes (`C:/Users/you/proj/src/main.rs`), even on Windows. Forward slashes are
valid path separators on Windows, and — unlike backslashes — they are never
misinterpreted as escape sequences by the JSON, markdown, or terminal layers of
MCP clients. (Earlier versions could render `C:\Users\…` as `CUsers…` in some
clients; that is fixed.) This is purely a display normalization; the underlying
file operations use native paths.

**Data directory.** On Windows the data dir resolves the same way (§ [paths
reference](https://github.com/yvgude/lean-ctx/blob/main/docs/reference/appendix-paths-and-config.md)):
`%LEAN_CTX_DATA_DIR%` → `~/.lean-ctx` with markers → XDG → fallback. `~` is your
user profile (`C:\Users\<you>`).

**Shell hook.** PowerShell uses
`~/Documents/PowerShell/Microsoft.PowerShell_profile.ps1`; Git Bash / MSYS2 uses
the bash hook. LeanCTX auto-detects MSYS-style `/c/Users/...` paths and converts
them to `C:/Users/...`.

**Autostart.** Windows has no LaunchAgent/systemd equivalent wired up; the proxy
and daemon run on demand rather than via an OS autostart unit.

If a path ever looks wrong in tool output, run `lean-ctx doctor` and, if it
persists, file an issue with the exact rendered path and your client name.

## 8. Reporting a problem — `report-issue`

When something is wrong and `doctor --fix` didn't resolve it, LeanCTX can open a
pre-filled GitHub issue that bundles your diagnostics:

```bash
lean-ctx report-issue              # (alias: lean-ctx report)
```

This gathers version, platform, integration status, and recent diagnostics into
an issue template so maintainers get a reproducible report without you hand-
collecting it. Review the contents before submitting — nothing is sent without
your confirmation, and secrets are not included.

> Best practice: run `lean-ctx doctor --json` first, attach that output, and
> describe the exact command and the client (Cursor/Claude/…) you were using.
