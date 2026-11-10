# Hive background sync

A LeanCTX | OSS Instance connected to LeanCTX | Hive keeps every shared
vault in sync by itself. You do not need to run `lean-ctx hive sync`; it
still works and syncs the current project at once.

## Who does the work

The Instance's daemon (`lean-ctx serve --daemon`). `lean-ctx setup` and
`lean-ctx hive connect` install it as a supervised service, so it starts at
login and restarts after a crash:

- Linux: the systemd user unit `lean-ctx-daemon` (`Restart=on-failure`).
  On a headless machine run `loginctl enable-linger <user>` once, so the
  unit also runs while you are logged out.
- macOS: the LaunchAgent `com.leanctx.daemon` (`KeepAlive`).

`lean-ctx doctor` says "Hive sync needs a supervised daemon" when this
machine is connected to Hive and the service is missing; `lean-ctx daemon
enable` installs it.

## When a vault syncs

| What changed | When it reaches Hive |
| --- | --- |
| A fact remembered, updated (a conflict), forgotten or imported with `ctx_knowledge` | about 0.3 s after the write: the agent's own LeanCTX process pushes the vault, a burst of writes as one push, at most once per 2 s per project and within the vault's shared budget (below) |
| Another local change (a doc page written, a fact from the CLI) | 1 s after the last change, at the latest 5 s after the first of a stream of changes |
| Hive announced a change of the vault (bus `log.advanced`, `trust.advanced`) | at once, at least 2 s after the vault's previous sync (a purge without waiting) |
| Nothing | every `[hive] pull_interval_minutes` (default 5) |
| A task proposal | at once (the session sends it); queued with the vault's next sync when Hive cannot be reached |
| A task claim, update, submit | at once (the session sends it) |

So a fact remembered on one Instance is on the other Instances of the team
within a few seconds: pushed by the writer, then pulled by every other
Instance on Hive's `log.advanced` hint.

One vault syncs at most 6 times in a burst and then once per 10 seconds
(a purge excepted), counted
across every process of the machine: the daemon and each agent's push on
write share one budget per vault (`<data>/hive/sync/<vault>.budget.json`).
A spent burst holds a vault for 10 seconds at most, so a change from
another Instance is still pulled within seconds.
A manual `lean-ctx hive sync` is never held back, but it counts toward
that budget too. A write the budget holds back goes out with the daemon's
next sync. Offline, nothing is lost: signed events wait in the vault's
replica and go out with the next sync that reaches Hive. A failing vault
backs off from 30 s up to 30 min, for every process; when Hive answers
with a rate limit, the vault waits as long as its `Retry-After` asks. No
vault is held longer than 36 min, whatever Hive asks or the wall clock
does.

Only vaults already shared from this machine sync (`projects.json`). A
project that is not shared never leaves the machine, and the capture gate
decides fact by fact what is shared.

## Facts that stay local

`ctx_knowledge remember` ends with a `hive:` line: where the fact goes, or
"Saved on this machine only … (local only: <reason>)" with how to fix it,
for example a full sentence or a source (`refs`). Fix the fact and remember
it again; the next sync considers it again. `lean-ctx hive sync status
--json` (kind `hive.sync`) lists, per vault, the last sync, pending
events, errors and the facts the gate kept local.

## Claims nobody works on

A claim is renewed while its session works. When the session has ended
and its agent no longer works on this machine, or the claim has expired,
the daemon gives it back to Hive ("the claiming session ended on its
machine"), so another agent can take the task.

## Policies

Hive signs the policy in force. The Instance pulls it every
`pull_interval_minutes` and at once when Hive sends `policy.advanced` or
the bus reconnects; it verifies the signature, applies it and acknowledges
it. `lean-ctx hive policy sessions --json` (kind `policy.sessions`) shows the
installed version and which version each app session
(`LEAN_CTX_SESSION_ID`) ran under, with its receipts. Its
`installed.enforcement` names the version enforced here (verified, in force
and acknowledged), the live sessions enforcing it, and when the Instance
reported it to Hive: once per version, the bus frame `policy.applied`
carries the version, digest, acknowledgement time and the number of live
sessions. An expired or unverified policy is never reported as applied.
