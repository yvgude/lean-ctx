# LeanCTX data model v1 (app, cloud, Hosts)

One versioned schema set that the app (leanctx-workspace), the cloud (this
repository) and the LeanCTX Hosts (lean-ctx core) read the same way: projects,
vaults, tasks, agent runs, artefacts, knowledge, policies and audit events. It
is consolidated from what the three code bases produce today; it invents no
field. It is the shared basis for handing work over between developers: a task,
its runs, its evidence and the knowledge it rests on mean the same thing on
every side.

The wire contracts stay where they are (`contracts/cloud-v1`, `contracts/cloud-v2`,
`contracts/process-v1`, C0 2.2 in `contracts/c0-2.2-docs.md`). Model v1 refers to
them (`$ref` into `cloud-v2` and `process-v1`) instead of copying them, and names the local views
(app, Host local API) the wire contracts do not cover.

## Layout

| Path | Content |
|---|---|
| `schemas/` | JSON Schema 2020-12, base URI `https://contracts.leanctx.com/model-v1/schemas/` (resolved locally, never fetched); one file per entity plus `common.schema.json` |
| `fixtures/valid/` | examples that must validate |
| `fixtures/invalid/` | examples that must fail (retired or malformed shapes) |
| `c0-fixtures.json` | the existing C0 fixtures (`[path below contracts/, schema ref, member]`) that must conform to the model; an empty member is the whole file, an array member is checked item by item |
| `types/` | TypeScript types generated from `schemas/` (see below); never edited by hand |
| `MANIFEST.sha256` | sha256, two spaces, path, for every file except itself |

Fixture names map to definitions: `<entity>.<def>[.<variant>].json` is checked
against `<entity>.schema.json#/$defs/<def>`.

## Entities

| Entity | Identity | Views (`$defs`) |
|---|---|---|
| `project` | stable identity digest (Host) → `vault_id` (cloud); local folder (app) | `record` (core `cloud/projects.json`), `resolve_request`, `setup`, `checkout` (app, local only) |
| `vault` | `vault_id` (32 hex), addressed as `@ns/name` | `vault`, `ref` |
| `task` | `(vault_id, number)` | `task`, `summary`, `list`, `result_request` and the shared parts (state, lifecycle, origin, update, result, decision, verification) |
| `agent_run` | `(vault_id, number, claim.lease_id)`; agent = `profile_id` | `claim_request`, `claim_ack`, `claim`, `usage`, `worker`, `reputation`, `profile` |
| `artifact` | `(kind, reference)` or `(kind, anchor.path, anchor.digest)` | `evidence`, `evidence_view`, `session_artifact` (app, local only) |
| `knowledge_node` | C0 2.x `node_id`; C0 1.x fact id within `(vault_id, revision)` | `event` (→ cloud-v2), `fact`, `page`, `relation`, `document`, `revision`, `fact_state` |
| `policy_bundle` | `(instance_id, version, digest)` | `bundle_response`, `filter_pack`, `effective` (→ cloud-v2), `ack_request`, `installed`, `sessions_doc`, `session_doc` (Host local API) |
| `audit_event` | `(org, seq)`; `hash` | `chain_entry` (stored), `entry_view`, `receipts_request`, `receipt_item`, `policy_ack`, `policy_applied` (Host → cloud) |

Each schema file names its identity and views in `x-entity`.

## Field → producer

Every property carries `x-producers`: a list of `"<project> <path> <symbol>"`
where project is `p37` (cloud), `p5` (core) or `p43` (app), path is the file in
that repository and symbol the type or constant (`Type.field`, or a quoted JSON
key). A definition that is only a `$ref` carries its producers at definition
level. A member only one side produces is optional and names only that side;
the test `every_field_names_its_producer` (crates/cloud-wire/tests/model_v1.rs)
rejects a field without a producer and checks that every p37 producer exists in
this repository: the cited file contains the type and the cited field (as a
Rust identifier, or as its serde rename / JSON key in quotes). p5 and p43 producers are checked by the follow-ups in their
repositories.

## Versioning and compatibility

- Each schema states `x-model-version` (`MAJOR.MINOR`); all files of a set move
  together.
- Shapes are closed per version: the model's objects set
  `additionalProperties: false`, and the request types of `cloud-wire` (and
  their core counterparts) use `#[serde(deny_unknown_fields)]`, so an unknown
  member is refused, not ignored. No model enum carries an `unknown` value.
- A new member or enum value is sent only to a peer that negotiated a
  `Cloud-Protocol` minor that has it (`cloud_wire::protocol_version_for`; C0 2.2
  engines gate doc nodes and decision anchors on `2.2`). A writer never sends a
  member newer than the negotiated minor; the server keeps the older shape for
  older minors (for example `vault.sealed: false` below 2.1).
- Tolerant reading exists only where the code has it: the `cloud-wire` task
  enums `TaskState`, `TaskOriginKind`, `TaskActorKind`, `EvidenceKind`,
  `TaskUpdateKind`, `DecidedBy` and `TaskDecisionOutcome` map an unknown value
  to their `#[serde(other)]` variant, and the answer types without
  `deny_unknown_fields` (`Task`, `TaskSummary`, `TaskList`) skip unknown
  members. The schemas still list only the known values; nothing else may rely
  on tolerance.
- Minor (`1.0` → `1.1`, same directory): an optional member, an enum value or a
  view, each gated on the `Cloud-Protocol` minor that introduces it, or a new
  producer. Existing fixtures stay valid; the schema widens in the same commit.
- Major (new directory `model-v2`, `model-v1` stays until every producer moved):
  removing or renaming a member, making a member required, narrowing a type or a
  limit, changing a meaning. A retired shape becomes an invalid fixture.
- A change edits the schema, its fixtures and `MANIFEST.sha256` in one commit;
  consumers copy the directory byte-identically and recompute the manifest.

## TypeScript types

`python3 scripts/gen-model-types.py` writes `types/<entity>.d.ts` (one per schema,
every `$defs` entry as an exported type: `claim_request` → `ClaimRequest`) and the
C0 2.x and process v1 definitions the model reaches through `$ref` into
`types/cloud-v2/<file>.d.ts` and `types/process-v1/<file>.d.ts`, then rewrites `MANIFEST.sha256`. The types carry structure: members, optionality,
nullability, enums, constants and unions. Value constraints (lengths, patterns,
ranges, `if`/`then`, `not`) stay in the schemas, which remain the authority.
Consumers (the app) copy `types/` with the rest of the directory.

CI runs `scripts/gen-model-types.py --check` (job `api-doc`; a schema change without
regenerated types or manifest fails) and compiles the types with `tsc --strict`
(job `web`).

## Policies: signed, versioned, enforced locally

The cloud signs one effective policy per Host (Ed25519, `version` grows
strictly, expires at `not_after`; `policy_bundle#/$defs/effective`). The Host
verifies it, refuses downgrades and enforces it without the cloud: a missing,
expired or damaged policy falls back to the safe local default
(`installed.signature_state`, `installed.policy_state`, `session_row.enforcement`).
The Host acknowledges what it applied (`ack_request` → `audit_event#/$defs/policy_ack`).

## Audit back to the cloud

- Hosts send signed receipts (`audit_event#/$defs/receipts_request`) and the
  `policy.applied` bus frame (`audit_event#/$defs/policy_applied`) for a verified,
  in-force, acknowledged policy. The cloud verifies the frame against the
  bundles it issued, keeps a newer version per Host (`policy_applied`,
  `crates/cloud-server/src/policy_bundle.rs`), audits it as `policy.applied`
  and serves it as `GET …/policies/applied` (shown on the Host page and
  Rules › Delivery).
- The cloud appends every audited action to a per-team hash chain
  (`chain_entry`; `hash = sha256_hex("leanctx.cloud.audit.v1\n" || canonical_json(entry))`,
  `prev` links the chain, genesis is 64 zeros). IP address and user agent are
  stored in the chain only as keyed hashes (`hmac:` + 32 hex); `entry_view`
  shows plaintext only while the team's retention keeps it, and a deleted
  account as "Deleted account".

## Gates

- `crates/cloud-wire/tests/model_v1.rs`: every definition compiles, the eight
  entities exist, fixtures validate (invalid ones fail), the listed C0 fixtures
  conform, every field names its producer, the manifest matches, and the
  `cloud-wire` task, claim, knowledge, vault and setup types serialise to the model.
- `crates/cloud-server/tests/model_v1.rs`: the chain entries `audit::record_at`
  writes (built by `audit::ChainEntry::to_json`, the code path `record_at`
  uses) and the views `audit::entry_json` serves conform.

## Known drifts (open)

- Core-only members without a cloud producer: task `process` (exactly
  `contracts/process-v1/task-process.schema.json`, core `TaskProcessV1`) and
  `process_gate` (the G6 inputs beyond it, core `TaskProcessGateV1`), actor
  `profile_id`, origin `message`, `merge`, `amendments`, progress and amendment
  update kinds; knowledge revision `state`, `fact_states`, `quarantined_after`,
  `ctxpkg_tree`, `ctxpkg_commit`.
- `cloud-v1` policy bundle fixtures still carry the retired `org_policy`, and
  `vault.code_graph.response.json` the retired `sealed`/`key_epoch`; both are
  outside the model (see `fixtures/invalid/`).
