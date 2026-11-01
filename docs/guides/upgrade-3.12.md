# Upgrading to LeanCTX 3.12

3.12 makes the local Engine and the whole Cockpit free and local: no account, no
sign-in, every view on your machine. The hosted services that needed an account close.
LeanCTX Hive (shared knowledge for teams and your own machines) follows with 3.12.1.
This guide says what changes for you, depending on how you use LeanCTX today. The full list of changes is in the [CHANGELOG](../../CHANGELOG.md); the
focus changes that ship in the same release (removed MCP tools, the Cockpit replacing the
built-in dashboard and more) are described in
[focus-2026-10.md](../migration/focus-2026-10.md).

## At a glance

| You use today | What 3.12 changes | What you do |
| --- | --- | --- |
| LeanCTX without an account (OSS) | Every Cockpit view is free; a few hosted extras are gone | Update. Nothing else. |
| Pro | Personal Cloud closes; every former Pro view of the Cockpit is free and local | Update. Your local data stays. |
| Team (hosted team server) | The team server closes | Update. Your local data stays. |
| Personal Cloud | Closes on the release day; stored content is deleted 30 days after release | Update. Your local data stays. |
| Enterprise | Enterprise plans are arranged by contract | Contact us |

## What goes away, and when

| Service | Closes | What happens |
| --- | --- | --- |
| Personal Cloud (cloud sync, knowledge and gotcha sync, hosted personal index, cloud dashboard metrics) | On the 3.12.0 release day | Writes stop and the hosted endpoints answer `410` with the error code `personal_cloud_ended`. Stored content is deleted 30 days after release. There is no export. |
| Hosted per-customer team servers | On the 3.12.0 release day | No new team servers are created and existing ones stop. Stored data is deleted 30 days after release. |
| ctxpkg.com registry | On the 3.12.0 release day | Publishing stops and the site and its packages are archived, without migration. The `.ctxpkg` package format stays. |
| Pro gating, teasers and upgrade prompts | 3.12.0 | Gone. Former Pro views of the Cockpit are free. |
| Leaderboard, README badge, hosted Wrapped publication, billing CLI | 3.12.0 | Removed from the Engine. Local Wrapped cards (`gain --wrapped`, `--svg`, `--share`, `--copy`, `--open`) stay. |

Removed commands and settings in 3.12:

- `lean-ctx login`, `register`, `forgot-password`, `account`, `sync`, `contribute`,
  `cloud`, `enterprise`, `team` and `billing`;
- `lean-ctx badge` and `gain --publish`, `--leaderboard`, `--link`, `--rejoin`, `--unpublish`;
- `pack update`, `pack publish`, remote `pack install <ns>/<name>` and
  `addon add <ns>/<name>`; packages and add-ons install from local files;
- `savings push` and `savings team`;
- the `[cloud]` and `[gain]` configuration sections and the `team_url`, `team_token` and
  `team_auto_push` keys.

An existing `config.toml` that still sets a removed key keeps loading. The automatic
config migration removes retired keys from a writable global config, and `lean-ctx doctor`
names them instead of reporting them as unknown. The setting
`cloud.contribute_enabled` keeps being migrated to `telemetry.enabled`.

## Pro and Team

1. Update LeanCTX to 3.12 (`lean-ctx update`, or the package manager you installed with).
2. Nothing else: the Cockpit (`lean-ctx cockpit`) shows every former Pro view locally,
   without an account.

Your hosted team server closes on the 3.12.0 release day. The `team` CLI and the team
server settings (`team_url`, `team_token`) are no longer used. Every engine keeps its
own project knowledge, sessions and index on its machine; nothing needs to be exported.

## What happens to your local data

**Nothing on your machines is deleted by the update**, with one bounded exception.
Project knowledge, sessions, the local index, savings ledger, gotchas and every other
file in the LeanCTX data directory stay where they are and keep working without an
account.

- **Decision receipts** (`<data_dir>/gateway`) are now kept 30 days and below the
  store's ceiling (1024 MB unless `LEAN_CTX_GATEWAY_STORE_MAX_MB` or `max_disk_mb`
  says otherwise). On the first start after the update a long-used store is packed
  and its oldest receipts beyond that are removed in the background; the last
  24 hours, each project's latest receipts and those of tasks in progress are always
  kept. To keep older receipts, copy `<data_dir>/gateway` and
  `<data_dir>/audit/trail.jsonl` away before updating, or raise
  `LEAN_CTX_GATEWAY_RETENTION_DAYS` / `LEAN_CTX_GATEWAY_STORE_MAX_MB`.
  `lean-ctx doctor` shows the store and its last maintenance.
- **The audit trail** (`<data_dir>/audit/trail.jsonl`) is kept whole, but no longer as
  one growing file. Once 3.12 has verified it, it is sealed into segments at 64 MB or
  at the first entry of a new day: each segment is signed, renamed to
  `<data_dir>/audit/trail.segments/<n>.jsonl` and never written again, and the trail
  you have today becomes segment `000000`. Nothing is deleted. `lean-ctx audit`
  verifies the whole chain across all segments. To copy or archive the trail, take
  `trail.jsonl` and the `trail.segments/` directory together.

- **Personal Cloud users:** the copies on your machines stay; only the hosted copy closes
  and is deleted 30 days after release. Nothing is exported.

## Security changes that may need a step

- **Project provider configs need a trusted workspace.** A project's own
  `.lean-ctx/providers.toml` (or `.lean-ctx/providers/`) is loaded only once you trust that
  workspace. If a project's providers are missing after the update, run `lean-ctx trust` in
  the project (or `lean-ctx trust <path>`); `lean-ctx trust status` shows the current state
  and `lean-ctx untrust` withdraws it.
- **OKF paths resolve against the project root.** `ctx_knowledge` OKF export and import take
  a relative `path` from the project root, not from the server's working directory, and a
  path outside the project is refused.
- **Context packages from other publishers need a pinned key (3.12.1).** `lean-ctx pack
  import`, `pack install --file` and `pack receive` refuse a package whose signer is not
  pinned in `trusted-signers.toml`, a local identity or this machine. Pin the publisher's key,
  or pass `--allow-untrusted-signer` to accept a validly signed package from an unknown key.
- **Addons from other publishers need a pinned key too (3.12.1).** `lean-ctx addon add`
  applies the same rule to executable addons: an addon signed by a key that is not pinned, a
  local identity or this machine is refused after the consent prompt. Addons you built with
  `addon release` on this machine install as before; for a third-party addon pin its
  publisher's key or pass `--allow-untrusted-signer`.
- **`compliance verify` can exit with 3 (3.12.1).** A report signed by a key other than this
  installation's is reported as `SELF-SIGNED / UNPINNED` with exit code 3. Scripts that verify
  reports from another machine pass that machine's key with `--pubkey <hex>`.
- **Writes outside the active project need an allow entry (3.12.1).** The default
  `path_jail_scope` is `projects`: tools still read every project below `~`, but write only
  in the session's project and in paths you allowed. A write into another project is refused
  with the command that admits it: `lean-ctx allow-path <dir>` for one directory, or
  `lean-ctx config set path_jail_scope home` to read and write all of `~` as in 3.11.3 to
  3.12.0. Loose files outside a project (`~/Documents`, `~/Downloads`) are no longer readable
  under the default either. If you set `path_jail_scope` yourself, nothing changes.
- **Shell security settings are read from your own config only (3.12.1).** The shell gate
  (the hooks, `lean-ctx -c` and `ctx_shell`) reads `shell_security`, `shell_allowlist`,
  `shell_allowlist_extra`, `shell_allowlist_subcommand_scoping`, `shell_strict_mode`,
  `shell_allow_inline_scripts`, `shell_allow_writes`, `shell_approval` and `shell_hook_mode`
  from the `config.toml` in the
  home directory your account has in the operating system (`~/.config/lean-ctx/`, or
  `~/.lean-ctx/` on an older layout), and no longer from the environment variables
  `LEAN_CTX_SHELL_SECURITY`, `LEAN_CTX_SHELL_ALLOWLIST`, `LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE`,
  `LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS`, `LEAN_CTX_SHELL_ALLOW_WRITES`,
  `LEAN_CTX_SHELL_APPROVAL`, `LEAN_CTX_SHELL_HOOK_MODE` or `LEAN_CTX_ALLOWLIST_WARN_ONLY`. If
  you set one of them, or keep your config elsewhere with `LEAN_CTX_CONFIG_DIR`,
  `XDG_CONFIG_HOME` or a different `HOME`, put these settings in that default `config.toml`;
  every other setting keeps its location. Without such a file the secure defaults apply.
- **lean-ctx calls from an agent's shell are limited (3.12.1).** An agent may run reads,
  views and searches (`read`, `grep`, `ls`, `gain`, `doctor`, `semantic-search`, …), `-c`,
  and the read actions of `config`, `knowledge` and `session`; `config set` only for
  presentation and compression keys. Everything else — `setup`, `init`, `allow`, `trust`,
  `config set` of other keys, `doctor --fix`, `dev-install` and the like — you run in your
  own terminal. `make dev` and `lean-ctx dev-install` replace the binary that enforces the
  gate: run them yourself in your own terminal. (An agent can still run `make`, which is
  admitted code under the limit described next.)
- **The allowlist is not a sandbox.** Code the allowlist admits — a script such as
  `python3 x.py`, a compiled program, `cargo run` in a lean-ctx checkout, a copied lean-ctx
  binary — runs with your account's rights and can still change the config in your home.
  The gate checks command lines, not what admitted programs do (#1367); only an OS sandbox
  closes that.

## Going back to 3.11

Downgrading to 3.11.x after 3.12 has sealed the audit trail is not supported for audit
verification. A 3.11 Engine reads only `trail.jsonl`; its first entry then links to the
seal of the previous segment instead of the start of the chain, so 3.11 reports the audit
chain as invalid, and with it the features that rely on it (attestation drift, signed
security tallies). Your data is unaffected: the segments stay in place, and the trail
verifies again once you are back on 3.12. Back up the data directory before you try a
downgrade.

## Telemetry is unchanged

3.12 sends exactly the same telemetry as 3.11, to the same endpoint, at the same cadence,
under the same consent. The code moved out of the removed cloud client without a change in
behaviour. Check or change it with:

```shell
lean-ctx telemetry status
lean-ctx telemetry show     # what would be sent
lean-ctx telemetry off      # or DO_NOT_TRACK=1, or LEAN_CTX_TELEMETRY=off
```
