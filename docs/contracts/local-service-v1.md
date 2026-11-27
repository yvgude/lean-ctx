# Local Service v1

Status: **experimental** (routes answer with `"contract": "local-read/1"` documents).

Routes keep their 3.12 paths (`/v1/local/cloud/*`) in this contract version; see
the wire id note in [Local Read v1](local-read-v1.md).

The local LeanCTX service (`lean-ctx serve --daemon`) is the channel of a
local reader such as LeanCTX Code: every [Local Read v1](local-read-v1.md)
kind as an HTTP route of the daemon socket, tool calls, live events and a
capabilities document listing all of them. The CLI (`lean-ctx … --json`)
stays the fallback and prints the same bytes.

## 1. Socket, discovery and lifecycle

- **Endpoint:** `<data dir>/daemon.sock` (Unix; the data dir is
  `LEAN_CTX_DATA_DIR` or the LeanCTX data dir that also holds `daemon.pid`).
  Windows: a named pipe derived from the same dir, owner-only DACL.
- **Discovery without a shell:** `lean-ctx serve --print-socket` prints the
  path (one line). `lean-ctx serve --print-socket --json` prints
  `{"contract":"local-read/1","kind":"service.socket","socket":"…","listening":true,"pid":1234}`
  (`pid` nullable). Neither starts anything.
- **Start on demand:** when `listening` is `false`, run `lean-ctx serve --daemon`
  (exit 0 = started; exit 1 with "Daemon already running" = another reader was
  faster) and connect once the socket answers.
- **Idle:** the daemon is the per-user singleton (scheduler, LeanCTX Cloud bus relay)
  and does not exit when idle.
- **Stop and control are scoped to the data dir:** `lean-ctx serve --stop`,
  `daemon stop|restart|status` and `serve --status` resolve the daemon only
  through the active data dir's `daemon.pid`, identity file and `daemon.sock`.
  They never use a default path, a process name or a service name. When no
  daemon runs for that dir, `serve --stop` and `daemon stop` print
  `No lean-ctx daemon is running for data dir <dir>. Nothing was stopped.`,
  exit 1 and signal nothing. The autostart service (systemd user unit
  `lean-ctx-daemon`, LaunchAgent `com.leanctx.daemon`) belongs to the
  default install (the account's own HOME, no `LEAN_CTX_*_DIR` override). It
  is stopped only when it runs exactly the PID of the data dir's daemon. An
  instance with a directory override or a sandbox HOME never stops, starts,
  installs or removes it.
- **Access:** no token. The socket is bound 0600 (on Unix inside a private
  staging directory, then moved into place), so only the owning user can
  connect. Plain HTTP/1.1 over the socket. The `/v1/local/*` routes are served
  on the daemon socket only, never by `lean-ctx serve` on TCP.
- **Compatibility:** read `GET /v1/local/capabilities`; a route that is not
  listed, a 405, or a 404 **without** a Local Read document means the route is
  not offered by this Instance — use the CLI. (A 404 that carries a Local Read
  error document, `not_found` or `no_vault`, is an answer of the route.)
  Within `local-read/1` changes are additive (readers ignore unknown fields).

## 2. Routes

Every body is a Local Read document, exactly the bytes the CLI prints for the
same arguments (the document and a final `\n`), `Content-Type: application/json`.

| Method | Path | Kind | CLI |
|---|---|---|---|
| GET | `/v1/local/capabilities` | `capabilities` | — (see §4) |
| GET | `/v1/local/cloud/docs` | `docs.list` | `cloud docs list --json` |
| GET | `/v1/local/cloud/docs/{id}` | `docs.page` | `cloud docs show <id> --json` |
| GET | `/v1/local/cloud/tasks` | `tasks.list` | `cloud tasks list --json` |
| GET | `/v1/local/cloud/tasks/{n}` | `tasks.task` | `cloud tasks show <n> --json` |
| POST | `/v1/local/cloud/tasks/{n}/labels` | `tasks.ack` | `cloud tasks labels <n> --json` |
| POST | `/v1/local/cloud/tasks/{n}/priority` | `tasks.ack` | `cloud tasks priority <n> --json` |
| GET | `/v1/local/cloud/sync` | `cloud.sync` | `cloud sync status --json` |
| GET | `/v1/local/cloud/policy/sessions` | `policy.sessions` | `cloud policy sessions --json` |
| GET | `/v1/local/cloud/policy/sessions/{id}` | `policy.session` | `cloud policy session <id> --json` |
| POST | `/v1/local/cloud/lease/refresh` | `cloud.lease.refresh` | `cloud lease refresh --json` |
| GET | `/v1/local/cloud/decisions` | `decisions.list` | `cloud decisions list --json` |
| POST | `/v1/local/cloud/decisions/create` | `decisions.create` | `cloud decisions create --json` |
| POST | `/v1/local/cloud/decisions/send` | `decisions.send` | (none: app-only, `x-leanctx-app-key`; optional `updated_at`) |
| POST | `/v1/local/cloud/decisions/supersede` | `decisions.supersede` | (none: app-only, `x-leanctx-app-key`) |
| POST | `/v1/local/cloud/decisions/revoke` | `decisions.revoke` | (none: app-only, `x-leanctx-app-key`) |
| GET | `/v1/local/cloud/pickup` | `cloud.pickup` | (none: app-only, `x-leanctx-app-key`) |
| POST | `/v1/local/cloud/pickup/set` | `cloud.pickup.set` | (none: app-only, `x-leanctx-app-key`) |
| POST | `/v1/local/cloud/pickup/key` | `cloud.pickup.key` | (none: app-only, `x-leanctx-app-key`) |
| GET | `/v1/local/cloud/members` | `cloud.members.list` | `cloud members list --json` |
| POST | `/v1/local/cloud/billing/checkout` | `cloud.billing.checkout` | `cloud billing checkout --json` |
| POST | `/v1/local/cloud/billing/portal` | `cloud.billing.portal` | `cloud billing portal --json` |
| GET | `/v1/local/cloud/billing/credit` | `cloud.billing.credit` | `cloud billing credit --json` |
| GET | `/v1/local/cloud/models/catalog` | `cloud.models.catalog` | `cloud models catalog --json` |
| POST | `/v1/local/cloud/billing/topup` | `cloud.billing.topup` | `cloud billing topup --json` |
| GET | `/v1/local/cloud/computers` | `cloud.computers` | `cloud computer list --json` |
| POST | `/v1/local/cloud/computers` | `cloud.computer.ack` | `cloud computer create --json` (body `name`, `role`, `team`, `personal`, `request_id`, `yes`; no size, region or idle stop) |
| POST | `/v1/local/cloud/computers/{id}/start` | `cloud.computer.ack` | `cloud computer start <id> --json` (body `request_id`, `yes`) |
| POST | `/v1/local/cloud/computers/{id}/stop` | `cloud.computer.ack` | `cloud computer stop <id> --json` (body `request_id`) |
| GET | `/v1/local/knowledge` | `knowledge.list` | `knowledge list --json` |
| GET | `/v1/local/chat/channels` | `chat.channels` | `cloud chat channels --json` |
| GET | `/v1/local/chat/channels/{id}/messages` | `chat.messages` | `cloud chat messages <id> --json` |
| GET | `/v1/local/chat/replies` | `chat.replies` | `cloud chat replies --json` |
| POST | `/v1/local/chat/post` | `chat.post` | `cloud chat post --json` |
| POST | `/v1/local/chat/read` | `chat.read` | `cloud chat read --json` |
| POST | `/v1/local/chat/dm` | `chat.dm` | `cloud chat dm --json` |
| GET | `/v1/local/chat/mentions` | `chat.mentions` | `cloud chat mentions --json` |
| GET | `/v1/local/messages` | `agent.messages` | `agent messages --json` |
| POST | `/v1/local/messages` | `agent.post` | `agent post --json` |
| GET | `/v1/local/presence` | `cloud.presence` | `cloud presence --json` |
| GET | `/v1/local/status` | `cloud.status` | `cloud status --json --contract local-read/1` |
| GET | `/v1/local/activity` | `activity.list` | `activity list --json` |
| GET | `/v1/local/session/stats` | `session.stats` | `session stats --json` |
| GET | `/v1/local/session/cost` | `session.cost` | `session cost --json` |
| GET | `/v1/local/session/usage` | `session.usage` | `session usage --json` ([session-usage-v1](session-usage-v1.md)) |
| GET | `/v1/local/gain/daily` | `gain.daily` | `gain --daily --json` |
| GET | `/v1/local/gain/projects` | `gain.projects` | `gain --by-project --json` |
| GET | `/v1/local/spend/projects` | `spend.projects` | `spend --by-project --json` |
| GET | `/v1/local/bus` | `bus.list` | `bus list --json` |
| POST | `/v1/local/bus/read` | `bus.read` | `bus read --json` |
| POST | `/v1/local/bus/post` | `bus.post` | `bus post --json` |
| GET | `/v1/local/bus/leases` | `bus.leases` | `bus leases --json` |
| POST | `/v1/local/bus/claim` | `bus.claim` | `bus claim --json` |
| POST | `/v1/local/bus/release` | `bus.release` | `bus release --json` |
| GET | `/v1/local/presence/agents` | `presence.list` | `presence list --json` |
| GET | `/v1/local/bus/revision` | `bus.revision` | `bus revision --json` |
| GET | `/v1/local/agent/diaries` | `agent.diaries` | `agent diaries --json` |
| GET | `/v1/local/cloud/workers` | `cloud.workers` | `cloud worker list --json` |
| GET | `/v1/local/archive/{id}` | `archive.entry` | `archive show <id> --json` |
| POST | `/v1/local/knowledge/remember` | `knowledge.remembered` | `knowledge remember --json` |
| POST | `/v1/local/knowledge/edit` | `knowledge.edited` | `knowledge edit --json` |
| POST | `/v1/local/knowledge/relate` | `knowledge.related` | `knowledge relate --json` |
| POST | `/v1/local/knowledge/remove` | `knowledge.removed` | `knowledge remove --json` |
| GET | `/v1/local/audit/verify` | `audit.verify` | `audit verify --json` |
| GET | `/v1/local/ledger/verify` | `ledger.verify` | `ledger verify --json` |
| POST | `/v1/local/evidence/export` | `evidence.export` | `evidence-export --json` |
| GET | `/v1/local/providers` | `providers.list` | `provider list --json` |
| GET | `/v1/local/policies` | `policies.list` | `policy list --json` |
| GET | `/v1/local/agents` | `agents.list` | `agents --json` |
| GET | `/v1/local/daemon` | `daemon.status` | `daemon status --json` |
| GET | `/v1/local/queens` | `queens.list` | — (socket only) |
| POST | `/v1/local/queens` | `queens.list` | — (socket only) |
| POST | `/v1/local/queens/remove` | `queens.list` | — (socket only) |

**Queen sessions (3.12.3).** Who sets a task's priority and labels: the person
(LeanCTX Code over this 0600 socket, the person's own CLI) and the Queen
sessions the app started. A bus role (`queen`) is self-declared and grants
nothing. The app registers a Queen session's `LEAN_CTX_SESSION_ID` here, and
`ctx_tasks action=prioritize` is answered only for a session whose MCP server
runs under a registered id; every other caller can read priority and labels
and is refused `forbidden` ("priority and labels are set by a person or a
Queen session the app registered; this session can read them").

- `POST /v1/local/queens` `{"session_id": "…", "name"?: "…"}`: register
  (again: keeps `since`, replaces `name`). `session_id` is 8–128 of
  `[A-Za-z0-9._:-]`, `name` at most 80 characters; at most 64 sessions
  (`forbidden` beyond).
- `POST /v1/local/queens/remove` `{"session_id": "…"}`: the app removes it when
  the session ends (no error when it is not registered).
- Every answer: `{"contract":"local-read/1","kind":"queens.list","queens":[{"session_id","name":string|null,"since":ts}]}`, sorted by `session_id`.
- Stored 0600 in the Instance's data dir (`queen-sessions.json`). There is no
  CLI verb and no MCP tool for it: only a process that talks to the 0600
  socket (the app) registers. The socket does not tell apart processes of the
  same OS user, so this keeps workers from setting priority through their
  tools; it is not a boundary against code that runs as the user.

The vault's docs and tasks live under `/v1/local/cloud/`; `/v1/local/docs`
and `/v1/local/tasks` are kept for the project's local docs and tasks.

- **Inputs:** query parameters (GET) or one JSON object (POST) whose keys are
  the CLI flag names without `--` (`-` → `_`): a repeatable flag takes the key
  repeated or a JSON array, a switch takes `true`/`false`. Each route's keys are
  listed in `capabilities.routes[].params`. An unknown key, or a value that
  starts with `--`, is `invalid_argument`.
- **Project:** the service has no working directory of the reader's.
  `capabilities.routes[].project` is `true`, `false` or `"optional"`.
  `project_root` must be named where it is `true` (`agent.messages` also takes
  `all=true`, every project); it must be an existing project directory inside
  the Workspace when one confines the machine, else `no_project` /
  `forbidden`. Every project-dependent read names it, `bus.leases`,
  `presence.list` and the posts (`agent.post`, `bus.post`: a message always
  belongs to a project) included: the service never falls back to its own
  working directory. Where it is `"optional"` (`cloud.status`), a named
  project is checked the same way, but without `project_root`, or with one
  that is no project (`~`, `/`, a missing path), the route answers its
  folder-less document instead of `no_project` (`cloud.status`: `project`
  `null`, local-read-v1 §6); outside the Workspace stays `forbidden`. A reader
  that knows only `true`/`false` treats `"optional"` as not required.
- **Errors:** the Local Read error document, with the HTTP status of its code:
  `invalid_argument`, `no_project`, `invalid_field` → 400; `plan_required` → 402;
  `not_found`, `no_vault` → 404 (also for an unknown `/v1/local/*` path);
  `forbidden`, `withheld`, `sso_required`, `email_unverified`,
  `policy_denied` → 403; `anchor_gone`, `conflict`,
  `invalid_state` → 409; `rate_limited` → 429; `not_connected`, `unavailable`
  → 503; `not_offered` → 501; `internal` → 500; `balance_empty` → 402;
  `no_computer` → 404; `computer_exists`, `computer_busy` → 409;
  `computer_provider_unavailable` → 503; `computer_not_available` → 503;
  `confirmation_required` → 428; `app_only` → 403;
  `chat_key_missing`, `signature_invalid`, `signature_required` → 403;
  `frame_expired`, `frame_unknown` → 409.
  `plan_required`, `invalid_state` and `invalid_field` (3.12.3) are LeanCTX
  Cloud's own refusals, kept for the reader (team chat, local-read-v1 §28);
  so is `sso_required` (3.12.3, team billing, local-read-v1 §27),
  `email_unverified` (top-up, local-read-v1 §33), and `policy_denied`
  (next release, LeanCTX Cloud's policy refuses it, local-read-v1 §26.2), and so are
  the LeanCTX computer codes (local-read-v1 §44; `confirmation_required`: a
  write that can cost money was asked without `--yes`, so nothing was sent).
  `not_offered` (team stop, local-read-v1 §34): the Cloud does not offer
  the feature; a refusal, never an outage to retry.
- **Posts (QN0):** `POST /v1/local/messages` and `POST /v1/local/bus/post`
  take `task_id`, `category`, `priority` and `expires_in_hours` besides
  their earlier keys; the answers add `task_id` and `message_kind` (always
  `null` here; local-read-v1 §5b, §11, §18a). A socket post is always the
  person's: the routes take neither `as` nor `kind`, and either is refused
  400 `invalid_argument` (`unknown parameter as` / `unknown parameter kind`).
  A Queen-network kind is posted only by the registered Queen, through the
  CLI (`--kind` with `--as` from her own session) or `ctx_agent post`.
  `to` `queen` names the registered Queen (none active → 403 `forbidden`,
  `no_queen: …`); a sender over 20 posts a minute is 503 `unavailable`
  (`rate_limited: …`), retry later.
- **Timeout:** a request that exceeds the service's request timeout (30 s by
  default) answers 503 `unavailable`. For `POST /v1/local/messages` that answer
  is **indeterminate**: the post may still be stored after it; read
  `/v1/local/messages` to find out.
- **Release:** documents carrying project or agent text (`knowledge.list`,
  `docs.*`, `agent.messages`, `activity.list`, and every kind of
  local-read-v1 §4 and §8–§17: `cloud.presence`, sessions, rollups, `bus.*`,
  `presence.list`, knowledge writes, evidence, `providers.list`,
  `policies.list`, `agents.list`, `daemon.status`, `cloud.sync`, `policy.*`) pass the
  same release point (redaction, gateway) as on the CLI. Metadata-only, not
  released: `capabilities` (a function of the binary and its
  configuration), `cloud.status`, `tasks.*` and `agent.post`
  as in Local Service v1. The receipt binds exactly the bytes of the response
  body (document and final `\n`) for the transport channel. When the release
  point masked a value (the masked value stands in its place), the response
  carries the header `x-leanctx-release-notice` with a content-free notice
  (the CLI prints it on stderr). A document withheld as a whole is answered
  with the `withheld` error document (403); that document is what the run
  prepares and receipts — exactly one receipt per answer, for the bytes sent.
  When no receipt can be stored the answer is 503 **without a body**.
- **Tool calls:** `POST /v1/tools/call` with
  `{"name":"ctx_read","arguments":{…},"projectRoot":"/abs/project"}` → `{"result":…}`;
  errors `{"error":msg,"error_code":code}` (`project_root_invalid`/`tool_error` 400,
  `request_timeout` 504). On the socket `projectRoot` runs the call in that
  project's own server.

## 3. Live events — `GET /v1/local/events`

Query: `project_root`, `types` (comma list, default all), `all` (`true`/`false`),
and for a view subscription (§3a) `view`, `limit`, `thread`, `from`, `to`,
`cursor`. An unknown key, an unknown type or another value of `all` is
`invalid_argument` (a Local Read error document). Server-sent events, a
keep-alive comment every 15 s:

```
id: 3
event: tool_call
data: {"contract":"local-read/1","kind":"event.tool_call","at":"…","event_id":81,"tool":"ctx_read","mode":"map","path":"src/a.rs","duration_ms":12,"tokens_original":900,"tokens_saved":610}
```

- **Always send `project_root`.** The journal events (`tool_call`,
  `activity`, `knowledge_changed`) are served through the observation gate of
  the named project. Without one the originless rule applies: under any
  protection (a policy pack, governed or sovereign mode, a configuration that
  cannot be loaded) every journal event is withheld, and without protection
  (Community) the stream carries the events of **every** project on the
  machine. `message` events need `project_root` or `all=true`.
- `id` counts the events of this stream. Without a view the stream is **live
  only**: nothing that happened before it opened is replayed; after a
  reconnect a reader refetches what it shows (`/v1/local/messages?since=<ts>`
  for messages). A view subscription (§3a) resumes with a snapshot instead.
- Every `data` line is one Local Read document. Events that carry free text
  — `message`, `knowledge_changed` and the `activity` kinds `policy_violation`,
  `verification_warning`, `agent_action`, `role_changed`, `profile_changed`
  (and kinds unknown to this list) — are released like a route answer: one
  receipt per event sent, binding exactly the `data` line's JSON and the
  line's final `\n`. A withheld one is sent under its own type with the
  `withheld` error document as data (that is what was receipted); without a
  stored receipt nothing is sent. An event in which a value was masked is
  followed at once by a `release_notice` event when the stream asked for
  that type (it follows `types` like every type). `tool_call` and the
  per-tool-call `activity` kinds `cache_hit`, `compression`,
  `threshold_shift`, `threshold_adapted` carry tool names, modes, admitted
  paths and numbers only and pass the observation gate without a release
  (one release costs about 2 ms per stream under protection).
- The first event is `hello`: `{"project_root":str|null,"all":bool,"types":[…],"view":object|null,"service":{"version":"…","build":{…}}}`; `build` as in local-read-v1 §14, `view` as in §3a.

| type | data (besides `contract`, `kind`) | source |
|---|---|---|
| `tool_call` | `at`, `event_id`, `tool`, `mode` (nullable), `path` (nullable), `duration_ms`, `tokens_original`, `tokens_saved` | the event journal of all LeanCTX processes of this user, served through the observation gate |
| `activity` | `at`, `event_id`, `activity` (`cache_hit`, `compression`, `agent_action`, `policy_violation`, `role_changed`, `profile_changed`, `verification_warning`, `threshold_shift`, `threshold_adapted`; show unknown values as-is), `detail` (object) | same journal; the free-text kinds released |
| `knowledge_changed` | `at`, `event_id`, `project_root` (the stream's, nullable), `route` (`/v1/local/knowledge`), `category`, `key`, `action` | same journal, released |
| `message` | one `agent.messages` row; with a view also `cursor` (§3a) | local agent bus, released |
| `snapshot` | only with a view: `view`, `mode`, `rows`, `cursor`, `truncated` (§3a) | local agent bus, released |
| `release_notice` | `event` (the type of the event just before it), `notice` (content-free) | the release point |
| `agents_changed` | `active` (live local agents of the project) | local agent registry |
| `presence_changed` | `route` (`/v1/local/presence`) | LeanCTX Cloud bus relay presence |

Bounded: each stream looks at its sources once per second (the newest 32
journal events; the registry and the presence map only when their file
changed); at most 16 streams at once (the next gets 503 `unavailable`). More
than 32 journal events within one second are not all delivered: the stream
is a live view, not a counter (use the routes for totals).

### 3a. View subscriptions and snapshot resume

A view subscription binds a stream to what one screen shows: a bounded set
of rows, filtered by the service, and resumed after a reconnect from a
cursor instead of a full reload. The one view is `messages` (the rows of
`agent.messages`, the person's bus console).

- **Query:** `view=messages` with `project_root` or `all=true` (neither is
  `no_project`). `limit`: rows the view holds, 1–200, default 100.
  `thread`, `from`, `to`: the filters of `agent messages` / `bus list`
  (`--thread`, `--from`, `--to`), applied to the snapshot and to the live
  `message` events alike. `cursor`: the last `cursor` the reader received
  (opaque; a value this service did not write is `invalid_argument`).
  `limit`, `thread`, `from`, `to` and `cursor` without `view`, or another
  `view`, are `invalid_argument`.
- **Hello:** `view` is `{"name":"messages","limit":n,"thread":…,"from":…,"to":…}`
  (filters nullable); `null` without a view.
- **Snapshot:** right after `hello` the stream sends one `snapshot` event,
  released like a `message` (one receipt for its `data` line):
  `{"view":"messages","mode":"full"|"delta","rows":[row…],"cursor":str|null,"truncated":bool}`,
  rows oldest first.
  - `delta` (only when `cursor` was sent and every row newer than it fits
    in `limit`): the rows stamped later than 2 s before the cursor. A row
    may repeat one the reader holds: readers keep rows by `id`. The reader
    adds them to what it shows.
  - `full` (no cursor, or more newer rows than `limit`): the newest `limit`
    rows; the reader **replaces** what it shows. `truncated` is `true` when
    the view holds fewer rows than exist.
  - `cursor`: the resume point after the newest row sent (a `delta` with no
    rows echoes the reader's cursor; an empty `full` is `null`).
- **Live:** each later `message` event carries `cursor`, the resume point
  after that row, in the order sent; the reader stores the last one. When
  more new rows arrive within one poll than `limit`, the stream sends a
  `full` `snapshot` instead of the single events.
- **Removals:** expired messages leave a `full` snapshot; a `delta` and the
  live events do not report them (readers drop rows past `expires_at`).
- **Bounds:** one view per stream; a snapshot holds at most `limit` rows. The
  stream counts against the 16 streams like any other.
- **Compatibility:** additive within `local-read/1`. The App and the Instance
  are versioned independently: a reader subscribes to a view only when
  `capabilities.events.views` lists it, and otherwise keeps the live-only
  stream and refetches after a reconnect (the behaviour before views). An
  Instance without views answers `view` with `invalid_argument` (unknown
  parameter), so a reader that skips the capability check falls back the
  same way.

## 4. Capabilities — `GET /v1/local/capabilities`

```json
{
  "contract": "local-read/1",
  "kind": "capabilities",
  "version": "3.12.1",
  "kinds": ["agent.messages", "agent.post", "capabilities", "docs.list", "docs.page", "error",
            "cloud.presence", "cloud.status", "knowledge.list", "tasks.list", "tasks.task"],
  "commands": {"cloud.tasks": [{"verb": "list", "via": ["cli", "socket"]},
                              {"verb": "show", "via": ["cli", "socket"]}],
               "…": "one entry per command path, see below"},
  "server": {"…": "the /v1/capabilities document"},
  "routes": [{"kind": "knowledge.list", "command": "knowledge", "verb": "list",
              "method": "GET", "path": "/v1/local/knowledge",
              "params": ["project_root", "category", "query", "limit", "include_noise"],
              "project": true, "cli": ["knowledge", "list", "--json"]}],
  "events": {"path": "/v1/local/events",
             "types": ["activity", "agents_changed", "hello", "knowledge_changed", "message",
                       "presence_changed", "release_notice", "snapshot", "tool_call"],
             "views": [{"name": "messages",
                        "params": ["cursor", "from", "limit", "thread", "to"],
                        "default_limit": 100, "max_limit": 200}]}
}
```

`events.views` (§3a) lists the view subscriptions of the stream;
a document without it offers none.

`routes` are sorted by path then method; `kinds` and every verb list are
sorted. A route row of a command verb names it (`command`: the command
path joined by `.`, e.g. `cloud.tasks`; `verb`); `capabilities` itself has
neither. `commands` keys are command paths; each verb
is `{"verb", "via"}`, `via` ⊆ `["cli", "socket"]` in that order.

The document is the CLI's: `lean-ctx capabilities --json` and this route
print the same bytes (one builder, local-read-v1 §14), with every CLI verb —
a verb a route answers carries `socket` in `via`, the others `cli` only, and
a missing verb is not supported — and the session fields (`session_env`,
`proxy_session_header`, `hooks`, `proxy`). The document is a function of the
binary and its configuration (no time, no state). A reader decides which
kinds and verbs exist from it, never from `--help`.
