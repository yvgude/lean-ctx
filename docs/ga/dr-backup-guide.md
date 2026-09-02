# Disaster Recovery and Backup Guide

> **Status: historical operations guide.** It does not establish an available
> hosted, organization, or managed-data service. Confirm any command against the
> installed local Runtime; canonical product scope is in
> `docs/internal/README.md` (internal, not in this repository).

## Overview

This guide covers backups and recovery for a standalone lean-ctx installation
on macOS or Linux. It excludes Kubernetes deployments. Store encrypted backup
archives separately from the workstation.

| Recovery item | Objective |
| --- | --- |
| Binary replacement | Less than 2 minutes |
| Full restore | Less than 10 minutes |
| Cache warm-up | 5–30 minutes, depending on project size |

For supported lifecycle commands, see [Lifecycle](../reference/06-lifecycle.md).
For recovery diagnosis, see [Troubleshooting](../reference/12-troubleshooting.md).

## Prerequisites

- A working `lean-ctx` CLI on the source machine.
- Protected storage that survives loss of the workstation.
- `tar`, `sha256sum` (or macOS `shasum`), and `cron` for scheduled backups.
- Access to the encryption key or passphrase used for backup storage.

Run `lean-ctx status` before a backup. Stop lean-ctx before a full restore so
restored files cannot be overwritten concurrently.

## Steps

### 1. Back up required components

| Component | Path | Contents or purpose |
| --- | --- | --- |
| Configuration and custom patterns | `~/.config/lean-ctx/` | Settings, rules, and custom patterns |
| Local data | `~/.lean-ctx/` | Sessions, caches, knowledge base, savings ledger |
| Proxy LaunchAgent | `~/Library/LaunchAgents/com.leanctx.proxy.plist` | macOS proxy autostart |
| Agent MCP configuration | Per-editor MCP files | Agent-to-runtime connection settings |

Run `lean-ctx doctor integrations` to identify every configured editor, then
back up each MCP configuration file it reports. Editor paths vary by platform
and version, so the diagnostic output is the authoritative inventory.

### 2. Create a manual full backup

Set `BACKUP_ROOT` to mounted protected storage. This creates an archive and
checksum in one procedure.

```bash
BACKUP_ROOT=/Volumes/lean-ctx-backups
STAMP=$(date +%Y%m%d-%H%M%S)
ARCHIVE="$BACKUP_ROOT/lean-ctx-full-$STAMP.tar.gz"

mkdir -p "$BACKUP_ROOT"
lean-ctx status
tar -czf "$ARCHIVE" \
  "$HOME/.config/lean-ctx" \
  "$HOME/.lean-ctx" \
  "$HOME/Library/LaunchAgents/com.leanctx.proxy.plist"
sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
```

On macOS systems without `sha256sum`, use:

```bash
shasum -a 256 "$ARCHIVE" > "$ARCHIVE.sha256"
```

The archive command fails when an expected component is absent. Confirm the
path or omit a component only if it is not used on that machine, such as the
LaunchAgent on Linux. Copy the identified agent MCP configuration files to the
same backup destination before treating the backup as complete.

Create a configuration-only backup before changing settings:

```bash
BACKUP_ROOT=/Volumes/lean-ctx-backups
STAMP=$(date +%Y%m%d-%H%M%S)
ARCHIVE="$BACKUP_ROOT/lean-ctx-config-$STAMP.tar.gz"

mkdir -p "$BACKUP_ROOT"
tar -czf "$ARCHIVE" "$HOME/.config/lean-ctx"
sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
```

### 3. Schedule daily configuration and weekly full backups

Save the configuration-only and full procedures above as executable scripts at
`~/.local/bin/lean-ctx-backup-config` and `~/.local/bin/lean-ctx-backup-full`.
Each script must run `lean-ctx status`, create its archive, write a checksum,
and return non-zero on failure. Install these user-crontab entries:

```cron
0 2 * * * /bin/sh "$HOME/.local/bin/lean-ctx-backup-config"
0 3 * * 0 /bin/sh "$HOME/.local/bin/lean-ctx-backup-full"
```

Keep the backup destination outside the workstation. Apply retention cleanup
only after a newer archive has passed checksum and restore testing.

### 4. Verify a backup

Verify archives after creation and in periodic recovery tests:

```bash
cd /Volumes/lean-ctx-backups
sha256sum -c lean-ctx-full-YYYYMMDD-HHMMSS.tar.gz.sha256
tar -tzf lean-ctx-full-YYYYMMDD-HHMMSS.tar.gz
```

On macOS, compute the digest and compare it with the sidecar file:

```bash
shasum -a 256 lean-ctx-full-YYYYMMDD-HHMMSS.tar.gz
```

The checksum must match and the listing must include configuration and state
paths. Record the recovery-test date with the backup inventory.

### 5. Full restore on a fresh machine

1. Install lean-ctx and confirm the CLI starts.

   ```bash
   curl -fsSL https://leanctx.com/install.sh | sh
   lean-ctx status
   ```

2. Copy the verified archive and checksum to the target machine, then verify it.

   ```bash
   sha256sum -c lean-ctx-full-YYYYMMDD-HHMMSS.tar.gz.sha256
   ```

3. Stop lean-ctx, inspect the archive's leading paths, and extract it at the
   filesystem root.

   ```bash
   lean-ctx stop
   tar -tzf lean-ctx-full-YYYYMMDD-HHMMSS.tar.gz
   tar -xzf lean-ctx-full-YYYYMMDD-HHMMSS.tar.gz -C /
   lean-ctx setup --fix
   lean-ctx doctor
   ```

4. Restart every configured editor and use it to call `ctx_read`. Open normal
   projects to allow cache and index rebuilds.

### 6. Config-only and selective restore

For config-only recovery, retain `~/.lean-ctx/`, replace only configuration,
and restart:

```bash
lean-ctx stop
tar -xzf lean-ctx-config-YYYYMMDD-HHMMSS.tar.gz -C /
lean-ctx restart
lean-ctx doctor
```

For selective recovery, list the archive first. Extract the exact member name
shown by the listing for the component you need, then run its health check:

```bash
tar -tzf lean-ctx-full-YYYYMMDD-HHMMSS.tar.gz
tar -xzf lean-ctx-full-YYYYMMDD-HHMMSS.tar.gz -C / \
  Users/ACCOUNT/.lean-ctx/sessions
lean-ctx sessions doctor
```

Replace `Users/ACCOUNT/.lean-ctx/sessions` with the exact archive member printed
by the first command. Make a current configuration backup before extraction
because selective restore can overwrite current files.

### 7. Respond to disaster scenarios

| Scenario | Impact | Recovery |
| --- | --- | --- |
| Binary corruption | Proxy stops | Reinstall with `curl -fsSL https://leanctx.com/install.sh \| sh`, then run `lean-ctx doctor` |
| Config corruption | Parse errors | Restore config backup or reset documented settings, then `lean-ctx restart` |
| Cache corruption | Degraded performance | Run `lean-ctx cache prune`; cache rebuilds automatically |
| Data loss | Lost sessions/knowledge | Restore verified full backup, then `lean-ctx sessions doctor` |
| macOS update breaks code signing | SIGKILL on launch | Run `codesign --force --sign - ~/.local/bin/lean-ctx`, then `lean-ctx status` |

## Verification

After recovery, all checks must succeed:

```bash
lean-ctx status
lean-ctx doctor
lean-ctx doctor integrations
```

Confirm expected settings are active, an editor can call `ctx_read`, and restored
session or knowledge data appears where applicable. Cache warm-up is complete
when normal reads and searches no longer report index construction.

## Troubleshooting

- Never extract an archive with a mismatched checksum.
- For integration drift after restoring MCP settings, run `lean-ctx setup --fix`
  and restart the editor.
- For session recovery issues, run `lean-ctx sessions doctor --fix`.
- On macOS, use `lean-ctx stop` rather than killing the proxy; its LaunchAgent
  has keep-alive behavior.
- For Kubernetes HA, PDB, and volume-snapshot DR, use the private
  `lean-ctx-deploy-template` documentation; those enterprise procedures are
  outside this standalone guide.
