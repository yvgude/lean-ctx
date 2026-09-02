# Operational Runbook Index

> **Status: historical operations index.** It is not an availability commitment
> for a hosted, team, or organization service. LeanCTX's current product scope is
> governed by `docs/internal/README.md` (internal, not in this repository).

## Overview

These runbooks provide first response for standalone lean-ctx incidents. Run one
at a time, preserve relevant output, and use `lean-ctx report-issue` when a
supported repair does not resolve the fault. For wider symptom diagnosis, see
[Troubleshooting](../reference/12-troubleshooting.md).

## Prerequisites

- Run commands as the user that owns the lean-ctx installation.
- Start with `lean-ctx status`; use `lean-ctx raw "<command>"` to preserve
  exact uncompressed output for an incident record.
- Restart an editor after any MCP configuration change.
- Back up state before configuration or cache remediation; see the
  [DR and Backup Guide](dr-backup-guide.md).

## Steps

### RB-01: Proxy Not Responding

**Trigger**: Agent requests fail, proxy endpoints time out, or service status is unhealthy.

**Impact**: Agent requests lose compression, caching, and proxy service.

**Steps**:

1. Check health and capture the proxy log.

   ```bash
   lean-ctx status
   lean-ctx doctor
   lean-ctx raw "tail -n 200 ~/.local/state/lean-ctx/proxy.log"
   ```

2. Restart the runtime.

   ```bash
   lean-ctx stop && lean-ctx start
   lean-ctx status
   ```

3. On macOS, reload the LaunchAgent if it remains unavailable.

   ```bash
   launchctl unload "$HOME/Library/LaunchAgents/com.leanctx.proxy.plist"
   launchctl load "$HOME/Library/LaunchAgents/com.leanctx.proxy.plist"
   lean-ctx status
   ```

**Verification**: `lean-ctx status` reports a healthy proxy and a new agent request succeeds.

**Escalation**: Escalate with the log excerpt and `lean-ctx report-issue` output if restart fails.

### RB-02: High Memory Usage

**Trigger**: lean-ctx memory growth affects the workstation or the process is killed.

**Impact**: Agent responses slow, the system experiences pressure, or the proxy restarts.

**Steps**:

1. Inspect cache and runtime state.

   ```bash
   lean-ctx stats
   lean-ctx status
   ```

2. Reduce the cache limit and restart.

   ```bash
   lean-ctx config set cache.max_size_mb 512
   lean-ctx restart
   ```

3. If memory remains high, remove rebuildable cache data.

   ```bash
   lean-ctx cache prune
   lean-ctx restart
   ```

**Verification**: `lean-ctx stats` shows the bounded cache and memory stabilizes under normal use.

**Escalation**: Escalate if memory grows again after pruning; include `lean-ctx stats` output.

### RB-03: Agent Connection Failed

**Trigger**: An editor cannot connect or does not expose `ctx_*` tools.

**Impact**: The affected editor loses lean-ctx capabilities.

**Steps**:

1. Identify the affected integration.

   ```bash
   lean-ctx doctor
   lean-ctx doctor integrations
   ```

2. Rebuild the wrapper, replacing `cursor` with the affected agent name.

   ```bash
   lean-ctx unwrap cursor && lean-ctx wrap cursor
   lean-ctx doctor integrations
   ```

3. Fully quit and reopen the editor.

**Verification**: The editor can call `ctx_read` and integrations diagnostics report success.

**Escalation**: Escalate after a clean rewrap and restart still fail; attach diagnostics.

### RB-04: Compression Producing Incorrect Output

**Trigger**: Compressed output omits required content or materially differs from raw output.

**Impact**: The affected task can make a decision from incorrect context.

**Steps**:

1. Bypass compression for the command and preserve both outputs.

   ```bash
   LEAN_CTX_DISABLED=1 <command>
   lean-ctx raw "<command>"
   ```

2. Assess quality and inspect the runtime.

   ```bash
   lean-ctx quality-lab
   lean-ctx status
   ```

3. Adjust `compression_level` or add a narrow exception pattern in
   `~/.config/lean-ctx/config.toml`, then restart.

   ```bash
   lean-ctx restart
   lean-ctx quality-lab
   ```

**Verification**: The command retains required content without bypassing compression.

**Escalation**: Escalate with the command, raw output, compressed output, and sanitized config.

### RB-05: Release Integrity Verification Failed

**Trigger**: An artifact does not match `SHA256SUMS` or release verification fails.

**Impact**: The release must not be installed or promoted.

**Steps**:

1. Download `SHA256SUMS` and all listed assets from the published release.
2. Verify the downloaded directory.

   ```bash
   sha256sum -c SHA256SUMS
   python scripts/verify-release-integrity.py verify --tag vX.Y.Z --dir ./release-files
   ```

3. Reject mismatched files, delete the untrusted directory, and retrieve every
   asset again from the release.

**Verification**: Every checksum passes and the verifier exits successfully.

**Escalation**: Escalate a repeatable mismatch; never bypass verification. See
[Release Integrity v1](../contracts/release-integrity-v1.md).

### RB-06: Secret Rotation

**Trigger**: A secret is near expiry, exposed, revoked, or changed by policy.

**Impact**: CI, release automation, or repository integrations can fail.

**Steps**:

1. Check secret expiry.

   ```bash
   python scripts/check-secret-expiry.py
   ```

2. Rotate the credential in GitHub: **Settings → Secrets → Update**.
3. Re-run the expiry check.

   ```bash
   python scripts/check-secret-expiry.py
   ```

**Verification**: The script reports no expired or soon-expiring required secret.

**Escalation**: Escalate when the external credential owner is unavailable or CI remains unauthorized.

### RB-07: macOS Code Signing Issue (Sequoia+)

**Trigger**: `lean-ctx` receives `SIGKILL` immediately after launch on macOS.

**Impact**: Proxy and local runtime cannot start.

**Steps**:

1. Confirm the failure.

   ```bash
   lean-ctx status
   ```

2. Re-sign the managed binary and check again.

   ```bash
   codesign --force --sign - ~/.local/bin/lean-ctx
   lean-ctx status
   ```

**Verification**: `lean-ctx status` succeeds and the agent reconnects.

**Escalation**: Escalate if re-signing fails or the binary is not at the managed path.

### RB-08: Shell Command Blocked by Allowlist

**Trigger**: A command is denied by the lean-ctx shell allowlist.

**Impact**: The command cannot run through the compression hook.

**Steps**:

1. Read diagnostics and identify the exact rejected command.

   ```bash
   lean-ctx raw "tail -n 200 ~/.local/state/lean-ctx/proxy.log"
   lean-ctx status
   ```

2. Add only the required command to the shell allowlist in
   `~/.config/lean-ctx/config.toml`, then restart.

   ```bash
   lean-ctx restart
   ```

3. Use a one-command bypass only when immediate uncompressed execution is needed.

   ```bash
   LEAN_CTX_DISABLED=1 <command>
   ```

**Verification**: The intended command runs after a narrowly scoped allowlist change.

**Escalation**: Escalate if the command is unsafe to allowlist or remains blocked after restart.

## Verification

Close an incident only after the affected command or agent works, `lean-ctx
status` is healthy, and `lean-ctx doctor` has no unresolved relevant failure.
Record the runbook ID, trigger, repair, and verification result.

## Troubleshooting

- Use `lean-ctx doctor --fix` for ordinary integration drift.
- Use `lean-ctx sessions doctor --fix` for session-restoration failures.
- Preserve a backup before state changes; see the
  [DR and Backup Guide](dr-backup-guide.md).
- Consult the [security audit schedule](../contracts/audit-schedule-v1.md) for
  scheduled audit controls rather than treating an incident repair as an audit.
