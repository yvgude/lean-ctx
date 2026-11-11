# Managed configuration v1 (`leanctx.managed-config/v1`)

**Status:** Current (engine 3.12.3). Wire type:
`lean_ctx_protocol::hive::managed::ManagedConfigV1`. Carrier IO:
`lean_ctx_fs::managed`. Engine overlay and gates: `lean_ctx::core::config::managed`.

An organisation preconfigures machines through its device management (MDM).
The managed configuration names the Hive, the org, an identity-provider hint
and the update mode. It carries **no secret and no policy content**: policy
stays in the signed policy bundle the Hive delivers.

## Carriers

The engine reads exactly one carrier, chosen by platform:

| Platform | Carrier | Location |
|---|---|---|
| macOS | configuration profile (managed preferences) | `/Library/Managed Preferences/com.leanctx.managed.plist` |
| Windows | Group Policy registry key | `HKLM\SOFTWARE\Policies\LeanCTX\Managed` |
| Linux and other Unix | file | `/etc/lean-ctx/managed.toml` |

- The macOS profile and the Linux file must be owned by root and not be
  writable by group or others, and so must the directory that holds them;
  the file itself must not be a symlink, and neither may that directory
  (only the directory that directly holds the carrier is checked). Otherwise
  the carrier is refused. A missing directory is an absent carrier.
- macOS `Payload*` keys added by the profile tooling are ignored.
- A carrier is read at most once every five seconds per process.
- A carrier larger than 64 KiB is refused.

Absent carrier, or a carrier with no members: the machine is not managed.

## Members

Members may be nested (`[hive] url = …`) or dotted (`"hive.url" = …`); both
mean the same thing. A `schema` member, when present, must be
`leanctx.managed-config/v1`.

| Member | Type | Effect |
|---|---|---|
| `hive.url` | origin (`https://…`; `http://` only for loopback) | Wins over `[hive] url`. Enrollment with another origin is refused. |
| `hive.root_key` | base64 Ed25519 public key | Wins over `[hive] root_key`. |
| `hive.org` | org wire id | Enrollment or a stored session for another org is refused: "this machine is managed by <org>". |
| `hive.idp_hint` | text | Sign-in hint (consumed by the sign-in flow; reserved here). |
| `hive.require` | bool | Every tool refuses until the machine is signed in (lease valid or in grace) **and** a policy floor is installed. |
| `workspace.vaults` | list of vault ids (or one comma-joined string) | Reserved for vault provisioning. |
| `updates.mode` | `mdm` · `hive` · `user` | Reserved for the update channel (ent4-3). |
| `updates.feed_base` | `https://` URL | Reserved for the update channel (ent4-3). |
| `telemetry.metrics_upload` | bool | Tighten-only: `false` forces `[hive] metrics_upload` off. |
| `telemetry.receipts_upload` | bool | Tighten-only: `false` forces `[hive] receipts_upload` off. |

Registry and plist carriers may give booleans as `0`/`1` or
`"true"`/`"false"`.

## Rules

- **Managed members win.** The overlay is applied when the config loads and is
  never written back to `config.toml`. `lean-ctx config set` on a managed key
  is refused and the message names the managing org and the carrier.
- **Fail closed.** A carrier that exists but cannot be read or validated (an
  unknown member, a wrong type, another schema, a wrong owner) is refused as a
  whole. While it is refused, the Hive origin and root key are unavailable,
  and under `hive.require` (or any refused carrier) tools refuse.
- **No probe, no tools.** The `hive.require` gate asks the Hive connector
  whether the machine is signed in. A build without the connector refuses.

## Status

`lean-ctx hive status` prints a `Managed:` block (carrier, location, each
member and its value, a refusal reason if any). The cockpit and Local Service
shape `HiveCliStatusV1` carries it as the additive `managed` field (since 1.7,
omitted when the machine is not managed):

```json
"managed": {
  "carrier": "linux_file",
  "location": "/etc/lean-ctx/managed.toml",
  "members": [{ "member": "hive.org", "value": "org_…" }],
  "digest": "<sha256 of the canonical configuration>",
  "error": null
}
```

## Out of scope for v1

- Update enforcement for `updates.*` (ent4-3).
- Use of `workspace.vaults` and `hive.idp_hint` (ent4-7, ent1-5).
- Recording the managed digest in audit receipts (ent3).
- **Revocation on member removal.** Removing a member from an org in the Hive
  does not revoke that member's instances today. This is a Hive server change
  (ent4-6), not part of the engine contract.
