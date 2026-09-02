# lean-ctx Upgrade Guide

> **Status: local Runtime upgrade reference.** It does not authorize website,
> Cloud, or managed-service changes. Confirm the installed Runtime with
> `lean-ctx doctor`; canonical scope is in
> `docs/internal/README.md` (internal, not in this repository).

## Overview

Use this guide to update an installed lean-ctx release, verify its integration
wiring, and restore the prior binary if an upgrade must be reversed. For normal
installations, `lean-ctx update` is the supported update path: it downloads the
platform release, verifies its checksum, replaces the binary safely, re-signs it
on macOS, and refreshes setup wiring.

Read the target release entry in the [changelog](../../CHANGELOG.md) before each
upgrade. For lifecycle commands and process-control details, see the
[lifecycle reference](../reference/06-lifecycle.md).

## Prerequisites

- Approval for the target release and its documented breaking changes.
- A known-good backup of local configuration and a retained copy of the current
  binary when a fast rollback is required.
- GitHub Releases access for `lean-ctx update`, or an approved,
  checksum-verified release artifact for offline upgrades.
- A maintenance window for shared gateways and time to restart AI tools after
  MCP configuration changes.
- For source builds, a current Rust toolchain and enough local resources for a
  release build.

## Version policy

lean-ctx releases follow semantic versioning. Plan upgrades so agents, their MCP
configuration, and the deployed lean-ctx binary are on the current release or
the immediately preceding compatible release (N/N-1). Do not assume a major
version preserves configuration, command, or API compatibility.

Before approving the target:

1. Compare the installed and target versions with `lean-ctx --version` and the
   [changelog](../../CHANGELOG.md).
2. Identify entries labelled breaking, migration, security, or deprecation.
3. Validate the target on a representative workstation or non-production
   gateway before broad rollout.
4. Retain the prior binary and deployment manifest until verification passes.

## Steps

### 1. Complete the pre-upgrade checklist

Capture baseline diagnostics before replacement:

```bash
lean-ctx --version
lean-ctx status --json
lean-ctx doctor --json
lean-ctx update --check
```

Back up organization-approved configuration and the installed binary through the
normal endpoint-management or artifact-retention process. lean-ctx creates
`*.lean-ctx.bak` siblings before editing configuration, but these do not replace
a rollback-ready binary or gateway data backup.

Review whether the proxy was enabled. The update refresh preserves that posture:
it re-enables the proxy only when it was already active, and respects the
existing rules-injection choice unless `--skip-rules` is specified.

### 2. Upgrade a release installation

Use the built-in updater for routine upgrades:

```bash
lean-ctx update
```

Check availability without changing the machine:

```bash
lean-ctx update --check
```

To refresh integration wiring without changing rules files:

```bash
lean-ctx update --skip-rules
```

The updater compares the installed version to the latest release, verifies the
asset against `SHA256SUMS`, replaces the binary atomically, and runs a
non-interactive wiring refresh. Avoid `--insecure` except for an explicitly
approved exception; it bypasses checksum verification.

Re-running the installer is also safe: it stops the running instance before its
atomic replacement.

```bash
curl -fsSL https://leanctx.com/install.sh | sh
```

For a Homebrew-managed installation, use the package manager’s normal path when
the formula is available:

```bash
brew upgrade lean-ctx
```

### 3. Upgrade from a release artifact manually

Use this route only when an approved release archive is supplied outside the
updater, such as in an air-gapped environment. Verify its SHA-256 checksum
before replacing the existing executable.

On macOS, the proxy is a LaunchAgent with `KeepAlive=true`; a direct kill can
make it respawn. Stop lean-ctx before the file swap:

```bash
lean-ctx stop
```

Install the verified binary at the approved path. On macOS Sequoia or later,
ad-hoc sign a manually supplied binary before restarting it:

```bash
codesign --force --sign - ~/.local/bin/lean-ctx
launchctl load ~/Library/LaunchAgents/com.leanctx.proxy.plist
```

The `launchctl load` command applies only when the proxy was enabled and its
LaunchAgent plist remains installed. Prefer lean-ctx lifecycle commands when
setup manages the service; do not load a plist that does not exist.

### 4. Upgrade a source checkout

For contributors building the checked-out source, build first without stopping
the installed runtime:

```bash
git pull
cd rust
cargo build --release
```

After a successful build, perform the atomic development install:

```bash
lean-ctx dev-install
```

`dev-install` builds the release binary, installs it atomically, and applies
macOS ad-hoc signing. It is not the standard release-channel updater; use
`lean-ctx update` for production clients.

### 5. Upgrade an enterprise gateway

Use the versioned Helm chart and container image from `lean-ctx-deploy-template`.
Follow that repository’s chart upgrade procedure, image compatibility matrix,
schema migration notes, and rollback instructions. Do not replace a running
gateway container in place without preserving configuration, secrets, and
Postgres data.

For a generated single-host gateway, run preflight after replacing the approved
image or Compose artifact:

```bash
lean-ctx gateway doctor --dir gateway
```

## Verification

Restart affected AI tools, then run:

```bash
lean-ctx --version
lean-ctx doctor
lean-ctx status
lean-ctx doctor integrations
```

`doctor` verifies the binary, data directory, shell hook, daemon, proxy, MCP
configuration, and capacity checks. `status` reports the current diagnostic
ratio and integration wiring. `doctor integrations` is the final check when an
upgrade refresh changed a client configuration.

When the local proxy is enabled, confirm process state too:

```bash
lean-ctx proxy status
lean-ctx daemon status
```

For gateways, run `lean-ctx gateway doctor --dir gateway` and the health probe
defined by the deployment. Resolve every failed preflight check before routing
production traffic to the new version.

## Rollback procedure

1. Stop lean-ctx before restoring the previous binary:

   ```bash
   lean-ctx stop
   ```

2. Restore the retained, checksum-verified prior binary through the approved
   endpoint or artifact procedure.
3. On macOS Sequoia or later, sign the restored binary:

   ```bash
   codesign --force --sign - ~/.local/bin/lean-ctx
   ```

4. Reload the enabled proxy LaunchAgent:

   ```bash
   launchctl load ~/Library/LaunchAgents/com.leanctx.proxy.plist
   ```

5. Run `lean-ctx --version`, `lean-ctx doctor`, and `lean-ctx status`, then
   restart affected AI tools.

For a gateway, roll back the Helm release or approved Compose image and restore
configuration and database only according to its rollback plan. Do not downgrade
a database schema without an explicitly supported migration path.

## Troubleshooting

### The updater says current but an integration fails

An unchanged release still performs a setup refresh when `lean-ctx update` runs.
Restart the AI tool, then use the safe repair:

```bash
lean-ctx doctor --fix
lean-ctx doctor integrations
```

### A manually replaced macOS binary exits or is rejected

Stop the LaunchAgent-safe runtime, ad-hoc sign the exact replacement binary,
then reload the enabled LaunchAgent as shown in the manual-upgrade procedure.
Never use `kill` or `pkill` to replace a KeepAlive proxy.

### The upgrade cannot download or verify a release

Check firewall and proxy policy for GitHub Releases. For controlled networks,
use the approved offline artifact and checksum; do not routinely bypass
verification with `--insecure`.

### An upgrade has breaking changes

Pause rollout, read the target changelog entry and linked migration notes, then
test against the N/N-1 plan. If the change cannot finish in the window, roll
back and retain diagnostic evidence for follow-up.
