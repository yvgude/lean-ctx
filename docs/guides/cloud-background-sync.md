# LeanCTX Cloud background sync

A LeanCTX | OSS Instance connected to LeanCTX Cloud keeps every shared
vault in sync by itself. You do not need to run `lean-ctx cloud sync`; it
still works and syncs the current project at once.

## Who does the work

The Instance's daemon (`lean-ctx serve --daemon`). `lean-ctx setup` and
`lean-ctx cloud connect` install it as a supervised service, so it starts at
login and restarts after a crash:

- Linux: the systemd user unit `lean-ctx-daemon` (`Restart=on-failure`).
  On a headless machine run `loginctl enable-linger <user>` once, so the
  unit also runs while you are logged out.
- macOS: the LaunchAgent `com.leanctx.daemon` (`KeepAlive`).

`lean-ctx doctor` says "LeanCTX Cloud sync needs a supervised daemon" when this
machine is connected to LeanCTX Cloud and the service is missing; `lean-ctx daemon
enable` installs it.

## When a vault syncs

| What changed | When it reaches LeanCTX Cloud |
| --- | --- |
| A fact remembered, updated (a conflict), forgotten or imported with `ctx_knowledge` | about 0.3 s after the write: the agent's own LeanCTX process pushes the vault, a burst of writes as one push, at most once per 2 s per project and within the vault's shared budget (below) |
| Another local change (a doc page written, a fact from the CLI) | 1 s after the last change, at the latest 5 s after the first of a stream of changes |
| LeanCTX Cloud announced a change of the vault (bus `log.advanced`, `trust.advanced`) | at once, at least 2 s after the vault's previous sync (a purge without waiting) |
| Nothing | every `[cloud] pull_interval_minutes` (default 5) |
| A task proposal | at once (the session sends it); queued with the vault's next sync when LeanCTX Cloud cannot be reached |
| A task claim, update, submit | at once (the session sends it) |

So a fact remembered on one Instance is on the other Instances of the team
within a few seconds: pushed by the writer, then pulled by every other
Instance on LeanCTX Cloud's `log.advanced` hint.

One vault syncs at most 6 times in a burst and then once per 10 seconds
(a purge excepted), counted
across every process of the machine: the daemon and each agent's push on
write share one budget per vault (`<data>/cloud/sync/<vault>.budget.json`).
A spent burst holds a vault for 10 seconds at most, so a change from
another Instance is still pulled within seconds.
A manual `lean-ctx cloud sync` is never held back, but it counts toward
that budget too. A write the budget holds back goes out with the daemon's
next sync. Offline, nothing is lost: signed events wait in the vault's
replica and go out with the next sync that reaches LeanCTX Cloud. A failing vault
backs off from 30 s up to 30 min, for every process; when LeanCTX Cloud answers
with a rate limit, the vault waits as long as its `Retry-After` asks. No
vault is held longer than 36 min, whatever LeanCTX Cloud asks or the wall clock
does.

Only vaults already shared from this machine sync (`projects.json`). A
project that is not shared never leaves the machine, and the capture gate
decides fact by fact what is shared.

## The session a write names

A fact, its `derived_from` edges and an episode written inside an agent
session carry a `session_ref` in the signed event, so your team sees in the
hosted LeanCTX which writes came from the same session. The value is `hs_`
and 32 hex characters of an HMAC-SHA256 over the LeanCTX session id, under
the receipt key that LeanCTX keeps on this machine and never uploads. It is
never the session id, a transcript, a path or a name: without the key, a
guessed session id cannot be tested against it. Only sessions LeanCTX
generated count; imports, maintenance runs and other labels carry none.
The older `ses_` reference in the body of an episode, a knowledge draft,
a task claim and a task proposal is keyed the same way, under a separate
label, so it cannot be tied to the session id or to the `hs_` value
either. It keeps its shape (`ses_` and 24 hex characters), so it is keyed
for every server, whether or not it announces `session_ref`. When the key
cannot be read or created, nothing that carries a session is uploaded: the
upload is skipped and reported on this machine, and sessions whose episode
was held back are sent by a later run once the key is back. Items queued by an older
version hold only the digest; they are keyed under their own label once,
before they are sent.

Context receipts (`[cloud] receipts_upload`) carry the session the app names
(`LEAN_CTX_SESSION_ID`) as an HMAC under the same key (`hmac-sha256:` and
64 hex characters), so the hosted LeanCTX can group policy evidence per app
session without learning the id. The app session and the LeanCTX session
are different ids, so an event's ref and a receipt's ref do not match each
other.

Both are sent only to a server that announces the `session_ref` feature in
`GET /v1/capabilities`. The Host asks when it connects and when it starts,
and keeps the answer per server; until a server announces the feature, no
event or receipt carries a session, so an older server never refuses one.

## Facts that stay local

`ctx_knowledge remember` ends with a `cloud:` line: where the fact goes, or
"Saved on this machine only … (local only: <reason>)" with how to fix it,
for example a full sentence or a source (`refs`). Fix the fact and remember
it again; the next sync considers it again. `lean-ctx cloud sync status
--json` (kind `cloud.sync`) lists, per vault, the last sync, pending
events, errors and the facts the gate kept local.

## Claims nobody works on

A claim is renewed while its session works. When the session has ended
and its agent no longer works on this machine, or the claim has expired,
the daemon gives it back to LeanCTX Cloud ("the claiming session ended on its
machine"), so another agent can take the task.

## Policies

LeanCTX Cloud signs the policy in force. The Instance pulls it every
`pull_interval_minutes` and at once when LeanCTX Cloud sends `policy.advanced` or
the bus reconnects; it verifies the signature, applies it and acknowledges
it. `lean-ctx cloud policy sessions --json` (kind `policy.sessions`) shows the
installed version and which version each app session
(`LEAN_CTX_SESSION_ID`) ran under, with its receipts. Its
`installed.enforcement` names the version enforced here (verified, in force
and acknowledged), the live sessions enforcing it, and when the Instance
reported it to LeanCTX Cloud: once per version, the bus frame `policy.applied`
carries the version, digest, acknowledgement time and the number of live
sessions. An expired or unverified policy is never reported as applied.
