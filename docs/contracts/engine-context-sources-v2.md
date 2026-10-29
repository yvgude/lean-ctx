# Engine context sources v2 — presenter capability

Status: additive, opt-in. `engine-context-sources-v1.md` and every v1 type are unchanged;
a caller that does not use the operations below sees exactly the v1 behaviour.

Schema ids: `leanctx.engine-context-materialization/v2`,
`leanctx.engine-presenter-revocation/v1`. First Engine release: 3.11.0
(`ENGINE_PRESENTER_CAPABILITY_MIN_ENGINE_VERSION`).

## What a presenter capability is — and is not

A caller that hands sources to the Engine may ask for a **presenter capability**: 32
random bytes, wire form `lcp1_` followed by 64 lowercase hex characters. A later request
that shows it is treated as coming from **the holder of the capability under which those
sources were presented**. That is the whole claim.

It is **not** an agent name, **not** an attested identity and **not** an authorization.
The Engine does not learn who the holder is. At every use the Engine still checks, as
without a capability: the project, the OS user, the role, the current policy identity,
the ten-minute freshness of the presentation, and the admission of every file source of
the same text. A valid capability answers only "was this source presented by this holder,
and recently".

Records name a presenter by its **presenter reference** only: `presenter:` followed by
32 lowercase hex characters, a digest of a stored verifier (never of the capability). A
reference reveals no secret and proves nothing by itself.

## The capability is host-internal

A host must keep the capability in its own memory and show it only through the three
transports below. It must never be placed in model context, a tool argument, a tool
result, a log, a receipt or a request body, and never be sent to a provider. The Engine
never writes it to its store, a log, a receipt or a tool result; the store holds one-way
verifiers.

| Where the host shows it | How |
|---|---|
| Engine CLI | `--presenter-capability-file <path>`: a regular file, not a symlink, at most 128 bytes, on Unix owned by the calling user with mode `0600`. Never an argument value, never an environment variable |
| LeanCTX proxy | request header `x-leanctx-presenter`. The proxy removes it before handling the request and never forwards it. Nothing in a request body is read as a capability |
| MCP `tools/call` | `_meta` key `leanctx/presenterCapability`, set by the host's MCP client. Tool `arguments` are never searched for a capability |

A host that shows nothing is served as before v2 existed for it: it reuses only what the
serving process itself presented. Whether a given third-party host can set a header or an
`_meta` key is outside this contract.

Limits of the three transports, stated:

- **Capability file.** On every platform the path must name a regular file and not a
  symlink. On Unix the open refuses a symlink and the file must be owned by the calling
  user with mode `0600`. **On Windows ownership and access rights are not checked**, and
  the check of the path and the open are not atomic; a host on Windows has to place the
  file where only its own user can write.
- **Proxy.** The header is taken as request state by the HTTP forward path and by the
  Responses WebSocket (shown on the upgrade, valid for every turn of the connection). It
  is not consulted — such a request reuses nothing presented elsewhere — on:
  `POST /v1/compress`, `GET /v1/references/{id}`, the ChatGPT backend passthrough (HTTP
  and WebSocket), and provider paths that do not go through the forward path. On every
  path it is never forwarded: the forward path and the Responses WebSocket forward by
  allowlist, and the two ChatGPT passthrough relays — which pass on every header they do
  not name — drop every header whose name starts with `x-leanctx-` (any letter case), so
  no header of the proxy's own namespace, present or future, reaches that upstream.
- **MCP.** The MCP library used by the Engine logs whole inbound messages below `INFO`.
  The Engine therefore caps every `rmcp` log target at `INFO` in all its process modes,
  whatever `RUST_LOG` / `LEAN_CTX_LOG` say. Cost: `rmcp` debug and trace logs are not
  available from any lean-ctx process. A host's own MCP client logging is the host's
  responsibility.

Decision receipts (`ContextDecisionReceiptV1`) of a call that showed a live issued
capability name the presenter as a principal of kind `session` whose id is the presenter
reference — "the holder of this session capability" — and never as kind `agent`.

## `context-materialize-sources-v2`

`lean-ctx engine context-materialize-sources-v2 --project-root <dir> --json-file <file|->
[--presenter-capability-file <path>]`

Request `EngineContextSourceMaterializationRequestV2`:

| Field | Type | Meaning |
|---|---|---|
| `schema_version` | `2` | |
| `materialization` | `EngineContextSourceMaterializationRequestV1` | unchanged |
| `presenter` | `{"mode":"issue","ttl_seconds":N}` or `{"mode":"renew"}` | `N` from 1 to 86400 |

- `issue`: no capability file. The Engine mints a capability, materializes, records the
  presentations under it and returns it once.
- `renew`: the capability file is required. The capability must have been issued and be
  neither expired nor revoked. It is not returned again; its lifetime is not extended.

Response `EngineContextSourceMaterializationResponseV2`:

| Field | Type | Meaning |
|---|---|---|
| `schema_version` | `2` | |
| `materialization` | `EngineContextSourceMaterializationResponseV1` | unchanged; never contains the capability |
| `presenter.presenter_ref` | string | presenter reference |
| `presenter.expires_at_unix` | integer | end of the capability's lifetime |
| `presenter.capability` | string, `issue` only | the capability |

Errors are the v1 codes, plus `presenter_capability_refused`: one code for a capability
file that is missing where required, present where none belongs, unusable, or holds a
capability that is unknown, expired or revoked. A failed `issue` returns no capability.

## `context-presenter-revoke`

`lean-ctx engine context-presenter-revoke --project-root <dir> --json-file <file|->
--presenter-capability-file <path>`

Request `{"schema_version":1}`; response `{"schema_version":1,"presenter_ref":…,
"revoked":true|false}` (`false`: no such presenter was on record). A revoked capability
presents nothing and reuses nothing.

## `context-presented-use`

`lean-ctx engine context-presented-use --project-root <dir> --json-file <file|->
--presenter-capability-file <path>`

The one-shot consumer of a presented lineage: what the holder of a capability may do
LATER with text that was delivered under it. It runs the same register and recovery
functions as the proxy and `ctx_expand`, for the capability in the file; its one addition
is the source constraint below. Schema id `leanctx.engine-presented-use/v1`; the wire
types are `EnginePresentedUseRequestV1`, `EnginePresentedUseV1` and
`EnginePresentedUseResponseV1` of the protocol crate (module `engine_presented_use`,
additive; the v1 and v2 materialization types are unchanged). A text that is empty or
larger than one materialized context, and a recovery id that is not 64 lowercase hex
characters, are `invalid_presented_use_request`.

Request `{"schema_version":1,"authorized_source_ids"?:[<id>…],"use":<one of>}`:

| `use` | Asks | A usable answer carries |
|---|---|---|
| `{"mode":"check","content":<text>}` | may this exact delivered text still be used by this holder now | `source_ids` |
| `{"mode":"retain","content":<text>}` | `check`, and keep a recovery copy of the text bound to its lineage | `source_ids`, `recovery_id` |
| `{"mode":"expand","recovery_id":<64 hex>}` | hand back the text of a kept recovery copy | `source_ids`, `recovery_id`, `content` |

Response `{"schema_version":1,"presenter_ref":…,"usable":true|false,"reason"?,
"source_ids":[…],"recovery_id"?,"content"?}`. `usable:false` carries a content-free
`reason` (`lineage.unknown`, `lineage.source_not_presented`,
`lineage.source_no_longer_admitted`, `recovery.missing`, `recovery.not_storable`, and the
`recovery.` forms of the lineage reasons), an empty `source_ids` and nothing else.

**Source constraint (`authorized_source_ids`).** The ids of exactly the sources the
sender has authorized this request for: content-free source ids (the protocol's
`SourceId`: 1 to 256 bytes, no control characters), 1 to 64 of them, distinct; order is
irrelevant. Anything else under that name is
`invalid_presented_use_request`.

- With a constraint the Engine answers only for a lineage whose source-id set EQUALS the
  constraint. Equality, not inclusion: a lineage with fewer sources belongs to another
  text than the one the sender authorized, and a lineage with more derives from a source
  nobody authorized for this request. Either is `usable:false` with `lineage.unknown` —
  the same answer as for a text nothing is on record for, in all three modes.
- The comparison comes before current rights are checked and before any mutation.
  **Nothing is stored for a refused request:** a `retain` whose lineage is not exactly
  the constraint leaves no recovery copy.
- Who sends it. The suite always does (the record ids of the governed sources its API
  key holds at that moment; see `POST /v1/engine/presented-use`). A local host that
  starts the Engine itself — the TypeScript `SubprocessEngineClient` is the one such
  caller today — may leave it out; absence means "no additional constraint", and
  capability and lineage alone decide.
- An Engine that does not know the member refuses the request unread
  (`invalid_presented_use_request`); it never answers it unconstrained.

Authority, in this order:

1. The capability file holds an ISSUED capability that is neither expired nor revoked;
   anything else is `presenter_capability_refused` (exit status 2, no output).
2. The lineage on record for the text (or stored with the copy) names caller-supplied
   sources only. A text that also derives from project files is `lineage.unknown` here.
   Under a source constraint the lineage's source ids are exactly the constraint's; any
   other lineage is `lineage.unknown` as well.
3. Current rights, re-checked at this moment under one pinned policy and role: project,
   caller and role of the request, the current policy identity, and a presentation of
   every source by exactly this presenter within the 600 seconds validity. A text
   delivered under another policy is never usable; the same text has to be materialized
   again.
4. For `retain`: the text may enter a store unchanged under the current content rules.
   For `expand`: the stored text still passes them unchanged.

`retain` writes its copy only after steps 1 to 4. `expand` resolves the copy through the
shared recovery function first (steps 3 and 4 for the lineage stored with the copy; the
released text's content-free lineage record is refreshed) and applies step 2 to the
resolved text before any of it is put into the answer: a copy of other sources than the
constraint's returns no text.

What it is and is not:

- The text is referenced by its exact bytes (`check`, `retain`) or by a recovery id.
  The materialization response's `materialized_digest` is not a handle.
- Materialization delivers its text whole and keeps no copy. A recovery copy exists only
  after `retain` (or, next to a LeanCTX proxy on the same store, after the proxy stored
  one). `expand` of an id nothing was retained for is `recovery.missing`.
- `usable:true` says the Engine's lineage authority holds for this holder now. It says
  nothing about the host's own copy, a provider's copy, or who the holder is.
- A request this operation cannot read is `invalid_presented_use_request`; an Engine
  without the operation answers the bare `invalid_request`.

## Reuse

- Several presenters may present the same source. Each has its own entry; one presenter's
  presentation, lapse, expiry or revocation never changes another's.
- At most 16 issued presenters are kept per source at a time. A further one is not
  recorded (it reuses nothing for that source until a slot frees); no live entry is
  removed to make room.
- A presentation is fresh for 600 seconds; a capability lives at most 24 hours. A policy
  change invalidates what was delivered under the old policy, as in v1.
- Every capability failure at reuse is the one existing refusal
  `source_not_presented` (`lineage.` / `recovery.` / `egress.sealed_` prefix by surface).
  It does not say whether a capability was valid for something else.

## Version meeting

| Client | Engine | Result |
|---|---|---|
| v1 only | any | v1 exchange, byte-identical |
| v2 | 3.11.0 or later | v2 exchange |
| v2 | older | the operation is unknown: the bare `invalid_request`. An Engine that serves the operation never answers a well-formed invocation with that code (an unreadable v2 request is `invalid_source_materialization_request`), so a client reports it as "Engine too old" — the TypeScript binding raises `UnsupportedEngineError` |

Store: presenter sets live in `<data dir>/archives/presented-v2`, presenter records in
`<data dir>/archives/presenters`. The `archives/presented` directory of Engines before
3.11.0 is neither read, written nor removed by this Engine; an older Engine on the same
store keeps using it by its own rules, and neither sees the other's presentations.

This transport writes no decision receipt. The presenter reference is returned in the
response and recorded with the lineage of the delivered text and with recovery copies.

## The Enterprise suite hop

A host that reaches materialization through the Enterprise suite uses two additive
routes. `POST /v1/engine/context-materialize` and its response are unchanged.

`POST /v1/engine/context-materialize-v2`

- Request body: the members of the v1 route's body, plus
  `presenter` = `{"mode":"issue","ttl_seconds":N}` or `{"mode":"renew"}`. Unknown
  members are refused, so a capability placed in a body is never read.
- Request header `x-leanctx-presenter`: required for `renew` (exactly one well-formed
  value), refused for `issue`.
- Response body: `{"schema_version":2,"tenant_id":…,"governance_revision":…,
  "materialization":<EngineContextSourceMaterializationResponseV2>}`. A newly issued
  capability is `materialization.presenter.capability` — returned once, to the
  authenticated caller, and nowhere else.

`POST /v1/engine/presenter-revoke` — body `{"schema_version":1}`, the capability in the
same header; response `{"schema_version":1,"tenant_id":…,"revocation":
<EnginePresenterRevocationResponseV1>}`.

What the suite checks, in this order:

1. Its own authority, unchanged and first: API key → tenant → licence → the caller's
   current rights to every requested source and the governance revision; again before
   the Engine's answer is disclosed. A capability replaces none of it.
2. For `renew` and for revocation, **before the Engine is asked**: the capability is
   registered for exactly this API key in this tenant and has not ended.
3. The Engine's own check of the capability, and of the source rights it re-checks at
   every use.

Every capability failure — absent, present where none belongs, malformed, unknown,
expired, revoked, issued to another API key or another tenant — is one answer:
HTTP 403 `presenter_capability_refused`. A suite whose Engine does not serve the
operation answers HTTP 501 `engine_presenter_unsupported`; a suite without the route
answers 404.

The claim over this hop is the same and no larger: the capability proves **the holder of
the capability**, used with the API key it was issued to. It is not an identity of an
agent or a person, and it grants no access to a source.

How the suite holds it:

- **Tenant binding** is by construction: every tenant has its own Engine state directory
  and the Engine keeps its verifiers there, so a capability of tenant A is unknown in
  tenant B's store. The suite's register entry also includes the tenant in its digest.
- **Key binding** is the suite's register `<tenant state dir>/suite-presenters/holders.json`
  (directory `0700`, file `0600`, owner-only): per capability a digest of tenant and
  capability, a digest of the API key id, the presenter reference and the end of its
  lifetime. No capability and no key id in clear. At most 4096 live entries per tenant; a
  full register refuses a new `issue` and never displaces a live holder; an entry ends
  with its capability (at most 24 hours). Changes are read-modify-write under a process
  lock and an advisory file lock, written by rename. An unreadable or malformed register
  refuses every capability until an operator removes it (holders then issue anew).
  This is a file, not a database table: it is not in a database backup, and after a
  restore of the state directory to an older point, capabilities issued since are refused.
- **To the Engine** the capability goes through `--presenter-capability-file`: a `0600`
  file in the tenant's private temporary directory, created for one call and removed
  after it. Never an argument value, never the environment. A suite that is killed
  during a call can leave such a file behind in that `0700` directory.
- **Logs and audit.** The suite's request tracing records method, path, status and
  latency, not headers or bodies; its error log prints an error code and a message the
  handler wrote, never request content. The audit
  records of the route carry a digest over the disclosed materialization and the
  presenter reference — never the capability. A reverse proxy or load balancer in front
  of the suite that logs request headers or response bodies is outside this contract and
  must be configured not to.
- A capability minted for a disclosure that is then refused (rights changed while the
  Engine ran) is revoked and never returned.

### `POST /v1/engine/presented-use` — the consumer

The suite-reachable use of a presented lineage: the Engine's `context-presented-use`
for the tenant's store, behind the suite's own authority.

- Request body `{"schema_version":1,"source_ids":[<uuid>…],
  "expected_governance_revision":N,"use":<check|retain|expand as above>}`; the capability
  in the `x-leanctx-presenter` header (required, exactly one well-formed value).
  `source_ids` are the governed sources the text was materialized from.
- Response body `{"schema_version":1,"tenant_id":…,"governance_revision":N,
  "use":<the Engine's answer>}`.

Authority, every step mandatory and fail-closed, in this order:

1. API key → tenant → licence.
2. This key's CURRENT rights to every named source at the named governance revision
   (a changed revision is "replan required": materialize again).
3. The register: the capability belongs to this API key in this tenant and has not
   ended. Otherwise 403 `presenter_capability_refused`; no Engine process is started.
4. Audit record `requested`, then the rights of step 2 again.
5. The Engine's own checks (the list under `context-presented-use`), with the capability
   in a `0600` file that is removed after the call and with the record ids of the sources
   authorized in step 2 as the request's source constraint (`authorized_source_ids`,
   always sent). A lineage of any other source set is `lineage.unknown` at the Engine,
   before anything is stored: the answer is `usable:false`, audited as `unusable`.
6. The sources of the lineage the Engine names must be EXACTLY the sources authorized in
   step 2 — a caller can neither leave out a source it has lost nor name one the text
   does not derive from. Otherwise 403. Since step 5 this is the second guard, on the
   answer itself; it is kept.
7. Audit record `disclosed` or `unusable`, then the rights of step 2 once more; only
   then the answer.

`retain` — nothing is stored for a request this route refuses. A `retain` makes the
Engine write a copy, so the route runs steps 5 to 7 TWICE for it: first with the Engine
asked to `check` the same text under the same source constraint (no mutation), then
step 6 on that answer and the rights of step 2 again (a second `requested` record, or
`unusable` if the check is — that refusal is then the answer and the Engine is not asked
to store); only after all of that is the Engine asked to `retain`, followed by steps 6
and 7 before the recovery id is disclosed.

What a suite host gets: the answer whether text it holds is still usable; on request a
recovery copy in the tenant's store; and that copy back, for as long as the key's source
rights, the governance revision, the policy and a presentation not older than 600
seconds all hold. `renew` (`context-materialize-v2`) is how a presentation is refreshed.

What it does not get: text it never held or retained; a copy across a policy change or a
governance revision change; a handle from `materialized_digest`; any use by another API
key or another tenant. Nothing is stored for a request the Engine refuses: a `retain`
for a text whose lineage is not exactly the authorized sources is refused by the Engine
before the tenant's archive store is touched. An Engine that does not know the source
constraint refuses the request unread and the suite answers 501
`engine_presenter_unsupported`; it is never asked without the constraint.

Two limits. First, the window that remains for `retain` by construction: the key's
source rights are withdrawn AFTER the route's re-check that precedes the `retain` call
and BEFORE the Engine has written the copy. The final step 7 then refuses the answer
and the copy stays in the tenant's store. It is bounded: the window is the start-up of
one Engine process; the recovery id is not disclosed (step 7); the copy is bound to its
lineage and is expanded only by a later request that passes every step again — this
key's current rights to exactly these sources, the capability, and at the Engine a
current presentation under the current policy plus the source constraint. Second, a
valid capability holder that names exactly the sources of a text learns from
`lineage.unknown` versus `lineage.source_not_presented` whether that exact text is on
record in its tenant's store.

The LeanCTX proxy, the Responses WebSocket, `ctx_expand` and `lean-ctx call` are still
not reachable through the suite; `presented-use` is the suite's consumer.

### Audit

Presenter operations have their own labels on the suite audit chain:

- `context-materialize-v2`: actions `engine.source.request_intent.v1` /
  `engine.source.result_access_intent.v1` with `operation` = `materialize_v2`.
- `presented-use` and `presenter-revoke`: action `engine.presenter.event.v1`, resource
  `{schema_version, operation: presented_use|presenter_revoke, outcome,
  presenter_ref_sha256?, key_id_sha256, source_ids_digest?, governance_revision?}` with
  `outcome` = `requested`, `disclosed`, `unusable`, `refused`, `revoked` or
  `not_on_record`.

Content-free: digests of the presenter reference, the key id and the source id set;
never a capability, never text. A revocation is carried out before its record is
written and is never held back by an unavailable audit store; the caller then gets the
audit failure instead of a success. A refusal is recorded when the audit store is
reachable and refused either way. The chain's own outcome column says that a record was
appended; the event is the resource's `outcome`.

## SDK bindings

Each binding returns the capability to the HOST PROGRAM as an opaque value whose every
string form is redacted, and shows it again only through its transport.

| Binding | Call | Transport | Capability type |
|---|---|---|---|
| TypeScript | `SubprocessEngineClient.contextMaterializeSourcesV2` | local Engine CLI; `0600` file in a private temporary directory, removed after the call | `PresenterCapability` (`toString`/`toJSON`/inspect redacted) |
| Python | `EnterpriseEngineClient.context_materialize_v2` | suite route; request header | `leanctx_sdk.presenter.PresenterCapability` (`repr`/`str`/`format` redacted; pickle and copy refused) |
| Go | `EnterpriseEngineClient.ContextMaterializeSourcesV2[Context]` | suite route; request header | `*PresenterCapability` (every `fmt` verb redacted; JSON and text encoding refused) |
| Rust | `EnterpriseEngineClient::context_materialize_v2` | suite route; request header | `PresenterCapability` (redacting `Debug`; no `Display`, no `Serialize`, no `PartialEq`) |
| JVM | `EnterpriseEngineClient.contextMaterializeV2` | suite route; request header | `PresenterCapability` (`toString` redacted; not `Serializable`) |
| .NET | `EnterpriseEngineClient.ContextMaterializeV2Async` | suite route; request header | `PresenterCapability` (`ToString` and debugger display redacted; excluded from JSON output of the grant) |

Use and revocation of a held capability (issue → use → revoke):

| Binding | Use | Revoke |
|---|---|---|
| TypeScript, local CLI | `SubprocessEngineClient.presentedUse(root, capability, use)` | `.revokePresenter(root, capability)` |
| TypeScript, suite | `EnterpriseEngineClient.presentedUse(capability, sourceIds, revision, use)`; also `.contextMaterializeSourcesV2` | `.revokePresenter(capability)` |
| Python | `EnterpriseEngineClient.presented_use(capability, source_ids, revision, mode, value)` | `.presenter_revoke(capability)` |
| Go | `EnterpriseEngineClient.PresentedUse[Context](PresentedUseRequest)` | `.RevokePresenter[Context](capability)` |
| Rust | `EnterpriseEngineClient::presented_use(&capability, &source_ids, revision, PresentedUse::…)` | `::presenter_revoke(&capability)` |
| JVM | `EnterpriseEngineClient.presentedUse(capability, sourceIds, revision, mode, value)` | `.revokePresenter(capability)` |
| .NET | `EnterpriseEngineClient.PresentedUseAsync(capability, sourceIds, revision, mode, value)` | `.RevokePresenterAsync(capability)` |

Each validates the answer against what was asked (text for `expand` only, the recovery
id it asked for, exactly the requested sources, never the capability inside the text)
and its tenant.

In every binding: `reveal()` (`Reveal()`) is the one deliberate way to the secret, for a
host that keeps it in its own secret store; a lifetime outside 1 to 86400 seconds and a
malformed capability are refused before any request; a response is refused when it
carries a capability for a renewal, none for an issue, or a capability that also appears
in the materialized content. A suite or Engine that does not serve the contract (HTTP
404, 405 or 501 on the route; the bare `invalid_request` on the CLI) is the binding's
`UnsupportedEngineError`, not a generic failure. A refused capability is the binding's
existing policy refusal for HTTP 403.

Not closed by a binding: the HTTP stack underneath it. A host that turns on wire or
header logging of its HTTP client (for example the JDK's `jdk.httpclient.HttpClient.log`
or an equivalent tracing switch) logs the header itself. Python, Go, JVM and .NET strings
cannot be erased from memory after use.
