# App presence v1

Status: Experimental · Since: 3.12 · Schema id: `leanctx.app-presence/v1`

LeanCTX Code, the free desktop app, can manage the LeanCTX installation on a
machine. This contract is the one explicit signal it sets so that the
LeanCTX CLI and local dashboard can tell. Nothing else is used for this:
no process scan, port probe or environment variable.

## Marker file

The app writes one JSON file into the LeanCTX data directory (the directory
`lean-ctx doctor` reports; `LEAN_CTX_DATA_DIR` when set):

```
<data dir>/app/leanctx-code.json
```

A v1 marker, as LeanCTX Code writes it:

```json
{
  "schema": "leanctx.app-presence/v1",
  "app": "leanctx-code"
}
```

| Field | Required | Meaning |
|---|---|---|
| `schema` | yes | exactly `leanctx.app-presence/v1` |
| `app` | yes | exactly `leanctx-code` |
| `open_url` | no | reserved for a later version: a link that opens the app. v1 writers omit it, because v1 has no `leanctx://` scheme. Readers must handle its absence. A reader that sees it accepts only `leanctx://` URLs of the characters `A-Z a-z 0-9 / . _ - ? = :` (no `&`, so the link goes into an HTML attribute unchanged), at most 256 bytes, and ignores any other value. |

Rules for the app:

- Write the marker on every app start when it finds a local LeanCTX
  installation. Rewriting an existing marker with the same content is
  expected.
- Never remove the marker: not when it stops, not on uninstall. A stale
  marker, left behind after the app was removed, is accepted and still
  reads as "managed". Only a missing or invalid marker means "not managed".
- Write the file atomically (temporary file, then rename), owner-only
  permissions, at most 4 KiB, as a regular file (never a symlink). Never put
  a path, account, token or other personal data into it.
- Do not write `open_url` in v1.

LeanCTX treats an unreadable or oversized file, a symlink, a missing `schema`
or `app`, another schema version or another app id as "not managed". Unknown
extra fields are ignored.

## What LeanCTX does with it

- **Local dashboard.** `GET /api/app` (behind the dashboard token like every
  `/api/*` route) answers
  `{"app":"LeanCTX Code","managed":true|false,"open_url":null,"schema":"leanctx.app-presence/v1"}`.
  `open_url` is always present in the answer and is `null` for a v1 marker;
  it carries a link only once a later version defines one (the reader
  already passes a value through when it meets the rules above). The
  Cockpit can show "Managed in LeanCTX Code" on the views the app owns and
  hide its own controls for them. Without the Cockpit, the start page shows
  the same calm state with "Open LeanCTX Code from your applications."
  instead of the install instructions.
- **CLI.** While the marker is present, `lean-ctx doctor` does not show the
  hint about LeanCTX Code.
- **Telemetry.** A `lean-ctx doctor` run that finds the marker counts the
  content-free feature code `app.code.detected`; the shown hint counts
  `app.code_hint.shown` (see `docs/privacy/TELEMETRY-DATA-DICTIONARY.md`).
  The marker's content is never sent.

Implementation: `rust/src/core/app_presence.rs`.
