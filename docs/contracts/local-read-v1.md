# Local Read v1

Status: **experimental** (`"contract": "local-read/1"`).

Changes in 3.12.3 (additive unless marked):
- Policy and sync (§23): `hive.sync`, `policy.sessions` and `policy.session` are served on
  the local service (`/v1/local/hive/sync`, `/v1/local/hive/policy/sessions[/{id}]`) and
  listed in capabilities. `policy.sessions` `installed` adds `vaults`: one row per vault the
  policy reaches here, with its `vault_id` and which policy applies. `installed` adds
  `enforcement`: the version enforced here, when Hive acknowledged it, the live sessions
  under it and when this Instance reported it to Hive (§23.2).
- Local tasks (§3c): a project linked to no vault keeps its tasks on this Instance;
  `lean-ctx tasks list|show|propose|decide|claim|update|submit|amend|release|verify|stop --json`
  (`tasks.list`, `tasks.task`, `tasks.ack`), the same on the local service under
  `/v1/local/tasks`; new error code `conflict`.
- Share (§19, §20): `hive vaults --json` lists the vaults this Instance knows; `hive link
  --vault V --json [--dry-run] [--bring-local] [--leave-unsettled-local]` links the project and
  can bring its local docs and open local tasks into the vault; `tasks.list` adds
  `local_not_shared`, and `--local` (§3c) reaches local tasks left unshared.
- Local docs (§2c): `docs.list` / `docs.page` carry `"source": "vault" | "local"`.
  **`vault` is `null` when `source` is `local`** (it was always an object before). `hive docs list|show --json`
  on a checkout with no vault-map record (signed in or not) now prints the local pages instead of
  error `no_vault`; a reader that took `no_vault` as "not shared" reads `source: "local"` instead.
  A linked checkout without a replica still answers `no_vault` ("sync first"); signed out it answers
  `not_connected`; a vault map that cannot be read answers `unavailable` (§2c).
- `hive.presence` rows (§4) add `kind`, `display_name`, `session_name` and `project`;
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
  before anything is written or sent to Hive; a withheld write writes nothing.
- `policies.list` (§15b) adds `pack_sources.org_policy` with the signature state.
- `agents.list` (§16) is a route and is released like every kind; `agents list --json` = `agents --json`.
- New `daemon.status` (§17).
- `ctx_agent register` (§11): `role` is a role id, `name` the display name; free text given
  as `role` is not stored as a role; the answer names the role and name it stored.
- Feature modules add kinds through one extension list: every kind, verb and route stays in
  the one capabilities document (§14).

LeanCTX | OSS prints these JSON documents for a local reader on the same
machine, such as LeanCTX | Hive Desktop, so the reader never parses private
state files or human text and never holds a Hive token. The reader spawns
`lean-ctx` and reads stdout. Everything is read only, except the writes named
below (§3c, §5b, §11, §12, §13, §20, §21): local task transitions, the project's link (`hive link` without `--dry-run`), bus posts, reads marked, leases, knowledge
writes (freshness clean-up included) and the evidence package, each through the store's own lock and
admission. Docs, presence and status come from local
state only; `hive tasks` reads through this Instance exactly like the text
`hive tasks list|show`.

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
  `hive.presence.instance` and `docs.*` `vault` for local pages, are named in the change
  list above).
- Enum values unknown to a consumer should be shown as-is (strings), not rejected.
- Errors: exit code 1 (2 for bad arguments) and on stdout:
  ```json
  {"contract":"local-read/1","kind":"error","error":{"code":"not_connected","message":"human text"}}
  ```
  `code` ∈ `invalid_argument` | `no_project` | `not_connected` | `no_vault` | `not_found` | `forbidden` | `withheld` | `anchor_gone` (`knowledge verify`: the code an anchor names is gone) | `conflict` (a local task's state does not allow the write, §3c) | `unavailable` | `rate_limited` (3.12.3: Hive holds this Instance's requests for the seconds the message names; never reported as `not_found`) | `internal`.
  A document withheld as a whole at the release point is this `withheld` error document on stdout (exit 1; its message is the content-free reason). Only when not even that refusal could be receipted is nothing printed on stdout, and the reason goes to stderr.
- Read-only, except:
  - `agent post` writes one bus message;
  - `knowledge verify <id>` and `knowledge dedupe --apply` write the knowledge store (§21);
  - `knowledge stale` writes back stored anchor marks that disagree with the live check (§21a);
  - the other writes named above.

  Never prints a Hive token.
- `knowledge.list` (§1) adds `withheld`: facts are released one by one, and a fact the release
  point would withhold no longer withholds the whole list (#1298).
- Money is USD as a JSON number rounded to 6 decimals; never a string.
- `--project-root P` (where accepted) picks the project; default = project root detected from the cwd.

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
- `status`: for facts that came from a Hive vault ∈ `verified` | `provisional` | `outdated` | `conflicting`; else `"local"`.
- `origin` ∈ `local` | `vault` | `provider` | `derived` | `unverified`.
- `source`: `{ "kind": "session" | "vault" | "provider" | "import", "ref": string }` or `null`.
  For `vault`, `ref` is the vault address `ws/vault`.
- `anchors[].symbol`: string or `null`.
- `review`: `null` | `"to_verify"` | `"gone"` (the code the fact rests on moved / vanished) | `"contradicted"` (an open contradiction with another fact).

## 2a. `lean-ctx hive docs list --json [--project-root P]`

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

## 2b. `lean-ctx hive docs show <id> --json [--project-root P]`

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
work locally; Hive adds sharing). `hive docs list|show --json` print the same documents there.
On a linked checkout `docs list|show --json` print the vault documents (§2a/2b) and the writes
answer `forbidden`. All take `--project-root P`.

The vault map alone decides whether a checkout is linked. A Hive state or vault map that cannot be
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

## 3a. `lean-ctx hive tasks list --json [--state S] [--role R] [--mine]`

Same source as today's `hive tasks list` (through this Instance; Desktop never sees a token).

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

- `partial` (3.12.3): `true` when a Hive answer came back full (100 tasks), so older tasks of
  these filters may be missing from `tasks`; the plain text then ends with a `(partial: …)`
  line. Never silently incomplete; narrow the list with `--state` or `--role`.
- `stale` (3.12.3, optional, also on `tasks.task`): present only when the answer is not
  current. Task reads are shared by every process of the Instance for 15 s and fetched once
  for all callers; when Hive holds the Instance's reads (429) or is not reachable, the last
  good answer is served with `stale: {age_s, reason: "rate_limited"|"unavailable",
  retry_after_s?}` instead of an error. Without a kept answer the error is `rate_limited`.
- `local_not_shared`: this project's local tasks that were neither brought into the vault nor
  closed (left local by `hive link --leave-unsettled-local`, or the link came in through git);
  they are acted on with `lean-ctx tasks … --local` (§3c), `[]` when none (also when the local
  task store cannot be read: the vault's list never fails on it). Its numbers are local ones,
  never the vault's.
- The listing does not carry `goal`/`acceptance`/claim holder: they are `null` in `list`
  (`claimed_by` is `{label:null, agent:null, mine:true|false}` while claimed, else `null`). `show` fills them.
- `state` ∈ `proposed` | `open` | `claimed` | `submitted` | `done` | `rejected` | `cancelled` | `failed`.
- `lifecycle`: `null` | `queued` | `claimed` | `running` | `submitted` | `completed` | `failed` | `cancelled`.
- `priority` ∈ `high` | `normal` (always printed; a task Hive sends without one is `normal`).
- `labels`: 0–16 unique `[a-z0-9-]{1,32}`, always an array.
- `fields_version`: `int` | `null`, +1 on every priority or labels write (C0 2.2 E47);
  `null` from a Hive that sends none. It is the `base_version` of the next write.
- Order: `high` first, then by `number` ascending (the board order, 3.12.3).
- `origin.kind` ∈ `person` | `agent` | `contradiction` | `question`.

## 3b. `lean-ctx hive tasks show <n> --json`

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

## 3b-1. `lean-ctx hive tasks priority|labels <n> … --json` (3.12.3)

The order of work on a board: a person sets a vault task's priority and labels
(C0 2.2 E47 `PATCH /v1/vaults/{vault}/tasks/{n}` at the task's `fields_version`; on
`409 stale_base` the Instance reads the task again and sends the write once more).
Hive answers `GET …/tasks` as one page, the newest 100 tasks by number. So that an
older high task is never cut off, the Instance also asks for `?priority=high` with the
same filters, merges both answers (each task once) and then sorts: high first, then by
number, before any display limit. Either answer coming back full (100 tasks) marks the
list `partial` (§3a): with more than 100 high tasks, the oldest are missing, and it says so.

```
lean-ctx hive tasks priority <n> high|normal [--labels a,b]... [--clear-labels] --json
lean-ctx hive tasks labels <n> <a,b,...> [--priority high|normal] --json
lean-ctx hive tasks labels <n> --clear --json
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
  `forbidden`. The socket reader is a person (Hive Desktop). Agents read priority and
  labels in every `tasks.list` / `tasks.task`.
- `task` is the task after the write (`null` when that read failed; the write stands).

## 3c. Local tasks: `lean-ctx tasks … --json [--project-root P]`

A project linked to no Hive vault keeps its tasks on this Instance (same task
model and states as Hive). For such a project `hive tasks list|show --json`
and `tasks list|show --json` answer from the local store with `"vault": null`
(a linked project answers from Hive as in 3a/3b, `vault` = its address).
`lean-ctx tasks` routes a linked project to Hive exactly like `hive tasks`.
With `--local` it acts on the project's own local tasks that were not brought into the vault
(`local_not_shared` in 3a): list, show and every write but `propose` (new tasks of a linked
project go to Hive); a task that was brought is refused (`forbidden`, naming the vault task).
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
  (Hive's state names; the event tells them apart).
- `--agent ID` reads as that bus agent (`claimed_by.mine`); `--mine` needs it.

Writes, each printing one `tasks.ack`:

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

- Without `--agent` a person acts (`--by NAME`, default `"person"`): Hive Desktop
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
  writes) hold for honest callers only. For a linked vault, Hive checks the
  identity on the server.
- When the Hive link of the checkout cannot be read, every `tasks` command fails
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
  Instance; it reaches another Instance only through hive-share (the task
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
  - On a linked vault it needs Hive's `POST /v1/vaults/{id}/tasks/{n}/amend`
    (`leanctx.hive.task-amend/v1`), which no Hive serves yet: the CLI checks the
    request and reads the task, then fails closed with `not supported by Hive
    yet` and sends no write (nothing changed).
  - On the local service: `POST /v1/local/tasks/{n}/amend`
    `{evidence?, summary?, merge_commit?, reason?, agent?, by?, local?}`.
- `change` is `null` when a claim only renewed. For a linked project `claim`,
  `update`, `submit`, `amend`, `release` and `verify` (`--agent` required) go to Hive;
  `change.seq` and `task` are `null` there.
- Refusals: `conflict` (the state does not allow it; the message names Hive's
  reason, e.g. `task_claimed`, `not_submitted`), `forbidden` (`self_verify`,
  `not_holder`, `not_submitter`, `agents_claim`, `agents_stop`), `not_found`, `invalid_argument`.
- Every local transition posts on the local bus (category `status`, from
  `local-tasks`, metadata `lean_ctx.request_id` = `task:<n>:<event>:<seq>:<i>`),
  the number only, no task text. `submitted` goes to every agent of the
  project except its worker; everything else to all.

## 4. `lean-ctx hive presence --json`

Remote workers seen on the workspace bus (local `presence.json` the relay maintains). No network.

```json
{
  "contract": "local-read/1",
  "kind": "hive.presence",
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
      "address": "#acme/app"
    }
  ]
}
```

- `name`: string or `null` (only when this Instance knows the worker, e.g. its own).
- `kind` ∈ `agent` | `person` (`agent` when the Hive does not say). `display_name`,
  `session_name`, `instance.id`, `instance.name`: what the Hive relayed with the presence
  (optional `presence.update` fields `display_name`, `session_name`, `instance_id`,
  `instance_name`, `kind`; bounded single lines), else `null` — `display_name` falls back to
  `name`. A reader shows display name → role → short id and invents nothing.
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
      "read_by": ["hive-person"]
    }
  ]
}
```

- `to`: agent id, or `null` = broadcast. `thread` = `metadata["hive.thread"]` or `null`.
- `task_id`: the task the message names, or `null`.
- `kind` (QN0, additive): the Queen-network kind (§18a), or `null` for an ordinary message,
  for a message stored before QN0 and for a kind this reader does not know.
- `remote_address`: for a message another machine's worker delivered through Hive, the bus address it came from (`#ws/vault`); `null` for local messages.
- `priority` ∈ `low` | `normal` | `high` | `critical`.
- `withheld` (3.12.3, optional): the number of messages the release point withheld from this
  answer. Present only when > 0, so a document with nothing withheld is unchanged. A count,
  carrying no message content. Messages are released one by one; under `--all` each under its
  own project's policy (a message without a project, or of a project no longer present, under
  the reader's). A message that trips the release is left out on its own; the others are
  returned. The CLI writes the content-free notice `[lean-ctx gateway: N record(s) withheld]`
  to stderr.

## 5b. `lean-ctx agent post --json (--to ID … | --broadcast) [--thread T] [--project-root P] [--priority P] [--category C] [--expires-in-hours H] [--task-id T] [--kind K] [--as ID] --message TEXT`

Posted as the person (`from` = `"hive-person"`, same as Desktop today) unless `--as` (from
that agent's own session, §11) names
a sender, one entry per recipient, 12 h lifetime by default; recipients read it on the bus
(read_by = ["hive-person"]). The Instance takes the registry lock.

```json
{
  "contract": "local-read/1",
  "kind": "agent.post",
  "ids": ["m_1…", "m_2…"],
  "from": "hive-person",
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

## 6. `lean-ctx hive status --json --contract local-read/1`

Only with `--contract local-read/1`; plain `--json` stays the cockpit shape. All existing fields unchanged (`available`, `connected`, `instance`, `org`, `last_seen`, `url`,
`drafts`, `filter_packs`, `policy_refusal`, `web_url`, optional `bus_controls`,
`bus_controls_inactive`, `metrics_v2`), plus:

```json
{
  "contract": "local-read/1",
  "kind": "hive.status",
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
- `trial_ends_at`: the lease does not carry it today → always `null` until it does.
- `capture`: cumulative capture-gate counts on this Instance, or `null` when none recorded.

Also printed (`[--project-root P]` picks the project, default from the cwd):

```json
{
  "enabled": true,
  "problem": null,
  "workspace": { "org_id": "org_1", "name": "Acme" },
  "project": { "root": "/Users/me/code/app", "vault": "acme/app", "linked": true },
  "capture_gate": { "enabled": true, "version": 1 },
  "trial": { "active": false, "ends_at": null },
  "seats_limit": 2,
  "org_settings": { "metrics_upload": true, "receipts_upload": true },
  "receipts": { "queued": 0, "off": null },
  "workers": { "active": 1, "retiring": 0 }
}
```

- `enabled`: `[hive] enabled` (`false` pauses all Hive traffic). `problem`: why the stored
  enrollment cannot be used, or `null`.
- `workspace`, `seats_limit`, `org_settings`: `null` without a lease (`org_settings` also `null`
  when the lease does not carry it). `workspace.name` from the enrollment, or `null`.
- `project`: `null` without a project: the socket route without `project_root`, or with one
  that is no project (`~`, `/`, a missing path), and the CLI when the project from the cwd is
  `~` or `/`. Every other field is printed as with a project; nothing is invented.
- `project.vault`: `ns/name` of the vault linked to the checkout, `null` (and `linked: false`) otherwise.
- `capture_gate.enabled`: `[hive] auto_capture`; `version`: the gate rules version.
- `trial.active`, `trial.ends_at`: `null` until the lease carries trial state (a Personal plan is not a trial).
- `receipts.off`: why receipts are not sent, or `null`. `workers`: durable workers in the book / retiring.

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

A launching app (LeanCTX | Hive Desktop) names each agent session it starts:

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
  → `{"kind": "bus.read", "bus": "agent-bus/1", "reader": "hive-person", "marked": true, "messages": [row…]}`.
  Unread messages of reader ID in the project: addressed to it or broadcast, not from it, not
  read by it. `--mark` records ID as their reader.
- `lean-ctx bus post --json (--to ID … | --broadcast) --message TEXT [--thread T] [--project-root P] [--priority low|normal|high|critical] [--category C] [--expires-in-hours H] [--task-id T] [--kind K] [--as ID]`
  → `{"kind": "bus.post", "bus": "agent-bus/1", "ids": ["m_1"], "from": "hive-person", "to": [], "thread": "t1", "task_id": null, "message_kind": null, "priority": "normal", "category": "direct", "timestamp": "…", "expires_at": "…", "sender": {"kind": "person", "id": "hive-person", "display_name": null}, "cloud": {"state": "local_only", "reason": "not_vault_address"}}`.
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
  handshake, `null` until identified. `instance`: this machine — the Hive Instance id when
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
    "hive.tasks": [ { "verb": "list", "via": ["cli", "socket"] }, { "verb": "claim", "via": ["cli"] } ]
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
  (`hive.tasks` = `lean-ctx hive tasks`) → EVERY verb it supports, each with `via`, the
  transports that answer it (`cli`, `socket`; in that order). A verb that is a flag is named
  without its dashes (`gain`: `daily`, `by-project`). Order: the CLI's verbs, then verbs only
  the socket serves (sorted); command paths only the socket serves come last (sorted). A
  missing key, verb or transport is not supported — readers hide the action instead of
  reading `--help`. The CLI and the socket print the same document.
- `proxy` is configuration, not a probe: `GET base_url + health_path` answers 200 with
  `"service":"lean-ctx-proxy"` when the proxy is up.
- `server`: the `/v1/capabilities` document, unchanged.
- `routes` (`[{kind, method, path, params, project, cli, command?, verb?}]`, sorted by path
  then method) and `events` (`{path, types}`) describe the local service
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

`bus post --to "#ws/vault" …` is the person's post (sender `hive-person`). `cloud` in its
answer:

- `state` ∈ `queued` (goes to the vault bus) | `local_only` (stays on this machine) |
  `withheld` (the contract keeps it here) | `held` (waits under a hold, reason
  `bus_on_hold`: a Hive hold or stop on the whole bus, or the control replay after a damaged
  relay state; it goes once the hold ends). `reason`: `null` | `awaiting_hive` (queued; the
  Hive has not accepted a person's publish on the last connection yet) |
  `not_vault_address` | `not_enrolled` | `unknown_vault` (a vault this Instance does not
  follow) | `vault_denied` (withheld: Hive denied this Instance the vault's bus) |
  `type_unsupported` (withheld: answers, findings, offers and handoffs need their own
  fields) | `screened` (withheld: the relay's screen blocks the text). Decided as the relay
  decides.
- The relay publishes it as `place.publish {schema, client_msg_id, to, type, body,
  sender: {"kind": "person"}}` without `worker_id`, only once the Hive's `hello.ack` lists
  the feature `person_publish` on the current connection (a new connection waits for its
  own `hello.ack`). The Hive attributes it to the account that owns this
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
- **Stored:** the kind in the row's metadata `hive.kind`, the thread in `hive.thread`, the
  task in the row's `task_id`; rows read them as `kind`, `thread`, `task_id` (§5a).
- **Rate limit:** at most 20 posts per sender per minute, with or without a kind, on the CLI
  and in `ctx_agent post`, at posting (`rate_limited`) and
  again per worker at the relay (an excess post waits for the next minute; it is never
  dropped).
- **Relay (HN0):** of this Instance's agents only the registered Queen's posts are
  published to a vault's Hive bus; every other worker's post is decided local (one carrying
  a kind counts as withheld). A person's own post keeps §18.
- **Frame:** a post is published as `place.publish` with the top-level fields `kind`,
  `task_id` and `thread`, each independently when set and well formed (a task or thread
  travels without a kind too), next to the C0 2.0 publish fields; a post with none of them
  is published without them. A delivered
  `message.deliver` that carries them lands on the local row with the same fields; a
  malformed one is dropped field by field, never the message. The Hive must accept the three
  fields on `place.publish` and refuses a kind from an id that is no registered Queen
  (`not_queen`); that check is Hive Cloud's.
- **Presence:** each `hello.presence` entry and `presence.update` frame also carries
  `display_name` (the worker's agent name), `instance_id` and `instance_name` (this
  Instance's id and enrollment label), each only when known; display only, never used to
  address or authorize. The Hive reads the C0 2.0 fields and ignores the rest.

---

## 19. `lean-ctx hive vaults --json [--project-root P]`

The vaults of the connected workspace this Instance knows, for choosing where to share a
project. Local state only: no request leaves the machine. Hive has no Instance route that lists
a workspace's vaults (the full list is on the web), so `complete` is always `false`: the rows are
the vaults this Instance has shared with plus the vault the project's link file names.
`not_connected` when this Instance is not connected.

```json
{
  "contract": "local-read/1",
  "kind": "hive.vaults",
  "project_root": "/Users/me/code/api",
  "workspace": { "id": "org_a", "name": "Platform", "label": "Team Platform", "kind": "team" },
  "complete": false,
  "web_url": "https://hive.leanctx.com",
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

- Sorted by `address` (`<ws>/<vault>[@<server>]`, what `hive link --vault` takes).
- `project_root`: `null` outside a project. `workspace.kind`: `personal` | `team` | `null`.
- `vault_id`: `null` for a vault only the link file names (not resolved yet).
- `role`, `private`: not known to an Instance, always `null` in local-read/1.
- `current`: the project is linked or mapped to this vault; a row only the link file names
  (`sources: ["link_file"]`, `vault_id: null`) is `current` before the first sync.
- `sources` ⊆ `shared` (this Instance shared with it) | `link_file`.

## 20. `lean-ctx hive link --vault V --json [--project-root P] [--dry-run] [--bring-local] [--leave-unsettled-local]`

Links the project to a vault and, on a connected Instance, maps it right away the way the next
sync would, without a prompt. `V` is an address `<ws>/<vault>[@<server>]`, or the `vault_id` or
name of a vault from section 19 (`not_found` when unknown, `invalid_argument` when a name is
ambiguous). Writes the link file `.lean-ctx-hive.json` (and `.lean-ctx-id` when the project has
no identity); `--dry-run` writes nothing and contacts nobody. The consent rules are unchanged: a
first share that waits for the person's yes comes back as `consent_required`; nothing here
approves. Creating a vault under a chosen name is not offered: Hive creates a project's own
vault (named after its folder) only through the first-share consent or `hive connect`.

```json
{
  "contract": "local-read/1",
  "kind": "hive.link",
  "project_root": "/Users/me/code/api",
  "vault": { "address": "platform/api", "ns": "platform", "name": "api" },
  "previous_vault": null,
  "dry_run": false,
  "link_file": "/Users/me/code/api/.lean-ctx-hive.json",
  "created_identity": false,
  "share": {
    "state": "consent_required",
    "vault_id": null,
    "consent_id": "cns_…",
    "answer_web": "lean-ctx hive consent --web cns_…",
    "answer_terminal": "lean-ctx hive consent approve cns_…",
    "message": "the first share waits for the person's yes"
  },
  "unsettled_local": [],
  "bring_local": null
}
```

**Unsettled local tasks refuse every link.** Once linked, a project's tasks are Hive's, so a
local task that is only proposed, claimed or submitted (and not brought yet) would be stranded.
While there is one, every path that links a project refuses and writes nothing: this command
(with or without `--bring-local`) answers error `conflict` naming them (`#3 (proposed), #5
(submitted)`), and the terminal `lean-ctx hive link <ws>/<vault>` refuses the same way. The
person settles them locally (decide, i.e. accept or decline; submit and verify, or release) and links again,
or links with `--leave-unsettled-local`: then they stay local, `unsettled_local`
(`[{ "local_number", "title", "state" }]`, else `[]`) names them, and they keep working with
`lean-ctx tasks … --local` (§3c). A dry run names them in `unsettled_local` and links nothing.
A link file that arrives through git cannot be refused: its local tasks that were not brought
stay reachable the same way and are listed as `local_not_shared` (§3a).

- `share.state` ∈ `not_checked` (dry run) | `not_connected` (linked locally; shared once the
  Instance connects) | `other_hive` (the address names another Hive than this Instance's) |
  `shared` (`vault_id` set) | `consent_required` | `declined` | `waiting` (a consent in another
  state, see `message`) | `pending` (Hive unreachable; the background sync retries) | `refused`.
- `answer_web` runs the browser flow: it opens the consent page, prints its URL and waits up to
  10 minutes for the answer. The URL exists only once that command asked Hive for it.
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
  - `tasks[].action` ∈ `propose` | `skip`. Only an **open** local task is brought: Hive opens
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
    config; Hive's number is recorded as soon as it answers. A link interrupted after Hive
    answered only marks the task on the next run, never sends it again; a send Hive never
    answered is repeated with the same key within 6 days (Hive replays the first answer for 7)
    and after that is not sent again: `error` then asks the person to look for
    `Local task #<n>` in the vault. An entry for another vault (a re-link) is not reused. A task
    refused for good (screened text, an invalid request, a 4xx from Hive) leaves the ledger: no
    vault task exists, so `error` says it was not created and the next link sends it again
    once it is changed locally. `error` (else `null`) says why a task did not finish this
    time.
  - `docs[].action` ∈ `import` | `skip`; `reason` ∈ `already_shared` (brought by an earlier link;
    `vault_page_id` is where it went) | `exists_in_vault` (the vault has a page with that slug;
    left alone). Links between brought pages point at their new vault pages, links to a slug the
    vault holds point at that page. Each brought page is marked shared in the local store
    (journaled, by the person, labelled "hive link"), so a second run skips it.
  - The pages reach Hive with the next push; the background sync retries what fails.

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
  - The stored *to verify* mark is what `knowledge list`, AGENTS.md and Hive read. It follows
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

- **Edge kinds**: only the Hive protocol's.
  - Recorded relations are printed as stored: `depends_on` | `related_to` | `supports` | `contradicts` | `supersedes`.
  - Derived edges are computed on every read and never stored:
    - `supersedes` (an update of a key → the version it replaced),
    - `contradicts` (current facts on one key that disagree, newer → older),
    - `applies_to` (fact → anchored code),
    - `contains` (file → symbol).
- **Node ids**:
  - facts carry the `knowledge list` ids (`<category>/<key>`, vault knowledge `hive/<address>/<node>`);
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

## 23. Policy and sync: `hive sync status`, `hive policy sessions|session`

Read only, from local files; nothing contacts Hive. Served on the local service
(Local Service v1 §2) and listed in capabilities (`hive.sync`: `status`;
`hive.policy`: `sessions`, `session`).

Timestamps are RFC 3339 strings. The examples show one possible document; any field marked
nullable below may be `null` or carry a value.

**Two id spaces.** `hive.sync` `vaults[].vault_id` is the vault's *place id* (the id the
replica and the sync status use). `policy.sessions` `installed.vaults[].vault_id` is the
*raw vault id* of the vault map and of the policy's vault-level entries (the id enforcement
matches on). They are different strings for the same vault: do not join the two documents on
`vault_id`; join on `roots` or on `@ns/name` (`vault` in `hive.sync`, `ns` + `name` here).

### 23.1 `hive.sync` (`GET /v1/local/hive/sync`, CLI `hive sync status --json`)

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
  the replica for Hive; `stored` / `pushed`: counts of the last successful sync; `failures`:
  failures in a row; `error`: the last error (`null` after a success); `next_attempt_at`: the
  backoff's next try (`null`: no backoff).
- `rejected[]`: facts the capture gate kept on this machine for that vault (at most 200, the
  newest), still waiting for a fix: `category`, `key`, `reason` (a snake_case reason code,
  e.g. `too_short`, `no_reference`, `sensitivity`), `hint` (how to fix it) and `at`. Fix the
  fact and remember it again; the next sync shares it.

### 23.2 `policy.sessions` (`GET /v1/local/hive/policy/sessions`, CLI `hive policy sessions --json`)

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

- `installed`: the signed policy installed here; **`null` when no Hive policy is installed**.
  - `version`, `digest`: the installed policy (`null` when `signature` is `invalid`).
  - `key_id`: the signing key of the effective policy (`null` when not known for this
    version, or `invalid`).
  - `signature`: `verified` (installed after its signature verified) | `invalid` (the
    installed files are damaged or older than required: fails closed, sensitive operations are
    refused).
  - `state`: `in_force` | `expired` (past its `not_after`: sensitive operations refused) |
    `none` (with `signature: "invalid"`).
  - `acked`, `acked_at`: whether this Instance acknowledged this version and digest to Hive,
    and when (`null`: not acknowledged).
  - `pulled_via`: what started the last policy pull: `nudge` (a bus hint) | `poll` (the
    periodic pull) | `manual` (`lean-ctx hive sync`); `null`: no pull recorded.
    `last_pull_at`: when (`null`: never).
  - `nudges`: Hive sends policy nudges to this Instance, so a change applies at once;
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
    - `acknowledged_at`: when Hive acknowledged `applied_version` (`null` with it).
    - `sessions`: the live app sessions enforcing the installed policy, sorted: those whose
      newest receipt (within the last 15 minutes) was made under this version and digest,
      verified and in force. Empty for an expired or invalid policy.
    - `reported_at`: when this Instance reported `applied_version` to Hive on the bus
      (frame `policy.applied`, once per version and digest, sent only when Hive's
      `hello.ack.features` names `policy_applied`); `null` until then.
- `sessions[]`, sorted by `session_id`: one row per app session with recorded evidence
  (at most 256 sessions, the most recently written). The row names the policy of the
  session's newest receipt: `session_id`, `version`, `digest`, `signature`, `state` (as in
  `installed`, at the time of that receipt), `receipts` (the session's receipts over all its
  applied versions) and `last_at` (its newest receipt).

### 23.3 `policy.session` (`GET /v1/local/hive/policy/sessions/{id}`, CLI `hive policy session <id> --json`)

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

- Hive's "waiting for you" items are not served yet: Hive Cloud answers
  `/api/v1/me/waiting` only for a person or a personal token, not for an Instance.

### 23.5 Pending: what Hive Cloud must add for `/v1/local/hive/waiting`

- **Today:**
  - `GET /api/v1/me/waiting` (leanctx-hive `crates/hive-server/src/routes/me.rs`, `waiting`)
    takes the `Caller` extractor (`crates/hive-server/src/principal.rs:709`). It accepts only
    `Principal::Human` and `Principal::Token`, and rejects `Principal::Instance`, so an
    Instance credential cannot read it.
  - `POST /api/v1/me/waiting/{id}/snooze` takes `Human` only.
- **Hive Cloud must add** an Instance read route (e.g. `GET /v1/instance/waiting`), or accept
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
- **Then the engine adds** `GET /v1/local/hive/waiting` (kind `hive.waiting`, CLI
  `hive waiting --json`), a released relay of that route, listed in capabilities.
