# Local Read v1

Status: **experimental** (`"contract": "local-read/1"`).

Wire ids keep their 3.12 spelling in this contract version: kinds `cloud.*`, the
command groups `cloud`, `cloud.docs`, `cloud.tasks`, `cloud.policy` and `cloud.sync`,
the reasons `awaiting_cloud` and `other_cloud`, knowledge categories `cloud/…` and
the report contract `cloud-savings-report/1`. The CLI command is `lean-ctx cloud`;
`lean-ctx cloud` stays a hidden alias, so every `cli` array keeps working.

Changes after 3.12.6 (additive):
- New kind `cloud.models.catalog` (§32, `GET /v1/local/cloud/models/catalog`, listed in
  capabilities under `cloud.models`: `catalog`; also `lean-ctx models --json`).
- `cloud.presence` rows (§4) add `person_name`, `machine_name` and `working_on` (team
  presence; always printed, `null` when the Cloud does not relay them).
- New kind `cloud.usage` (§29, CLI only, listed in capabilities under `cloud`: `usage`).
- New kind `cloud.gateway.key` (§30, CLI only, listed in capabilities under
  `cloud.gateway`: `key`).
- `cloud.gateway.key` (§30) adds `state: too_many_keys` (Cloud answered
  `429 rate_limited` with `reason: gateway_keys`); `forbidden` also covers
  `403 plan_required`; Cloud's key document (`schema:
  "leanctx.cloud.gateway_key.v1"`, `models`) is accepted with fields it adds later.
- New kind `cloud.cpao` (§31, CLI only, listed in capabilities under `cloud`: `cpao`).
- `cloud.cpao` (§31) `by_agent[].cost_usd_micros` may be `null` (Cloud does not price per agent yet).
- `cloud.cpao` (§31) periods add `settled` (bool, absent when the cloud sent none);
  `cloud cpao` without `--json` prints lines for people instead of exiting 2.
- New kinds `cloud.computers`, `cloud.computer` and `cloud.computer.ack` (§44; `cloud.computers` and `cloud.computer.ack` also on the local service socket for LeanCTX Code; listed in
  capabilities under `cloud.computer`: `list`, `show`, `add`, `create`, `start`, `stop`), and the
  error codes `no_computer`, `balance_empty`, `computer_exists`, `computer_busy`,
  `computer_provider_unavailable`, `computer_not_available` and `confirmation_required` (§1). An `error` document
  may carry `error.details`: `confirmation_required` carries the terms
  `{billing, usd_per_hour, free_hours_left, topup_usd}`. `create` and `start` without
  `--yes` send nothing to LeanCTX Cloud.
- New kinds `run.ack`, `run.list`, `run.status` and `run.events` (§35, CLI only, listed in
  capabilities under `run`: `list`, `status`, `events`, `stop`, `fetch`); `run --cloud`
  sends no paid start without a terminal ("Starting a paid computer needs you at a terminal
  or in LeanCTX Code.") nor without `--yes` or a yes at the prompt, and answers
  `confirmation_required` with the run's terms in `error.details`; a run whose computer
  stopped ends with `stop_reason` `balance_empty` or `computer_stopped` (§35).
- New kind `cloud.exchange.audit` (§40, CLI only, listed in capabilities under
  `cloud.exchange`: `audit`): the audit of the agent exchange through LeanCTX Cloud.


Changes in 3.12.3 (additive unless marked):
- Engine files (§24): `agent.diaries` (`agent diaries --json`, `GET /v1/local/agent/diaries`),
  `cloud.workers` (`cloud worker list --json`, `GET /v1/local/cloud/workers`), `archive.entry`
  (`archive show <id> --json`, `GET /v1/local/archive/{id}`) and `bus.revision`
  (`bus revision --json`, `GET /v1/local/bus/revision`): a reader no longer parses
  `agents/diaries`, `cloud/workers.json`, the archives or `agents/registry.json`.
- Policy and sync (§23): `cloud.sync`, `policy.sessions` and `policy.session` are served on
  the local service (`/v1/local/cloud/sync`, `/v1/local/cloud/policy/sessions[/{id}]`) and
  listed in capabilities. `policy.sessions` `installed` adds `vaults`: one row per vault the
  policy reaches here, with its `vault_id` and which policy applies. `installed` adds
  `enforcement`: the version enforced here, when LeanCTX Cloud acknowledged it, the live sessions
  under it and when this Instance reported it to LeanCTX Cloud (§23.2).
- Local tasks (§3c): a project linked to no vault keeps its tasks on this Instance;
  `lean-ctx tasks list|show|propose|decide|claim|update|submit|amend|release|verify|stop --json`
  (`tasks.list`, `tasks.task`, `tasks.ack`), the same on the local service under
  `/v1/local/tasks`; new error code `conflict`.
- Share (§19, §20): `cloud vaults --json` lists the vaults this Instance knows; `cloud link
  --vault V --json [--dry-run] [--bring-local] [--leave-unsettled-local]` links the project and
  can bring its local docs and open local tasks into the vault; `tasks.list` adds
  `local_not_shared`, and `--local` (§3c) reaches local tasks left unshared.
- Local docs (§2c): `docs.list` / `docs.page` carry `"source": "vault" | "local"`.
  **`vault` is `null` when `source` is `local`** (it was always an object before). `cloud docs list|show --json`
  on a checkout with no vault-map record (signed in or not) now prints the local pages instead of
  error `no_vault`; a reader that took `no_vault` as "not shared" reads `source: "local"` instead.
  A linked checkout without a replica still answers `no_vault` ("sync first"); signed out it answers
  `not_connected`; a vault map that cannot be read answers `unavailable` (§2c).
- `cloud.presence` rows (§4) add `kind`, `display_name`, `session_name` and `project`;
  **`instance` is an object `{id, name}`** (it was always `null` before). This is a
  documented exception to the version rule below: no Instance ever printed a value there,
  so no value changes meaning; a reader that typed it `string|null` must accept the object
  (or treat it as unknown) and needs no other change. The other exception is `docs.*`
  `vault: null` for local pages (§2c): those requests answered `no_vault` before, so no
  earlier document changes shape.
- `presence.list` rows (§11) add `kind`, `session_name`, `display_name`, `host`,
  `instance {id, name}` and `project {root, vault}`; `LEAN_CTX_SESSION_NAME` (§8) names them.
- `bus.post` and `agent.post` take `task_id`, and on the CLI only `kind` and `as` (from the
  agent's own session) (QN0, §18a), rows gain `kind`; `to` `queen` names the registered Queen;
- `bus.post` (§11) adds `sender` and `cloud`; a person's post to a vault bus leaves the
  machine (§18). `--request-id` makes a post idempotent.
- Writes (`bus.post`, `bus.claim`, `bus.release`, knowledge writes, every `tasks` write of
  §3c and the local `docs write|move|delete` of §2c): the request passes the release point
  before anything is written or sent to LeanCTX Cloud; a withheld write writes nothing.
- `policies.list` (§15b) adds `pack_sources.org_policy` with the signature state.
- `agents.list` (§16) is a route and is released like every kind; `agents list --json` = `agents --json`.
- New `daemon.status` (§17).
- `ctx_agent register` (§11): `role` is a role id, `name` the display name; free text given
  as `role` is not stored as a role; the answer names the role and name it stored.
- Feature modules add kinds through one extension list: every kind, verb and route stays in
  the one capabilities document (§14).

LeanCTX | OSS prints these JSON documents for a local reader on the same
machine, such as LeanCTX Code, so the reader never parses private
state files or human text and never holds a LeanCTX Cloud token. The reader spawns
`lean-ctx` and reads stdout. Everything is read only, except the writes named
below (§3c, §5b, §11, §12, §13, §20, §21): local task transitions, the project's link (`cloud link` without `--dry-run`), bus posts, reads marked, leases, knowledge
writes (freshness clean-up included) and the evidence package, each through the store's own lock and
admission. Docs, presence and status come from local
state only; `cloud tasks` reads through this Instance exactly like the text
`cloud tasks list|show`.

Knowledge is selected from the project's local store, its gotchas and the
local replica of the vault linked to the checkout; automatic read-noise
(`auto:` machine-derived facts, `finding`s that only record a read, search or
lookup) is left out unless `--include-noise` asks for it. Documents carrying
project or agent text are released through the CLI tool run (redaction,
gateway) like every other CLI read: knowledge, docs, tasks (`ctx_tasks`)
and agent messages (`ctx_agent`).

## Common rules

- Every output is ONE JSON object on stdout, one line, ending in `\n`.
- Every object carries `"contract": "local-read/1"` and `"kind"` (named per command below).
- Timestamps are RFC 3339 UTC strings (`"2026-11-04T09:12:33Z"`, fractional seconds allowed).
- Fields documented as nullable are always present (printed as `null`), never omitted.
  Arrays are always present (possibly empty).
- Additive rule: consumers MUST ignore unknown fields; new fields may appear within `local-read/1`.
  A breaking change bumps to `local-read/2` (the 3.12.3 exceptions,
  `cloud.presence.instance` and `docs.*` `vault` for local pages, are named in the change
  list above).
- Enum values unknown to a consumer should be shown as-is (strings), not rejected.
- Errors: exit code 1 (2 for bad arguments) and on stdout:
  ```json
  {"contract":"local-read/1","kind":"error","error":{"code":"not_connected","message":"human text"}}
  ```
  `code` ∈ `invalid_argument` | `no_project` | `not_connected` | `no_vault` | `not_found` | `forbidden` | `withheld` | `anchor_gone` (`knowledge verify`: the code an anchor names is gone) | `conflict` (a local task's state does not allow the write, §3c) | `unavailable` | `rate_limited` (3.12.3: LeanCTX Cloud holds this machine's requests for the seconds the message names; never reported as `not_found`) | `plan_required` (3.12.3, team chat §28: the team's plan does not include it, or the team is locked) | `invalid_state` (3.12.3, team chat §28: the channel's or message's state does not allow it) | `invalid_field` (3.12.3, team chat §28: LeanCTX Cloud refused a field of the request) | `sso_required` (3.12.3, team billing §27: the team requires SSO for the person; sign in with SSO in the browser) | `email_unverified` (top-up §33: the person has not confirmed their email address; the link is in the confirmation email) | `policy_denied` (next release, §26.2: LeanCTX Cloud's policy refuses it; the error object adds `cloud_code`, `member`, `level` and `action`) | `not_offered` (team stop §34: LeanCTX Cloud does not offer the feature; a refusal, not an outage to retry) | `internal` | `no_computer`, `balance_empty`, `computer_exists`, `computer_busy`, `computer_provider_unavailable` (LeanCTX computers §44: LeanCTX Cloud's own refusal, its message passed on) | `computer_not_available` (§44: LeanCTX Cloud offers no computers yet; the message is "LeanCTX computers are not available yet.") | `confirmation_required` (§44: a write that can cost money was asked without `--yes`, or by a caller that is neither at a terminal nor LeanCTX Code, so nothing was sent, or LeanCTX Cloud answered `428` to a confirmed one; the message says who would pay and `error.details` carries the terms `{billing: free|billed|null, usd_per_hour, free_hours_left, topup_usd}`; the refusal of a caller without a person has no `details`). | `app_only` (§26.4: a route only LeanCTX Code may ask for, asked without its app key; nothing was done). Only `confirmation_required` carries `error.details`.
An error may carry `error.reason`, a stable word that tells refusals sharing a `code` apart (additive; a reader that does not know it ignores it). Today only `forbidden` from Cloud billing (§27, §33) carries one: `reason: "company_billed"` when the team is billed through its company. A plain `forbidden` has no `reason`.
  A document withheld as a whole at the release point is this `withheld` error document on stdout (exit 1; its message is the content-free reason). Only when not even that refusal could be receipted is nothing printed on stdout, and the reason goes to stderr.
- Read-only, except:
  - `agent post` writes one bus message;
  - `knowledge verify <id>` and `knowledge dedupe --apply` write the knowledge store (§21);
  - `knowledge stale` writes back stored anchor marks that disagree with the live check (§21a);
  - the other writes named above.

  Never prints a LeanCTX Cloud token.
- `knowledge.list` (§1) adds `withheld`: facts are released one by one, and a fact the release
  point would withhold no longer withholds the whole list (#1298).
- Money is USD as a JSON number rounded to 6 decimals; never a string.
- `--project-root P` (where accepted) picks the project; default = project root detected from the cwd.
  `--root P` is the same flag (both also as `--flag=P`). A read and a write resolve the same
  folder, and the release checks that folder's policy. Naming two different folders
  (`--root A --project-root B`) is `invalid_argument` ("--root and --project-root name different
  folders (A, B); name one"), and so is a blank name; nothing is read or written. `activity list`
  also refuses `--project` naming another project than `--project-root`/`--root` (§7). `bus
  revision` names its folders with repeated `--project-root` only (§24.4).

---

## 1. `lean-ctx knowledge list --json [--project-root P] [--category C] [--query Q] [--limit N] [--include-noise]`

`--category` may repeat. `--query`: case-insensitive substring over id + text.
`--limit` default 200, max 1000. Default EXCLUDES automatic read-noise (facts captured
automatically from file reads/tool output); `--include-noise` brings them back.
Sorted by `updated` descending.

```json
{
  "contract": "local-read/1",
  "kind": "knowledge.list",
  "project_root": "/Users/me/code/app",
  "total": 57,
  "returned": 50,
  "truncated": true,
  "excluded_noise": 312,
  "withheld": 0,
  "facts": [
    {
      "id": "architecture/auth-flow",
      "type": "decision",
      "category": "architecture",
      "key": "auth-flow",
      "text": "Sessions are validated in middleware, not per route.",
      "status": "local",
      "origin": "local",
      "confidence": 0.9,
      "anchors": [ { "path": "src/auth/mw.rs", "symbol": "validate" } ],
      "source": { "kind": "session", "ref": "mcp-1234-abcd" },
      "review": null,
      "created": "2026-11-01T08:00:00Z",
      "updated": "2026-11-03T14:22:10Z"
    }
  ]
}
```

- `withheld` (3.12.3): facts the release point withheld from this answer. Every fact is
  released on its own; a fact it would withhold is left out (not in `facts`, not counted in
  `returned`; `total` still counts it) and the other facts are delivered. A reader that sees
  `withheld > 0` knows the list is incomplete.
  - A fact that still carries a text the release masked or withheld in another fact (a
    withheld fact's key named in a clean fact's `text`, say) is withheld too.
  - `truncated` reports `--limit` only: `total` exceeds the facts selected before the release.
    It ignores withheld facts. `withheld > 0` with `truncated: false` is a list cut by the
    release, not by the limit.
  - The CLI writes the content-free notice `[lean-ctx gateway: N record(s) withheld]` to
    stderr. On the local socket the response's `notice` is the generic
    `[lean-ctx gateway: content redacted]`; the count is `withheld`. Neither carries fact content.
  - The document as a whole is withheld when the part outside `facts` cannot be released, or
    when, after the facts were released one by one, the rebuilt document still carries a text
    the release masked or withheld (a masked value that survives across records, for example in
    a field outside `facts`).
- `id`: `"<category>/<key>"` (what `ctx_knowledge` addresses); a second current fact on the same key gets `"<category>/<key>@<version>"`.
- `type` ∈ `fact` | `decision` | `gotcha` | `procedure` | `reference`.
- `status`: for facts that came from a LeanCTX Cloud vault ∈ `verified` | `provisional` | `outdated` | `conflicting`; else `"local"`.
- `origin` ∈ `local` | `vault` | `provider` | `derived` | `unverified`.
- `source`: `{ "kind": "session" | "vault" | "provider" | "import", "ref": string }` or `null`.
  For `vault`, `ref` is the vault address `ws/vault`.
- `anchors[].symbol`: string or `null`.
- `review`: `null` | `"to_verify"` | `"gone"` (the code the fact rests on moved / vanished) | `"contradicted"` (an open contradiction with another fact).

## 2a. `lean-ctx cloud docs list --json [--project-root P]`

The project's linked vault docs, read from the local replica (as `ctx_doc` does). No network.

```json
{
  "contract": "local-read/1",
  "kind": "docs.list",
  "project_root": "/Users/me/code/app",
  "vault": { "address": "acme/app", "linked": true },
  "pages": [
    {
      "id": "auth-overview",
      "title": "Auth overview",
      "page_kind": "reference",
      "status": "canonical",
      "anchor_state": "to_verify",
      "sections": 4,
      "notes": 1,
      "anchors": 3
    }
  ]
}
```

- `status` ∈ `draft` | `canonical` | `retired`.
- `anchor_state` (page = worst of its anchors) ∈ `fresh` | `to_verify` | `gone`
  (`fresh` also when no anchors, or a note covers the change).
- No linked vault → error `no_vault`.

## 2b. `lean-ctx cloud docs show <id> --json [--project-root P]`

```json
{
  "contract": "local-read/1",
  "kind": "docs.page",
  "project_root": "/Users/me/code/app",
  "vault": { "address": "acme/app", "linked": true },
  "page": {
    "id": "auth-overview",
    "title": "Auth overview",
    "page_kind": "reference",
    "status": "canonical",
    "anchor_state": "to_verify",
    "sections": [
      { "order": 0, "heading": null, "text": "Intro paragraph…\n" },
      { "order": 1, "heading": "Middleware", "text": "## Middleware\n…" }
    ],
    "anchors": [
      { "anchor_kind": "file", "path": "src/auth/mw.rs", "symbol": null, "role": "derived", "state": "to_verify", "noted": false }
    ],
    "notes": [
      { "body": "Moved token refresh into mw.", "covers": ["file:src/auth/mw.rs"], "anchor_state": "fresh", "section": 1 }
    ]
  }
}
```

- `sections[].text` is raw markdown incl. its own heading line; concatenating in `order` = body.
- Text is given only for vaults linked to this checkout on this machine (the person's own); otherwise error `no_vault`.
- Unknown id → error `not_found`.
- Both documents carry `"source": "vault"`. A checkout that is **not linked** to a vault answers
  from its local pages instead (§2c): `"source": "local"`, `"vault": null`.

## 2c. Local docs: `lean-ctx docs list|show|history|write|move|delete --json`

A project's pages on this Instance alone, for a checkout no vault is linked to (owner 4 Nov: docs
work locally; LeanCTX Cloud adds sharing). `cloud docs list|show --json` print the same documents there.
On a linked checkout `docs list|show --json` print the vault documents (§2a/2b) and the writes
answer `forbidden`. All take `--project-root P`.

The vault map alone decides whether a checkout is linked. A LeanCTX Cloud state or vault map that cannot be
read is neither: every docs document answers `unavailable` and `ctx_doc` refuses, so a linked
checkout's pages are never written locally by mistake. Signed out, a linked checkout stays linked:
reads answer `not_connected`, writes are refused, and nothing is kept locally to share later.

The store is an append-only journal of the same doc events a vault holds, under
`<data dir>/docs/<project hash>/` (keyed like knowledge). Every write is one **revision**
(`rev`, 1-based) with its time and author; nothing is rewritten, so every earlier state stays readable.

- `docs list --json [--rev N]` → `docs.list`: the §2a shape plus `"source": "local"`, `"vault": null`,
  `"rev"` (`null` = now, else the revision shown) and per page `updated` (ts|null),
  `updated_by` (`{ "kind": "agent"|"person", "label": string }`|null), `shared`
  (`null` | `{ "vault": "ns/name", "page_id": string, "at": ts }` once a link brought it into a vault).
- `docs show <slug> --json [--rev N]` → `docs.page`: the §2b shape plus `source`, `vault: null`, `rev`,
  and in `page`: `links` (slugs this page links to, sorted), `updated`, `updated_by`, `shared`.
  Links in `sections[].text` are `/slug#heading`; they follow a moved page.
- `docs history <slug> --json` → `docs.history` (also for a moved-away or deleted slug: the page it named last):
  ```json
  {
    "contract": "local-read/1", "kind": "docs.history", "project_root": "/Users/me/code/app", "source": "local",
    "page": { "id": "guide/retry", "page_id": "n_…", "live": true },
    "revisions": [
      { "rev": 1, "at": "2026-11-04T10:00:00Z", "author": { "kind": "agent", "label": "mcp-…" }, "action": "write", "events": 5 }
    ]
  }
  ```
  `action` ∈ `write` | `edit` | `note` | `confirm` | `move` | `delete` | `share`. Oldest first.
- Writes print `docs.saved`:
  `{ "source": "local", "action": …, "slug": <slug after the write>, "rev": n|null, "events": n }`
  (`rev: null`: nothing changed).
  - `docs write <slug> (--content TEXT | --stdin) [--type T] [--status draft|canonical] [--author NAME]`:
    the whole page, `# Title` first; creates or revises. At most 1 MiB.
  - `docs move <slug> <new-slug>`; `docs delete <slug>` (the page leaves the listing; its history stays).
  - Writes from the CLI are by `{ "kind": "person", "label": --author or "person" }`; agents write through
    `ctx_doc` as `{ "kind": "agent", "label": <bus agent id> }`.
- Slugs: lowercase words joined by `-` and `/`, at most 8 segments, ≤ 128 bytes; else `invalid_argument`.
  Unknown slug → `not_found` (show, history; a write names it `invalid_argument`). Bounds: journal ≤ 64 MiB, ≤ 4096 events per revision.

## 3a. `lean-ctx cloud tasks list --json [--state S] [--role R] [--mine]`

Same source as today's `cloud tasks list` (through this Instance; Desktop never sees a token).

```json
{
  "contract": "local-read/1",
  "kind": "tasks.list",
  "project_root": "/Users/me/code/app",
  "vault": "acme/app",
  "tasks": [
    {
      "number": 12,
      "title": "Fix flaky login test",
      "goal": null,
      "acceptance": null,
      "state": "claimed",
      "lifecycle": "running",
      "priority": "normal",
      "role": "tester",
      "labels": [],
      "claimed_by": { "label": null, "agent": null, "mine": true },
      "updated": "2026-11-04T07:10:00Z",
      "origin": { "kind": "person", "facts": [], "message": null }
    }
  ],
  "partial": false,
  "local_not_shared": [
    { "local_number": 2, "title": "Write the release notes", "state": "proposed" }
  ]
}
```

- `partial` (3.12.3): `true` when a LeanCTX Cloud answer came back full (100 tasks), so older tasks of
  these filters may be missing from `tasks`; the plain text then ends with a `(partial: …)`
  line. Never silently incomplete; narrow the list with `--state` or `--role`.
- `stale` (3.12.3, optional, also on `tasks.task`): present only when the answer is not
  current. Task reads are shared by every process of the Instance for 15 s and fetched once
  for all callers; when LeanCTX Cloud holds the Instance's reads (429) or is not reachable, the last
  good answer is served with `stale: {age_s, reason: "rate_limited"|"unavailable",
  retry_after_s?}` instead of an error. Without a kept answer the error is `rate_limited`.
- `local_not_shared`: this project's local tasks that were neither brought into the vault nor
  closed (left local by `cloud link --leave-unsettled-local`, or the link came in through git);
  they are acted on with `lean-ctx tasks … --local` (§3c), `[]` when none (also when the local
  task store cannot be read: the vault's list never fails on it). Its numbers are local ones,
  never the vault's.
- The listing does not carry `goal`/`acceptance`/claim holder: they are `null` in `list`
  (`claimed_by` is `{label:null, agent:null, mine:true|false}` while claimed, else `null`). `show` fills them.
- `state` ∈ `proposed` | `open` | `claimed` | `submitted` | `done` | `rejected` | `cancelled` | `failed`.
- `lifecycle`: `null` | `queued` | `claimed` | `running` | `submitted` | `completed` | `failed` | `cancelled`.
- `priority` ∈ `high` | `normal` (always printed; a task LeanCTX Cloud sends without one is `normal`).
- `labels`: 0–16 unique `[a-z0-9-]{1,32}`, always an array.
- `fields_version`: `int` | `null`, +1 on every priority or labels write (C0 2.2 E47);
  `null` from a LeanCTX Cloud server that sends none. It is the `base_version` of the next write.
- Order: `high` first, then by `number` ascending (the board order, 3.12.3).
- `origin.kind` ∈ `person` | `agent` | `contradiction` | `question`.

## 3b. `lean-ctx cloud tasks show <n> --json`

Same fields as a list item, filled, plus:

```json
{
  "contract": "local-read/1",
  "kind": "tasks.task",
  "project_root": "/Users/me/code/app",
  "vault": "acme/app",
  "task": {
    "number": 12, "title": "…", "goal": "…", "acceptance": "…",
    "state": "claimed", "lifecycle": "running", "priority": "normal",
    "role": "tester", "labels": [],
    "claimed_by": { "label": "Claude Code · tester", "agent": "claude", "mine": true },
    "claim_until": "2026-11-04T07:25:00Z",
    "updated": "2026-11-04T07:10:00Z",
    "created": "2026-11-03T10:00:00Z",
    "created_by": { "kind": "person", "label": "Yves" },
    "origin": { "kind": "person", "facts": [], "message": null },
    "updates": [ { "at": "2026-11-04T07:10:00Z", "by": "Claude Code · tester", "summary": "Reproduced locally." } ],
    "result": null
  }
}
```

- `result`: `null` | `{ "summary": string|null, "outcome": "completed"|"failed", "evidence": [ { "kind": string, "ref": string|null } ], "at": ts }`.
- `updated` in show = newest of created / update / result / claim time.

## 3b-1. `lean-ctx cloud tasks priority|labels <n> … --json` (3.12.3)

The order of work on a board: a person sets a vault task's priority and labels
(C0 2.2 E47 `PATCH /v1/vaults/{vault}/tasks/{n}` at the task's `fields_version`; on
`409 stale_base` the Instance reads the task again and sends the write once more).
LeanCTX Cloud answers `GET …/tasks` as one page, the newest 100 tasks by number. So that an
older high task is never cut off, the Instance also asks for `?priority=high` with the
same filters, merges both answers (each task once) and then sorts: high first, then by
number, before any display limit. Either answer coming back full (100 tasks) marks the
list `partial` (§3a): with more than 100 high tasks, the oldest are missing, and it says so.

```
lean-ctx cloud tasks priority <n> high|normal [--labels a,b]... [--clear-labels] --json
lean-ctx cloud tasks labels <n> <a,b,...> [--priority high|normal] --json
lean-ctx cloud tasks labels <n> --clear --json
```

```json
{
  "contract": "local-read/1",
  "kind": "tasks.ack",
  "project_root": "/Users/me/code/app",
  "vault": "acme/app",
  "change": { "number": 12, "event": "fields", "state": "open", "seq": null },
  "task": { "number": 12, "priority": "high", "labels": ["core"], "fields_version": 4, "…": "as tasks.task" }
}
```

- A set of labels replaces the whole list; `--clear` / `--clear-labels` empties it.
- A wrong value is refused before any request: `invalid_argument`, e.g.
  `priority is high or normal (got "urgent")`, `labels: a label is 1-32 of [a-z0-9-]`,
  `labels: labels are unique`, `labels: at most 16 labels`.
- Who sets them: a person. A CLI process under an agent host (`CLAUDECODE`,
  `CODEX_THREAD_ID`, …, `LEAN_CTX_AGENT_ID`, `LEAN_CTX_AGENT`, `AI_AGENT`) is refused
  `forbidden`. The socket reader is a person (LeanCTX Code). Agents read priority and
  labels in every `tasks.list` / `tasks.task`.
- `task` is the task after the write (`null` when that read failed; the write stands).

## 3c. Local tasks: `lean-ctx tasks … --json [--project-root P]`

A project linked to no LeanCTX Cloud vault keeps its tasks on this Instance (same task
model and states as LeanCTX Cloud). For such a project `cloud tasks list|show --json`
and `tasks list|show --json` answer from the local store with `"vault": null`
(a linked project answers from LeanCTX Cloud as in 3a/3b, `vault` = its address).
`lean-ctx tasks` routes a linked project to LeanCTX Cloud exactly like `cloud tasks`.
With `--local` it acts on the project's own local tasks that were not brought into the vault
(`local_not_shared` in 3a): list, show and every write but `propose` (new tasks of a linked
project go to LeanCTX Cloud); a task that was brought is refused (`forbidden`, naming the vault task).
MCP `ctx_tasks` takes the same as `local: true`, the local service as the body key `local: true`.

Local differences from 3a/3b (additive):

- `tasks.list` rows carry `goal`, `acceptance` and `claimed_by` filled.
- `tasks.task` adds `approved_by` (`null` | `{kind, label, agent}`),
  `verification` (`null` | `{verdict: "accept"|"reject", by, at, reason}`),
  `transitions` (every transition, newest last, at most 50) and `shared`
  (`null` | `{vault: "ws/vault", number, at}` once brought into a vault).
  `transitions[]` = `{seq, event, from, to, by: {kind: "person"|"instance", label, agent}, at, reason}`;
  `event` ∈ `proposed` | `accepted` | `declined` | `claimed` | `updated` | `released` |
  `submitted` | `failed` | `verified` | `rejected` | `stopped` | `amended` | `shared` | `fields`
  (priority and/or labels set; no state change, no bus notice).
  A declined proposal and a stopped task both end in the state `cancelled`
  (LeanCTX Cloud's state names; the event tells them apart).
- `--agent ID` reads as that bus agent (`claimed_by.mine`); `--mine` needs it.

Writes, each printing one `tasks.ack`:

On a project linked to a LeanCTX Cloud vault, `decide` and `stop` (3.12.3)
are a person's: the engine calls LeanCTX Cloud's engine routes
(`POST /v1/vaults/{vault_id}/tasks/{n}/decide` `{accept, reason?}` and
`…/stop` `{reason}`) with its instance token, and the Cloud acts as the person
the machine is linked to. A reason is cut at 280 characters; a decline reason
under 2 characters is left out, a stop needs one (a stop without one is
refused `invalid_argument` before the connection is checked). `change.state`
is `open` after an accept and `declined` after a decline (the Cloud cancels
the task).
Refusals: `conflict` (the task's state, Cloud `invalid_state`, or an accept
of a task the Cloud already cancelled), `not_found`,
`forbidden` (role, plan, an agent's process, or a machine the Cloud no longer
accepts: reconnect it), `rate_limited`, `unavailable`.

```
lean-ctx tasks propose --title T --acceptance A [--goal G] [--role R] --json
lean-ctx tasks decide <n> --accept|--decline [--reason R] --json
lean-ctx tasks claim <n> --agent ID [--minutes 5-240] --json
lean-ctx tasks update <n> --summary S --agent ID [--tokens N] --json
lean-ctx tasks submit <n> --summary S --agent ID --evidence kind:ref… [--failed] --json
lean-ctx tasks amend <n> --agent ID --evidence kind:ref… [--summary S] [--reason R] --json
lean-ctx tasks amend <n> [--agent ID] --merge SHA [--reason R] --json   (a done task)
lean-ctx tasks release <n> [--agent ID] [--reason R] --json
lean-ctx tasks verify <n> --verdict accept|reject [--findings F] [--agent ID] --json
lean-ctx tasks stop <n> --reason R --json          (a person only)
lean-ctx tasks priority <n> high|normal [--labels a,b]... [--clear-labels] --json   (a person only)
lean-ctx tasks labels <n> <a,b,...>|--clear [--priority high|normal] --json        (a person only)
```

```json
{
  "contract": "local-read/1",
  "kind": "tasks.ack",
  "project_root": "/Users/me/code/app",
  "vault": null,
  "change": { "number": 3, "event": "verified", "state": "done", "seq": 17 },
  "task": { "number": 3, "state": "done", "…": "as tasks.task" }
}
```

- Without `--agent` a person acts (`--by NAME`, default `"person"`): LeanCTX Code
  decides, releases, verifies and stops this way. A process that runs
  for an agent never acts as a person:
  - `LEAN_CTX_AGENT_ID` (or `LCTX_AGENT_ID`) set: the process acts as that agent;
    `--by`, or an `--agent` naming someone else, is refused `forbidden`.
  - an agent marker without an id (`CLAUDECODE`, `CODEX_THREAD_ID`,
    `CURSOR_TASK_ID`, `LEAN_CTX_AGENT`, `AI_AGENT`): every write needs
    `--agent ID`; `--by`, or a write without `--agent`, is refused `forbidden`.
    Reads are unaffected.
- The CLI does not authenticate bus identities: a process on this machine can
  name any agent with `--agent`, so the local rules (no self-verify, holder-only
  writes) hold for honest callers only. For a linked vault, LeanCTX Cloud checks the
  identity on the server.
- When the LeanCTX Cloud link of the checkout cannot be read, every `tasks` command fails
  `unavailable`; it never falls back to local tasks.
- The action and the number are positionals; flags may stand anywhere
  (`tasks --json show 1`).
- Priority and labels (3.12.3) follow §3b-1 on a local task too: the same
  values and refusals, `fields_version` counted on this Instance, lists sorted
  `high` first, then by number. On the local service:
  `POST /v1/local/tasks/{n}/priority` `{priority?, labels?: [..], clear_labels?}` and
  `POST /v1/local/tasks/{n}/labels` `{labels?: [..], clear?, priority?}`. A closed
  task keeps them (`conflict`, `task_closed`); an agent, or a write naming
  `--agent`, is refused `forbidden`. A local task's priority stays on this
  Instance; it reaches another Instance only through cloud-share (the task
  brought into a vault), a dependency of this feature, not part of it.
- Rules: anyone decides on a proposal (`decide` takes exactly one of `--accept`
  and `--decline`); only an agent claims (the proposer too); the holder updates,
  submits and releases (a person may release); a result is verified by a person
  or another agent, never the one that submitted it; `reject` (`--findings`
  required; they are the verification's `reason`) opens the task again; only a
  person stops a task, and only with `--reason` (event `stopped`, state
  `cancelled`): `--agent`, `LEAN_CTX_AGENT_ID` or an agent marker is refused
  `forbidden` (`agents_stop`); the agents' MCP tool `ctx_tasks` has no stop.
  On the local service: `POST /v1/local/tasks/{n}/stop` `{reason, by?}`. A claim lasts `--minutes` (default 60), renewed by `update`;
  an expired claim opens the task again.
- `amend` (3.12.3): a result is corrected without a claim (a submit ends the
  claim). The write goes through the release first (G08): a request the release
  withholds writes nothing.
  - `submitted`: only the agent that submitted it; `--evidence` (the whole new
    list) and `--summary` replace the old ones. `--merge` is `conflict`
    (`not_merged_yet`).
  - `done` (accepted, waiting for its merge record): only `--merge SHA` (7-64
    lowercase hex), once, by the submitter, a person or a Queen session the app
    registered; never by the reviewer (`verified_by`): `forbidden` (`not_merger`),
    checker and merger stay separate. The result gets `merge: {commit, by, at}`, its
    evidence and summary stay. Anything else is `conflict` (`record_merge_only`);
    a second record is `conflict` (`merge_recorded`).
  - Any other state is `conflict` (`not_amendable`); anyone else is `forbidden`
    (`not_submitter`). The state does not change (event `amended`); the task's
    `updates` get one `kind: "amendment"` entry with `amended_from` (evidence
    before) and `amended_to` (evidence after, or the merge commit): old → new SHA.
  - On a linked vault it needs LeanCTX Cloud's `POST /v1/vaults/{id}/tasks/{n}/amend`
    (`leanctx.cloud.task-amend/v1`), which no LeanCTX Cloud serves yet: the CLI checks the
    request and reads the task, then fails closed with `not supported by LeanCTX Cloud
    yet` and sends no write (nothing changed).
  - On the local service: `POST /v1/local/tasks/{n}/amend`
    `{evidence?, summary?, merge_commit?, reason?, agent?, by?, local?}`.
- `change` is `null` when a claim only renewed. For a linked project `claim`,
  `update`, `submit`, `amend`, `release` and `verify` (`--agent` required) go to LeanCTX Cloud;
  `change.seq` and `task` are `null` there.
- Refusals: `conflict` (the state does not allow it; the message names LeanCTX Cloud's
  reason, e.g. `task_claimed`, `not_submitted`), `forbidden` (`self_verify`,
  `not_holder`, `not_submitter`, `agents_claim`, `agents_stop`), `not_found`, `invalid_argument`.
- Every local transition posts on the local bus (category `status`, from
  `local-tasks`, metadata `lean_ctx.request_id` = `task:<n>:<event>:<seq>:<i>`),
  the number only, no task text. `submitted` goes to every agent of the
  project except its worker; everything else to all.

## 4. `lean-ctx cloud presence --json`

Remote workers seen on the workspace bus (local `presence.json` the relay maintains). No network.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.presence",
  "workers": [
    {
      "worker_id": "wkr_abc123",
      "kind": "agent",
      "name": "Laptop tester",
      "display_name": "LeanCTX · Contracts",
      "session_name": "LeanCTX · Contracts",
      "role": "tester",
      "state": "active",
      "instance": { "id": "ins_7", "name": "fitim" },
      "project": { "root": null, "vault": "acme/app" },
      "last_seen": "2026-11-04T09:00:00Z",
      "address": "#acme/app",
      "person_name": "Fitim Example",
      "machine_name": "fitim",
      "working_on": "Fix the login page"
    }
  ]
}
```

- `name`: string or `null` (only when this Instance knows the worker, e.g. its own).
- `kind` ∈ `agent` | `person` (`agent` when the LeanCTX Cloud server does not say). `display_name`,
  `session_name`, `instance.id`, `instance.name`: what the LeanCTX Cloud server relayed with the presence
  (optional `presence.update` fields `display_name`, `session_name`, `instance_id`,
  `instance_name`, `kind`; bounded single lines), else `null` — `display_name` falls back to
  `name`. A reader shows display name → role → short id and invents nothing.
- Team presence (h7b), always printed, each a bounded single line or `null`:
  `person_name` is the linked account's name of the person the worker belongs to (set by
  the Cloud, never by an Instance); `machine_name` is the short name of the relayed
  `machine_name`, else of `instance.name` (first label only, so never a domain; an IP
  address or a label with spaces stays whole); `working_on` is the status line the
  worker's agent set itself (`ctx_agent action=status`), never one of LeanCTX's own
  lifecycle notes, with absolute paths cut to their last part and secrets redacted
  (`redact_text`) on the sending Instance, then cut to 128 characters without an ellipsis.
  An Instance adds `machine_name` and `working_on` to its own `presence.update` frames only
  when the `hello.ack` names the feature `presence_identity` (the Cloud then shows them to
  members of the same team only) and `[cloud] presence_names` is on (the default); they are
  never part of the `hello`. Once on, both members are always sent (`null` when unknown), so
  the Cloud replaces a cleared line. An older Cloud leaves all three `null`.
- Since h7b an Instance sends its own `instance_name` (hello and `presence.update`) as the
  same short name; an older LeanCTX may still relay a full host name, which `instance.name`
  shows as relayed.
- `project.vault`: `ns/name` of the bus the worker was seen on; `project.root` is `null`
  (a path of another machine).
- `state` ∈ `active` | `idle` | `held`. Rows reported within the last 24 h, newest first.
- Released like every Contracts document (one receipt for the bytes sent).

## 5a. `lean-ctx agent messages --json [--project-root P | --all] [--since TS] [--limit N] [--to ID]`

The local agent bus (registry scratchpad), for the reader's project: messages of that project and messages without a project. `--all` (the person's console): the messages of every project and those without one, each served as a reader of its own project is served. `--since`: strictly newer than TS. `--limit` default 200 (newest kept). Sorted oldest → newest. A message is the reader's project's when its `project_root` names the same project, however it is spelled (separators, a trailing slash, a symlink).

```json
{
  "contract": "local-read/1",
  "kind": "agent.messages",
  "messages": [
    {
      "id": "m_9f…",
      "from": "mcp-5544-a880…",
      "to": null,
      "thread": null,
      "task_id": null,
      "kind": null,
      "remote_address": null,
      "category": "direct",
      "priority": "normal",
      "message": "Shapes posted.",
      "project_root": "/Users/me/code/app",
      "timestamp": "2026-11-04T09:01:00Z",
      "expires_at": "2026-11-04T21:01:00Z",
      "privacy": "team",
      "read_by": ["cloud-person"]
    }
  ]
}
```

- `to`: agent id, or `null` = broadcast. `thread` = `metadata["cloud.thread"]` or `null`.
- `task_id`: the task the message names, or `null`.
- `kind` (QN0, additive): the Queen-network kind (§18a), or `null` for an ordinary message,
  for a message stored before QN0 and for a kind this reader does not know.
- `remote_address`: for a message another machine's worker delivered through LeanCTX Cloud, the bus address it came from (`#ws/vault`); `null` for local messages.
- `priority` ∈ `low` | `normal` | `high` | `critical`.
- `withheld` (3.12.3, optional): the number of messages the release point withheld from this
  answer. Present only when > 0, so a document with nothing withheld is unchanged. A count,
  carrying no message content. Messages are released one by one; under `--all` each under its
  own project's policy (a message without a project under the reader's; a message of a project
  whose folder is no longer present here is withheld and counted, never released under another
  project's policy). A message that trips the release is left out on its own; the others are
  returned. The CLI writes the content-free notice `[lean-ctx gateway: N record(s) withheld]`
  to stderr.

## 5b. `lean-ctx agent post --json (--to ID … | --broadcast) [--thread T] [--project-root P] [--priority P] [--category C] [--expires-in-hours H] [--task-id T] [--kind K] [--as ID] --message TEXT`

Posted as the person (`from` = `"cloud-person"`, same as Desktop today) unless `--as` (from
that agent's own session, §11) names
a sender, one entry per recipient, 12 h lifetime by default; recipients read it on the bus
(read_by = ["cloud-person"]). The Instance takes the registry lock.

```json
{
  "contract": "local-read/1",
  "kind": "agent.post",
  "ids": ["m_1…", "m_2…"],
  "from": "cloud-person",
  "to": ["mcp-5544-a880…", "mcp-77-…"],
  "thread": "t_desk_1",
  "task_id": null,
  "message_kind": null,
  "priority": "normal",
  "category": "direct",
  "timestamp": "2026-11-04T09:02:00Z",
  "expires_at": "2026-11-04T21:02:00Z"
}
```

- QN0 (additive within agent-bus/1): `priority`, `category`, `expires_in_hours`, `task_id`,
  `kind` and `as` as for `bus post` (§11, §18a); the answer adds `task_id`, `message_kind`
  (the message's kind; the answer's own `kind` names the document),
  `priority`, `category` and `expires_at`. Without them a post is filed as before.
- `to: []` = broadcast (one id). All or nothing: errors `forbidden` (a recipient may not receive messages) and `withheld` (not admitted) write nothing. `timestamp` is the stored message time.
- The message belongs to the project `--project-root` names, or else to the project root detected from the cwd (error `no_project` when the cwd is no project); a post never stores a project-free message.

## 6. `lean-ctx cloud status --json --contract local-read/1`

Only with `--contract local-read/1`; plain `--json` stays the cockpit shape. All existing fields unchanged (`available`, `connected`, `instance`, `org`, `last_seen`, `url`,
`drafts`, `filter_packs`, `policy_refusal`, `web_url`, optional `bus_controls`,
`bus_controls_inactive`, `metrics_v2`), plus:

```json
{
  "contract": "local-read/1",
  "kind": "cloud.status",
  "plan": "personal",
  "lease_state": "valid",
  "lease_valid_until": "2026-11-05T09:00:00Z",
  "lease_grace_until": "2026-11-08T09:00:00Z",
  "rights": ["knowledge.read", "knowledge.draft", "tasks.read"],
  "locked": null,
  "trial_ends_at": null,
  "capture": { "considered": 120, "captured": 14, "rejected": { "no_reference": 80, "too_short": 26 } }
}
```

- `plan`: `null` (no lease) | `none` | `personal` | `team` | `enterprise`.
- `lease_state`: `null` | `valid` | `grace` | `lapsed`.
- `locked`: `null` | `trial_ended` | `plan_ended`.
- `trial_ends_at`: when the one-step trial ends (RFC 3339, `Z`), from the signed lease;
  `null` without a running trial (and from a Cloud that names no trials).
- `capture`: cumulative capture-gate counts on this Instance, or `null` when none recorded.

Also printed (`[--project-root P]` picks the project, default from the cwd):

```json
{
  "enabled": true,
  "problem": null,
  "workspace": { "org_id": "org_1", "name": "Acme" },
  "project": { "root": "/Users/me/code/app", "vault": "acme/app", "linked": true },
  "capture_gate": { "enabled": true, "version": 1 },
  "trial": { "active": null, "ends_at": null },
  "seats_limit": 2,
  "org_settings": { "metrics_upload": true, "receipts_upload": true },
  "receipts": { "queued": 0, "off": null },
  "workers": { "active": 1, "retiring": 0 }
}
```

- `enabled`: `[cloud] enabled` (`false` pauses all LeanCTX Cloud traffic). `problem`: why the stored
  enrollment cannot be used, or `null`.
- `workspace`, `seats_limit`, `org_settings`: `null` without a lease (`org_settings` also `null`
  when the lease does not carry it). `workspace.name` from the enrollment, or `null`.
- `project`: `null` without a project: the socket route without `project_root`, or with one
  that is no project (`~`, `/`, a missing path), and the CLI when the project from the cwd is
  `~` or `/`. Every other field is printed as with a project; nothing is invented.
- `project.vault`: `ns/name` of the vault linked to the checkout, `null` (and `linked: false`) otherwise.
- `capture_gate.enabled`: `[cloud] auto_capture`; `version`: the gate rules version.
- `trial.ends_at`: same as `trial_ends_at`. `trial.active`: `true` while the lease carries a
  trial end, else `null` (a lease without one may come from a Cloud that names no trials; a
  Personal plan is not a trial).
- `receipts.off`: why receipts are not sent, or `null`. `workers`: durable workers in the book / retiring.

## 6a. `lean-ctx cloud connect --json [--trial [--plan personal|team]] [--yes] [--url O] [--label L] [--no-browser]`

Not a Local Read document: connect is a write and prints **line-delimited JSON events** on
stdout (one compact object per line, nothing else on stdout). With `--json`, stdout carries
only event lines and nothing is printed before the first event; diagnostics go to stderr.
Readers rely on this rule: a first stdout line that is not an event means a lean-ctx without
`connect --json` (an older one), while no line yet means it is still working. It never
prompts; `--yes` is accepted and changes nothing. The browser opens `verification_uri_complete` unless
`--no-browser` or `LEAN_CTX_NO_BROWSER` is set; the reader may show the link itself.

`--trial` starts the plan's one-step trial while connecting (`--plan` defaults to `personal`
and is only valid with `--trial`): the device code is requested with `intent: trial` and
`plan` (Cloud contract p37 !134). Without `--trial` the request is the plain sign-in older
Clouds know.

```json
{"event":"code","verification_uri":"https://cloud.leanctx.com/device","verification_uri_complete":"https://cloud.leanctx.com/device?user_code=WDJB-MJHT&intent=trial&plan=personal","user_code":"WDJB-MJHT","expires_in":600}
{"event":"connected","plan":"personal","trial_ends_at":"2026-12-02T10:00:00Z"}
```

or, instead of `connected` (exit status 1):

```json
{"event":"error","code":"trial_used","message":"this address already had its Personal trial; no second trial starts. Choose a plan at https://cloud.leanctx.com/app/plans, then run `lean-ctx cloud connect`"}
```

- `code`: printed once before polling; not printed with an enrollment token. `expires_in`
  in seconds. The secret `device_code` is never printed.
- `connected`: `plan` is the lease's wire name (`none` | `personal` | `team` | `enterprise`);
  `trial_ends_at` as in §6 (`null` without a running trial). An already connected machine
  answers `connected` for its connection at once, without a `code`. `--trial` is then
  ignored: no trial starts, and `trial_ends_at` is the lease's (`null` when no trial runs).
- `error.code`: `trial_used` (no trial starts for this address: the message follows the
  service's `reason`, `trial_used` | `trial_ended` | `no_plan`; choose a plan). It needs a
  Cloud that answers `402 trial_used` at `/v1/device/token`; an older Cloud shows the refusal
  only in the browser, and the engine then ends with `expired` or `denied`.
  `trial_unsupported` (this Cloud refused the trial request: start the trial on the Cloud's
  web page, or sign in with `lean-ctx cloud connect`; the request is never resent without
  the trial), `expired` (the code expired before approval; with `--trial` the message says to
  choose a plan if the browser said the trial was used, else to run `--trial` again),
  `denied` ("Not mine" in the browser),
  `not_approved`, `plan_required`, `instance_limit`, `quota_exceeded`, `offline`,
  `unauthorized`, `instance_revoked`, `rate_limited`, `upgrade_required`, `config` (bad flags,
  e.g. `--plan` without `--trial`), `protocol`, `local`, the service's own code for any other
  answer (`[a-z0-9_.-]`), else `error`. `message` is terminal-safe text for people.
- Sharing is not asked: it follows the `[cloud]` settings until a person runs
  `lean-ctx cloud connect` in a terminal.

## 7. `lean-ctx activity list --json [--session ID] [--project-root P | --project P] [--since TS] [--kind K]… [--limit N]`

One activity stream per agent session, whatever the agent: what the person
asked, what the agent replied, which tools it called, which files it read or
wrote, which shell commands it ran, which permissions it asked for, where its
turns began and ended, and its errors. Fed by host hooks (`source: hook`),
LeanCTX MCP calls (`mcp`) and ACP client callbacks (`acp`). Local state only,
read only.

- `--session ID` alone: that session across projects (`project_root` is `null`).
  With a project, or with neither flag, the project's sessions (default = the
  project detected from the cwd; `no_project` for a broad root). Project roots
  are compared after normalization and promotion to the git root.
- The stream read and the policy checked are the same project: the git root of the folder
  `--project`, `--project-root` or `--root` names. Those flags must name one project
  (`invalid_argument` otherwise, §Common rules).
- `--since TS`: strictly newer than TS. `--kind` may repeat (values below).
- `--limit` default 200, max 1000; the newest are kept, sorted oldest → newest
  by `at`, then `session_id`, then `seq` (deterministic for the same state).

```json
{
  "contract": "local-read/1",
  "kind": "activity.list",
  "session_id": "4f1c…",
  "project_root": null,
  "since": null,
  "total": 3,
  "returned": 3,
  "truncated": false,
  "events": [
    {
      "seq": 7,
      "at": "2026-11-04T09:12:33.120Z",
      "session_id": "4f1c…",
      "agent": "claude",
      "project_root": "/Users/me/code/app",
      "kind": "shell",
      "source": "hook",
      "tool": "Bash",
      "target": "cargo test -p app",
      "outcome": "ok",
      "text": null,
      "truncated": false
    }
  ]
}
```

- `kind` ∈ `user_message` | `agent_message` | `tool_call` | `file_read` | `file_write` | `shell` | `permission` | `turn_start` | `turn_end` | `error`.
- `source` ∈ `hook` | `mcp` | `acp`.
- `outcome`: `null` | `ok` | `error` | `denied` | `cancelled` (tool calls, shell, files) | `asked` | `allowed` | `denied` (permission).
- `target`: the path (file kinds), the command (shell), or the tool's main argument (path, URL, pattern, query); `null` when none. `tool`: the host's tool name or `null`.
- `text`: the message (user/agent message, permission prompt, error) or `null`.
- Bounds: `text` ≤ 2000 bytes, `target` ≤ 512 bytes, labels ≤ 128 bytes, cut at a
  char boundary with `…`, and then `truncated: true`.
- Secrets in `text` and `target` are masked when stored (the shell's redaction:
  a token becomes a `[REDACTED…]` marker), and the document is released through
  the CLI tool run like every other read. A withheld hook payload keeps its
  event without `text`/`target`.
- `--session ID` without a named project (3.12.3): the events span projects, so each event is
  released under its own project's policy (as `bus.list --all` §5a and `agent.diaries --all`
  §24.1 release theirs), then the document once more under the reader's (the cwd's project on
  the CLI; none on the socket). An event of a project whose policy withholds the read, or of a
  project whose folder is no longer present here, is left out and never released under
  another project's policy. An event without a project is released under the reader's.
- `withheld` (3.12.3, optional): the number of events the release point withheld from this
  answer, one by one. Present only when > 0. A count, carrying no event content; `returned` is
  the number of events shown. The CLI writes the content-free notice
  `[lean-ctx gateway: N record(s) withheld]` to stderr.
- `seq`: 1-based, increasing per session. Each session keeps about its newest
  1 MiB (a trim leaves a gap at the start); each project keeps its 64 most
  recently written sessions.
- `agent`: `claude` | `codex` | `cursor` | `gemini` | `codebuddy` | `copilot` | …, or
  `agent` when the host is unknown; show unknown values as-is.
- Session ids: `LEAN_CTX_SESSION_ID` when the session's environment sets it
  (§8); else hooks use the host's `session_id` (Cursor: `conversation_id`) and
  MCP calls use the host session id from the environment when the host
  exports one (`CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`, …), else the
  LeanCTX session id. LeanCTX's own MCP tools are recorded once, by the MCP
  server, not by the hook.

Every document of §7–§15 is released (redaction and one receipt for the
bytes the reader gets), on the CLI and on the socket alike; none is printed
outside the release.

## 8. Session identity and hook profile (inputs)

A launching app (LeanCTX Code) names each agent session it starts:

- `LEAN_CTX_SESSION_ID=<id>` in the session's environment (`id`: 1–128 bytes of
  `[A-Za-z0-9._:-]`, anything else is ignored). The MCP server and the hooks record
  activity under this id (it wins over the host's own session id); the LeanCTX session
  opened under it carries it (`session stats`); a bus presence registered under it
  carries it (`app_session_id`) and keeps its bus id per role across MCP restarts.
- `x-leanctx-session-id: <id>` on requests through the proxy (e.g. Claude Code
  `ANTHROPIC_CUSTOM_HEADERS`): the proxy books measured usage per app session
  (`session cost`); the 256 most recently used sessions are kept. Attribution only.
- `LEAN_CTX_HOOK_PROFILE=governor`: the standard hooks run as the governor's —
  `rewrite` and `redirect` as `rewrite-enforce` (allowlist wrap and secret-path deny
  only), `read-dedup` as `govern` (R1.read); `observe` is unchanged. Unset or
  `standard`: every hook is itself. The global setup is never changed.

- `LEAN_CTX_SESSION_NAME=<name>`: the app's name of the session (1–128 characters, no
  control characters, else ignored). A bus presence registered under it shows it
  (`presence.list` `session_name`/`display_name`). Display only.

`--session ID` below takes the app id or a LeanCTX session id.

## 9. Sessions

### 9a. `lean-ctx session stats --json --session ID`

```json
{
  "contract": "local-read/1",
  "kind": "session.stats",
  "session_id": "app-7f3a",
  "lean_sessions": ["20261104-091233u120p551s0"],
  "project_root": "/Users/me/code/app",
  "started_at": "2026-11-04T09:12:33Z",
  "updated_at": "2026-11-04T10:01:00Z",
  "tokens": { "input": 182000, "saved": 121000, "tool_calls": 88, "cache_hits": 31, "files_read": 42, "commands_run": 17 },
  "tools": { "compose": 12 },
  "knowledge": { "written": 2 },
  "kept_out": { "total": 5, "secrets_redacted": 2, "pii_redacted": 0, "content_withheld": 1, "shell_blocked": 1,
                "path_blocked": 1, "injection_flagged": 0, "coverage_incomplete": 0 },
  "waste": {
    "repeated_reads": { "files": 3, "extra_reads": 9, "top": [ { "path": "src/a.rs", "reads": 5 } ] },
    "failing_commands": { "failures": 4, "top": [ { "command": "cargo test -p app", "failures": 3, "last_at": "2026-11-04T09:58:00.000Z" } ] }
  }
}
```

- `lean_sessions`: the LeanCTX sessions attributed to the id, sorted; `tokens` and
  `kept_out` sum their counters. `started_at`/`updated_at` are `null` while none is.
- `tools.compose`: the `ctx_compose` calls of the attributed sessions, including those
  routed through `ctx_call` (counted under the inner tool, never twice). It is a subset
  of `tokens.tool_calls`, counted at the same point as that total and not an extra
  call: `tools.compose <= tokens.tool_calls` always holds. The Compose share is
  `tools.compose / tokens.tool_calls`.
- `knowledge.written`: the `ctx_knowledge` calls with `action="remember"` (directly or
  through `ctx_call`) that persisted to the project's knowledge store, either as a new
  or revised fact or as a near-duplicate merged into an existing one (`Merged …`).
  Not counted: a refused remember (missing category or value, input withheld by policy,
  store not saved — replies starting `Error`), a low-salience rejection (`Skipped …`),
  a tool error, and every other action (`recall`, `import`, `forget`, `consolidate` …).
- Both are additive (WP2): an engine before them leaves `tools` and `knowledge` out; a
  session saved before them counts `0`.
- `project_root`: of the newest attributed session, else of the newest activity event, else `null`.
- `repeated_reads`: files read more than once — per path the larger of LeanCTX's own
  read count and the activity stream's `file_read` events; `extra_reads` = Σ(reads − 1).
- `failing_commands`: activity `shell` events with outcome `error`, per command.
- `top` lists hold at most 10 rows (most first, then by path / newest / command).
  Activity is read from the newest 1000 read and shell events per session id.
- An id with neither a session nor activity → error `not_found`.

### 9b. `lean-ctx session cost --json --session ID`

```json
{
  "contract": "local-read/1",
  "kind": "session.cost",
  "session_id": "app-7f3a",
  "available": true,
  "source": "proxy",
  "total_usd": 1.234567,
  "measured_usd": 1.2,
  "requests": 14,
  "input_tokens": 90000,
  "output_tokens": 12000,
  "cache_read_tokens": 400000,
  "cache_write_tokens": 20000,
  "per_model": [
    { "model": "claude-opus-5-5", "requests": 14, "input_tokens": 90000, "output_tokens": 12000,
      "cache_read_tokens": 400000, "cache_write_tokens": 20000, "cost_usd": 1.234567, "pricing_estimated": false }
  ]
}
```

- `available: false` (numbers `0`, `per_model: []`) when the proxy saw no request of the session.
- `measured_usd`: the part the provider itself reported. `per_model` sorted by cost, then model.

## 10. Rollups

### 10a. `lean-ctx gain --daily --json [--days N]`

```json
{
  "contract": "local-read/1",
  "kind": "gain.daily",
  "days": [ { "date": "2026-11-04", "commands": 310, "input_tokens": 900000, "output_tokens": 120000, "tokens_saved": 780000, "saved_usd": 2.34 } ],
  "total": { "commands": 310, "input_tokens": 900000, "output_tokens": 120000, "tokens_saved": 780000, "saved_usd": 2.34 }
}
```

`--days` default 30, max 366; oldest → newest; `total` sums the days shown. `saved_usd`
is priced like the `gain --daily` table (the default cost model's input price).

### 10b. `lean-ctx gain --by-project --json`

```json
{
  "contract": "local-read/1",
  "kind": "gain.projects",
  "projects": [
    { "project_key": "a1b2c3d4e5f60718", "project_root": "/Users/me/code/app", "commands": 120,
      "input_tokens": 400000, "output_tokens": 50000, "tokens_saved": 350000, "saved_usd": 1.05,
      "last_use": "2026-11-04 09:00:00" }
  ]
}
```

`project_root` is `null` when no saved session on this machine names the key's folder.
`last_use` is the store's own timestamp text, or `null`. Sorted by `tokens_saved`, then key.

### 10c. `lean-ctx spend --json --by-project`

Plain `spend --json` is unchanged.

```json
{
  "contract": "local-read/1",
  "kind": "spend.projects",
  "available": true,
  "total_usd": 12.5,
  "projects": [ { "project_key": "a1b2c3d4e5f60718", "project_root": "/Users/me/code/app", "total_usd": 10.0, "requests": 120, "per_model": [] } ]
}
```

`per_model` rows as in `session.cost`. Sorted by `total_usd`, then key.

## 11. Agent bus (`agent-bus/1`)

The local agent bus is a supported surface: every `bus.*` and `presence.list` document
carries `"bus": "agent-bus/1"`; a breaking change bumps it. Reads and writes go through
the same registry (and its lock) as the agents' own `ctx_agent` calls, so a reader never
writes the registry file. Message rows are the `agent.messages` rows (§5a).

- `lean-ctx bus list --json [--project-root P] [--all] [--thread T] [--from ID] [--to ID] [--since TS] [--limit N]`
  → `{"kind": "bus.list", "bus": "agent-bus/1", "project_root": "/…", "messages": [row…]}`.
  The project's messages and project-free ones (`--all`: every project); oldest → newest,
  the newest N kept (default 200, max 2000). Adds the optional `withheld` count of §5a
  (present only when > 0, no content).
- `lean-ctx bus read --json --as ID [--project-root P] [--mark]`
  → `{"kind": "bus.read", "bus": "agent-bus/1", "reader": "cloud-person", "marked": true, "messages": [row…]}`.
  Unread messages of reader ID in the project: addressed to it or broadcast, not from it, not
  read by it. `--mark` records ID as their reader.
- `lean-ctx bus post --json (--to ID … | --broadcast) --message TEXT [--thread T] [--project-root P] [--priority low|normal|high|critical] [--category C] [--expires-in-hours H] [--task-id T] [--kind K] [--as ID]`
  → `{"kind": "bus.post", "bus": "agent-bus/1", "ids": ["m_1"], "from": "cloud-person", "to": [], "thread": "t1", "task_id": null, "message_kind": null, "priority": "normal", "category": "direct", "timestamp": "…", "expires_at": "…", "sender": {"kind": "person", "id": "cloud-person", "display_name": null}, "cloud": {"state": "local_only", "reason": "not_vault_address"}}`.
  As `agent post` (§5b, all or nothing); H 1–168, default 12; `category` 1–64 visible ASCII.
  `bus post` is the person's post unless `--as` names a sender. `cloud` says whether it
  leaves the machine (§18).
  QN0 (additive): `--task-id T` (1–128 characters, no blanks or control characters) files
  the post under a task; `--kind K` gives it a Queen-network kind (§18a; absent, empty or
  `null` = an ordinary message, an unknown name is `invalid_argument` with the fourteen
  kinds named); `--as ID` posts as a local agent of this Instance, and only from that
  agent's own session: the caller's `LEAN_CTX_SESSION_ID` must be the agent's app session
  (an id that is no active agent here, or a caller from another or no session, is
  `forbidden`); then `sender` is `{"kind": "agent", "id": ID, "display_name": …}`. The
  socket routes take neither `as` nor `kind` (local-service-v1; refused as an unknown
  parameter): a socket post is the person's, who posts no kind. `--to
  queen` names the registered Queen (§18a; none active → `forbidden`, `no_queen:`).
  Refusals, all writing nothing: a kind from a sender that is no
  registered Queen → `forbidden` with a message starting `not_queen:`; priority `high` or
  `critical` with a kind other than `conflict`, `escalate`, `stop` → `invalid_argument`;
  more than 20 posts by the same sender within a minute → `unavailable` with a message
  starting `rate_limited:` (try again shortly).
  `--request-id ID` (1–112 bytes, no control characters; socket `request_id`): a retry of
  the same post answers with the stored ids and writes nothing; the same ID with another
  post is `invalid_argument`. Without it a retry posts again.
- Every write here (`bus.post`, `bus.claim`, `bus.release`, and the knowledge writes of §12)
  passes its request through the release point first: when it would be withheld, nothing
  is written and the answer is the `withheld` document under the run's one receipt.
  `bus.claim` and `bus.release` are idempotent by their lease, knowledge writes by their key.
- `lean-ctx bus leases --json [--project-root P]`
  → `{"kind": "bus.leases", "bus": "agent-bus/1", "leases": [lease…]}`, active leases, soonest expiry first:
  `{"lease_ref": "…", "resource_kind": "path"|"symbol"|"work_graph_node", "resource_ref": "project-path:…/src/a.rs", "path": "src/a.rs"|null, "owner_agent_id": "mcp-…", "expires_at": "…"}`.
  `path`: the project-relative path of a path lease of the reader's project (lower-cased where paths fold).
- `lean-ctx bus claim --json --agent ID --resource-kind K --resource R [--minutes M] [--project-root P]`
  → `{"kind": "bus.claim", "bus": "agent-bus/1", "granted": true, "lease": {lease}|null, "held_by": null|"mcp-…"}`.
  M 1–60, default 15. Held by someone else → `granted: false`, `held_by` set (exit 0).
- `lean-ctx bus release --json --agent ID --lease REF`
  → `{"kind": "bus.release", "bus": "agent-bus/1", "lease_ref": "…", "released": true}`.
  Only ID's own lease; another agent's or an unknown one → `released: false` (exit 0).
- `lean-ctx presence list --json [--project-root P] [--all] [--all-projects]`
  → `{"kind": "presence.list", "bus": "agent-bus/1", "agents": [agent…], "logical_sessions": [session…]}`:

  ```json
  { "agent_id": "mcp-3037-…", "agent_type": "claude", "role": "tester", "project_root": "/…",
    "status": "active", "status_message": null, "started_at": "…", "last_active": "…", "alive": true,
    "pid": 3037, "transport": { "host": "claude-code", "name": null, "version": null }, "parent": null,
    "app_session_id": "app-7f3a", "kind": "agent", "session_name": "LeanCTX · Contracts",
    "display_name": "LeanCTX · Contracts", "host": "claude-code",
    "instance": { "id": "ins_7", "name": "fitim" }, "project": { "root": "/…", "vault": "acme/app" } }
  ```

  `display_name` (and `session_name`, the same value): the `name` the agent registered
  with, else the registering process's `LEAN_CTX_SESSION_NAME`, else `null` (a reader falls
  back to role, then id).

  Registering (`ctx_agent register`, agent-bus/1): `role` is a role id — trimmed and
  lower-cased, 1–48 characters of `a-z 0-9 _ -`, starting with a letter (`coder`,
  `reviewer`, `tester`, `docs-writer`, …); `name` is the display name (1–128 characters,
  one line). Free text given as `role` (e.g. a session name) is never stored as a role: the
  role stays as it was (`none` on a new presence), the text becomes the display name when no
  `name` is given, and the answer carries `warning: unknown_role`. A re-register of the same
  presence takes over the new role and name. The answer says what was stored:
  `Agent registered: <id> (type: <t>, role: <role|none>, name: <name|none>)`, then one line
  per warning. `host`: the agent host from the MCP
  handshake, `null` until identified. `instance`: this machine — the LeanCTX Cloud Instance id when
  enrolled (else `null`) and its name (the enrollment label, else the hostname).
  `project.vault`: `ns/name` of the vault linked to the agent's project, else `null`.

  A session: `{"source": "claude", "workspace": "/…", "session_id": "…", "opened_at": "…", "last_heartbeat": "…"}`.
  `status` ∈ `active` | `idle` | `finished` (the `agent presence` rule: a live process active
  within 60 s is `active`). Default: live agents of the project and those without one,
  most recently active first; `--all` adds finished ones, `--all-projects` every project.

## 12. Knowledge writes

Each runs the `ctx_knowledge` action (memory policy, admission and locks unchanged) and
answers with the fact as `knowledge.list` prints it (§1). A write the gateway refused is
error `withheld`; an unknown fact id is `not_found`.

- `lean-ctx knowledge remember --json --category C --key K --value V [--confidence 0..1] [--project-root P]`
  → `{"kind": "knowledge.remembered", "project_root": "/…", "created": true, "fact": {…}}`
  (`created: false`: a current fact on the key got the new value).
- `lean-ctx knowledge edit --json --id C/K --value V [--project-root P]`
  → `{"kind": "knowledge.edited", "project_root": "/…", "fact": {…}}`.
- `lean-ctx knowledge relate --json --from C/K --to C/K [--relation R] [--project-root P]`
  → `{"kind": "knowledge.related", "project_root": "/…", "edge": {"from": "C/K", "to": "C/K", "relation": "related_to", "created": true}}`;
  R ∈ `depends_on` | `related_to` (default) | `supports` | `contradicts` | `supersedes`.
- `lean-ctx knowledge remove --json --id C/K [--project-root P]`
  → `{"kind": "knowledge.removed", "project_root": "/…", "id": "C/K", "removed": true}` (`false`: nothing there).

## 13. Evidence

A broken chain is a verdict, not an error: `valid: false`, exit 0.

- `lean-ctx audit verify --json` → `{"kind": "audit.verify", "valid": true, "total_entries": 1200, "first_invalid_at": null, "source_present": true, "unknown_event_types": 0}`
  (full pass from genesis; `unknown_event_types`: entries of event types this build does not know).
- `lean-ctx savings verify --json` or `lean-ctx ledger verify --json` → `{"kind": "ledger.verify", "valid": true, "total": 5400, "first_invalid_at": null}`.
- `lean-ctx evidence-export --json` writes the evidence package into a NEW directory under
  `<data dir>/evidence/` (never a path the caller names: `--output` is `invalid_argument`) and
  answers `{"kind": "evidence.export", "output": "/abs/dir", "files": [ {"path": "evidence_summary.md", "bytes": 1234, "sha256": "…"} ], "audit": {…}, "ledger": {…}}`.
  `files` lists exactly the files the export wrote (regular files, each at most 16 MiB), by name.

### 13a. Signed reports: `lean-ctx report sign --json` and `lean-ctx report verify --json`

LeanCTX Code exports the monthly savings report (contract `cloud-savings-report/1`) with a
SHA-256 digest of its `report` object's compact JSON with sorted keys. When `capabilities`
lists `report` › `sign` via `cli`, the app asks this Instance to sign that digest:

- `lean-ctx report sign --json --contract cloud-savings-report/1 --digest <64 lowercase hex>` →
  `{"kind": "report.signature", "report_contract": "cloud-savings-report/1", "digest_alg": "sha256",
  "digest": "<hex>", "alg": "ed25519", "key_id": "id:sha256:<hex>", "public_key": "<64 hex>",
  "signature": "<base64>"}`.

The key is the one that signs execution receipts: the Ed25519 identity of `LEAN_CTX_AGENT_ID`
(default `local`) under `<data dir>/keys/`. The signature is Ed25519 over the canonical JSON
(recursively sorted keys, compact, UTF-8) of the document's own fields without `contract` and
`signature` — the receipt and evidence-manifest format. `key_id` is `id:` plus the SHA-256 of
the 32 public-key bytes. If that identity has no key yet, `report sign` creates one and says
so on stderr. Nothing is sent anywhere.

- `lean-ctx report verify --json --file <export.json>` checks an exported report: the digest
  against the SHA-256 of `report`'s canonical JSON, and `signature` (`alg`, `key_id`,
  `public_key`, `value`) over it.
- `lean-ctx report verify --json --contract C --digest HEX --public-key HEX --signature B64
  [--key-id ID]` checks one signature.

Both answer `{"kind": "report.verification", "valid": true, "report_contract": "…", "digest": "…",
"key_id": "…", "digest_matches": true | false | null, "reason": null | "…"}`. A failed check is a
verdict (`valid: false` with a plain `reason`, exit 0); a malformed argument or an unsigned export
is `invalid_argument`. `lean-ctx report` without `sign` or `verify` stays `report-issue`.

Verifying with the `public_key` embedded in the export proves that the file is unchanged since
that key signed it, not who signed it: anyone can edit the figures, recompute the digest and
sign with their own key. To know the signer, compare `public_key` (or `key_id`) with a trusted
key obtained another way, for example from the signing Instance's own `report sign` output.

### 13b. Landing receipts: `lean-ctx evidence import-landing <file> [--json]` and `lean-ctx evidence landings [--json]`

A landing service (merge queue) records each landing outcome as a `landing_receipt` document
(schema 1, at most 16 KiB, metadata only: forge, project, request number, SHAs, verdict, check
selection digest, `decided_at`; unknown fields are refused). Type and canonical JSON rules:
`lean_ctx_protocol::landing_receipt::LandingReceiptV1`.

- `import-landing` validates the file, verifies the Ed25519 signature when `signing` is present,
  and stores it as `.lean-ctx/proofs/landing-receipt-<first 16 hex of the id>.json`. `--json`
  answers `{"id": "sha256:<hex>", "integrity": "hashed" | "signed", "signer_key_id": null |
  "id:sha256:<hex>", "path": "/abs/file", "already_present": bool, "receipt": {…}}`. The id is
  the SHA-256 of the canonical JSON without `signing`, so a signed and an unsigned copy of one
  landing share it. A stored signed receipt is never replaced by an unsigned copy (the import
  is a no-op) and a copy signed by another key is refused (exit 1).
- `landings` lists the stored receipts, oldest `decided_at` first: `--json` answers
  `{"landings": [{"id", "integrity", "signer_key_id", "receipt"}], "invalid": ["<file>: <reason>"]}`;
  exit 1 when any stored file no longer validates.

`signed` means the signature verifies against the key embedded in the receipt, not who holds
it. `signer_key_id` is `id:` plus the SHA-256 of the 32 public-key bytes (the `key_id` form of
13a); compare it with a trusted key obtained another way.

## 14. `lean-ctx capabilities --json`

```json
{
  "contract": "local-read/1",
  "kind": "capabilities",
  "version": "3.12.x",
  "build": { "kind": "dogfood", "commit": "f78aff8dc5…", "dirty": false, "official": false },
  "kinds": ["activity.list", "agent.messages", "…"],
  "commands": {
    "bus": [ { "verb": "list", "via": ["cli", "socket"] }, { "verb": "claim", "via": ["cli"] } ],
    "cloud.tasks": [ { "verb": "list", "via": ["cli", "socket"] }, { "verb": "claim", "via": ["cli"] } ]
  },
  "session_env": "LEAN_CTX_SESSION_ID",
  "proxy_session_header": "x-leanctx-session-id",
  "hooks": { "profile_env": "LEAN_CTX_HOOK_PROFILE", "profiles": ["standard", "governor"] },
  "proxy": { "enabled": true, "host": "127.0.0.1", "port": 4444, "base_url": "http://127.0.0.1:4444",
             "health_path": "/health", "health_service": "lean-ctx-proxy", "session_header": "x-leanctx-session-id" },
  "server": {}
}
```

- `build`: which pipeline built this binary, stamped at compile time (never a runtime git
  call). `kind` ∈ `official` (the upstream release workflow `release.yml` of
  `yvgude/lean-ctx`, on a push or dispatch of `refs/heads/main` or a `refs/tags/v<digit>…`
  tag; never a pull request, fork or feature branch) | `dogfood` (the promoted
  `dogfood/instance` state) | `dev` (any other checkout) | `source` (a packaged crate);
  `commit` is the 40-hex source commit or `null`; `dirty`: tracked files under `rust/`
  differed from it (the binary re-checks whenever a crate source, a manifest or `HEAD`
  changes); `official` = `kind == "official"`. The kind is read from the GitHub Actions
  variables of the build; exporting the same variables by hand also gives `official`, so it
  is a provenance hint, not an attestation (the release artifacts carry that). `lean-ctx --version` prints the same as
  `lean-ctx 3.12.2 (official, https://github.com/yvgude/lean-ctx)` or
  `lean-ctx 3.12.2 (dogfood f78aff8dc5)` / `(dev f78aff8dc5, dirty)`;
  `lean-ctx --version --json` prints `{name, version, repository, unmodified, build}`.
- `kinds`: every kind this build prints, sorted. `commands`: command path joined by `.`
  (`cloud.tasks` = `lean-ctx cloud tasks`) → EVERY verb it supports, each with `via`, the
  transports that answer it (`cli`, `socket`; in that order). A verb that is a flag is named
  without its dashes (`gain`: `daily`, `by-project`). Order: the CLI's verbs, then verbs only
  the socket serves (sorted); command paths only the socket serves come last (sorted). A
  missing key, verb or transport is not supported — readers hide the action instead of
  reading `--help`. The CLI and the socket print the same document.
- `proxy` is configuration, not a probe: `GET base_url + health_path` answers 200 with
  `"service":"lean-ctx-proxy"` when the proxy is up.
- `server`: the `/v1/capabilities` document, unchanged.
- `routes` (`[{kind, method, path, params, project, cli, command?, verb?}]`, sorted by path
  then method) and `events` (`{path, types, views}`) describe the local service
  (`docs/contracts/local-service-v1.md`); its `GET /v1/local/capabilities` serves this same
  document.

## 15. Providers and policies

### 15a. `lean-ctx provider list --json [--project-root P]`

Configuration only; nothing is contacted.

```json
{
  "contract": "local-read/1",
  "kind": "providers.list",
  "project_root": "/Users/me/code/app",
  "enabled": true,
  "providers": [
    { "id": "github", "display_name": "GitHub", "source": "builtin", "config_path": null,
      "status": "available", "requires_auth": true, "actions": ["issues", "pull_requests"], "checkable": true }
  ]
}
```

- `enabled`: `[providers] enabled`; `false` → `providers: []`.
- `source` ∈ `builtin` | `mcp_bridge` | `space` | `config`; `config_path`: the file a
  `config` provider was read from (project-local files apply only in a trusted workspace).
- `status` ∈ `available` | `not_configured`. Sorted by `id`.
- `checkable`: `lean-ctx provider check <id> --json` can test the connection. Its answer
  `{provider, connected, account, scope, error, reason, latency_ms}` carries `reason`
  (`null` when connected) ∈ `not_configured` | `auth` | `unreachable` | `timeout` |
  `rate_limited` | `not_found` | `unavailable` | `unsupported` | `invalid_input` | `error`.

### 15b. `lean-ctx policy list --json [--project-root P]`

Read only; listing creates nothing. Plain `policy list` is unchanged.

```json
{
  "contract": "local-read/1",
  "kind": "policies.list",
  "project_root": "/Users/me/code/app",
  "project_file": null,
  "policies": [
    { "name": "never_include_secrets", "match": "**/.env*", "action": "exclude", "action_params": {},
      "condition": null, "condition_params": {}, "reason": "secrets", "source": "security_default", "overridable": false }
  ],
  "pack": { "name": "baseline", "version": "1", "chain": [], "default_read_mode": "map",
            "allow_tools": null, "deny_tools": [], "max_context_tokens": null, "audit_retention_days": 90 },
  "pack_sources": { "project_file": "/Users/me/code/app/.lean-ctx/policy.toml", "org": true,
                    "org_policy": { "source": "/…/org-policy.json", "org": "acme", "version": "7",
                                    "issued_at": "2026-11-01T08:00:00Z", "signature": "verified",
                                    "applied": true, "error": null } },
  "pack_error": null
}
```

- `policies`: the context policies in effect order. `source` ∈ `security_default` (always
  first, `overridable: false`) | `default` (built-ins while the project has no
  `.lean-ctx/policies.json`) | `project`. `action` ∈ `exclude` | `include` | `pin` |
  `set_view` | `max_tokens` | `mark_outdated` | `redact` | `audit` with `action_params`
  (`{"view"}`, `{"limit"}` or `{}`); `condition` is `null` or a name with `condition_params`.
- `pack`: the active policy pack (organization floor and project pack, resolved as
  enforcement resolves them), `null` when none applies. `pack_error`: why it could not be
  resolved (enforcement then denies), else `null`.
- `pack_sources.org_policy`: the installed organization policy (`null` when none is):
  `signature` ∈ `verified` (valid signature by a key pinned for that organization) |
  `untrusted` (valid, key not pinned for it) | `invalid` | `none` (not readable as a signed
  artifact); `applied`: enforcement applies it now; `error`: why not, else `null`.
  A diagnosis of what is stored: nothing is written or asked.

### 15c. `policies.layers` (`GET /v1/local/policies/layers`, CLI `policy layers --json --project-root P`)

The layered policy editor's read model: Cloud > project > personal. A lower layer may
only tighten a higher one. Read only; nothing is created.

```json
{
  "contract": "local-read/1",
  "kind": "policies.layers",
  "project_root": "/Users/me/code/app",
  "precedence": ["cloud", "project", "personal"],
  "cloud": {
    "state": "in_force", "signature": "verified", "version": 3, "digest": "sha256:…",
    "key_id": "k1", "issued_at": "2026-11-01T08:00:00Z", "not_after": "2026-12-01T00:00:00Z",
    "last_pull_at": "2026-11-15T07:00:00Z", "vault": null,
    "covered": ["secrets/**"], "covers_everything": false, "error": null,
    "rules": [
      { "id": "cloud:excluded_paths[0]", "name": "security.excluded_paths", "match": "secrets/**",
        "action": "exclude", "action_params": {}, "condition": null, "condition_params": {},
        "reason": null, "layer": "cloud", "locked": true, "editable": false,
        "origin": { "level": "org", "vault": null, "version": 3, "key_id": "k1",
                    "issued_at": "2026-11-01T08:00:00Z" } }
    ]
  },
  "project": {
    "level": "project", "file": "/Users/me/code/app/.lean-ctx/policies.json", "exists": true,
    "damaged": false, "revision": "sha256:…", "editable": true, "enforced": true, "error": null,
    "rules": [
      { "id": "security:never_include_secrets", "name": "never_include_secrets", "match": "**/.env*",
        "action": "exclude", "action_params": {}, "condition": null, "condition_params": {},
        "reason": "…", "layer": "project", "locked": true, "editable": false,
        "origin": { "level": "security_default", "vault": null, "version": null, "key_id": null, "issued_at": null } }
    ]
  },
  "personal": {
    "level": "personal", "file": "/Users/me/.local/share/lean-ctx/policy-layers/personal.json",
    "exists": false, "damaged": false, "revision": "sha256:e3b0…", "editable": true,
    "enforced": false, "rules": [], "error": null
  }
}
```

- `cloud`: the installed LeanCTX Cloud policy floor, read and checked exactly as enforcement does.
  `state` ∈ `in_force` | `offline_last_known` (past `not_after`: the last valid policy still
  restricts) | `none` | `invalid` (damaged or refused: fails closed, `covers_everything: true`).
  `signature` ∈ `verified` | `invalid` | `none`. `vault`: the vault whose entry applies to
  this project, else `null` (the default entry). `covered`: the path globs of
  `excluded_paths` and `never_send`, sorted. Every Cloud rule is `locked`, never `editable`;
  `action` ∈ `exclude` | `never_send` | `restrict_roots` (`{"roots"}`) | `block_secrets`
  (`{"enabled"}`) | `filter` (`{"action"}`) | `allow_tools` / `deny_tools` (`{"tools"}`) |
  `max_tokens` (`{"limit"}`).
- Rule `id`: `cloud:<member>[i]` / `cloud:<member>`, `security:<name>`, `project:<name>`,
  `personal:<name>`. Local rules use the `policies.list` action and condition shapes (15b).
  `origin.level` ∈ `org` | `vault` | `security_default` | `default` | `project` | `personal`.
- `project`: `.lean-ctx/policies.json`, read only through PathJail (a symlinked `.lean-ctx`
  is refused: `damaged: true`, `error` set, and `rules` lists only the security defaults).
  Its rules are those in effect (security defaults first, locked).
- `personal`: `<data_dir>/policy-layers/personal.json`. Free for everyone: adding, editing
  and removing need no license (`editable` is `false` only when the file is damaged).
  `enforced: false`: shown and stored, not yet applied by the engine.
- `revision`: `sha256:<hex>` of the layer file's bytes (of no bytes when absent); a change
  names it as `expected_revision`. `null` when the file exists but cannot be read (`damaged:
  true`).
- Determinism: `policies.layers` and `policies.history` are functions of the stored files
  and of whether the Cloud policy is still in force; neither generates a timestamp. A
  `policies.changed` result carries the time of its write (`evidence.at`).

### 15d. `policies.changed` (`POST /v1/local/policies/change`, CLI `policy change --json --project-root P --level project|personal --expected-revision R`)

One rule added, edited or removed, tighten-only. The body `{"op": "add"|"edit"|"remove",
"rule": {…}}` comes on stdin for the CLI (or `--op` and `--rule '<json>'`); the socket takes
`project_root`, `level`, `expected_revision`, `op` and `rule` (an object or its JSON text).
`rule` has the row shape of 15c (`name` or `id`, `match`, `action`, `action_params`,
`condition`, `condition_params`, `reason`; other fields are ignored). The release check runs
on the request before anything is written.

```json
{
  "contract": "local-read/1",
  "kind": "policies.changed",
  "project_root": "/Users/me/code/app",
  "level": "project", "op": "add", "rule_id": "project:keys",
  "applied": true, "refused": null, "revision": "sha256:…",
  "evidence": { "at": "2026-11-15T09:00:00Z", "actor": "local:me", "level": "project",
                "project_root": "/Users/me/code/app", "op": "add", "rule_id": "project:keys",
                "rule": { "id": "project:keys", "match": "secrets/keys/**", "action": "exclude", "…": "…" },
                "before_digest": "sha256:…", "after_digest": "sha256:…", "evidence_ref": "audit:…" }
}
```

A refusal is a document too (`applied: false`, exit 0); nothing is written:

```json
{ "contract": "local-read/1", "kind": "policies.changed", "project_root": "/Users/me/code/app",
  "level": "project", "op": "add", "rule_id": "project:api", "applied": false,
  "refused": { "reason": "policy.loosens_higher_layer", "layer": "cloud", "rule": "cloud:excluded_paths[0]",
               "origin": { "level": "org", "vault": null, "version": 3, "key_id": "k1", "issued_at": "…" },
               "message": "…", "current_revision": null },
  "revision": "sha256:…", "evidence": null }
```

- `refused.reason`:
  - `policy.cloud_locked`: the rule is a Cloud rule (`cloud:` id, `layer: "cloud"`, or
    `--level cloud`); Cloud rules change in LeanCTX Cloud only.
  - `policy.locked`: a security default; it cannot be changed or removed.
  - `policy.revision_changed`: `expected_revision` is not the layer's revision;
    `current_revision` names it.
  - `policy.cloud_unreachable`: the Cloud policy is `offline_last_known` or `invalid` and the
    old or new rule's match overlaps what it covers (`invalid`: any rule). Fails closed.
  - `policy.loosens_higher_layer`: an `include`, `pin` or `mark_outdated` rule whose match may
    overlap a Cloud `exclude` / `never_send` glob, or (personal) a project `exclude`, `redact`
    or `max_tokens` rule. `layer`, `rule` and `origin` name the higher rule.
- Overlap is conservative and ASCII case-insensitive. A local glob is anchored by its
  literal prefix (`p/**`, `p*`, `a**b`), cut at the first `*`, `?`, `[` or `{`; an
  unanchored glob (`**/x`, a bare name) may overlap any path. Cloud globs are gitignore
  globs: one that starts with `/` or `!`, or has no `/` other than a trailing one
  (`secrets*` matches at any depth), is unanchored; otherwise its literal prefix is cut the
  same way (`src/*/keys/**` is anchored at `src/`).
- `match` is a project-relative glob: a leading `/` or `./`, or a `..` segment, is
  `invalid_argument`.
- Errors (error document): `invalid_argument` (bad level, op or rule; a rule `id` whose
  layer is not `level`, e.g. `personal:x` with `--level project`), `not_found` (edit or
  remove of a missing rule), `conflict` (add of an existing name, damaged layer file),
  `forbidden` (the project layer's path is unsafe, e.g. a symlinked `.lean-ctx`, or the layer
  file cannot be read), `internal` (the audit record or the history entry could not be
  written: the change is not applied).
- Concurrency: one exclusive lock (`<data_dir>/policy-layers/.lock`) is held while the layer
  is re-read, its revision compared, the evidence recorded and the layer written. Of two
  changes naming the same `expected_revision`, one applies and the other gets
  `policy.revision_changed`.
- Storage: the project layer via PathJail (`.lean-ctx/policies.json`, 0600; the first write
  keeps the built-in defaults in effect); the personal layer in
  `<data_dir>/policy-layers/personal.json` (0600).
- Evidence first: the change is recorded in the local audit trail as `x-policy_changed`
  (digests only; `evidence_ref` names the entry) and appended to
  `<data_dir>/policy-layers/history.jsonl` (last 1000 kept) before the layer is written. If
  either fails, nothing is written; if the layer write fails, the history entry is taken
  back (the audit entry stays; its `after_digest` names no revision the layer had). The write's release receipt goes to LeanCTX Cloud through the existing receipt
  outbox, content-free and only where receipt uploads are allowed.

### 15e. `policies.history` (`GET /v1/local/policies/history`, CLI `policy history --json --project-root P [--limit N]`)

The recorded changes of this project's layer and of the personal layer, newest first.
`limit` 1–500, default 50. Read only.

```json
{ "contract": "local-read/1", "kind": "policies.history", "project_root": "/Users/me/code/app",
  "entries": [ { "at": "…", "actor": "local:me", "level": "project", "project_root": "…",
                 "op": "edit", "rule_id": "project:a", "rule": { "…": "…" },
                 "before_digest": "sha256:…", "after_digest": "sha256:…", "evidence_ref": "audit:…" } ],
  "total": 3 }
```

- `entries`: the evidence of 15d. `before_digest` of one entry is the `after_digest` of the
  previous change of the same layer. `total`: matching entries before `limit`.

## 16. `lean-ctx agents --json`

The coding agents on this machine that LeanCTX knows, in a fixed order
(`claude`, `codex`, `copilot`, `gemini`, `cursor`, `opencode`, `goose`, `qwen`,
`cline`, `droid`, `kiro`). No network. Never reads a credential: sign-in comes
from whether the agent's own credential file exists and from the names of its
API-key variables set in this process's environment. Each found binary runs
once with `--version` (3 s timeout, in parallel).

```json
{
  "contract": "local-read/1",
  "kind": "agents.list",
  "agents": [
    {
      "id": "codex",
      "name": "Codex CLI",
      "installed": true,
      "binary": "/opt/homebrew/bin/codex",
      "version": "0.98.0",
      "config_dir": true,
      "sign_in": { "state": "signed_in", "evidence": "credentials_file", "api_key_env": ["OPENAI_API_KEY"] },
      "leanctx": { "mcp": "configured", "hooks": "installed", "proxy": "not_wired", "mode": "governor", "coverage": "partial" },
      "acp": { "support": "adapter", "command": ["npx", "-y", "@agentclientprotocol/codex-acp"], "package": "@agentclientprotocol/codex-acp" },
      "resume": { "supported": true, "command": ["codex", "resume", "{session_id}"] },
      "setup_key": "codex"
    }
  ]
}
```

- `binary`: absolute path found on `PATH` or a usual install directory, else `null`.
  `version`: parsed from `--version`, else `null`.
- `config_dir`: the agent's own config directory exists.
- `sign_in.state` ∈ `signed_in` | `signed_out` | `unknown`; `evidence` ∈ `credentials_file` | `account_config` | `api_key_env` | `null` (`account_config`: the agent's own config names a signed-in account, e.g. Claude Code's `oauthAccount` in `~/.claude.json` while the token stays in the macOS keychain; only the key's presence is read).
  Agents that keep credentials in the system keychain report `unknown` unless a key variable is set.
  `api_key_env` lists variable NAMES only.
- `leanctx.mcp` ∈ `configured` | `missing` | `unsupported`; `leanctx.hooks` ∈ `installed` | `missing` | `unsupported`
  (`unsupported` = LeanCTX has no hook integration for this agent); `leanctx.proxy` ∈ `wired` | `not_wired` | `unsupported`.
- `leanctx.mode`: `governor` | `replace` | `hybrid` | `mcp` | `null` (not attached).
  `leanctx.coverage`: what the gateway sees (`enforced` | `partial` | `not_observable` | …, see `host-coverage.md`), `null` when not attached.
- `acp.support` ∈ `native` | `adapter`; `acp.command` starts the agent as an ACP server on stdio; `acp.package` is the adapter's npm package or `null`.
- `resume.command`: argv with the literal placeholder `{session_id}` (it may sit inside an argument, e.g. `--resume={session_id}`), or `null`.
- `setup_key`: the key for `lean-ctx setup --agents` / `init --agent`, or `null` (no LeanCTX setup yet).
- Route `GET /v1/local/agents`; `lean-ctx agents list --json` prints the same document.
  Released like every Contracts document.

## 17. `lean-ctx daemon status --json`

```json
{ "contract": "local-read/1", "kind": "daemon.status", "running": true, "pid": 4711,
  "socket": "/run/user/501/lean-ctx/daemon.sock", "autostart": "enabled", "service_name": "com.leanctx.daemon" }
```

- `running`: a verified LeanCTX daemon owns the recorded PID (a reused PID is not running);
  `pid` then, else `null`. `socket`: the daemon socket (named pipe on Windows) while it
  listens, else `null`.
- `autostart` ∈ `enabled` | `disabled` | `unknown` (no autostart service on this OS);
  `service_name`: `com.leanctx.daemon` (macOS LaunchAgent), `lean-ctx-daemon` (systemd user
  unit), `null` where there is none. Read only: nothing is started or repaired.
- Route `GET /v1/local/daemon`.

## 18. Person posts to a vault bus

`bus post --to "#ws/vault" …` is the person's post (sender `cloud-person`). `cloud` in its
answer:

- `state` ∈ `queued` (goes to the vault bus) | `local_only` (stays on this machine) |
  `withheld` (the contract keeps it here) | `held` (waits under a hold, reason
  `bus_on_hold`: a LeanCTX Cloud hold or stop on the whole bus, or the control replay after a damaged
  relay state; it goes once the hold ends). `reason`: `null` | `awaiting_cloud` (queued; the
  LeanCTX Cloud has not accepted a person's publish on the last connection yet) |
  `not_vault_address` | `not_enrolled` | `unknown_vault` (a vault this Instance does not
  follow) | `vault_denied` (withheld: LeanCTX Cloud denied this Instance the vault's bus) |
  `type_unsupported` (withheld: answers, findings, offers and handoffs need their own
  fields) | `screened` (withheld: the relay's screen blocks the text). Decided as the relay
  decides.
- The relay publishes it as `place.publish {schema, client_msg_id, to, type, body,
  sender: {"kind": "person"}}` without `worker_id`, only once the LeanCTX Cloud server's `hello.ack` lists
  the feature `person_publish` on the current connection (a new connection waits for its
  own `hello.ack`). The LeanCTX Cloud attributes it to the account that owns this
  Instance's enrollment (the frame names no user and no handle) and refuses it when that
  account is not a member of the vault's workspace; it does not deliver it back to the
  connection that sent it. Delivered elsewhere as `sender: {kind: "person", user_id, handle,
  label}`. Screening, holds and privacy apply as to an agent's post (a private post or a
  screened text is withheld).
- HN0: a post `--as` a local agent leaves the machine only when that agent is the Queen
  session the app registered (`POST /v1/local/queens`, local-service-v1); then it is
  published as her worker and needs no `person_publish`. Any other agent's post to a vault
  stays on the local bus: `cloud` `{"state": "local_only", "reason": "not_queen"}`.

## 18a. Queen-network kinds and the relay (QN0, HN0)

The Queen network (leanctx-workspace `docs/queen-network.md`) gives bus messages a `kind`:
`hello`, `status`, `offer`, `request`, `handoff`, `coordinate`, `conflict`, `escalate`,
`ask`, `answer`, `decide`, `consent`, `execute`, `stop`; `null` is an ordinary message.
All of it is additive within agent-bus/1: a reader ignores fields it does not know, a post
without the new inputs is filed and relayed byte for byte as before.

- **Who posts a kind:** only the Queen session the app registered (an active agent whose
  app session is in `queen-sessions.json`, not a sub-agent). Anyone else, the person
  included, is refused `not_queen` (`forbidden`). A role is only a label: an agent that
  registers with role `queen` is no Queen and is neither refused at registration nor
  allowed a kind.
- **Address `queen`:** a post to `queen` (CLI `--to queen`, socket `to`, `ctx_agent post`
  `to_agent`) is filed to the registered Queen's agent id (the most recently active one);
  with none active it is refused `no_queen` (`forbidden`; `ctx_agent` answers
  `Error: not posted: no_queen: …`).
- **Where a kind is posted:** the CLI (`--kind`, as the Queen through `--as` from her own
  session) and `ctx_agent post` (`kind`, `task_id`, `thread`, with the same checks, stored as
  below). The socket post routes take no `kind` (local-service-v1).
- **Priority:** `high` and `critical` only with `conflict`, `escalate` and `stop`.
- **Stored:** the kind in the row's metadata `cloud.kind`, the thread in `cloud.thread`, the
  task in the row's `task_id`; rows read them as `kind`, `thread`, `task_id` (§5a).
- **Rate limit:** at most 20 posts per sender per minute, with or without a kind, on the CLI
  and in `ctx_agent post`, at posting (`rate_limited`) and
  again per worker at the relay (an excess post waits for the next minute; it is never
  dropped).
- **Relay (HN0):** of this Instance's agents only the registered Queen's posts are
  published to a vault's LeanCTX Cloud bus; every other worker's post is decided local (one carrying
  a kind counts as withheld). A person's own post keeps §18.
- **Frame:** a post is published as `place.publish` with the top-level fields `kind`,
  `task_id` and `thread`, each independently when set and well formed (a task or thread
  travels without a kind too), next to the C0 2.0 publish fields; a post with none of them
  is published without them. A delivered
  `message.deliver` that carries them lands on the local row with the same fields; a
  malformed one is dropped field by field, never the message. The LeanCTX Cloud must accept the three
  fields on `place.publish` and refuses a kind from an id that is no registered Queen
  (`not_queen`); that check is the Cloud's.
- **Presence:** each `hello.presence` entry and `presence.update` frame also carries
  `display_name` (the worker's agent name), `instance_id` and `instance_name` (this
  Instance's id and enrollment label), each only when known; display only, never used to
  address or authorize. The LeanCTX Cloud reads the C0 2.0 fields and ignores the rest.

---

## 19. `lean-ctx cloud vaults --json [--project-root P]`

The vaults of the connected workspace this Instance knows, for choosing where to share a
project. Local state only: no request leaves the machine. LeanCTX Cloud has no Instance route that lists
a workspace's vaults (the full list is on the web), so `complete` is always `false`: the rows are
the vaults this Instance has shared with plus the vault the project's link file names.
`not_connected` when this Instance is not connected.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.vaults",
  "project_root": "/Users/me/code/api",
  "workspace": { "id": "org_a", "name": "Platform", "label": "Team Platform", "kind": "team" },
  "complete": false,
  "web_url": "https://cloud.leanctx.com",
  "vaults": [
    {
      "address": "platform/api",
      "ns": "platform",
      "name": "api",
      "vault_id": "vlt_…",
      "role": null,
      "private": null,
      "current": true,
      "sources": ["shared"]
    }
  ]
}
```

- Sorted by `address` (`<ws>/<vault>[@<server>]`, what `cloud link --vault` takes).
- `project_root`: `null` outside a project. `workspace.kind`: `personal` | `team` | `null`.
- `vault_id`: `null` for a vault only the link file names (not resolved yet).
- `role`, `private`: not known to an Instance, always `null` in local-read/1.
- `current`: the project is linked or mapped to this vault; a row only the link file names
  (`sources: ["link_file"]`, `vault_id: null`) is `current` before the first sync.
- `sources` ⊆ `shared` (this Instance shared with it) | `link_file`.

## 20. `lean-ctx cloud link --vault V --json [--project-root P] [--dry-run] [--bring-local] [--leave-unsettled-local]`

Links the project to a vault and, on a connected Instance, maps it right away the way the next
sync would, without a prompt. `V` is an address `<ws>/<vault>[@<server>]`, or the `vault_id` or
name of a vault from section 19 (`not_found` when unknown, `invalid_argument` when a name is
ambiguous). Writes the link file `.lean-ctx-cloud.json` (and `.lean-ctx-id` when the project has
no identity); `--dry-run` writes nothing and contacts nobody. The consent rules are unchanged: a
first share that waits for the person's yes comes back as `consent_required`; nothing here
approves. Creating a vault under a chosen name is not offered: LeanCTX Cloud creates a project's own
vault (named after its folder) only through the first-share consent or `cloud connect`.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.link",
  "project_root": "/Users/me/code/api",
  "vault": { "address": "platform/api", "ns": "platform", "name": "api" },
  "previous_vault": null,
  "dry_run": false,
  "link_file": "/Users/me/code/api/.lean-ctx-cloud.json",
  "created_identity": false,
  "share": {
    "state": "consent_required",
    "vault_id": null,
    "consent_id": "cns_…",
    "answer_web": "lean-ctx cloud consent --web cns_…",
    "answer_terminal": "lean-ctx cloud consent approve cns_…",
    "message": "the first share waits for the person's yes"
  },
  "unsettled_local": [],
  "bring_local": null
}
```

**Unsettled local tasks refuse every link.** Once linked, a project's tasks are LeanCTX Cloud's, so a
local task that is only proposed, claimed or submitted (and not brought yet) would be stranded.
While there is one, every path that links a project refuses and writes nothing: this command
(with or without `--bring-local`) answers error `conflict` naming them (`#3 (proposed), #5
(submitted)`), and the terminal `lean-ctx cloud link <ws>/<vault>` refuses the same way. The
person settles them locally (decide, i.e. accept or decline; submit and verify, or release) and links again,
or links with `--leave-unsettled-local`: then they stay local, `unsettled_local`
(`[{ "local_number", "title", "state" }]`, else `[]`) names them, and they keep working with
`lean-ctx tasks … --local` (§3c). A dry run names them in `unsettled_local` and links nothing.
A link file that arrives through git cannot be refused: its local tasks that were not brought
stay reachable the same way and are listed as `local_not_shared` (§3a).

- `share.state` ∈ `not_checked` (dry run) | `not_connected` (linked locally; shared once the
  Instance connects) | `other_cloud` (the address names another LeanCTX Cloud server than this Instance's) |
  `shared` (`vault_id` set) | `consent_required` | `declined` | `waiting` (a consent in another
  state, see `message`) | `pending` (LeanCTX Cloud unreachable; the background sync retries) | `refused`.
- `answer_web` runs the browser flow: it opens the consent page, prints its URL and waits up to
  10 minutes for the answer. The URL exists only once that command asked LeanCTX Cloud for it.
  A local app may run it, since the person answers in the browser. `answer_terminal` is for the
  person's own terminal: a local app shows it and never spawns it (that would answer the consent
  on the person's behalf). Both are `null` unless `consent_required`.
- `previous_vault`: `{ "address" }` of the vault the project was linked (link file) or mapped to
  before, when it is another one, also on `--dry-run`; else `null`. A local app confirms with the
  person before a link that moves the project away from it.
- Re-linking a mapped project to another vault drops this checkout from the old vault's local
  record, so the mapping sends the new link.
- `bring_local`: `null` unless `--bring-local`. Then it brings the project's local tasks
  (`tasks.list` of an unlinked project) and local doc pages (`docs.list` of an unlinked project)
  into the vault, once each, tasks first, and the document is released through the CLI tool run
  like a docs read (titles and slugs are project text):

  ```json
  "bring_local": {
    "applied": true,
    "tasks_applied": true,
    "docs_applied": true,
    "reason": null,
    "message": null,
    "tasks": [
      { "local_number": 3, "title": "Explain the retry budget", "state": "open", "action": "propose", "reason": null, "vault_number": 41, "error": null },
      { "local_number": 1, "title": "Set up CI", "state": "done", "action": "skip", "reason": "done", "vault_number": null, "error": null }
    ],
    "docs": [
      { "local_id": "n_…", "slug": "guide", "title": "Guide", "action": "import", "reason": null, "vault_page_id": "n_…" },
      { "local_id": "n_…", "slug": "faq", "title": "FAQ", "action": "skip", "reason": "exists_in_vault", "vault_page_id": null }
    ]
  }
  ```

  - `applied`: everything that was to go went; `tasks_applied` / `docs_applied` say it per part
    (true when that part had nothing to bring). When not `applied`, `reason` ∈ `dry_run` |
    `not_shared` (`share.state` is not `shared`: link again once the consent is answered) |
    `pending` (nothing went, see `message`) | `partial` (some went and the rest waits: the items
    say which, e.g. the tasks went and the pages wait for the vault's replica before its first
    sync; `message` names what waits). Pages that wait are listed as the plan from local state
    alone, where a page the vault already holds still shows as `import`. Linking again with
    `--bring-local` finishes what waits and skips what went.
  - `tasks[].action` ∈ `propose` | `skip`. Only an **open** local task is brought: LeanCTX Cloud opens
    every task an Instance proposes (there is no way to send a proposal that waits for
    acceptance), so it arrives open in the vault with the same title, acceptance and role; its
    goal ends with `Local task #<n>`, the local number as the reference. `reason` ∈
    `already_shared` (`vault_number` is the vault task) | `not_accepted` (a local proposal: it
    stays local until it is accepted, so it never arrives in the vault as accepted) |
    `in_progress` (claimed or submitted: its claim, result and evidence stay local) | `done` |
    `cancelled` | `rejected` | `failed` | `unknown_state` | `no_title` | `no_acceptance`. A
    brought task is marked shared in the local store with its vault number, so a second run
    skips it; the local task keeps its own state.
  - Before a task is sent, this Instance records it (with its proposal key) in a ledger in its
    config; LeanCTX Cloud's number is recorded as soon as it answers. A link interrupted after LeanCTX Cloud
    answered only marks the task on the next run, never sends it again; a send LeanCTX Cloud never
    answered is repeated with the same key within 6 days (LeanCTX Cloud replays the first answer for 7)
    and after that is not sent again: `error` then asks the person to look for
    `Local task #<n>` in the vault. An entry for another vault (a re-link) is not reused. A task
    refused for good (screened text, an invalid request, a 4xx from LeanCTX Cloud) leaves the ledger: no
    vault task exists, so `error` says it was not created and the next link sends it again
    once it is changed locally. `error` (else `null`) says why a task did not finish this
    time.
  - `docs[].action` ∈ `import` | `skip`; `reason` ∈ `already_shared` (brought by an earlier link;
    `vault_page_id` is where it went) | `exists_in_vault` (the vault has a page with that slug;
    left alone). Links between brought pages point at their new vault pages, links to a slug the
    vault holds point at that page. Each brought page is marked shared in the local store
    (journaled, by the person, labelled "cloud link"), so a second run skips it.
  - The pages reach LeanCTX Cloud with the next push; the background sync retries what fails.

---

## 21. Knowledge freshness: `knowledge stale | verify | dedupe | outdated | conflicts`

Which facts are no longer current truth, and the primitives a weekly clean-up runs. Every
command takes `--json` and `--project-root P`.

- **Fact reference** (`fact`, `keep`, `a`, `b`, list entries):
  `{id, category, key, text, confidence, anchors: [{path, symbol}], review, last_confirmed, updated}`.
  - `id` uses the `knowledge list` ids: `<category>/<key>`; a second current fact on one key,
    and every superseded version, is `…@<version>`.
  - `review` is `null` | `to_verify` | `gone` | `contradicted`.
- **Reason**: `{code, path, symbol, moved_to}`. `code` is one of `anchor_changed`,
  `anchor_moved` (`moved_to` names the new path), `anchor_missing`, `contradiction` or
  `source_removed`. The anchor fields are `null` for the last two.
- **One source of truth for anchors**: the live check of the anchored code on disk. Only
  files this Instance may read for knowledge are read.
  - The stored *to verify* mark is what `knowledge list`, AGENTS.md and LeanCTX Cloud read. It follows
    the live check: an agent's edit through this Instance marks it, a reverted edit clears it,
    and `stale` writes back every stored mark that disagrees before it lists.
  - A `contradiction` or `source_removed` mark is never cleared by a check; it needs a person.

### 21a. `lean-ctx knowledge stale --json [--limit N]`

Current facts that are not current truth, first `gone`, then `to_verify`, then
`contradicted`; within each by id. `--limit` 1–1000 (default 200).

```json
{ "contract": "local-read/1", "kind": "knowledge.stale", "project_root": "/Users/me/code/app",
  "total": 1, "returned": 1, "truncated": false,
  "facts": [ { "fact": { … }, "since": "2026-11-04T09:12:33Z", "reasons": [ { … } ] } ] }
```

- `since` is when the stored mark was set, or `null` when the live check found the change
  and the store could not be written.

### 21b. `lean-ctx knowledge verify <id> --json`

"Checked, still true". It re-anchors every anchor to the code as it is now, clears an anchor
or source mark, and records a confirmation (`last_confirmed`, confirmation count).

```json
{ "contract": "local-read/1", "kind": "knowledge.verify", "project_root": "/Users/me/code/app",
  "fact": { … }, "reanchored": { "changed": 1, "moved": 0, "unchanged": 0 },
  "cleared": "anchor_changed" }
```

- `cleared` is the reason code of the mark it removed, or `null`.
- Errors:
  - `not_found`: no current fact has the id;
  - `anchor_gone`: an anchored file or symbol is gone, so rewrite or remove the fact instead.
    Nothing is written.
- The person's command: an agent's shell allowlist does not include it.

### 21c. `lean-ctx knowledge dedupe --json [--threshold T] [--dry-run | --apply]`

Exact and near duplicates among this Instance's own current facts (origin local or
unverified), per category. Without `--apply` it is a dry run and writes nothing.

- `--threshold` 0.5–1.0 (default 0.9) is the text similarity of a near match.
- **Never merged**:
  - a pair judged `contradicts` or `unrelated`;
  - contradicted facts;
  - two texts that differ in a polarity word (`not`, `never`/`always`, `must`/`may`,
    `true`/`false`, `enable`/`disable`, `allow`/`deny`, `only`, `all`, `any`, …) or in a
    number.
- **The survivor** of a group is the fact with the most confirmations, then the longest text,
  then the oldest, then the smallest id.
- **`--apply`** needs an explicit `--project-root`, and only the person runs it.
  - The survivor keeps its own text (it is never rewritten) and gains the other facts'
    anchors and confirmations.
  - The other facts are archived as superseded by it, with a judged `supersedes` pair. Their
    text stays in the history (`outdated`, `knowledge list` versions).
  - `--apply` with `--dry-run` is `invalid_argument`.

```json
{ "contract": "local-read/1", "kind": "knowledge.dedupe", "project_root": "/Users/me/code/app",
  "applied": false, "threshold": 0.9, "merged": 0,
  "groups": [ { "category": "ops", "keep": { … },
                "merge": [ { "fact": { … }, "match": "exact", "similarity": 1.0 } ] } ] }
```

- `match` is `exact` (same text up to case and spacing) or `near`.
- Ids name the facts as they were before the merge.

### 21d. `lean-ctx knowledge outdated --json [--days N] [--limit N]`

Two groups, superseded facts first, then by id:
- superseded facts, with the current fact that replaced them;
- current facts nobody confirmed since the cut-off.

The cut-off is midnight UTC, `--days` days before today. It stays the same all day.

- `--days` 0–36500 (default 90); a larger value is `invalid_argument`.
- `--limit` 1–1000 (default 200).

```json
{ "contract": "local-read/1", "kind": "knowledge.outdated", "project_root": "/Users/me/code/app",
  "days": 90, "cutoff": "2026-08-06T00:00:00Z", "total": 2, "returned": 2, "truncated": false,
  "facts": [ { "fact": { … }, "reason": "superseded", "superseded_by": "ops/deploy-day",
               "valid_until": "2026-11-04T09:12:33Z" } ] }
```

- `reason` is `superseded` or `unverified`. `superseded_by` and `valid_until` are `null` for
  `unverified` facts.

### 21e. `lean-ctx knowledge conflicts --json`

Contradictions to show a person:
- judged `contradicts` pairs that are both still current (`source: "judged"`, with `judged_at`);
- two current facts under one `category/key` with different text (`source: "same_key"`,
  `judged_at: null`);
- `unpaired`: contradicted facts whose partner is gone.

```json
{ "contract": "local-read/1", "kind": "knowledge.conflicts", "project_root": "/Users/me/code/app",
  "conflicts": [ { "a": { … }, "b": { … }, "source": "judged", "judged_at": "2026-11-04T09:12:33Z" } ],
  "unpaired": [ { … } ] }
```

- Pairs are ordered by `a.id`, then `b.id`, with `a.id < b.id`.

---

## 22. Knowledge graph: `knowledge related` and `knowledge graph`

Nodes and typed edges of the project knowledge.

- **Edge kinds**: only the LeanCTX Cloud protocol's.
  - Recorded relations are printed as stored: `depends_on` | `related_to` | `supports` | `contradicts` | `supersedes`.
  - Derived edges are computed on every read and never stored:
    - `supersedes` (an update of a key → the version it replaced),
    - `contradicts` (current facts on one key that disagree, newer → older),
    - `applies_to` (fact → anchored code),
    - `contains` (file → symbol).
- **Node ids**:
  - facts carry the `knowledge list` ids (`<category>/<key>`, vault knowledge `cloud/<address>/<node>`);
  - superseded versions, and a second current fact on one key, are `<category>/<key>@<version>`;
  - code nodes are `code:<path>` and `code:<path>#<symbol>`.
- **Node**: `{id, node_kind: fact|code, label, category, key, version, state: current|superseded|missing|null, text, path, symbol, distance}`.
- **Edge**: `{from, to, kind, origin: recorded|derived, count, strength, last_seen}`. The last three are `null` for derived edges.
- **Missing nodes**: a recorded relation whose end names no current fact or vault node keeps
  that end as a `state: "missing"` node, but only when its other end is a real node. A relation
  between two missing ends (or to excluded noise) is left out and adds nothing to the counts.
- **Later (not in 3.12.3)**: a `references` edge kind, session, task and doc nodes, and writing
  links (`knowledge link`). Until then the graph is read-only and only has the kinds above.

### 22a. `lean-ctx knowledge related <id> --json [--project-root P] [--depth N] [--direction in|out|both] [--limit N] [--include-noise]`

The nodes one node reaches, or that reach it, along the edges.
- `--direction`:
  - `out` follows edges from the root: what it depends on, supersedes, applies to.
  - `in` follows edges into the root: what depends on it, what applies to a code node.
  - `both` (the default) is the union of the two walks. Each path keeps one direction, so
    `a → x ← b` does not make `b` related to `a`.
- `--depth` 1–3 (default 1).
- `--limit` = max nodes 1–1000 (default 200; by distance, then id; the root is always kept).
- Flags also take the `--name=value` form. `--contract local-read/1` is accepted and is never read
  as the id.

```json
{ "contract": "local-read/1", "kind": "knowledge.related", "project_root": "/Users/me/code/app",
  "root": "architecture/auth-flow", "depth": 1, "direction": "both", "total_nodes": 4,
  "total_edges": 3, "truncated": false, "nodes": [ … ], "edges": [ … ] }
```

- `nodes` sorted by `distance`, then `id`; `edges` = every edge among the returned nodes;
  `total_*` before the cap.
- Unknown id → `not_found`.
- `invalid_argument` for: a missing id, a second id, a bad `--depth` or `--direction`, or a flag
  the command does not take.

### 22b. `lean-ctx knowledge graph --json [--project-root P] [--limit N] [--include-noise]`

All non-noise current facts, the vault knowledge, the superseded versions and code they reach,
and every edge among them. `--limit` = max nodes 1–2000 (default 500); under the cap the best
connected nodes (degree desc, then current first, then id).

```json
{ "contract": "local-read/1", "kind": "knowledge.graph", "project_root": "/Users/me/code/app",
  "total_nodes": 812, "total_edges": 1630, "truncated": true, "excluded_noise": 312,
  "nodes": [ … ], "edges": [ … ] }
```

- `invalid_argument` for any argument or for a flag the command does not take (`--depth` and
  `--direction` belong to `related`).
- `nodes` sorted by `id`. Noise = `knowledge list`'s read-noise (`auto:` facts and `finding`s that
  only record a read); `--include-noise` brings it back, and a noise id is `not_found` in
  `related` without it. Recorded relations to excluded noise are left out.
- Without `--json` both commands print `from -[kind]-> to` lines and errors as text on stderr.
- Note: the document's top-level keys come out in alphabetical order (the shared Local Read
  writer); the order carries no meaning.

## 23. Policy and sync: `cloud sync status`, `cloud policy sessions|session`

Read only, from local files; nothing contacts LeanCTX Cloud. Served on the local service
(Local Service v1 §2) and listed in capabilities (`cloud.sync`: `status`;
`cloud.policy`: `sessions`, `session`).

Timestamps are RFC 3339 strings. The examples show one possible document; any field marked
nullable below may be `null` or carry a value.

**Two id spaces.** `cloud.sync` `vaults[].vault_id` is the vault's *place id* (the id the
replica and the sync status use). `policy.sessions` `installed.vaults[].vault_id` is the
*raw vault id* of the vault map and of the policy's vault-level entries (the id enforcement
matches on). They are different strings for the same vault: do not join the two documents on
`vault_id`; join on `roots` or on `@ns/name` (`vault` in `cloud.sync`, `ns` + `name` here).

### 23.1 `cloud.sync` (`GET /v1/local/cloud/sync`, CLI `cloud sync status --json`)

```json
{
  "running": true,
  "interval_s": 300,
  "last_pass_at": "2026-11-09T10:00:00Z",
  "vaults": [
    {
      "vault_id": "p_3f2a…",
      "vault": "@acme/web",
      "last_sync_at": "2026-11-09T10:00:00Z",
      "last_ok_at": "2026-11-09T10:00:00Z",
      "last_trigger": "periodic",
      "pending": 0,
      "stored": 12,
      "pushed": 3,
      "failures": 0,
      "error": null,
      "next_attempt_at": null,
      "parked": false,
      "needs_login": false,
      "code": null,
      "retry_after_s": null,
      "rejected": [
        {"category": "decision", "key": "db-choice", "reason": "too_short",
         "hint": "…", "at": "2026-11-09T09:58:00Z"}
      ]
    }
  ]
}
```

- `running`: a live sync thread wrote the status within the last 120 s (its heartbeat).
- `interval_s`: the periodic sync interval; `last_pass_at`: the last pass over all vaults
  (`null`: none yet).
- `vaults[]`, in the order of the status file. `vault_id`: place id (see above); `vault`:
  `@ns/name`; `last_sync_at` / `last_ok_at`: the last attempt / the last success;
  `last_trigger`: `local` | `bus` | `periodic` | `manual`; `pending`: signed events waiting in
  the replica for LeanCTX Cloud; `stored` / `pushed`: counts of the last successful sync; `failures`:
  failures in a row; `error`: the last error (`null` after a success); `next_attempt_at`: the
  backoff's or recheck's next try (`null`: no backoff, or a vault parked until its plan
  changes).
- `code`: the last failure's machine-readable kind (`null` after a success): `rate_limited`
  (the token mint or a request; repeated ones back off exponentially), `upload_rate_limited`,
  `plan_required` (the vault is parked until the plan state changes or a manual sync
  succeeds), `storage_full` (parked, rechecked every 6 h), `not_found` (only with
  `parked: true`: LeanCTX Cloud answered `404` three times in a row, each at least 10 min after
  the last counted one; rechecked every 6 h, ended early by a reconnect or another
  `[cloud] url`; a `404` before the third is an `error` that backs off), `needs_login` (always
  with `needs_login: true`, see below), `quota_exceeded`, `policy_denied`, `offline`, `upgrade_required`, or `error`.
  Earlier versions wrote `unauthorized` for a `401`; treat it as `needs_login`. More codes may
  follow; treat an unknown one as `error`. `retry_after_s`: the seconds the cloud asked to wait
  with that failure (`null`: it asked for none).
- `parked` (always written): `true` while the vault is parked (`plan_required`, `storage_full`,
  `not_found`). `needs_login` (always written): `true` after a `401` (Cloud refused this
  instance's credentials, `code` `needs_login`): not parked, but a push on write skips the
  vault and the background sync tries it again only after 6 h, then every 24 h while Cloud
  keeps answering `401` (`next_attempt_at`), until a login or reconnect, or a sync succeeds;
  never `true` together with `parked`. Earlier versions
  wrote neither key when `false`; read a missing one as `false`. `plan_park` appears only while a `plan_required` park holds the vault: an opaque stamp
  of the plan state the park was taken under (a newly received lease ends it); `link_park`
  appears only while a `not_found` park or `needs_login` holds it: an opaque stamp of the link
  state (a login, reconnect or another `[cloud] url` ends it, which also clears `code` and
  `error`). `not_found_streak` / `not_found_at` appear only while `404`s are being counted.
  Do not parse the stamps.
- `rejected[]`: facts the capture gate kept on this machine for that vault (at most 200, the
  newest), still waiting for a fix: `category`, `key`, `reason` (a snake_case reason code,
  e.g. `too_short`, `no_reference`, `sensitivity`), `hint` (how to fix it) and `at`. Fix the
  fact and remember it again; the next sync shares it.

### 23.2 `policy.sessions` (`GET /v1/local/cloud/policy/sessions`, CLI `cloud policy sessions --json`)

```json
{
  "installed": {
    "version": 7,
    "digest": "sha256:…",
    "key_id": "k3",
    "signature": "verified",
    "state": "in_force",
    "acked": true,
    "acked_at": "2026-11-09T08:00:00Z",
    "pulled_via": "nudge",
    "last_pull_at": "2026-11-09T08:00:00Z",
    "nudges": true,
    "vaults": [
      {"vault_id": "v_…", "ns": "acme", "name": "web",
       "roots": ["/home/me/web"], "policy": "default"},
      {"vault_id": "v_…", "ns": null, "name": null, "roots": [], "policy": "vault"}
    ],
    "enforcement": {
      "applied_version": 7,
      "acknowledged_at": "2026-11-09T08:00:00Z",
      "sessions": ["app-1"],
      "reported_at": "2026-11-09T08:00:02Z"
    }
  },
  "sessions": [
    {"session_id": "app-1", "version": 7, "digest": "sha256:…", "signature": "verified",
     "state": "in_force", "receipts": 42, "last_at": "2026-11-09T09:59:00Z"}
  ]
}
```

- `installed`: the signed policy installed here; **`null` when no LeanCTX Cloud policy is installed**.
  - `version`, `digest`: the installed policy (`null` when `signature` is `invalid`).
  - `key_id`: the signing key of the effective policy (`null` when not known for this
    version, or `invalid`).
  - `signature`: `verified` (installed after its signature verified) | `invalid` (the
    installed files are damaged or older than required: fails closed, sensitive operations are
    refused).
  - `state`: `in_force` | `expired` (past its `not_after`: sensitive operations refused) |
    `none` (with `signature: "invalid"`).
  - `acked`, `acked_at`: whether this Instance acknowledged this version and digest to LeanCTX Cloud,
    and when (`null`: not acknowledged).
  - `pulled_via`: what started the last policy pull: `nudge` (a bus hint) | `poll` (the
    periodic pull) | `manual` (`lean-ctx cloud sync`); `null`: no pull recorded.
    `last_pull_at`: when (`null`: never).
  - `nudges`: LeanCTX Cloud sends policy nudges to this Instance, so a change applies at once;
    `false`: the periodic pull brings it (within `pull_interval_minutes`).
  - `vaults`: one row per vault the policy reaches on this machine, sorted by `vault_id`
    (raw vault id, see above): every vault linked here and every vault with a vault-level
    entry in the installed policy. Row: `vault_id`, `ns`, `name` (`null` for a vault not
    linked here), `roots` (the checkouts linked to it, sorted) and `policy`:
    - `vault`: the policy carries a vault-level entry for it;
    - `default`: the policy's default applies;
    - `unverified`: the installed policy files are damaged or older than required, so which
      applies cannot be told (every linked vault is `unverified`, and `signature` is
      `invalid`).
  - The linked vaults come from the vault map (`projects.json`). A vault map that cannot be
    read is taken as empty, the same as "no checkout linked here": `vaults` then lists only
    the policy's vault-level entries.
  - `enforcement`: the installed policy as enforced on this machine.
    - `applied_version`: the version enforced, only when `signature` is `verified`, `state`
      is `in_force` and `acked` is `true`; `null` otherwise (an expired, unverified or
      unacknowledged policy is never shown as enforced).
    - `acknowledged_at`: when LeanCTX Cloud acknowledged `applied_version` (`null` with it).
    - `sessions`: the live app sessions enforcing the installed policy, sorted: those whose
      newest receipt (within the last 15 minutes) was made under this version and digest,
      verified and in force. Empty for an expired or invalid policy.
    - `reported_at`: when this Instance reported `applied_version` to LeanCTX Cloud on the bus
      (frame `policy.applied`, once per version and digest, sent only when LeanCTX Cloud's
      `hello.ack.features` names `policy_applied`); `null` until then.
- `sessions[]`, sorted by `session_id`: one row per app session with recorded evidence
  (at most 256 sessions, the most recently written). The row names the policy of the
  session's newest receipt: `session_id`, `version`, `digest`, `signature`, `state` (as in
  `installed`, at the time of that receipt), `receipts` (the session's receipts over all its
  applied versions) and `last_at` (its newest receipt).

### 23.3 `policy.session` (`GET /v1/local/cloud/policy/sessions/{id}`, CLI `cloud policy session <id> --json`)

```json
{
  "session_id": "app-1",
  "applied": [
    {"version": 7, "digest": "sha256:…", "key_id": "k3", "signature": "verified",
     "state": "in_force", "acked": true, "first_at": "2026-11-09T08:01:00Z",
     "last_at": "2026-11-09T09:59:00Z", "receipts": 42,
     "receipt_ids": ["r_…", "r_…"]}
  ]
}
```

- `{id}` (or `--session ID`): 1–128 characters of `A-Z a-z 0-9 . _ : -`, otherwise
  `invalid_argument`; an unknown id is `not_found`.
- `applied[]`: oldest first, at most 16 rows (the newest); the last row is the policy of the
  session's newest receipt. Row: `version`, `digest`, `key_id`, `signature`, `state`,
  `acked` (as in `installed`, when the row was recorded), `first_at` / `last_at` (its first /
  newest receipt), `receipts` (its receipt count, never capped) and `receipt_ids`: **the
  newest 32 local receipt ids only, oldest first** — `receipts` can be larger than
  `receipt_ids` has entries.

### 23.4 Not served yet

- Cloud's "waiting for you" items are not served yet: the Cloud answers
  `/api/v1/me/waiting` only for a person or a personal token, not for an Instance.

### 23.5 Pending: what the Cloud must add for `/v1/local/cloud/waiting`

- **Today:**
  - `GET /api/v1/me/waiting` (leanctx-cloud `crates/cloud-server/src/routes/me.rs`, `waiting`)
    takes the `Caller` extractor (`crates/cloud-server/src/principal.rs:709`). It accepts only
    `Principal::Human` and `Principal::Token`, and rejects `Principal::Instance`, so an
    Instance credential cannot read it.
  - `POST /api/v1/me/waiting/{id}/snooze` takes `Human` only.
- **The Cloud must add** an Instance read route (e.g. `GET /v1/instance/waiting`), or accept
  `Principal::Instance` on `GET /api/v1/me/waiting`. Either way:
  - **Identity:** it acts as `InstanceAuth.owner_user_id`.
  - **Scope:** it is scoped to `InstanceAuth.org_id` like a token (the open-items query binds
    the org to `org_id`, never `NULL`), so an Instance never sees another org's items.
  - **Same query parameters:** `limit` 1..200 (default 20), and `workspace`, which must name
    the Instance's org, otherwise 404.
  - **Same response:** `{"items": [{"id", "kind", "severity", "title_vars", "place",
    "due_at", "created_at", "action": {"href"}}], "total"}`.
  - **Rate limit:** its own budget, keyed per Instance.
  - **Capability:** an entry in the server capabilities, so the engine can detect the route.
- **Snooze stays person-only;** Desktop opens `action.href` for it.
- **Then the engine adds** `GET /v1/local/cloud/waiting` (kind `cloud.waiting`, CLI
  `cloud waiting --json`), a released relay of that route, listed in capabilities.

## 24. Engine files: `agent diaries`, `cloud worker list`, `archive show`, `bus revision` (3.12.3)

Four kinds for engine state a local reader used to parse from the data directory itself
(`agents/diaries/**.json`, `cloud/workers.json`, the tool-result archives and
`agents/registry.json`). A reader switches when `capabilities.kinds` lists the kind and must
not read those files any more. Every document passes the release point like every other read
(one receipt per answer). Output is deterministic for the same engine state.

### 24.1 `agent.diaries` (`GET /v1/local/agent/diaries`, CLI `agent diaries --json [--project-root P | --all] [--limit N]`)

The agents' diaries (decisions, discoveries, blockers, progress, insights) of one project, or
with `--all` (`all=true`) of every project on this machine (the person's console; the route
then needs no `project_root`). `limit` 1..400, default 100 diaries.

```json
{"contract": "local-read/1", "kind": "agent.diaries",
 "project_root": "/abs/project",
 "diaries": [{"agent_id": "claude-1", "agent_type": "claude", "project_root": "/abs/project",
   "updated_at": "2026-11-15T10:00:00Z",
   "entries": [{"type": "decision", "content": "…", "context": null,
     "at": "2026-11-15T10:00:00Z", "truncated": false}]}],
 "withheld": 0}
```

- `project_root` is `null` with `--all`.
- Diaries newest `updated_at` first (ties: `agent_id`, then `project_root`); one per agent and
  project (the newest copy wins).
- `entries` newest first, at most 100; `type` is `discovery` | `decision` | `blocker` |
  `progress` | `insight` (show unknown values as-is). `content` is cut to 4000 characters and
  `context` to 512; `truncated` says one of them was cut.
- Diaries carry agent text: they are released diary by diary. A diary the release withholds
  is left out and counted in `withheld`. With `--all` each diary is released as its own
  project's reader is; a diary of a project whose folder is no longer present here is left out
  and counted, never released under another project's policy.
- Bounded reads: the engine reads at most 400 diary files per request (one project: its
  current partition first; `--all`: the most recently written), never follows a symlinked
  partition or file (checked on the opened file too), and skips files over 1 MiB before
  reading them.

### 24.2 `cloud.workers` (`GET /v1/local/cloud/workers`, CLI `cloud worker list --json`)

The durable workers this machine's instance keeps (the local worker book). Content-free.

```json
{"contract": "local-read/1", "kind": "cloud.workers", "connected": true, "cap": 32,
 "workers": [{"worker_id": "wkr_…", "host_app": "codex", "role": "general", "name": null,
   "registered": true, "registered_at": "2026-11-15T10:00:00Z",
   "last_attached": "2026-11-15T11:00:00Z", "last_seen": "2026-11-15T11:00:00Z",
   "per_session": false}]}
```

- `connected`: an instance is enrolled here; `false` → `workers` is empty.
- `cap`: how many durable workers one instance keeps. Rows sorted by `worker_id`, at most `cap`.
- `name`: `null` → the default name. `registered`: LeanCTX Cloud registered the worker
  (`registered_at`). `last_seen` is `last_attached`, else `registered_at`; all times RFC 3339
  (seconds, UTC), `null` when unknown.
- `per_session`: the worker of one app session; it ends with that session.
- `capabilities.commands["cloud.worker"]` lists `list` and `retire`, so a reader decides on the
  retire action from data (`cloud worker retire …` is unchanged).

### 24.3 `archive.entry` (`GET /v1/local/archive/{id}?project_root=P`, CLI `archive show <id> --json [--project-root P]`)

One archived tool result (the id `ctx_expand` and the compressed output name) of the request's
project.

```json
{"contract": "local-read/1", "kind": "archive.entry", "id": "0123456789abcdef",
 "tool": "ctx_shell", "bytes": 48213, "text": "…", "truncated": false}
```

- `id` is 16 or 64 lowercase hex characters; anything else is `invalid_argument` (no path is
  ever built from it). The CLI form is `archive show <id>` only; `archive <id>` is
  `invalid_argument`.
- Only an archive of the named project (or a source-bound one it still admits) resolves:
  `not_found` otherwise. An archive the content protection refuses is `withheld`.
- `tool`: the tool whose output it is, `null` when not recorded. `bytes`: the size of the whole
  released text. `text` carries at most 2 MiB (cut on a character boundary); `truncated` says
  it was cut.
- The text passes the release point as the project's (`ctx_expand` scope).

### 24.4 `bus.revision` (`GET /v1/local/bus/revision`, CLI `bus revision --json --project-root P [--project-root Q …]`)

A content-free revision of the agent bus per project, so a reader polls cheaply and calls
`bus.list` only when it changed (instead of watching `agents/registry.json`).

```json
{"contract": "local-read/1", "kind": "bus.revision", "bus": "agent-bus/1",
 "roots": [{"project_root": "/abs/project", "revision": "9f2c…(32 hex)", "messages": 12,
   "next_expiry": "2026-11-16T10:00:00Z"}]}
```

- 1..16 `project_root` values (repeat the parameter); each is resolved and authorized like
  every route's `project_root` (canonical, a project, inside the Workspace: `no_project` /
  `forbidden` otherwise), on the CLI too. Rows sorted by the canonical `project_root`,
  duplicates once (`/p/` and `/p` are one row). The CLI accepts `--project-root P` and
  `--project-root=P`.
- Each row is released under its own root's policy. A root whose policy withholds the read has
  no row: it is left out and counted in `withheld` (optional, present only when > 0), so
  `roots` may hold fewer rows than roots named. Before, every row was released under the first
  root's policy and each named root had a row.
- `revision` changes whenever `bus.list` for that project would serve other messages (a new,
  expired, changed or newly read message), and only then; another project's messages never
  move it. It is opaque: compare for equality only.
- `messages`: the live messages `bus.list` serves that project (at most 2000).
  `next_expiry`: when the next of them expires (the revision changes then), `null` if none
  expires.

## 25. Plan lease: `cloud lease refresh` (3.12.3)

`cloud.lease.refresh` (`POST /v1/local/cloud/lease/refresh`, CLI `cloud lease refresh --json`,
no body, no project) renews this computer's plan lease with LeanCTX Cloud now, instead of at
the scheduler's next renewal (the app asks after a checkout or a plan change). The engine sends
the scheduler's own request, `POST /v1/instances/{id}/lease/renew` with its instance token,
verifies the signed lease against the stored key set and stores it. Nothing else leaves the
computer; the engine holds no payment keys and contacts no payment provider. Released as a
write (G08) under `ctx_agent` with no request fields.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.lease.refresh",
  "plan": "team",
  "state": "valid",
  "issued_at": "2026-11-16T10:00:00Z",
  "lease_valid_until": "2026-11-17T10:00:00Z",
  "grace_until": "2026-11-23T10:00:00Z"
}
```

- `plan`: `none` | `personal` | `team` | `enterprise` (an unknown plan of a newer Cloud: `unknown`).
- `state`: `valid` | `grace` | `lapsed`, at service time when the lease arrived.
- `lease_valid_until`: the lease's `expires_at`; paid capabilities hold until then, then in grace
  until `grace_until`.
- Errors: `not_connected` (no connected instance, revoked, or the Cloud refused the instance
  token), `unavailable` (Cloud unreachable), `rate_limited`, `internal` (a lease that fails
  verification is not stored).

### 25.1 Billing

The lease only carries the plan; refreshing it opens no checkout and changes no subscription.
Checkout and billing-portal links are served since 3.12.3 as `cloud.billing.checkout` and
`cloud.billing.portal` (§27). After a checkout or a plan change there, the app calls
`cloud.lease.refresh` so this computer holds the new plan without waiting for the scheduler.

## 26. Cloud decisions: `cloud decisions list` (3.12.3)

`decisions.list` (`GET /v1/local/cloud/decisions?project_root=P`, CLI
`cloud decisions list --json --project-root P`, project required) lists the decisions of the
project's LeanCTX Cloud vault. The repo's own records (`.leanctx/decisions/*.toml`) are not in
this document; a reader takes them from the worktree. The engine sends
`GET /v1/vaults/{vault_id}/decisions` with its instance token and passes the Cloud's `items`
(`leanctx.cloud.decisions/v1`) through unchanged. Nothing is written. Released under
`ctx_knowledge`.

```json
{
  "contract": "local-read/1",
  "kind": "decisions.list",
  "project_root": "/work/app",
  "source": "cloud",
  "vault": { "address": "acme/platform", "vault_id": "vlt_1" },
  "items": [{ "id": "dec-use-postgres", "title": "Use Postgres", "status": "accepted" }]
}
```

- Errors: `no_project`, `not_connected` (no connected instance or the Cloud refused the token),
  `no_vault` (the project is not linked to a vault), `forbidden` (the instance lacks
  `knowledge.read`), `policy_denied` (§26.2), `not_found`, `rate_limited`, `unavailable`,
  `internal` (an answer without `items`).

On a connected machine `cloud link` (plain and `--json`, §20) shares the linked project at once,
so `decisions.list` and `decisions.create` answer right after the link, without a `cloud sync`
first (next release).

### 26.1 `decisions.create` (`POST /v1/local/cloud/decisions/create`, CLI `cloud decisions create --json --decision '<JSON>' [--project-root P]`)

Records a team decision in LeanCTX Cloud as the person the instance is linked to: the engine
sends `POST /v1/decisions` (Cloud API §17.2) with its instance credential, and the Cloud acts as
that person in the engine's own team, with the person's role, plan and rate budgets; its audit
log names the person and the engine. Released under `ctx_knowledge` as a write; project optional.

- `decision` (a JSON object, or the same as a JSON string): the decision record without the
  Cloud's own fields. Taken: `id`, `title` and `statement` (both required), `rationale`,
  `decided_by`, `decided_at` (`YYYY-MM-DD`), `source`, `scope`, `check`; `null` fields are left
  out. `status` may only be `active` and `supersedes` only empty (a decision recorded here
  supersedes nothing); any other field is `invalid_argument` before anything is sent.
- `project_root`: a project linked to a vault records the decision in that vault (the engine
  sends the vault's name); without one (or `""`) the decision is the team's. A named project that
  is not linked answers `no_vault`.

```json
{
  "contract": "local-read/1",
  "kind": "decisions.create",
  "project_root": "/work/app",
  "vault": { "address": "acme/platform", "vault_id": "vlt_1" },
  "decision": { "id": "use-postgres", "title": "Use Postgres", "origin": "cloud", "status": "active" }
}
```

`decision` is the Cloud's record as it answered it. Errors keep the Cloud's words: `conflict`
(`decision_exists`, or the team holds as many decisions as it may), `invalid_argument`
(`invalid_field`), `forbidden` (role or plan), `policy_denied` (§26.2), `not_found` (the vault
is not the team's), `no_vault`, `not_connected`, `rate_limited`, `unavailable`, `internal` (an
answer without the decision).

### 26.2 Policy refusals: `policy_denied` (next release)

LeanCTX Cloud enforces its sharing policy. A refusal by policy is never `forbidden`, `internal`
or an empty list: `decisions.list` and `decisions.create` answer the error `policy_denied`
(socket `403`) for each of these Cloud answers:

| Cloud answer | `cloud_code` | means |
|---|---|---|
| `403 policy_denied` with `member`, `level` (and `action` for a consent rule) | `policy_denied` | the policy member refuses it |
| `409 non_mergeable` with `field` | `non_mergeable` | the policy chain does not merge; never "no rules" |
| `403 sharing_not_confirmed` | `sharing_not_confirmed` | a person has not confirmed sharing yet |

```json
{"contract":"local-read/1","kind":"error","error":{"code":"policy_denied","message":"LeanCTX Cloud policy refuses this: this vault is not among the vaults the policy allows (policy sharing.allowed_vaults, set at the team level); it is not retried until an admin changes the policy in LeanCTX Cloud","cloud_code":"policy_denied","member":"sharing.allowed_vaults","level":"workspace"}}
```

- `cloud_code`: the Cloud's code. `member`: the dotted policy member as the Cloud sent it
  (`member`, else `field`): `sharing.allowed_vaults`, `sharing.task_claim_min_role`,
  `sharing.consent_required_for`, `sharing.cross_team_sharing`, `sharing.personal_workspace`.
  `level`: the Cloud's level (`org`, `workspace`, `vault`, `instance`), passed through as data.
  `action`: the action a person confirms on the LeanCTX Cloud web. Each is present only when the
  Cloud sent it as a plain token (`[a-z0-9_.-]`); readers ignore fields they do not know.
- `message` is LeanCTX's own text (deterministic for one answer); the Cloud's text is not
  repeated. A reader shows `message` and keys on `member` and `level`.

### 26.3 Team members: `cloud.members.list` (`GET /v1/local/cloud/members`, CLI `cloud members list --json`)

Lists the members of the team the instance is linked to. The engine sends `GET /v1/members` with
its instance token and passes the Cloud's `items` and `next_cursor` through. Only the caller's
own email comes back: the Cloud sends `user.email` on the caller's row alone (absent on every
other row), and the engine passes an answer that carries more than one email on with every email
removed. Nothing is written. Released under `ctx_knowledge`; no project.

- Row: `user` (`id`, `name`, `email` only on the caller's own row), `role`, `via` (one of
  `invite`, `sso`, `scim`, `owner`, `break_glass`, `cloud`), `joined_at`, `instances_connected`
  and `instances` (integers).

```json
{
  "contract": "local-read/1",
  "kind": "cloud.members.list",
  "source": "cloud",
  "items": [{ "user": { "id": "usr_1", "name": "Ada", "email": "ada@example.com" }, "role": "owner", "via": "owner", "joined_at": "2026-11-01T00:00:00Z", "instances_connected": 1, "instances": 2 }],
  "next_cursor": null
}
```

- Errors: `not_connected` (also the Cloud's `401`), `forbidden` (`error.reason`
  `step_up_required` when the Cloud asks the person to confirm on the web), `plan_required`,
  `policy_denied` (§26.2), `not_found`, `rate_limited` (`error.reason` `members_read_instance`
  when the Cloud limits this instance's member reads), `unavailable`, `internal` (an answer
  without `items`, or the Cloud's `upgrade_required`: the message names the lean-ctx version it
  needs).

### 26.4 Send to agents: `decisions.send` (`POST /v1/local/cloud/decisions/send`, socket only)

After the person's confirmed "Send to agents" click in LeanCTX Code, the app asks the engine to
have LeanCTX Cloud send one team decision to the team's agents. The engine sends
`POST /v1/decisions/{decision_id}/send` with `{"confirm": true}` and its instance token; the
Cloud then announces the decision to the team's machines on the bus (`decision.changed`), and each
machine's relay delivers it to its agents as a directive. Released under `ctx_knowledge` as a
write; project optional.

- Body: `decision_id` (required; the Cloud's decision id: `dec-` and lowercase kebab segments
  of `[a-z0-9]`, at most 80 characters, else `invalid_argument`), `project_root` (optional).
- App-only. At each launch LeanCTX Code writes a fresh random key (64 lowercase hex characters)
  to `<data dir>/agents/app-launch.key` (mode 0600) and sends it in the request header
  `x-leanctx-app-key`. The daemon reads the file on each such request and compares it in constant
  time; without the file, without the header, or with another key the answer is `403 app_only`
  and nothing is sent. The CLI has no verb for it (`capabilities` lists `send` for
  `cloud.decisions` as a socket verb only).

```json
{
  "contract": "local-read/1",
  "kind": "decisions.send",
  "project_root": null,
  "decision_id": "dec-0042",
  "sent": { "decision_id": "dec-0042" }
}
```

`sent` is the Cloud's answer as it sent it. Errors: `app_only`, `invalid_argument`,
`not_connected`, `forbidden` (`error.reason` `step_up_required` when the Cloud asks the person to
confirm on the web), `policy_denied` (§26.2), `not_found`, `conflict`, `rate_limited`,
`unavailable`, `internal`.

## 27. Team billing: `cloud billing checkout | portal` (3.12.3)

LeanCTX Code holds no Cloud token, so it opens the team's Stripe checkout or billing portal
through these kinds. Each is one call of LeanCTX Cloud's engine billing routes
(`POST /v1/billing/checkout`, `POST /v1/billing/portal`, Cloud API §17.2): the engine calls them
with its instance credential and the Cloud acts as the person the instance is linked to, in the
engine's own team, with that person's role (owner or admin), two-factor rule and rate budgets.
The engine never talks to Stripe. The document carries only the link the Cloud answered, and only
when it is an `https` link on `checkout.stripe.com` or `billing.stripe.com`; nothing else of the
answer reaches the reader. Both are writes released under `ctx_agent`; no project.

| kind | route | CLI | body |
|---|---|---|---|
| `cloud.billing.checkout` | `POST /v1/local/cloud/billing/checkout` | `cloud billing checkout --json --plan personal\|team --interval monthly\|yearly [--currency usd] [--seats N]` | `{plan, interval, currency?, seats?}` |
| `cloud.billing.portal` | `POST /v1/local/cloud/billing/portal` | `cloud billing portal --json` | `{}` |

`plan` is `personal` or `team`, `interval` `monthly` or `yearly`, `currency` only `usd` (not
sent to the Cloud), `seats` a positive whole number and only for `team`; anything else is
`invalid_argument` before anything is sent. The seat range is the Cloud's: a count it does not
sell comes back as `invalid_argument` with the Cloud's words.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.billing.checkout",
  "url": "https://checkout.stripe.com/c/pay/cs_test_1"
}
```

Refusals (the Cloud's billing state is read from `error.code`, `error.state` or `error.reason`):

| LeanCTX Cloud | code | message |
|---|---|---|
| `invalid_state` `subscribed` | `conflict` | already subscribed, open the portal |
| `invalid_state` `no_customer` | `not_found` | this team has no subscription yet, start one with checkout |
| `invalid_state` `company_billed` | `forbidden`, `reason` `company_billed` | billed through your company |
| `not_configured` (503) | `unavailable` | billing is not configured on LeanCTX Cloud |
| `upstream` (502) | `unavailable` | the payment provider did not answer, retry later |
| `totp_required`, `step_up_required` (`totp_not_enrolled`) | `forbidden` | confirm with two-factor in the browser |
| `sso_required` (403), `step_up_required` (`sso_required`) | `sso_required` | sign in with SSO in the browser |
| `invalid_field` (400/422) | `invalid_argument` | the Cloud's message |
| `forbidden`, `plan_required` (403/402) | `forbidden` | the Cloud's message (only an owner or admin manages billing) |
| 429 | `rate_limited` | |
| other 5xx | `unavailable` | |

`not_connected` when no instance is connected or the Cloud no longer accepts its credential;
`internal` when the Cloud answers without a Stripe link.

`sso_required` (3.12.3, additive) is a `403` on the local socket like `forbidden`, with the same
message. Before it, an SSO refusal was `forbidden` "sign in with SSO in the browser". A reader
that knows `sso_required` offers the SSO sign-in in the browser; one that does not treats it as
`forbidden` and shows the message. An engine from before it still answers `forbidden` with that
message, so a reader that also matches the message works with both.

### 27.1 Model credit: `cloud billing credit` (mz16)

LeanCTX Code shows the team's model credit for the month in agent-hours. `cloud.billing.credit`
is one call of LeanCTX Cloud's engine route `GET /v1/billing` (Cloud API §17.2), which answers
like the web's `GET /api/v1/orgs/{o}/billing` for the person the instance is linked to: only an
owner or admin of the team sees it, and a team that requires SSO for the person refuses it. It is
a read released under `ctx_knowledge`; no project. Of the Cloud's answer the document keeps only
the plan, whether a company pays (`company_billed`), the model credit, the credit pack on sale
and whether this person may buy it (`manage_billing`); seats, prices and the subscription stay
in the Cloud. Buying more agent-hours happens on the Cloud's billing page in the browser; the
engine never talks to Stripe.

| kind | route | CLI |
|---|---|---|
| `cloud.billing.credit` | `GET /v1/local/cloud/billing/credit` | `cloud billing credit --json` |

```json
{
  "contract": "local-read/1",
  "kind": "cloud.billing.credit",
  "plan": "team",
  "company_billed": false,
  "model_credit": {
    "used_usd_micros": 2500000,
    "limit_usd_micros": 10000000,
    "allowance_usd_micros": 7500000,
    "topup_available_usd_micros": 2500000,
    "left_usd_micros": 7500000,
    "agent_hour_usd_micros": 250000,
    "used_agent_hours": 10.0,
    "limit_agent_hours": 40.0,
    "left_agent_hours": 30.0,
    "state": "ok",
    "reset_at": "2026-12-01T00:00:00Z"
  },
  "credit_topup": { "pack_usd_micros": 5000000, "pack_agent_hours": 20.0, "max_packs": 10 },
  "manage_billing": true
}
```

`model_credit` is never `null`: the Cloud reports the credit on every plan, and a missing or
`null` credit is `internal` (never guessed). `credit_topup` is `null` when the Cloud sells no
pack. `manage_billing` (additive) is `true` only when the Cloud says so: the person holds the
owner role and the team is not billed through a company. It is `false` for an admin (who sees
the credit but does not buy), in a company's team (checkout, portal and packs answer
`company_billed` there), and when a Cloud from before the field omits it or sends anything but
`true`; the app offers "Buy agent hours…" only on `true`. Agent-hours are rounded down to one
decimal by the Cloud. `state` is `ok`,
`soft` (most of the credit is used) or `hard` (used up: models answer `quota_exceeded` until the
reset or a top-up).

Refusals: `forbidden` "only an owner or admin of the team sees its model credit" for a member,
`sso_required` "sign in with SSO in the browser" where SSO is required, `unavailable` when the Cloud
has no engine route yet (`404`: open billing in the browser), `rate_limited` and `unavailable` as
in §27, `not_connected` without a connected instance, and `internal` when the Cloud's credit
cannot be read (a missing or malformed field is never guessed).

## 28. Team chat: `cloud chat channels | messages | replies | post | read | dm | mentions` (3.12.3)

LeanCTX Code holds no Cloud token, so it reaches the team chat through these kinds. Each is
one call of LeanCTX Cloud's engine chat routes (`/v1/chat/…`): the engine calls them with its
instance credential and the Cloud acts as the person the instance is linked to, in that
person's team only, with that person's membership, role, plan, rate budgets and errors. No
project is named (`project: false`).

| kind | route | Cloud route | inputs |
|---|---|---|---|
| `chat.channels` | `GET /v1/local/chat/channels` | `GET /v1/chat/channels` | `archived` (bool), `cursor`, `limit` (1–500) |
| `chat.messages` | `GET /v1/local/chat/channels/{id}/messages` | `GET /v1/chat/channels/{id}/messages` | `tail`, `before_seq`, `after_seq` |
| `chat.replies` | `GET /v1/local/chat/replies` | `GET /v1/chat/channels/{channel}/messages/{message}/replies` | `channel`, `message`, `tail`, `before_seq`, `after_seq` |
| `chat.post` | `POST /v1/local/chat/post` | `POST /v1/chat/channels/{channel}/messages` | `channel`, `body`, `parent_id`, `mentions` (array), `orchestrator` (bool) |
| `chat.read` | `POST /v1/local/chat/read` | `POST /v1/chat/channels/{channel}/read` | `channel`, `seq` |
| `chat.dm` | `POST /v1/local/chat/dm` | `POST /v1/chat/dms` | `members` (array) |
| `chat.mentions` | `GET /v1/local/chat/mentions` | `GET /v1/chat/mentions` | `unread` (bool), `orchestrator` (bool) |

- The document is the Cloud's answer object under `contract` and `kind`, unchanged
  (`items`, `next_cursor`, `viewer`, `last_seq`, `read_seq`, `next_before`, `next_after`,
  `parent`, `message`, `channel`, as the Cloud API names them).
- Ids are 1–64 characters of `[A-Za-z0-9_-]`; numbers are non-negative integers. A bad input
  is `invalid_argument` and nothing is sent.
- A refusal keeps the Cloud's reason: `plan_required` (HTTP 402: the team's plan does not
  include the chat, or the team is locked), `invalid_state` (409), `invalid_field` (400),
  `forbidden`, `not_found`, `rate_limited` (429). Without a known Cloud code (a proxy's page,
  a code such as `totp_required`) the HTTP status decides: 402 `plan_required`, 409
  `invalid_state`, 400/422 `invalid_field`, 403 `forbidden`, 404 `not_found`, 429
  `rate_limited`, 5xx `unavailable`. A Cloud that cannot answer is `unavailable`.
- An instance that is not connected is `not_connected`. A credential the Cloud no longer
  accepts (the instance was revoked, the person left the team) is `forbidden` with what to do
  (reconnect this machine), the same as `tasks decide|stop` on a linked vault (§3).
  The Cloud's message is passed on without control characters, at most 300 characters.
- `chat.post`, `chat.read` and `chat.dm` are writes: the request passes the release point
  before anything is sent (`withheld` otherwise).

## 29. `lean-ctx cloud usage --json` (kind `cloud.usage`)

LeanCTX Cloud quotas of the connected plan (`GET /v1/usage`), for a local reader.
Asked only when the server announced the `usage` feature; otherwise no request
leaves the machine. CLI only (no local service route yet).

```json
{
  "contract": "local-read/1",
  "kind": "cloud.usage",
  "available": true,
  "usage": {
    "schema": "leanctx.cloud.usage/v1",
    "plan": "personal",
    "read_only": false,
    "quotas": {
      "storage_bytes": {"limit": 42949672960, "used": 36507222016, "reset_at": null, "state": "warn", "next_step": "upgrade_team"}
    },
    "notices": [],
    "model_credit": {},
    "suggest_team": false
  },
  "as_of": "2026-11-17T12:00:00Z",
  "live": true
}
```

- `available: false`, `usage: null`: the server does not offer usage (older LeanCTX Cloud).
- `live: false`: the server did not answer; `usage` is the last answer of that
  server, read at `as_of`. With no earlier answer the command prints the `error` document.
- `usage` is the cloud's answer: `quotas` are interpreted (warnings in `cloud status`);
  `notices`, `model_credit` and `suggest_team` are passed through as the cloud sent
  them and are absent when it sent none.
- `state`: `ok`, `warn` (from 80 %), `full`; a state this build does not know reads `unknown`.

## 30. `lean-ctx cloud gateway key --json` (kind `cloud.gateway.key`)

A short-lived key with which this computer's sessions run on models from LeanCTX, paid
from the team's balance (`POST /v1/gateway/key`, engine token, no body). Asked only when
a reader asks; the key is printed to the reader and never written to disk, cached or
logged. The reader keeps it in memory until `expires_at`.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.gateway.key",
  "available": true,
  "state": "ready",
  "gateway": {
    "base_url": "https://gateway.example/v1",
    "key": "…",
    "expires_at": "2026-11-18T13:00:00Z"
  },
  "next_step": null,
  "retry_after_s": null
}
```

- `state` is one of:
  - `ready`: `gateway` is set.
  - `stopped`: the balance is used up (Cloud answered `429 quota_exceeded` for
    `model_credit_usd_micros`). `next_step` is `top_up` when a top-up helps, and
    `retry_after_s` is set when Cloud named a wait.
  - `too_many_keys`: this computer already holds the most live keys Cloud allows
    (Cloud answered `429 rate_limited` with `reason: gateway_keys`). `available` is
    `true`, `next_step` is `null` (a top-up does not help: reuse a held key until it
    expires), and `retry_after_s` is set when Cloud named a wait. Any other
    `429 rate_limited` gets an `error` document. Cloud sends `retry_after_s` (and a
    `Retry-After` header) as the seconds until the oldest live key expires (1–3600).
    The wait does not promise a new key: a reader reuses a key it holds instead of
    polling.
  - `forbidden`: Cloud answered `403 forbidden`; this person may not use the team's
    balance. It also covers `403 plan_required` (the team has no plan or trial).
  - `not_configured`: Cloud answered `503 not_configured`, or `404` because it has
    no gateway route; `available` is `false`.
- `retry_after_s` is capped at 86400 (a day).
- Cloud's key document is `{schema, base_url, key, expires_at, models}`. `gateway`
  carries only `base_url`, `key` and `expires_at`; `models` is not passed on, and
  fields Cloud adds later are ignored. A `schema`, when sent, must be
  `leanctx.cloud.gateway_key.v1`.
- `gateway_concurrency` (`429 rate_limited` from Cloud's chat route) concerns model
  requests made with the key, not this read; it never reaches this document.
- A key document must have an `https` URL, a printable key and a future `expires_at`.
  Any other key document gets an `error` document instead, and so does every other
  Cloud failure (`not_connected`, `unavailable`, …).
- `cloud gateway` without `--json` exits 2.

## 31. `lean-ctx cloud cpao --json` (kind `cloud.cpao`)

The team's cost per accepted outcome over the last 7 and 30 days (`GET /v1/cpao`,
engine token), for a local reader. An accepted outcome is a landed merge request or an
accepted task result. Asked only when the server announced the `cpao` feature;
otherwise no request leaves the machine. CLI only (no local service route yet).

```json
{
  "contract": "local-read/1",
  "kind": "cloud.cpao",
  "available": true,
  "cpao": {
    "schema": "leanctx.cloud.cpao/v1",
    "periods": [
      {
        "period": "7d",
        "cost_usd_micros": 12500000,
        "accepted_outcomes": 5,
        "cpao_usd_micros": 2500000,
        "trend_pct": -12.5,
        "settled": false,
        "by_agent": [
          {"agent": "claude", "cost_usd_micros": null, "accepted_outcomes": 4},
          {"agent": "codex", "cost_usd_micros": null, "accepted_outcomes": 1}
        ]
      },
      {"period": "30d", "cost_usd_micros": 0, "accepted_outcomes": 0, "cpao_usd_micros": null}
    ]
  },
  "as_of": "2026-11-18T12:00:00Z",
  "live": true
}
```

- `available: false`, `cpao: null`: the server does not offer CPAO (not announced,
  Cloud answered `503 not_configured`, or `404` because it has no route).
- A person without a team (personal plan, or not a member of any team) has no team
  CPAO: Cloud answers `503 not_configured`, so the document is `available: false`,
  not an error. A reader hides the card.
- Cloud answered `403`: the `error` document with code `forbidden`; never the last
  answer. Cloud sends `403` only to a team member whose role may not see the team's
  cost.
- `live: false`: the server did not answer; `cpao` is the last answer of that server,
  read at `as_of`. With no earlier answer the command prints the `error` document.
- `cpao` is the cloud's answer, never recomputed. Amounts are millionths of a US dollar.
  `cpao_usd_micros` is `null` without an accepted outcome; `trend_pct` (percent against
  the period before) and `by_agent` are absent when the cloud sent none. An agent row's
  `cost_usd_micros` is `null` when the cloud does not price per agent (it does not yet);
  it is passed on as `null`, never as `0`.
- Checked, not repaired: another `schema`, or `periods` that is not a list, gives the
  `error` document. A period that is not `7d`/`30d`, repeats a period, lacks a count,
  has a non-number `trend_pct`, or names a `cpao_usd_micros` with zero outcomes is left
  out; so is an agent row with a missing count or a name outside `[a-z0-9_.-]{1,64}`,
  or a repeated agent.
- At most 64 agent rows per period are kept (the first 64 valid ones, in the cloud's
  order); further rows are left out.
- `settled` (per period, Cloud p37 !154): `true` means the period's revert window
  is closed; the figures are not frozen (a later revert still lowers the count).
  `false` means the window is still open and the figures are provisional. Passed on
  as sent; absent when the cloud sent none or sent a non-bool (an older cloud), and
  such a period is kept. A reader treats absent as unknown, not as provisional.
- `cloud cpao` without `--json` prints the same read as lines for people; not a
  contract. One line per period, with the change labelled against the previous
  period (zero prints `0.0%`, never `-0.0%`). A period with `settled: false` is
  marked `(provisional)`, one without `settled` `(status unknown)`. Not connected,
  a Personal account and a server without CPAO each get their own line; when not
  connected or connected to a Personal account (`org_kind` of the enrolment), no
  request is sent. An enrolment that cannot be read does not decide the account:
  a server without CPAO still prints its line. A `403` that names a team role (`forbidden` with
  `required_role`) names the role (only owners and admins of the team see CPAO);
  any other `403` prints the server's message (or a plain refusal when it sent
  none). `--json` answers `forbidden` for every `403`.

## 32. `lean-ctx cloud models catalog --json` (kind `cloud.models.catalog`, 3.12.7)

The models LeanCTX Cloud lists for your plan (Personal or team), for a local reader such
as LeanCTX Code. `cloud.models.catalog` (`GET /v1/local/cloud/models/catalog`, CLI `cloud
models catalog --json`, also `lean-ctx models --json`) is one call of the LeanCTX Cloud
route `GET /v1/models/catalog`, made with this computer's Cloud credential. It is a read
released under `ctx_knowledge`; no project.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.models.catalog",
  "catalog": {
    "schema": "leanctx.cloud.model-catalog/v1",
    "catalog_version": "3",
    "price_version": "2",
    "models": [
      {
        "id": "leanctx/qwen3-coder",
        "display_name": "Qwen3 Coder",
        "vendor": "Alibaba",
        "origin_country": "CN",
        "license": "apache-2.0",
        "status": "available",
        "plans": ["personal", "team", "enterprise"],
        "context_window": 262144,
        "hosts": [
          {
            "provider": "nebius",
            "upstream_model": "Qwen/Qwen3-Coder-480B-A35B-Instruct",
            "host_country": "FI",
            "retention": "zero",
            "price_in_per_mtok_micros": 400000,
            "price_out_per_mtok_micros": 1800000
          }
        ]
      }
    ],
    "direct": [
      {"vendor": "anthropic", "hosts": ["api.anthropic.com"], "host_country": "US"}
    ],
    "key_id": "0123456789abcdef",
    "signature": "…"
  },
  "verified": true,
  "models_subscription": true,
  "gateway_base_url": "https://cloud.example.com/gateway/v1"
}
```

- `catalog` is the Cloud's document, passed on unchanged: never filtered, reordered or
  completed. Only a model in `models` is served by LeanCTX (`id` starts with
  `leanctx/`); `status: unpriced` models are listed for their origin and hosts and are
  not served yet. `direct` names vendors' own APIs, which a person uses with their own
  key (BYOK); LeanCTX never serves them, and a reader must not show them as LeanCTX
  models. Fields the Cloud adds later are passed on.
- `verified`: the Cloud signs the catalog with its key set v3 `policy` key (Ed25519,
  `verify_strict`) over `"leanctx.cloud.model-catalog.v1\n"` ‖ the canonical JSON of the
  catalog without `signature`, the trust path of the effective policy. `key_id` names
  the key. `true`: the signature verifies with a `policy` key of the key set this
  computer holds (the key set is fetched once when the stored one lacks `key_id`; a
  `key_id` the fetched set still lacks is not fetched again for 60 seconds).
  `false`: signed, but the signature, the key or the key's purpose does not match; a
  reader shows the list as unconfirmed. `null`: the catalog carries no `signature` (an
  older Cloud); nothing is checked and nothing is assumed.
- `models_subscription`: this computer's plan is valid (or in grace) and grants the
  right `models.subscription`. `false` means the LeanCTX models are not part of the
  plan; the list is still shown.
- `gateway_base_url`: the LeanCTX gateway of the connected Cloud (its address plus
  `/gateway/v1`), derived by LeanCTX on this computer; the Cloud does not send it in the catalog. A
  key for it comes from `cloud.gateway.key` (§30).
- No token or key is in the document, the logs or an error.
- Refusals: `not_connected` without a connected Cloud (or a lapsed plan), `not_found`
  when the Cloud has no catalog route (an older Cloud), `forbidden` when the Cloud refuses
  it, `rate_limited`, `unavailable` when the Cloud does not answer, `internal` when the
  answer is not a JSON object.

## 33. `lean-ctx cloud billing topup [--packs N] --json` (kind `cloud.billing.topup`)

LeanCTX Code holds no Cloud token, so it opens a top-up's Stripe checkout through this kind.
A top-up pays model use once the plan's model credit is used up, and computer minutes after
the free hours; computer minutes beyond the free hours come from the top-up only. It is one call of LeanCTX Cloud's engine route `POST /v1/billing/credit-topup`
(Cloud API §17.2): the engine calls it with its instance credential and the Cloud acts as the
person the instance is linked to, in the engine's own team, with that person's role, plan,
confirmed-email rules and rate budgets; a top-up asks no two-factor step-up. The engine never talks to Stripe. The
document carries only the link the Cloud answered, and only when it is an `https` link on
`checkout.stripe.com` or `billing.stripe.com` (as §27). A write released under `ctx_agent`; no
project.

| kind | route | CLI | body |
|---|---|---|---|
| `cloud.billing.topup` | `POST /v1/local/cloud/billing/topup` | `cloud billing topup --json [--packs N] [--idempotency-key UUID]` | `{packs?, idempotency_key?}` |

`packs` is a whole number from 1 to 10 (a JSON number or its text); each pack is $10. Anything
else is `invalid_argument` before anything is sent. Without `packs` the Cloud's body is `{}` and
the Cloud sells its default ($20). The Cloud is the authority on the range and the price: a count
it does not sell comes back as `invalid_argument` with the Cloud's words.

`idempotency_key` (additive) is a UUID in its hyphenated form (8-4-4-4-12 hex digits); anything
else is `invalid_argument` ("idempotency_key is a UUID (8-4-4-4-12 hex digits)") before anything
is sent. It is sent on as the `Idempotency-Key` header of the Cloud call, never in its body, so a
retry with the same key gets the same checkout from the Cloud. Without it no such header is sent
and the call is as before.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.billing.topup",
  "url": "https://checkout.stripe.com/c/pay/cs_test_1"
}
```

Refusals:

| LeanCTX Cloud | code | message |
|---|---|---|
| `plan_required` (402), reason `trial` | `plan_required` | no top-up during the trial |
| `plan_required` (402), other reasons | `plan_required` | a top-up needs a paid plan in effect, choose a plan first |
| `email_unverified` (403) | `email_unverified` | confirm your email address before paying, the link is in the confirmation email |
| `forbidden` (403) | `forbidden` | the Cloud's message, else: only an owner of the team tops up credit |
| `invalid_state` `company_billed` (409) | `forbidden`, `reason` `company_billed` | billed through your company |
| other 409 | `conflict` | the Cloud's message |
| `invalid_field` `packs` (400) | `invalid_argument` | the Cloud's message |
| `sso_required` (403) | `sso_required` | as §27 |
| 404 (a Cloud without the engine route) | `unavailable` | this LeanCTX Cloud does not sell top-ups to the app yet, open billing in the browser |
| `not_configured` (503) | `unavailable` | top-ups are not on sale on this LeanCTX Cloud right now |
| `upstream` (502) | `unavailable` | the payment provider did not answer, retry later |
| 429 | `rate_limited` | |
| other 5xx | `unavailable` | |

`email_unverified` (additive) is a `403` on the local socket like `forbidden`; a reader that
does not know it treats it as `forbidden` and shows the message. `not_connected` when no
instance is connected or the Cloud no longer accepts its credential; `internal` when the Cloud
answers without a Stripe link. The document is a function of the Cloud's link alone (#498).
A company-billed team's refusal names why, so the app points to the company instead of the
person's role:
```json
{"contract":"local-read/1","kind":"error","error":{"code":"forbidden","message":"billed through your company","reason":"company_billed"}}
```
Every other `forbidden` has no `reason`; the same field and value apply to `cloud billing checkout`
and `portal` (§27).
`cloud billing topup` without `--json` prints the link for a person, or the message on stderr
with the same exit status.

## 34. Team stop: `cloud stop` (kinds `cloud.stop`, `cloud.stop.release`, `cloud.stop.status`)

The team's kill switch (owner decision 19.11.). LeanCTX Cloud holds the stop and
enforces it: while a team is stopped, its engines get `423 team_stopped` on cloud
calls, landings and running Code sessions. **Nothing on this machine is gated by it.**
Local agent tools, reads and the local service keep working. Each kind is one call of
the Cloud's engine route `/v1/stop` (p37 `docs/API.md` §2.6), made with the instance
bearer credential. The route names no org: the credential names the team. (The web
route `/api/v1/orgs/{org}/stop` takes a browser session only and is never called by
the engine.) The Cloud acts as the linked person and enforces the role (contributor and up stop; an admin of the
team or of its company releases), the two-factor step-up and the slug. The engine
checks only the shape of the arguments and, as a courtesy, that `--confirm` names this
instance's team.

| kind | CLI | local route | Cloud call |
|---|---|---|---|
| `cloud.stop.status` | `cloud stop --status --json` | `GET /v1/local/cloud/stop` | `GET /v1/stop` |
| `cloud.stop` | `cloud stop --reason <text> --json` | `POST /v1/local/cloud/stop` `{reason}` | `POST /v1/stop {reason}` |
| `cloud.stop.release` | `cloud stop --release --confirm <team-slug> [--note <text>] --json` | `POST /v1/local/cloud/stop/release` `{confirm, note?}` | `GET /v1/stop`, `DELETE /v1/stop {confirm, note?}`, then `GET /v1/stop` |

`cloud.stop` and `cloud.stop.release` pass the release point as writes (`ctx_agent`).
`cloud.stop.status` passes it as a read (`ctx_knowledge`). Without `--json` the
command prints the same answer in words, and a refusal exits with its code. The calls
follow `[cloud] enabled` and the lease like every other cloud call: with
`enabled = false`, without an enrollment or with a lapsed lease the answer is
`not_connected`, and nothing is sent. Before any call of `/v1/stop` the engine reads
`GET /v1/capabilities`: when the Cloud does not announce the feature `team_stop`, the
answer is `not_offered` ("the team stop is not available on this cloud") and nothing
is sent to `/v1/stop`. Every state these kinds read is kept for `cloud status` (below).

Arguments: `reason` and `note` are 1–500 characters after trimming and contain no
control characters. `confirm` is a team slug (`[a-z0-9-]`). A bad argument is
`invalid_argument`, and nothing is sent.

State document (`cloud.stop.status`; `cloud.stop` adds `created`):

```json
{
  "kind": "cloud.stop",
  "team": "acme",
  "stopped": true,
  "since": "2026-11-19T10:00:00Z",
  "by": "Ada",
  "reason": "runaway agent",
  "generation": 3,
  "role": "admin",
  "may_stop": true,
  "may_release": true,
  "company": { "admin": false },
  "step_up_url": "https://cloud.leanctx.com/o/acme/stop/",
  "created": true
}
```

- `since`, `by` and `reason` are `null` when the team is not stopped. `by` is the
  stopping person's display name, or their email when they set no name.
- `created`: `true` when the Cloud answered `201` (a new stop), `false` on `200`
  (the team was already stopped, and the existing stop is shown).
- `company` is present only when the Cloud names it.
- `step_up_url` is the browser page that releases the stop. It is set only while the
  team is stopped and the Cloud allows the person to release. It is the Cloud's link
  when that link lies below the web app's origin; otherwise `<web>/o/<team>/stop/`.

Release document:

```json
{ "kind": "cloud.stop.release", "team": "acme", "released": false,
  "step_up_url": "https://cloud.leanctx.com/o/acme/stop/" }
```

- The engine reads the state first. If `--confirm` names another team, the answer is
  `invalid_argument` and no `DELETE` is sent.
- A release is confirmed in the browser with two-factor. The Cloud answers
  `403 {error:{code:"step_up_required", reason:"web_release", step_up_url}}`. That
  answer is not an error: it gives `released: false` plus the page to open. A
  `step_up_url` outside the web app's origin is never passed on. An SSO step-up
  (`reason: "sso_required"`) is the `sso_required` refusal.
- `released: true` when the Cloud answered `2xx`. The engine then reads the state
  again and keeps it for `cloud status`.
- A release raises the Cloud's `generation`.
- The Cloud checks a `DELETE` in this order: role (`403 forbidden`, `required_role: admin`),
  slug (`400 invalid_field`, `field: confirm`), state (`409 invalid_state`, `state: not_stopped`),
  then always `403 step_up_required`. The engine never releases a stop itself.

Refusals (the `error` document):

| Cloud answer | code |
|---|---|
| `409 invalid_state` `state: personal_org` (a stop asked from a personal account) | `conflict` (a personal account has no team stop) |
| `409 invalid_state` `state: not_stopped` | `conflict` |
| `403 forbidden` (role too low) | `forbidden` (the Cloud's message, if it is safe) |
| `403` `sso_required` | `sso_required` |
| `403` totp / step-up on a stop | `forbidden` |
| `422`/`400 invalid_field` (`field: confirm`: slug mismatch) | `invalid_argument` |
| `423 team_stopped` (an engine write while stopped) | `conflict`, naming the Cloud's `since` and `generation` |
| `404` (no route), or `team_stop` not in `GET /v1/capabilities` | `not_offered` (not available on this cloud; a refusal, not an outage: a reader does not retry it) |
| `429` (`GET` 120 per instance, `POST`/`DELETE` 30 per person, per the Cloud's window) | `rate_limited` with `retry_after_s` (shared mapping of cloud calls) |
| `5xx` | `unavailable` (shared mapping) |
| `401` after one token refresh | `not_connected` |
| any other answer, or an unreadable state | the shared mapping (`internal` for an unreadable state) |

`cloud status --json` (`CloudCliStatusV1` 1.8 members; absent when not connected or
when the Cloud does not offer the stop, so an older reader sees the same document;
`team_stop: null` when connected but not known yet). **Status never calls the Cloud** for them: it
reads only the answer kept in `<data>/team-stop.json`, for this enrolment only (its
server, team and instance; after a new `cloud connect` it reads none of the earlier one). The scheduler tick refreshes
that file each pass (every `pull_interval_minutes`, after the same capabilities check),
and so do the three kinds above. A failed refresh keeps the last answer.

- `team_stop`: `{active, reason, since, by, generation, may_stop, may_release, cache, age_s}`.
  - `cache`: `fresh`, or `stale` (older than two pull intervals, or of unknown age).
  - `team_stop: null` (present, `null`): connected, but no answer kept yet (no tick or
    `cloud stop` ran). The engine emits no defaults then, so nothing reads as a running
    team or as "may not stop"; a reader treats it as unknown.
  - `age_s`: seconds since the Cloud gave the kept answer, by this machine's clock;
    `null` when unknown. It is a data field, not a timestamp in the body.
  - Absent when the Cloud does not offer the stop (no `team_stop` capability, or a
    `404`), and when not connected.
  - Fail-safe: when the Cloud stops offering it while the kept answer says the team is
    stopped, that stop stays kept, of unknown age (`cache: "stale"`, `age_s: null`),
    until the Cloud answers the state again. A kept running team is dropped then.
  - A reader ignores members it does not know (no `deny_unknown_fields`), because the
    Cloud and newer engines add members.
- `role`, `org_slug`: the person's role and the team slug, as the Cloud named them.
- `company`: `{admin}`, only when the Cloud names it.
- `step_up_url`: the release page, only while the team is stopped.

Apart from `age_s`, the members are deterministic functions of the kept answer (#498).
Text `cloud status` adds a `Team stop:` line only while the kept answer says the team
is stopped, and marks it when the answer is stale.

## 35. `lean-ctx run "<task>" [--cloud] … --json` (kinds `run.ack`, `run.list`, `run.status`, `run.events`)

A coding agent (`claude` or `codex`) on a task in a fresh git worktree, on the branch
`leanctx/run/<id>`, either on this machine or (`--cloud`) on the person's LeanCTX computer
(§44). The agent starts only on this explicit command, never from a reader, a queue or the
cloud. Its model access is the person's choice: `own` (the agent CLI's own login) or
`balance` (the person's own LeanCTX balance through the gateway key of §30). With `--cloud`,
`--model-access` is required; locally it defaults to `own`. CLI only (no local service
route).

Run ids are `run_` and 16 lowercase hex digits. Records live under `<data dir>/runs/<id>/`
(mode 0700 directory, 0600 files): `run.json`, `task.txt`, `events.jsonl`, `agent.log`.

### The run row

```json
{
  "id": "run_00112233aabbccdd",
  "place": "computer",
  "computer": "dev",
  "project_root": "/work/project",
  "agent": "claude",
  "model_access": "balance",
  "state": "running",
  "branch": "leanctx/run/run_00112233aabbccdd",
  "base": "4f1c…",
  "commits": ["9a0e…"],
  "tool_calls": 12,
  "exit_code": null,
  "stop_reason": null
}
```

- `place`: `local` or `computer`; `computer` is the computer's name, `null` for `local`.
- `state`: `starting`, `running`, then one final state: `finished` (the agent exited 0),
  `failed` (it exited non-zero, or could not start), `stopped` (`run stop`, or
  `--max-minutes` ran out: `stop_reason` `user` or `max_minutes`; a computer run whose
  computer stopped: `balance_empty` or `computer_stopped`, see "When the computer stops").
  A `failed` run that could not start names why in `stop_reason` (for example
  `model_access`). `run stop` and `--max-minutes` send SIGTERM to the agent's process
  group and SIGKILL after 10 s to whatever of the group is still alive.
- `commits`: the run branch's commits after `base`, oldest first. When the agent leaves
  changes uncommitted, the run commits them as one last commit before it ends.
- `tool_calls`: counted from the agent's own output (`claude` `tool_use` items; `codex`
  completed command, MCP, file and web items).
- No field carries a time of this machine's (or the computer's) clock (#498): the row and
  the events are functions of the run's state; `seq` orders the events.
- The row never carries the task text, the worktree path on the computer, a process id or
  a key.

### `run "<task>" --json` (kind `run.ack`)

```json
{"contract": "local-read/1", "kind": "run.ack", "action": "start", "run": { /* the row */ }}
```

The command returns once the agent has started (`state: running`) or failed to start. The
task is one argument, or `-` for stdin; at most 64 KiB. `--base <ref>` (default `HEAD`);
`--include-dirty` starts from the uncommitted changes to tracked files as well (a
`git stash create` commit; the work tree is untouched) and cannot be combined with `--base`.

### `run --cloud`

LeanCTX computers are not available yet (owner decision for this release): until
LeanCTX Cloud offers them (`GET /v1/computers` answers `available: false`, or a cloud
without the route, or `503 computer_not_available`), `run --cloud` ends with
`computer_not_available` ("LeanCTX computers are not available yet.", as in §44) before
any start is sent. The steps below hold once computers are offered.

1. The CLI lists the computers (`GET /v1/computers`) and picks the chosen one
   (`--computer <name|id>`, or the only one). A run is billed from its first minute at the
   price LeanCTX Cloud lists; `purpose: "run"` is always `billing: billed`. The terms come from the list row alone, in the §44 vocabulary: `billing: billed`,
   `usd_per_hour` (the row's `price`, else its `balance`), `topup_usd` (the row's
   `balance.usd`) and `topup_state` (its `balance.state`).
2. Only a person starts a paid computer (owner rule, as for `cloud computer create|start`
   in §44): `run --cloud` needs a CLI whose stdin is a terminal. Without one (a script, an
   agent) nothing is sent, not even with `--yes`, and the answer is `confirmation_required`
   with the message "Starting a paid computer needs you at a terminal or in LeanCTX Code."
   and the terms in `error.details`. No environment variable counts as a person. `run` has
   no LeanCTX Code route yet. At a terminal, no paid call without the person's
   confirmation. Without `--yes`:
   - on a terminal without `--json`, the CLI shows the terms and asks; a no ends the
     command with `confirmation_required`;
   - with `--json` it prints the `error` document with
     `code: confirmation_required` and `error.details`
     `{billing: free|billed|null, usd_per_hour, topup_usd, topup_state}` (always `billed`
     for a run; a malformed value is `null`, never a guess), and exits 1.

   In both cases nothing was sent to start the computer. Only after `--yes` or a yes does
   the CLI send `POST /v1/computers/{id}/start` with the body
   `{"request_id": "<id>", "purpose": "run", "confirm": true}`; `confirm` is always
   `true` (an unconfirmed start is never sent). Nothing else: no code, no diff, no task
   text, no repository name. LeanCTX Cloud decides role, price and top-up; a
   `428 confirmation_required` in answer to this confirmed start is an error
   (`confirmation_required`, its `error.details` the cloud's terms), never asked again.
   Other refusals map as in §44 (`balance_empty`, `computer_busy`, `forbidden`, …).
3. Once the computer runs, the CLI pins its host keys (§44 `ssh.host_key_fingerprints`) and
   moves the code over SSH only: `lean-ctx run receive --init` on the computer creates the
   bare repository `~/.leanctx/repos/<hash>.git` (`<hash>`: 16 hex digits of the SHA-256 of
   the project path), the base commit is pushed to `refs/leanctx/runs/<id>/base`, and
   `lean-ctx run start --id <id> … --json` on the computer starts the agent there, the task
   on stdin. The computer's `run.ack` is passed on with `place: computer`.
4. Without `--detach` the CLI follows the run and, at its end, fetches the run branch into
   the project as `leanctx/run/<id>` (also `refs/remotes/leanctx-<name>/run/<id>`).
   `run fetch <id>` does the same later. The branch is created when missing and
   fast-forwarded when the computer's tip is ahead; a branch with commits of its own, or
   the checked-out branch, is never moved: `conflict` (the fetched tip stays in
   `refs/remotes/…`; `git merge --ff-only` it). Stopping a run does not stop the computer.
5. Text from the computer (an `error.message` of its document, the last line of `ssh`'s
   stderr) reaches the person with credentials redacted, control characters removed and at
   most 300 characters; an `error.code` that is not lowercase `a-z_` is `internal`.

#### Model access `balance` on the computer

The agent on the computer gets its gateway key (§30) from the LeanCTX Cloud session of the
computer user it runs as: the person's own `lc-…` user (§44), connected there with the
person's own login (`lean-ctx cloud connect` over SSH). The key, and the balance it draws
on, are that person's. A team's, a service's or another person's login is never used:
LeanCTX Cloud issues the key only for the session's own person (`403` otherwise). When the
computer user has no cloud session, the run fails to start with `stop_reason:
model_access` and the message "--model-access balance needs this machine connected to
LeanCTX Cloud as you …, or use --model-access own". A used-up balance names a top-up only
when LeanCTX Cloud offers one (`next_step: top_up`); in a trial it points to
`--model-access own`.

#### When the computer stops

A run on the computer ends when the computer stops: LeanCTX Cloud stops it when the
person's balance (plan credit and top-up for models, top-up for a run's computer
minutes) is at $0, or the person stops it. While the computer is out of reach, `run
status` asks LeanCTX Cloud for the computer's row: when it is `stopped`, `stopping`,
`failed` or `deleted`, the run is recorded `stopped` with `stop_reason: balance_empty`
(the row's `stop_reason` is `balance_empty`) or `computer_stopped` (any other reason); the
agent's commits after the last fetch stay on the computer's disk for `run fetch` once it
runs again. While the row still says `running` or `starting`, the run is unchanged and the
answer is `unavailable`. `run events --follow` and `run logs` end with `unavailable`.

### `run list --json` (kind `run.list`), `run status <id> --json` (kind `run.status`)

```json
{"contract": "local-read/1", "kind": "run.list", "runs": [ /* rows, newest first */ ]}
{"contract": "local-read/1", "kind": "run.status", "run": { /* the row */ }}
```

`status` of a computer run asks the computer (over SSH) and updates the local row. An
unknown id: `not_found`; a malformed one: `invalid_argument`; an unreachable computer:
`unavailable`.

### `run events <id> --json` (kind `run.events`)

```json
{"contract": "local-read/1", "kind": "run.events", "id": "run_00112233aabbccdd",
 "events": [{"seq": 1, "type": "started", "agent": "claude",
             "branch": "leanctx/run/run_00112233aabbccdd"}],
 "next_since": 1}
```

- `--since <seq>` returns the events after `seq`; `--limit` at most 500 (default 500).
  `next_since` is the last returned `seq`, or `--since` when none came.
- Event `type`: `started`, `tool_calls` (`tool_calls`: the running total), `commit`
  (`sha`), and one final event `finished`, `failed` or `stopped` (`state`, `stop_reason`,
  `exit_code`, `commits`: the count).
- `--follow --json` prints one event per line (the bare event objects, no envelope) until
  the final event.

### `run stop <id> --json` (kind `run.status`)

Stops the agent (SIGTERM to its process group, SIGKILL after 10 s), commits what it left,
records `stopped` with `stop_reason: user`, and prints the row as `run status` does. A run
that has already ended is printed unchanged. The computer keeps running
(`lean-ctx cloud computer stop <name>`).

### Security

- No code, diff, task text or repository name reaches LeanCTX Cloud; only the start
  request above. Code moves over SSH with the pinned host keys (`StrictHostKeyChecking
  yes`, `ForwardAgent no`).
- A `balance` run's agent gets a gateway key (§30) in its environment only
  (`ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN` or `OPENAI_BASE_URL`/`OPENAI_API_KEY`); it is
  never written to the record, the events or a document.

## 37. Plan updates to running agents (bus feature `plan_updates`)

When a person changes a plan or a team decision in LeanCTX Code, Cloud tells
the relay of every Instance that follows the vault; the relay turns it into a
directive for each local agent it concerns and reports each agent's answer.
Engine side: `lean_ctx_cloud::bus::plan_events`, `lean_ctx::core::agents::directives`.

Delivery needs a click (owner decision): agents hear of a change only after
a person clicked "Send to agents" in LeanCTX Code and confirmed it. A
`plan.advanced` frame only **offers**: each directive is recorded `offered`,
invisible to the agent (no bus entry, no pending line in `read`/`wait`/tool
answers, no ack, nothing reported to Cloud). Code then releases each offered
plan directive with `lean-ctx cloud plan release` (release id on stdin, 37.9);
only that CLI call delivers a plan. `decision.changed` is the click itself:
Cloud sends it only after the person confirmed "Send to agents" on a team
decision (`decisions.send`, §26.4), so the relay delivers its directive to each
active agent of the checkout at once, without a second offer. The same
decision, status and successor again (a frame delivered twice) posts nothing;
no catch-up, sync or list delivers one. "Deliver" means one `cloud` directive on the local agent bus
(37.4), which the agent reads with `ctx_agent`: nothing is typed into an
agent, and no agent is started or restarted.

### 37.1 Feature and frames

- The relay acts only when `hello.ack` lists the feature `plan_updates`;
  without it both frames are ignored and nothing is pulled.
- A frame's `vault` is the subscription address `#<org slug>/<vault name>`
  and only routes; the plan document names the vault by its id (37.3).
- `{"t":"plan.advanced","vault":"#ws/vault","plan_id":"plan_<8–40 alnum>","version":n}`
  (`n ≥ 1`). A hint only: nothing reaches an agent before the version is
  pulled and verified (37.3).
- `{"t":"decision.changed","vault":"#ws/vault","decision_id":"dec-<kebab>","status":"active|superseded|revoked","superseded_by":"dec-…"|absent}`.
  A decision id is `dec-` and lowercase kebab segments (`[a-z0-9]+` joined by
  single `-`, no empty segment), at most 80 characters in all. Reserved: a
  valid frame is ignored (no directive, nothing recorded); the decision
  directive of 37.4 applies once Cloud sends it only after the click.
- Unknown fields, another id shape or `version: 0` make the frame invalid
  (a bus error, ignored like any bad frame).

### 37.2 Routes the relay calls (vault id from the vault map, Bus right)

- `GET /v1/vaults/{vault_id}/plans/{plan_id}?version=n` → the plan document (37.3).
- `GET /v1/vaults/{vault_id}/plans?state=active` →
  `{"schema":"leanctx.cloud.plans/v1","vault","items":[{"plan_id","version",…}]}`
  (each item the newest version's document without `body`; the engine reads
  `plan_id` and `version` only, and accepts the earlier name `plans` for
  `items`), read once after each `hello.ack` with the feature; versions newer
  than the applied one are pulled in `plan_id` order.
- `POST /v1/vaults/{vault_id}/plans/{plan_id}/ack` with an `Idempotency-Key`
  (37.5). `409` counts as delivered (stale version, or the key's earlier answer).
- `POST /v1/vaults/{vault_id}/plans` publishes a version (37.10); only the
  CLI of 37.10 calls it, never the relay.
- Refusal: without a control key Cloud answers publish and both `GET` routes
  with `409` and `error.code` `not_ready`, never with an unsigned document. The
  engine shows "plan updates not available", records no error, applies and
  acks nothing, and does not retry; the next `hello.ack` reads again.

### 37.3 Plan document `leanctx.cloud.plan/v1`

`{schema, plan_id, vault, version, supersedes_version?, content_sha256:"sha256:<64 hex>", issued_by, published_at?, body:"<base64>", sessions:[{slot, task_id|null, worker_id?, brief_sha256?, changed}], signature:{key_id, sig}}`,
unknown fields tolerated (not kept, signed or shown), at most 32 sessions, `slot` `[a-z0-9-]{1,32}` or
`all` (every session, 37.4). `published_at` is RFC 3339, set by Cloud (not
signed). Cloud keeps plans encrypted at rest and serves the body as plain
text: `body` is standard base64 with padding of the body bytes (1 B to
256 KiB decoded), and `content_sha256` is `"sha256:" + hex(sha256(decoded))`.
The body is a `leanctx.plan-body/v1` object:

`{schema: "leanctx.plan-body/v1", title, goal, decisions?, sessions:[{slot, role, brief, folder}]}`
(`schema` may be left out on read and is then v1; another value is not a
plan body):
`title` ≤ 200, `goal` ≤ 4000, `role` ≤ 64, `brief` ≤ 8000 characters,
non-blank, without control characters other than `\n` and `\t` and without
a character a Cloud text field refuses (`lean_ctx_protocol::cloud::invisible`);
`folder` a string (≤ 512) or `null`; at most 32 sessions with unique slots
(`[a-z0-9-]{1,32}`, not `all`); `decisions` optional, each
`^dec-[A-Za-z0-9-]{1,64}$`; unknown fields refused.

The engine takes the document only when, in order:

1. `schema` is `leanctx.cloud.plan/v1`, `plan_id` and `version` match the
   frame, and `vault` equals the enrolled vault id the document was read from
   (the `{vault_id}` of the GET; 32 hex, compared lowercase without `-`). The
   frame's address is not compared. Else recorded locally as invalid, no ack;
2. `sha256(base64-decode(body)) == content_sha256` (else refused `digest_mismatch`);
3. `signature.sig` (128 hex) is an Ed25519 signature (strict) by the control
   key `key_id` of the bus control keys over the statement Cloud signs,
   rebuilt from the document:
   `"leanctx.cloud.plan.v1\n" ‖ canonical_json({"schema":"leanctx.cloud.plan-statement/v1", vault, plan_id, version, content_sha256, issued_by, key_id})`
   (else refused `signature_invalid`; `signature: null`, a missing signature
   or an unknown key too, fail closed). The statement binds the vault, the
   version, the body digest (checked against the body in step 2) and the
   issuer; the sessions and `published_at` are not signed;
4. the verified body is a `leanctx.plan-body/v1` (else recorded locally as
   invalid, no ack);
5. every `sessions[].slot` is `all` or a slot of the body, and its
   `brief_sha256` equals the SHA-256 of that slot's brief (else refused
   `digest_mismatch`, nothing offered). `sessions` only routes (task, worker,
   `changed`); goal and briefs come only from the verified body;
6. `version` is above the applied one for this vault plan (else `older_than_applied`
   when Cloud announced it as new; an equal version is a no-op).

A taken version is stored as `<cloud data>/plans/<plan_id>.json` and applied
monotonically (relay state `plans`). An offered directive carries ids, the
version, the slot, the session's `brief_sha256` and the goal's SHA-256 of
the verified body, never plan text: its stored `text` stays empty until the
release, then holds the released content. The text an agent receives comes
with the person's release from Code (37.9) and is checked against both
digests.

### 37.4 Directives on the local bus

- A local agent is named by a plan when its attached worker's id equals a
  session's `worker_id`. It gets that session's directive:
  `directive_id = "<plan_id>:<version>:<slot>"`. Every other active agent of
  the vault's checkout gets one plan-wide directive
  `"<plan_id>:<version>:all"` (check that its work still fits the plan).
- Reserved (37.1, not issued yet): a decision change will give every active
  agent of the vault's checkout one directive `directive_id = "<decision_id>:<status>:<n>"`. `n` numbers the
  decision's changes this Instance saw: the same status again (a frame
  delivered twice) keeps the last id, a change back (superseded, then active
  again) gets `n + 1` and so a new directive.
- A directive starts `offered` with a release id (37.9) and no bus entry.
  Offering it again (reconnect, catch-up) is a no-op per agent and directive
  id; a newer version supersedes the agent's older **offered** one (its
  release id then releases nothing), while an already released one stays
  pending until the newer one is released.
- Released, a directive is a bus entry from `cloud`, category `directive`, priority
  `high`, privacy `team`, addressed to the agent, `task_id` from the session,
  metadata exactly `directive_id` and `plan_id`/`plan_version` (or
  `decision_id`) besides the engine's own `lean_ctx.*` keys. Its message is
  exactly the confirmed body of 37.9 and nothing else:

  ```text
  LeanCTX Cloud: plan <plan_id> v<n>, sent by the person from LeanCTX Code.
  Goal:
  <goal>
  Your part (<slot>):
  <brief>
  Answer with ctx_agent action=ack directive_id=<id> verdict=accept, or verdict=refuse reason_code=conflicts_with_decision|task_finished|out_of_scope|unverified_plan|other.
  ```

  For a plan-wide directive (`…:all`) the two part lines are
  `No part of this plan names you: check that your work still fits its goal.`
  No other agent's part, no other version and no release id is in it.
  Its key carries the Cloud inbound prefix: it is fenced and never republished.
  Releasing a newer version supersedes the agent's older unanswered one.
- The records live in `<agents>/directives.json` (at most 500, written under
  the registry lock), not in `registry.json`. That file holds only the
  SHA-256 of each release id; the ids of the still offered directives are in
  `<agents>/directive-release-ids.json` (mode 0600, `{sha256: rid}`), a
  secret path that `ctx_read`, `ctx_search`, `ctx_glob`, `ctx_tree`,
  `ctx_compose`, `ctx_shell`, `lean-ctx read` and the hooked native read
  refuse (`io_boundary::is_secret_like`, enforce mode). The agents
  directory (`<agents>/`, records and id file) is not readable from the
  agent shell: the shell policy (`ctx_shell`, shell hook) refuses a command
  that reads it. It is written
  crash-atomically without following a symlink (a symlink in its place is an
  error, never written through or read). A damaged file is never overwritten
  unseen: it is kept as `directive-release-ids.json.damaged` (also a secret
  path) with a warning, and the ids it held release nothing (fail closed).

### 37.5 Answer: `ctx_agent action=ack`

- Arguments: `directive_id`, `verdict` (`accept|refuse`), with `refuse` a
  `reason_code` of `conflicts_with_decision|task_finished|out_of_scope|unverified_plan|other`;
  `message` is a reason kept on this machine only (≤ 500 chars).
- An offered directive does not exist for the agent: acking it is the
  unknown-directive error.
- Binding: an ack names one directive delivered to the calling agent's own
  registered id; the version is part of `directive_id`. Another agent's
  directive, a version that was not delivered to the caller (older, newer
  or without version) and a superseded one are errors, so an ack never
  carries over between agents or versions.
- Errors: unknown directive for the agent, a superseded directive, a refusal
  without a valid code. Answering again replaces the earlier answer.
- Output: `Directive <id>: accepted. Cloud is told on the next relay pass.` or
  `Directive <id>: refused (<code>). Cloud is told on the next relay pass.`
- Until answered, each unanswered directive is one line on top of `read`/`wait`
  and after every tool answer (where meta lines are shown), sorted by id, no
  counters or times:
  `[directive pending: plan <plan_id> v<n>, ack with ctx_agent action=ack directive_id=<id>]`
  (`decision <decision_id>` for decisions).
- The relay reports plan directives whose state changed since the last report
  (delivered, acked, refused), grouped per vault, plan, version and digest,
  at most 64 agents per request:
  `{"version":n,"content_sha256":"sha256:…","agents":[{"agent_ref","worker_id","slot","agent_type":"claude|codex|cursor|other","state":"delivered|acked|refused","reason_code"}],"refused":null}`.
  An engine refusal of a version (37.3) sends `"agents":[]` and
  `"refused":{"reason_code","applied_version"}`.
  `agent_ref` is `ag-` + 32 hex of an HMAC of the local agent id with the
  Instance's receipt key (stable per agent, no local id leaves the machine).
  `Idempotency-Key: plan-ack:<plan_id>:<version>:<32 hex of sha256(body)>`.
  A failed report is retried after a minute. Decision directives are not
  reported (no route).

### 37.6 `lean-ctx cloud plan updates --json` (kind `cloud.plan.updates`)

The plan versions this Instance applied, with each local agent's answer, for
a local reader such as LeanCTX Code. Local files only (37.3, 37.4); nothing is
sent to Cloud. Route `GET /v1/local/cloud/plan/updates[?project_root=P]`; with
a project, only the plans of its vault (`no_vault` when it is not linked).
Plans by id, agents by id; no times besides Cloud's `published_at`.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.plan.updates",
  "project_root": null,
  "plans": [
    {
      "id": "plan_K7mQ2xVb9Z",
      "vault": "#acme/payments",
      "version": 3,
      "digest": "sha256:…",
      "issued_by": "…",
      "verified": true,
      "published_at": "2026-11-19T10:00:00Z",
      "title": "Refunds",
      "goal": "Ship refunds for card payments.",
      "parts": [{"slot": "api", "role": "backend", "task_id": "t-7", "worker_id": "w-api",
                 "brief": "Build POST /refunds.", "folder": "api",
                 "brief_sha256": "sha256:…", "changed": true}],
      "agents": [{"agent_id": "…", "version": 3, "directive_id": "plan_K7mQ2xVb9Z:3:api", "slot": "api",
                  "state": "offered|delivered|acked|refused", "release_id": "<64 hex>|null",
                  "released_at": "2026-11-19T10:05:00Z|null", "reason_code": null, "reason": null,
                  "plan_id": "plan_K7mQ2xVb9Z", "title": "Refunds",
                  "goal": "Ship refunds for card payments.", "role": "backend",
                  "brief": "Build POST /refunds.", "folder": "api", "changed": true}],
      "body": {"schema": "leanctx.plan-body/v1", "title": "Refunds",
               "goal": "Ship refunds for card payments.",
               "decisions": ["dec-refund-original-card"],
               "sessions": [{"slot": "api", "role": "backend",
                             "brief": "Build POST /refunds.", "folder": "api|null"}]},
      "body_error": null,
      "releases": [{"version": 3, "agent_id": "…", "directive_id": "plan_K7mQ2xVb9Z:3:api",
                    "released_at": "2026-11-19T10:05:00Z"}]
    }
  ],
  "offers": [
    {"release_id": "<64 hex>|null", "plan_id": "plan_K7mQ2xVb9Z", "version": 3, "slot": "api",
     "agent_id": "…", "title": "Refunds", "goal": "Ship refunds for card payments.",
     "role": "backend", "brief": "Build POST /refunds.", "folder": "api|null", "changed": true}
  ]
}
```

- `offers` lists every offered directive (one per agent and version, in
  plan id then agent id order) with exactly
  `{release_id, plan_id, version, slot, agent_id, title, goal, role, brief, folder, changed}`:
  what Code shows, confirms and passes to `cloud plan release` (37.9).
  `role`, `brief` and `folder` are `null` for slot `all`; `folder` may be
  `null` for any part. On the socket route `release_id` and the text fields
  are `null`.
- `body` is the whole verified plan body `leanctx.plan-body/v1` (37.3) as
  decoded JSON: `schema`, `title`, `goal`, `decisions` (left out when the publisher
  gave none) and every `sessions[]` part `{slot, role, brief, folder}`
  (`folder` may be `null`). It carries no routing (`task_id`, `worker_id`),
  no release ids and no agent state. The engine opens it again for every
  read, in the order of 37.3: the signature was checked when the version
  was taken (only verified versions are kept), then the decoded bytes must
  hash to `content_sha256`, then they must parse as plan-body/v1. When that
  fails, `body` is `null`, `body_error` is `digest_mismatch` or `invalid`,
  and the entry's text fields are `null` too; otherwise `body_error` is
  `null`. Like the other text, `body` is filled only on the CLI read outside
  an agent; the local route answers `body` and `body_error` `null`.
  `offers` is unchanged by it.
- Capabilities list `cloud.plan: ["updates", "publish", "release"]`.

- Only verified versions are kept (digest and Cloud signature, 37.5);
  `verified` says whether the kept body still hashes to `digest`. When it
  is `false` the body is not opened (`body_error` `digest_mismatch` for
  the app's read).
- `vault` is the address `#<workspace>/<vault>` of a vault this Instance
  follows, else the document's vault id (32 hex). `?project_root=` limits
  the view to that project's enrolled vault id.
- `issued_by` is the publisher as Cloud signed it (37.3), an opaque id; the
  reader resolves a name itself. It is never null and keeps the
  publisher's id after that person is deleted.
- An agent entry's `version` is the `<version>` of its `directive_id`; it
  answers only that version. There are no `roots`: a reader limits the view
  to a project with `?project_root=` (`--project-root`).
- Text comes only from the verified body (37.3). `title`, `goal`, a part's
  `role`, `brief` and `folder`, and an agent entry's `title`, `goal`, `role`,
  `brief`, `folder` are filled only on the CLI read outside an agent (Code's
  read); the local route answers them `null`. An agent entry carries the
  text Code confirms and releases for that agent: the plan goal and, for a
  session directive, its part (`role`, `brief`, `folder`; `null` for a
  plan-wide `…:all` directive), with `changed` from the routed session
  (`false` for `…:all`).
- Superseded directives are left out; `reason` is the agent's local words.
- `release_id` is set only while `state` is `offered`, and only on the CLI
  read outside an agent (Code's read); it is what Code passes to
  `cloud plan release` (37.9). `released_at` is when the person released it.
- `releases` is the local audit of every release of this plan still on
  record (any version): version, agent and time, sorted by time.
- Release ids reach no agent:
  - the local route (`GET /v1/local/cloud/plan/updates`) has no caller
    authentication beyond the socket's 0600 mode, so it always answers
    `release_id: null` and no plan text;
  - the CLI refuses with `forbidden` (exit 1) when an agent marker is set
    (non-empty), from the engine's one marker list
    (`shell_hook::agent_env_markers`): the shell hook's `LEAN_CTX_AGENT`,
    `CLAUDECODE`, `CODEBUDDY`, `CODEX_CLI_SESSION`, `GEMINI_SESSION`, and
    `CURSOR_AGENT`, `CURSOR_TASK_ID`, `CODEX_SANDBOX`, `CODEX_THREAD_ID`,
    `LEAN_CTX_AGENT_ID`, `LCTX_AGENT_ID`, `AI_AGENT`; the agent shell
    policy refuses `cloud plan …` and `cloud decision …` before that;
  - no MCP tool or `ctx_agent` action renders this kind, and the id file is a
    secret path (37.4).

### 37.7 Open

- Cloud: the routes of 37.2, the frames and feature of 37.1, a versioned
  plan object (encrypted at rest) with publish (37.10) and plan signing with
  a control key (37.3), `published_at`, an acks read route for Code.
- The signature covers `content_sha256`, the digest of the plain body. The
  engine checks it, then takes goal and briefs only from that verified body
  and checks every routed session against it (37.3); a release must match
  the goal and brief digests of the verified body (37.9).
- `decision.changed` has no scope and no pull route: every active agent of the
  vault is asked to re-read, and a decision change missed while offline is not
  caught up.
- Known gap: `decision.changed` is not signed; the engine trusts it as a
  frame of the authenticated bus connection. It carries no text, only ids and
  a status, and asks agents to re-read. The plan signature (37.3) does not
  cover `sessions`: which agent gets which verified part (`task_id`,
  `worker_id`) and the `changed` flags are as trusted as the connection;
  the text itself is checked against the signed body.
- `decrypt_failed` is reserved for opened bodies and not sent yet.

### 37.9 `lean-ctx cloud plan release` (Send to agents)

Code calls it once per agent entry it releases, after the person clicked
"Send to agents" and confirmed. CLI only: there is no local route, MCP tool,
`ctx_agent` action, bus message or Cloud frame that releases. When an agent
marker of 37.6 is set it answers exactly `{"ok":false,"code":"agent_refused"}`
(exit 1, no detail on which marker), before reading stdin, and the agent
shell policy (`ctx_shell`, shell hook) refuses `lean-ctx cloud plan …` and
`lean-ctx cloud decision …`.

- No argument but `--json`. The release id is never in argv (other processes
  of the same user can read it there): an argument `--release-id` is refused
  `invalid_argument` (exit 2) with a message saying it moved to stdin, and so
  is any other argument.
- stdin: one JSON object, at most 64 KiB, unknown fields refused:
  `{"release_id": "<rid>", "version": <n>, "goal": "<plan goal>", "brief": "<this agent's brief>"|null}`;
  a decision release is `{"release_id": "<rid>"}` only. `release_id` is
  checked first; a missing or invalid one is `invalid_argument` (exit 2).
- `rid` is the `release_id` of one `agents[]` entry of 37.6 with state
  `offered`: 64 lowercase hex (256 bits from the OS CSPRNG), drawn by the
  engine per offered directive and per agent. It is bound to that directive
  and agent; it releases nothing else (not the agent's newer version, not
  another agent).
- The body is what the person confirmed. `version` must be the directive's
  version. `brief` must be the brief of the
  agent's own part, its SHA-256 equal to the session's `brief_sha256`
  (`"sha256:" + hex(sha256(UTF-8 bytes))`, exact bytes); `null` for a
  plan-wide (`…:all`) directive. `goal` must be the verified body's goal
  (its SHA-256 equal to the one the directive was offered with, 37.3).
  `goal` and `brief` are non-blank, at most 16 000 and 32 000 bytes, without
  control characters other than `\n` and `\t`
  and without a character a Cloud text field refuses (bidi and other format
  controls, zero-width characters: `lean_ctx_protocol::cloud::invisible`).
  The engine delivers exactly these (37.4). A decision release takes no body.
- stdout is one JSON line; `--json` is accepted and changes nothing.
  - released: `{"ok":true,"released":true,"directive_id","agent_id","version","released_at"}`, exit 0;
    the bus entry is written before the records, so a record write that fails
    after it still answers `ok:true`, with the failure in `warning` (delivered);
  - the same id again after its release (idempotent): `{"ok":true,"released":false,…}`
    with the first release's fields, exit 0, nothing changed (the body is not checked);
  - a directive superseded by a newer offer before its release:
    `{"ok":false,"released":false,"error":{"code":"superseded","message"}}`, exit 1;
  - unknown id (never drawn here, or pruned): `error.code` `not_found`, exit 1;
  - any argument but `--json` (`--release-id` included), no `release_id` on
    stdin, a value that is not 64 lowercase hex (a directive id
    `<plan>:<v>:<slot>` included), a missing, malformed or mismatching body:
    `invalid_argument`, exit 2, nothing delivered;
  - inside an agent: `{"ok":false,"code":"agent_refused"}`, exit 1; another
    failure: `internal`, exit 1.
  Anything but `ok:true` with exit 0 means not delivered.
- A release posts the directive on the local bus (37.4), sets it pending,
  records `released_at` (37.6 `releases`) and drops the id from the id file.
- Known limits: the markers and the shell policy are not an OS boundary.
  A process an agent starts runs with the person's rights; one that runs
  without the markers and outside the lean-ctx shell policy can read the id
  file or call this CLI. The shell policy judges one command line at a
  time: it follows variables, directory changes, substitutions and pipes
  within that line, but a list of names that one command writes to a file
  and a later, separate command reads is outside that check. The release
  audit (37.6 `releases`) lives
  in the directive records, which keep at most 500 entries: it is a local
  view, not a durable log.

### 37.10 `lean-ctx cloud plan publish` (publish a version)

Code calls it when the person publishes a plan or a new version. CLI only;
when an agent marker of 37.6 is set it answers exactly
`{"ok":false,"code":"agent_refused"}` (exit 1, no detail on which marker)
before reading stdin (agents may not publish), and the agent shell policy
refuses `lean-ctx cloud plan …`. Plan text is never in argv and never logged.
The vault is the one the project root of the working directory is bound
to, as for the other `cloud` commands; there is no project flag.

- No argument but `--json`; any other argument is `invalid_argument`
  (exit 2) and its value is not echoed.
- stdin: one JSON object, at most 400 KiB (a body may have 256 KiB),
  unknown fields refused:
  `{"idempotency_key": "<uuid>", "plan_id"?: "plan_…", "supersedes_version"?: n, "body": {plan-body/v1}}`.
  A new plan has no `supersedes_version`. It may name its `plan_id`
  (`plan_` and 8–40 letters or digits): Cloud then creates version 1 under
  that id, or refuses `plan_exists` (409) when a plan with that id exists;
  it never becomes a new version. Without `plan_id`, Cloud draws one. A new
  version names both (`supersedes_version ≥ 1`); `supersedes_version`
  without `plan_id` is `invalid_argument`. A body session may also carry
  `task_id` and `worker_id`: they are routing, taken out of the body.
- The engine checks the body (37.3) and serializes it as compact JSON in
  schema order, whatever the key order of stdin: `schema` (always written),
  `title`, `goal`, `decisions` (only when given), `sessions` with `slot`,
  `role`, `brief`, `folder` (`null` when not given). The digest and the
  base64 are of exactly these bytes, which are Cloud's signed test vector
  (`crates/lean-ctx-cloud/tests/fixtures/plan.vector.json`, byte for byte
  LeanCTX Cloud's `contracts/cloud-v2/fixtures/vectors/plan_statement.vectors.json`,
  sha256 `7d1fa2bb03457bf1db78ddc59da0bcd8bffa75f49829189435c9d3ae9681ef26`). It
  sends `POST /v1/vaults/{vault_id}/plans` (the bound vault) with the header `Idempotency-Key: <uuid>`, at most
  400 KiB:
  `{"schema":"leanctx.cloud.plan-publish/v1", "plan_id", "supersedes_version", "content_sha256", "body":"<base64>", "sessions":[{"slot", "task_id", "worker_id", "brief_sha256"}]}`
  (`plan_id`, `supersedes_version`, `task_id` and `worker_id` are written
  as `null` when not given; at most 32 sessions; `slot` `all` routes to
  every session).
- Cloud answers `201` with `leanctx.cloud.plan-published/v1`
  `{plan_id, vault, version, supersedes_version, content_sha256, issued_by, published_at, sessions:[{…, changed}]}`;
  the engine requires the same `content_sha256`, else `protocol`.
- stdout is one JSON line (`--json` changes nothing):
  - published: `{"ok":true,"plan_id","version","content_sha256"}`, exit 0;
    a replay with the same `idempotency_key` and the same request gets
    Cloud's first answer and prints exactly the same shape, no extra field
    (there is no "in progress" answer);
  - the key was used for another request (`409`): exactly
    `{"ok":false,"code":"idempotency_key_reused"}`, exit 1; Code draws a
    new key;
  - a newer version exists: exactly
    `{"ok":false,"code":"stale_version","current_version":n}`, exit 1;
  - the working directory is bound to no vault: exactly
    `{"ok":false,"code":"not_bound"}`, exit 1, nothing sent;
  - inside an agent: exactly `{"ok":false,"code":"agent_refused"}`, exit 1;
  - otherwise refused: `{"ok":false,"code","message"}`, exit 1, with `code`
    `invalid` (400), `forbidden` (403), `not_found` (404), `plan_exists`
    or `not_ready` (409), `rate_limited` (429), `protocol`, `not_connected`
    or `offline`;
  - an argument, empty or invalid stdin, stdin or a body text that is not
    UTF-8 (also a lone `\ud800`-style escape), a body outside 37.3:
    `{"ok":false,"code":"invalid_argument","message"}`, exit 2, nothing sent
    (Cloud refuses a non-UTF-8 body with `400`, field `body`).

### 37.8 Fixtures

`frame.plan_advanced.json` and `frame.decision_changed.json` belong in the
Cloud C0 tree (`bus/`) and reach the engine copy through `scripts/sync-cloud-c0.sh`;
until then the engine tests carry the frames inline.

## 38. `lean-ctx decisions suggest --json` / `decisions confirm` (decision suggestions, trust levels)

Draft decisions derived from the local git history (commit and merge-request
messages), plus the trust of the decision records confirmed earlier. Local only: no
request leaves the machine, and nothing becomes a decision without a person.

`decisions suggest [--since <ref>] [--max-commits N] [--json]` scans `HEAD` (or
`<ref>..HEAD`, at most `N` commits, default 1000) and writes the same document to
the review file `.leanctx/decisions-review/suggested.json`. It never writes
`.leanctx/decisions/`. A symlinked `.leanctx` or review directory, or a review file
that is not a regular file, is refused. The file is written through a fresh,
randomly named temp file (created exclusively, never through an existing link) and
renamed into place. The review directory gets a `.gitignore` with `*` (created once,
never through a link), so drafts are not committed by accident.

The previous review file is untrusted input (it sits in the working tree): drafts
whose id is not `[a-z0-9-]{1,128}`, whose commit is not 7 to 64 hex digits, or whose
date is not a calendar date `YYYY-MM-DD` are dropped when it is read, and the `title`
of a carried draft is derived from its statement again. Human output (including the
invalid-record errors of `decisions query`) replaces control characters and bidi
overrides with `?`; JSON output is escaped by serde.

The history is read with `git log -z` (NUL-separated records) and checked against a
separate list of commit ids: a record whose id is not hex, whose date is not a
calendar date, or ids that differ between both reads make `suggest` fail (exit 1)
instead of guessing. A message line holding a control character (tab aside) never
becomes a quote.

```json
{
  "schema": "leanctx.decisions-suggest/v1",
  "range": "HEAD",
  "already_recorded": ["dec-merge-with-merge-commits"],
  "records_superseded": ["dec-squash-releases"],
  "records": [
    {"id": "dec-merge-with-merge-commits", "trust": "confirmed"},
    {"id": "dec-old-rule", "trust": "re-check",
     "recheck_reason": "evidence commit 0123456789ab is no longer in the history"},
    {"id": "dec-squash-releases", "superseded_by": "dec-never-squash-release-commits-4f1c2a"}
  ],
  "drafts": [
    {
      "id": "dec-never-squash-release-commits-4f1c2a",
      "title": "Never squash release commits",
      "statement": "Never squash release commits",
      "status": "draft",
      "trust": "confirmed",
      "evidence": {
        "commit": "4f1c2a…",
        "date": "2026-11-01",
        "kind": "merge-request",
        "ref": "!412",
        "quote": "Decision: Never squash release commits.",
        "hash": "9c0d…"
      },
      "supersedes": ["dec-squash-releases"],
      "also_in": ["77aa01…"]
    }
  ]
}
```

Trust (`trust`, kebab-case):

- `confirmed`: an explicit statement: a line starting with `Decision:`,
  `Owner decision:` or `Decided:`, or a quote after an owner answer verb
  (`Owner answered/decided/chose/replied/said "…"`). `quote` is the history line
  verbatim. A bare `Owner:` line or a quote that merely mentions an owner is not
  explicit; with policy wording it is `reconstructed`, else no draft.
- `reconstructed`: derived from policy wording (never, no longer, from now on, …).
  `quote` is still the verbatim line it was derived from. When a later commit states
  the same decision explicitly, the draft becomes `confirmed` and its evidence moves to
  that commit; the earlier commit is listed in `also_in`.
- `re-check`: the evidence changed. For a draft: its evidence hash differs from the
  previous review file, its commit left the history, or (a draft carried over from
  outside the scanned range) its commit no longer yields this exact quote, statement
  and trust (`the draft no longer matches commit …`). For a record: its evidence
  commit is no longer reachable from `HEAD`, or its quote is no longer a line of that
  commit's message. `recheck_reason` says which. A draft stays `re-check` until the
  review file is removed.
- Reachability comes from one `git rev-list HEAD` per run. When git cannot answer
  (an error, a timeout, a commit beyond a shallow clone's boundary) nothing is flagged:
  a draft keeps its trust and a record passes its stored trust on; the next run asks
  again.

Rules:

- `evidence.hash` is the first 16 hex digits of blake3 of `commit + "\n" + quote`.
  `kind` is `merge-request` (with `ref` `!N`) or `commit` (with `ref` the short sha).
- A draft whose statement is already recorded (normalised) is skipped; the record's
  id is listed in `already_recorded`.
- One draft per statement: a previous draft outside the scanned range whose statement
  a current draft repeats (for example after a narrower `--since`) is not carried
  again; its commit joins that draft's `also_in`.
- Superseded: a draft is superseded by a revert of its commit, a `Supersedes:` trailer
  naming it, or a later draft on the same topic with the opposite polarity
  (`superseded_by`, and the later draft's `supersedes`). An active record is superseded
  the same way: a `Supersedes:` trailer naming its id, a revert of its evidence commit,
  or a later opposite draft dated on or after the record's `decided_at`. Its id is then
  listed in `records_superseded`. Nothing is rewritten; a person updates the record.
- `records` has one row per active record that has evidence or is superseded, sorted by
  id. A record without `[evidence]` has no `trust`.
- Deterministic (#498): the document is a function of the history, the records and the
  previous review file. No timestamps or counters; dates are commit dates. Drafts are
  in history order.

`decisions confirm <draft-id> [--id <slug>] [--decided-by owner|team|review]` turns one
draft from the review file into `.leanctx/decisions/<id>.toml`:

- The review file is untrusted: `confirm` resolves the draft's commit to its full id
  and runs the extraction on that commit's message again; the draft's `quote`,
  `statement` and `trust` must be one of its results exactly.
- Refused: no review file, an unknown draft, a superseded or `re-check` draft, an
  evidence commit no longer in the history, a quote no longer in its message, a draft
  that does not match its commit (`draft … does not match commit …`), an id that
  already exists, git unable to read the evidence commit, or a symlinked `.leanctx`
  path. Exit code 2 for a usage error or a missing review file or draft,
  1 for the others. `suggest` exits 2 on a usage error and 1 on a runtime error.
- The id is `dec-<slug>` from `--id`, else from the first words of the statement.
  `decided_by` defaults to `owner` when the quote names the owner, else `team`;
  `decided_at` is the commit's author date read from git; `[source]` (`mr`/`!N` or
  `commit`/short sha) comes from the commit message; `title` is derived from the
  statement; `[evidence].commit` is the full id. None of these is taken from the
  review file.
- `supersedes` lists the draft's superseded ids that git confirms for this commit:
  records named by a `Supersedes:` trailer of the evidence commit, or active records of
  the same topic and opposite polarity decided on or before its date. The review file
  can only narrow this list; an edited file never retires a record.
- The record carries the verbatim evidence; `rationale` and `scope` start empty for the
  person to fill in:

```toml
[evidence]  # verbatim history line (contract §38)
commit = "4f1c2a…"          # 7 to 64 hex digits
quote = "Decision: Never squash release commits."
trust = "confirmed"         # confirmed | reconstructed
```

- The draft leaves the review file and its record id joins `already_recorded`. Every
  later `suggest` re-checks the record's evidence (see `records`).
- Without `--json`, `suggest` prints the same read as lines for people; not a contract.

## 40. Agent exchange: `lean-ctx cloud exchange audit --json` (kind `cloud.exchange.audit`)

Agents exchange knowledge and tasks through the vault bus of LeanCTX Cloud across
machines and with people: a knowledge item is a `finding` (a statement with at
least one ref, for example `node:<id>`, `file:<path>`, `commit:<sha>`), a task
hand-over is a `handoff` to a worker (`resource` such as `task:<n>`), answered by an
`answer` with `accept`; `question` and `offer` are the conversation around them.
Agents send them with `ctx_agent action=send|reply|handoff` (a `wkr_…` target or a
`type`), unchanged.

Every exchange passes these rules, in this order. Outbound (before the publish is
queued):

1. `account`: a device enrolled in a personal account exchanges nothing.
2. `policy`: an expired or damaged Cloud policy refuses. The vault's effective
   policy applies: `sharing.allowed_vaults` (when set, the vault must be listed) and
   `sharing.agent_exchange`, the sorted list of message types that may leave (absent:
   all; not a sorted list of `answer|finding|handoff|offer|question`: none).
3. `content`: the installation screen (secrets, personal data, redaction and
   forbidden patterns, local home paths; broken rules refuse) over every field
   that leaves: text, handoff resource and every ref.
4. `person_rules`, `message`: the always-enforced person rules and the C0 message
   validation, as for the local bus.

Inbound (a `message.deliver` of an exchange type from a worker or person of another
machine): `person_rules`, `content` over every body string and ref, and `policy` for
the linked vault. A refused delivery moves the bus cursor only and never reaches the
local bus. `progress` and this machine's own messages are not exchanges.

Every outcome leaves a record in `<cloud data>/bus/exchange-audit.jsonl` (0600; a
header line, then one JSON line per record, appended; past 4 MiB it is compacted to the
newest 2000). A send whose record cannot be written is refused; a delivery whose
record cannot be written is not applied (it is delivered again).

```json
{
  "contract": "local-read/1",
  "kind": "cloud.exchange.audit",
  "total": 2,
  "entries": [
    {
      "at": "2026-11-19T10:02:00Z",
      "direction": "in",
      "outcome": "dropped",
      "kind": "handoff",
      "address": "#acme/payments",
      "msg_id": "msg_00000000000000b1",
      "peer": "wkr_gawyravj36igdp3irdfo",
      "rule": "policy",
      "reason": "the team policy does not allow exchanging a handoff (sharing.agent_exchange)",
      "digest": "sha256:…"
    },
    {
      "at": "2026-11-19T10:00:00Z",
      "direction": "out",
      "outcome": "queued",
      "kind": "finding",
      "address": "#acme/payments",
      "msg_id": "01M5NKA2Z8B000000000000002",
      "agent": "claude-4711",
      "digest": "sha256:…"
    }
  ]
}
```

- `entries` newest first; `--limit N` (1 to 2000, default 50) bounds them, `total`
  counts every record kept.
- `direction`: `out` | `in`. `outcome`: `queued` | `refused` (out), `delivered` |
  `dropped` (in). `kind`: the message type.
- `msg_id`: the `client_msg_id` (out) or Cloud's `msg_…` id (in). `agent`: the local
  agent (out). `peer`: the addressed worker (out) or the sender (`wkr_…` or
  `person:<handle>`, in). `rule` and `reason` only on `refused`/`dropped`; `rule` is
  `account`, `policy`, `content`, `person_rules`, `message` or `queue`.
- `digest`: `sha256:` of the canonical JSON of the message body. No record carries the
  content; a `reason` names what kind of content was found, never the content.
- A damaged ledger gives the `error` document; the next exchange moves it aside
  (`exchange-audit.jsonl.damaged-<n>`) and starts a new one.
- Cloud enforces `sharing.agent_exchange` and `sharing.allowed_vaults` itself as well
  (`403 policy_denied`) and keeps the team's audit log; this record is the machine's
  own.
- `cloud exchange audit` without `--json` prints the same records as lines for
  people; not a contract.

## 44. `lean-ctx cloud computer list|create|start|stop|show|add … --json` (kinds `cloud.computers`, `cloud.computer.ack`, `cloud.computer`)

The LeanCTX computers of the person and their teams, as LeanCTX Cloud runs them
(`/v1/computers`, cloud design `cloud-computer.md` §4.3). LeanCTX Cloud decides who may
create or start a computer, its role, the free hours, the top-up and the trial; the CLI
passes the request on and the cloud's refusal back. Computer minutes after the free
hours are paid from the top-up only. No code and no file content is sent: the bodies
carry the request id, the name, the role, the team and `confirm`. LeanCTX Cloud picks
the size and the region and does not stop an idle computer: a computer runs, and is
billed, until you stop it (`lean-ctx cloud computer stop <name>`).

Local service routes (LeanCTX Code, local-service-v1): `GET /v1/local/cloud/computers`
(`cloud.computers`), `POST /v1/local/cloud/computers` (`create`; body `name`, `role`,
`team`, `personal`, `request_id`, `yes`) and
`POST /v1/local/cloud/computers/{id}/start` (`request_id`, `yes`) and `…/{id}/stop`
(`request_id`), all `cloud.computer.ack`. They answer the same documents as the CLI with
`--json`; `yes: true` is the CLI's `--yes`. The local registrations (`add`, `show`) stay
CLI only.

**A person starts a paid computer** (owner rule). `create` and `start` are sent only when
a person appears to have asked: a CLI whose stdin is a terminal, or a call over the local
service socket (LeanCTX Code). Any other caller (a script, an agent's shell, a pipe) is
refused before anything is sent, not even the list, `--yes` or not:
`confirmation_required` with the message "Starting a paid computer needs you at a
terminal or in LeanCTX Code." (exit 1, no `details`). The app sets no client marker
when it runs the CLI, and no environment variable or header counts. `stop` costs nothing
and needs no person.

This check guards against an agent creating or starting a computer **by accident**; it
is **not a security boundary**. The socket is bound `0600` to the person's own user and
has no token, so any process of that user (an agent's shell with `curl --unix-socket`
included) can send `yes: true` to `…/start`, and a CLI run under a pseudo-terminal
(`script`) passes the terminal check. A real boundary (a peer check of the socket
client, or the confirmation in LeanCTX Cloud) is required before computers are offered.

Dependency: `confirm` and `428 confirmation_required` on `POST /v1/computers` and
`POST /v1/computers/{id}/start` are not yet in the cloud design (§4.3). This CLI never
relies on them: without `--yes` it sends no create or start at all. It must not ship
before LeanCTX Cloud has `confirm` and the `428` on production.

### `list` (kind `cloud.computers`)

```json
{
  "contract": "local-read/1",
  "kind": "cloud.computers",
  "available": true,
  "live": true,
  "computers": [
    {
      "id": "cmp_01HZX",
      "name": "weave-host",
      "scope": {"kind": "team", "team": "org_7c1d"},
      "state": "running",
      "role": "weave_host",
      "price": {"usd_per_hour": 0.09},
      "stop_reason": null,
      "ssh": {"host_alias": "leanctx-weave-host", "hostname": "203.0.113.7", "user": "lc-6f0b7c3e",
              "port": 22, "host_key_fingerprints": ["SHA256:…"], "config_written": false},
      "balance": {"usd": 12.4, "state": "ok", "free_hours_left": 494.8, "usd_per_hour": 0.09}
    }
  ]
}
```

- Rows are the cloud's rows, passed on unchanged (every member it sent), except that a
  row whose `ssh` is an object gets the local fact `ssh.config_written`: `true` only when
  this machine registered a computer of that name whose host and user (and port, when
  the row sends one) are the row's `ssh.hostname`, `ssh.user` and `ssh.port`, its
  `Host leanctx-<name>` entry is in `~/.ssh/leanctx/config`, and `~/.ssh/config`
  includes that file. A local entry of the same name for another host or user is
  `false`. A row without a string `id` (`cmp_…`), `name` and `state` is left out;
  at most 64 rows are kept, in the cloud's order. `state` is passed on as sent
  (`provisioning`, `starting`, `running`, `stopping`, `stopped`, `suspended_balance`,
  `deleting`, `deleted`, `failed`, or a newer one).
- There is no top-level balance (cloud design §4.3): each row carries its own scope's
  `balance` as the cloud sent it (`free_hours_left` is `null` for a `general` computer
  and `0` for a personal one or during a trial).
- The document has no local clock (#498): the same cloud answer gives the same bytes.
- `scope.team` is the team's org id, the same id `--team` takes and `create` sends; the
  CLI matches scopes by that id, never by a team's display name.
- `available: false`, `live: false`, `computers: []`: LeanCTX Cloud offers no computers
  yet. It answers `200 {available: false, computers: []}` without prices while it has no
  provider; it may also answer `503 computer_not_available`, and an older cloud answers
  `404` without an error code or `503 not_configured`. All of them give this document,
  never an `error` document (the CLI `list --json` and `GET /v1/local/cloud/computers`
  alike). A reader shows its "LeanCTX computers are not available
  yet." card and offers no create (and no costs); the text `list` prints that sentence.

### `create`, `start`, `stop` (kind `cloud.computer.ack`)

- `create [--name <name>] [--role weave_host|general] [--team <id> | --personal]
  --yes`: `POST /v1/computers` with `{request_id, name, team?, role, confirm: true}` and
  nothing else (LeanCTX Cloud refuses unknown fields). The default role follows the
  scope: `weave_host` on a machine connected to a team (default name `weave-host`),
  `general` on a machine connected to a personal account (a name is needed). A
  `weave_host` is for teams only (LeanCTX Cloud answers `400 invalid_field`), so
  `--role weave_host` on a personal machine is `invalid_argument` (exit 2) before
  anything is sent and before any terms are shown. The scope is the one this machine is
  connected to, because LeanCTX Cloud creates the computer there whatever the request
  names: a team machine sends its team's org id, a machine connected to a personal
  account sends no `team`. `--team <id>` must be that connected team's org id, and
  `--personal` is accepted only on a machine connected to a personal account; anything
  else (another team, `--personal` on a team machine, `--team` on a personal machine) is
  `invalid_argument` (exit 2) before anything is sent, so a person never confirms a
  personal computer that would be created and billed as a team's. There are no size,
  region or idle-stop options (`--size`, `--region`, `--idle-stop` are unknown options)
  until LeanCTX Cloud takes and enforces them.
- `start <name|id>` and `stop <name|id>`: the computer is found in `GET /v1/computers`
  by id (`cmp_…`) or by name (a name in several scopes picks the team this machine is
  connected to, else `invalid_argument`), then `POST /v1/computers/{id}/start|stop`. An unknown name:
  `no_computer`.
- Names: 1-31 of `a-z`, `0-9`, `-`, starting with a letter or digit (the cloud's rule).
- Every write sends a `request_id` (`req_` + 32 hex, or `--request-id <8-64 of A-Za-z0-9_->`
  to retry the same write): the cloud answers a repeated id with its first answer.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.computer.ack",
  "action": "start",
  "id": "cmp_01HZX",
  "name": "weave-host",
  "state": "starting",
  "role": "weave_host",
  "request_id": "req_6f0b7c3e9a2d4b1c8e7f6a5b4c3d2e1f",
  "billing": {"basis": "free", "free_hours_left": 494.8, "usd_per_hour": 0.09,
              "topup_usd": 12.4, "topup_state": "ok"}
}
```

- `id`, `name`, `state`, `role` are the cloud's answer (`provisioning` after `create`,
  `starting` after `start`, `stopping` after `stop`), `null` when it sent none.
- `billing` (`create`, `start`; `null` for `stop`): who pays the first hours, from the
  row the cloud answered (scope and balance the answer leaves out come from the listed
  row). `basis` is the confirmation's vocabulary: `free` only for a team-scoped
  (`scope.kind: "team"`) `weave_host` computer while `free_hours_left > 0` (the team's
  free hours), else `billed` (billed to the top-up at `usd_per_hour`); `null` for a
  team's `weave_host` whose row has no finite, non-negative `free_hours_left` (unknown,
  never guessed). A personal computer is never `free`. `topup_usd` and `topup_state` are
  the row's `balance.usd` and `balance.state` (what is left of the top-up). The numbers
  are the cloud's, `null` when absent, not finite or negative; the text output says a
  missing price is unknown and never prints `$0.00` for it. The cloud decides what is
  charged; `billing` only names it.
- `create` and `start` can cost money and fail closed: **without `--yes` nothing is sent
  to LeanCTX Cloud but the list** (`GET /v1/computers`, which changes nothing). The CLI
  prints the `error` document with code `confirmation_required` (exit 1), the terms in
  `error.details`:

  ```json
  {
    "contract": "local-read/1",
    "kind": "error",
    "error": {
      "code": "confirmation_required",
      "message": "start computer 'weave-host': it runs on the team's free hours (494.8 left this month), then $0.09 per hour from the top-up (top-up $12.40 left). Nothing was sent; repeat with --yes to start.",
      "details": {"billing": "free", "usd_per_hour": 0.09, "free_hours_left": 494.8,
                  "topup_usd": 12.4}
    }
  }
  ```

  The terms come from data LeanCTX Cloud already sent (the cloud design has no
  non-mutating quote route): for `start`, the listed row (`billing` as the ack's
  `basis`: `free` or `billed`; `usd_per_hour` from `price`, else the balance;
  `free_hours_left` and `topup_usd` (`balance.usd`) from its `balance`); for `create`, the `balance` of
  a listed computer in the same scope (the balance is the scope's). A personal computer
  (`--personal`, or a row with `scope.kind: "personal"`) and a `general` one are always
  `billed`; a team's `weave_host` with no listed computer in its scope has
  `billing: null`. Numbers the cloud did not send are `null`, and the message then says
  the price was not reported, never `$0.00`. A list the cloud refuses gives its refusal
  instead. A list with `available: false` (or a cloud without computers) gives
  `computer_not_available` with the message "LeanCTX computers are not available yet."
  and no `details`: availability is checked before any cost is shown.
- With `--yes` the write goes through the release point (G08) first: the request's
  fields are released inside the CLI tool run before anything is sent, and a withheld
  release sends nothing (`withheld`). Then `create` and `start` are sent with
  `confirm: true`; an unconfirmed request is never sent, so a cloud can never run one.
  A LeanCTX Cloud that still answers `428 confirmation_required` (a second guard) gives
  the same `confirmation_required` document with the terms of its `error.details`
  (`billing`, `usd_per_hour`, `free_hours_left`, `topup_usd`, read from `balance_usd`
  when the cloud sends that name instead); since `--yes` was already
  given, its message does not say to repeat it but to check the terms
  (`lean-ctx cloud computer list`) before trying again.
  `stop` takes no confirmation and always runs.
- The cloud's refusal is the `error` document with its code, and its `error.message`
  followed by `(LeanCTX Cloud: <status> <code>)`:
  `402 balance_empty` → `balance_empty` (message adds "Add a top-up to continue." when the cloud
  names `next_step: top_up`); `402 plan_required` → `plan_required`; `403 forbidden` →
  `forbidden`; `403 sso_required` → `sso_required`; `404 no_computer` → `no_computer`;
  `409 computer_exists`, and `409 invalid_state` whose `error.reason` or
  `error.details.reason` is `computer_exists`, → `computer_exists`; `409 computer_busy`
  → `computer_busy`; any other `409 invalid_state` → `invalid_state`; `400`/`422 invalid_request` → `invalid_field`;
  `428 confirmation_required` → `confirmation_required` (the second guard, above); `429` → `rate_limited`
  (message names `retry_after_s`); `503
  computer_provider_unavailable` → `computer_provider_unavailable`; `503
  computer_not_available` (no provider yet, on `POST /v1/computers`, `…/start`, `…/stop`),
  a list with `available: false`, `404` without a code and `503 not_configured` →
  `computer_not_available` with only the message "LeanCTX computers are not available
  yet." (no cloud text, no `(LeanCTX Cloud: …)` suffix); not connected → `not_connected`. An unknown code takes the
  code of its status.
- No command has a `sudo` or root path: a `weave_host` computer is a locked appliance.

### Local registrations (`add`, `show`; kind `cloud.computer`)

A computer reached as an SSH remote host with host keys the person pinned (`add <name>
--host … --host-key … | --fingerprint …`). The text `list` shows them after the cloud's
rows; `cloud.computers` does not.

```json
{
  "contract": "local-read/1",
  "kind": "cloud.computer",
  "available": true,
  "live": false,
  "as_of": null,
  "computer": {
    "name": "dev",
    "source": "local",
    "ssh": {
      "host_alias": "leanctx-dev",
      "hostname": "203.0.113.7",
      "user": "lc-6f0b7c3e",
      "port": 22,
      "host_key_fingerprints": ["SHA256:0ffjc0LKN6E6K2TAoU7JnGFm66vaaBps0il/sCsWtOs"],
      "config_written": true
    }
  }
}
```

- `add` without `--user` takes the person's own user on that cloud computer
  (`ssh.user`, one `lc-…` user per person) from `GET /v1/computers`; when the cloud
  cannot name it, `add` asks for `--user` (`invalid_argument`). There is no shared
  default user.
- `ssh-config --write` writes LeanCTX's own files (`~/.ssh/leanctx/config`,
  `known_hosts`) by a new temporary file and a rename. `~/.ssh/config` is the person's:
  a symlink is followed and its target written (a link that cannot be resolved is
  refused, with the `Include` line to add by hand), the file keeps its mode, the new
  text goes to a new temporary file (`O_EXCL`) next to the real target, and it is
  renamed onto it only if the file still reads as it did; otherwise nothing is written.
- `source: "local"`: the row comes from this machine's registry; `live` is `false` and
  `as_of` is `null` because no Cloud answer stands behind it. Fields only Cloud can
  answer (state, role, price) are absent, never guessed.
- `host_key_fingerprints`: the pinned keys, as `ssh-keygen -l` prints them. SSH to the
  computer refuses any other host key (`StrictHostKeyChecking yes` with its own
  known_hosts, keyed by `host_alias`).
- `show` of an unknown name: the `error` document with code `not_found`; a bad name or
  flag: `invalid_argument` (exit 2).
