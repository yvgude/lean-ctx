# Team Context Contract v1

Status: versioned V4 contract; runtime implementation and Team release remain gated. Publication of these schemas is not evidence that Team is shipped.

This contract defines the tenant-bound records required to turn personal context into shared organizational context. It does not promote the research-only local Agent Bus into a Team backend.

## Normative rules

- Every record uses `v = 1`; unknown versions and unknown fields fail closed. Every schema sets `additionalProperties: false`.
- `organization_id` and `workspace_id` are authorization inputs, not display metadata. A server must derive the authenticated member and compare every scope component before reading, mutating, approving, or returning a record.
- IDs, versions, parent digests, object digests, roles, classifications, validity windows and idempotency keys are part of the canonical signed or AEAD-authenticated message.
- Canonical bytes use RFC 8785 JCS over the complete validated JSON object. Digests are lowercase `sha256:` or `hmac-sha256:` plus 64 lowercase hexadecimal characters. Which prefix and which preimage apply to which field is fixed by the table below; a field is not free to choose.
- Authorization is deny-by-default. `owner` and `admin` manage workspace membership; `approver` may decide promotions; `member` may submit candidates; `viewer` is read-only. A deployment may narrow, never widen, this matrix without a versioned policy decision.
- Invite tokens and recipient identifiers never appear in stored records, receipts, telemetry or logs. Only digests are stored; acceptance consumes the nonce exactly once.
- Raw context, prompts, file paths, credentials, tokens and secrets are forbidden in provenance, receipts, telemetry and error bodies. Those surfaces carry bounded identifiers, enums, counts and digests only.
- Arrays are bounded, unique where specified, and sorted in ascending lexicographic order before signing. Implementations apply a request-body limit before JSON decoding.
- Contradictory authoritative objects produce a `conflict` record. Retrieval must not return both as equally authoritative; unresolved conflicts are explicit.
- Supersession and expiry preserve immutable history. Deletion, export and retention operate on the tenant scope and emit redaction-safe receipts; the object side of a redaction or deletion is `context-object.lifecycle_state`.

### Identifier namespaces

`common.schema.json#/$defs/id` excludes `:`. Every typed identifier carries its own reserved prefix — `member:` (`$defs/memberId`), `key:` (`$defs/keyId`). An untyped id can therefore never collide with a typed namespace, and `object_id: "member:a1"` is rejected.

### Lineage vocabulary

Exactly two lineage relations exist, and they are never mixed:

| Relation | Field | Meaning |
| --- | --- | --- |
| Same-record CAS | `parent_digest` | Record digest of the immediate predecessor **version of this same logical record**. |
| Cross-object supersession | `supersedes` / `superseded_by` | Record digest of a **different** object whose authority this object replaces, or that replaced this object. |

`supersedes_digest` does not exist in v1. Any record carrying it fails closed.

### Genesis and compare-and-swap

- `version = 1` is the genesis record. It MUST NOT carry `parent_digest`. There is no sentinel or all-zero digest, so no implementation has to invent one and cross-implementation digest determinism is preserved.
- `version > 1` MUST carry `parent_digest` equal to the record digest of the version immediately below it, and `version` MUST be exactly `previous.version + 1`.
- The rule is enforced structurally by `common.schema.json#/$defs/casLineage`, applied through `allOf` by every mutable record schema: `organization`, `workspace`, `member`, `membership`, `workspace-role`, `lease`, `invite`, `context-object`, `promotion`, `authority-decision`, `policy`, `checkpoint`, `provenance`, `conflict`, `team-receipt`, `signature-envelope`.
- Mutations are append-only and compare-and-swap against `parent_digest`. A stale parent is retained as a conflict branch, never silently overwritten.
- Idempotency is scoped by `(organization_id, workspace_id, operation kind, idempotency_key)` and binds the canonical request digest. Reuse with different bytes is a conflict. Every mutable record carries `idempotency_key`; `team-receipt.idempotency_key` echoes the originating operation's key so duplicate suppression is auditable from the receipt alone.

### Timestamps and clock skew

Canonical timestamps match exactly:

```
^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,9})?Z$
```

The pattern is asserted rather than left to `format`, because `format` is annotation-only under the 2020-12 default vocabulary. Only the `Z` designator is admitted: `2026-01-01T02:00:00+02:00` and `2026-01-01T00:00:00Z` are the same instant but different JCS bytes and therefore different record digests.

Numeric skew bounds, not "a documented policy":

- A server MUST reject any server-authored timestamp more than **300 s** ahead of its own trusted clock.
- A server MUST reject a client-supplied timestamp more than **300 s** ahead of, or more than **900 s** behind, its own trusted clock.
- A signature envelope whose `signed_at` is more than **300 s** after the server's receipt time is rejected.
- These bounds are absolute; a deployment may narrow them, never widen them.

### Digest and preimage table

Every digest-typed field belongs to exactly one preimage class. The classes are structurally distinguishable: pseudonyms use the `hmac-sha256:` prefix, everything else uses `sha256:`.

| Class (`common.schema.json`) | Preimage | Fields |
| --- | --- | --- |
| `recordDigest` | SHA-256 over RFC 8785 JCS canonical bytes of a **complete validated team-context-v1 record** | `parent_digest`, `supersedes`, `superseded_by`, `object_digest`, `object_digests[]`, `receipt_digests[]`, `provenance_digest`, `authority_decision_digest`, `decision_digest`, `promotion_digest`, `policy_digest`, `signature_envelope_digest`, `left_digest`, `right_digest`, `membership_digest`, `role_binding_digest`, `signed_payload_digest`, `deletion_receipt_digest`, `redaction_receipt_digest` |
| `contentDigest` | SHA-256 over the canonical bytes of a referenced payload that is **not** a team-context-v1 record (JCS when JSON, raw octets otherwise) | `content_digest`, `rules_digest`, `source_digest` |
| `canonicalBytesDigest` | SHA-256 over the JCS canonical bytes of an operation's request or response body, as admitted after the request-body limit | `input_digest`, `output_digest` |
| `nonceDigest` | SHA-256 over a single-use nonce of at least 128 bits from a CSPRNG; the preimage is never stored, logged or returned | `nonce_digest` |
| `pseudonymDigest` | `HMAC-SHA-256(per_tenant_pepper, NFC(lowercase(subject)))`, pepper at least 256 bits from a CSPRNG, unique per organization, stored outside the record store, never emitted | `subject_digest`, `recipient_digest`, `actor_digest` |

Rationale for the pseudonym class: an unsalted SHA-256 of an email address is dictionary-recoverable, so a plain record digest cannot support the "tenant-safe pseudonymous IDs" claim. The distinct `hmac-sha256:` prefix makes storing an unsalted identity hash in a pseudonym field a schema violation rather than a review finding. Pepper rotation is a versioned policy decision that rewrites the derived pseudonyms and emits receipts.

`signature_envelope_digest` is a **reference**, not a signature: it is the record digest of a `signature-envelope` record that carries `alg`, `key_id`, `key_epoch`, `public_key` and the raw `signature`. A hash of a signature is not verifiable; the envelope is.

### Signatures

`signature-envelope.schema.json` is a detached Ed25519 (RFC 8032) envelope. `alg` is `const: "ed25519"`; algorithm agility is a versioned contract change, never runtime negotiation. `signature` is the 64-byte signature as unpadded base64url (86 characters) over the ASCII bytes of:

```
leanctx-team-context-v1:<signed_payload_kind>:<signed_payload_digest>
```

`signed_payload_digest` is the record digest of the signed record **with its own `signature_envelope_digest` member omitted**, because the record references the envelope and the envelope references the record. `authority-decision`, `policy` and `team-receipt` require `signature_envelope_digest`; `checkpoint` may carry one.

### Authority and approval

- `context-object.authority_state = "authoritative"` requires `approval_state = "approved"`, `provenance_digest` **and** `authority_decision_digest`. `team_candidate` and `historical` require `provenance_digest`.
- `approval_state = "approved"` requires `authority_decision_digest`; any other approval state forbids it. `authority_state = "personal"` cannot be `approved`.
- `authority_state = "historical"` requires `superseded_by` or `valid_until`, so history is never implicit.
- `promotion.state ∈ {approved, rejected}` requires `decided_by`, `decided_at`, `decision_reason_code` **and** `decision_digest`; every other state forbids all four. An approved promotion is terminal: later loss of effect is recorded on the context object (`superseded_by`, `valid_until`), not by mutating the promotion's state.
- `authority-decision` requires `promotion_id`, `promotion_digest`, `object_id`, `object_digest`, `policy_digest` and `signature_envelope_digest`.

### Mandatory cross-field and cross-record validation

JSON Schema cannot express these; a server MUST enforce every one of them and reject on failure. Failing to enforce them is a contract violation, not a hardening opportunity.

1. **Non-self-approval.** `authority-decision.decided_by != promotion.requested_by` for the promotion named by `promotion_id`/`promotion_digest`. `promotion.decided_by` must equal `authority-decision.decided_by`.
2. **Same-scope approval.** `authority-decision.scope` equals, component by component, the scope of the referenced promotion, the referenced policy and the referenced context object.
3. **Approver authority.** `decided_by` holds an active same-scope `membership` whose `workspace-role` binding grants `promotion.decide` at decision time, and the binding is neither revoked nor outside its validity window.
4. **Signer binding.** The `signature-envelope` referenced by `signature_envelope_digest` verifies against `public_key`, its `key_id`/`key_epoch` is at or above the tenant's minimum accepted epoch, and its signer equals the record principal: `signed_by` equals `decided_by` for an authority-decision or `created_by` for a policy; a team-receipt instead requires `signed_by_digest` equal to its privacy-safe `actor_digest` and forbids `signed_by`. Its `signed_payload_digest` matches the recomputed digest.
5. **Policy agreement.** `promotion.policy_digest` equals `authority-decision.policy_digest`, and that policy is `active`, same-scope, of kind `promotion_authority`, and inside its validity window at `decided_at`.
6. **Object linkage.** `context-object.authority_decision_digest` resolves to an `authority-decision` with `decision = "approved"`, the same `object_id`, and `object_digest` equal to the digest of the object version being approved.
7. **Conflict winner.** `conflict.winner` is `left` or `right` and is the sole representation of the outcome; the winning digest is read from `left_digest` or `right_digest`. There is no `winner_digest` field to disagree with them, so a phantom winner is structurally unrepresentable rather than merely validated against. Both branches must be same-scope authoritative or team-candidate objects.
8. **Validity ordering.** Wherever both are present, `valid_until > valid_from` strictly; equality and inversion are rejected. The same applies to `lease.expires_at > lease.granted_at`, `invite.expires_at > invite.created_at` and `revoked_at >= created_at`.
9. **Provenance agreement.** `context-object.provenance_digest` resolves to the provenance record. To avoid an impossible digest cycle, `provenance.object_digest` equals the subject context-object digest recomputed with its own `provenance_digest` member omitted. `provenance.object_id`, `authority_state`, `valid_from`, `valid_until` and `classification` equal that subject object version.
10. **Lease derivation.** `lease.membership_digest` and `lease.role_binding_digest` resolve to active same-scope records at grant time. Revoking a membership or role binding invalidates every lease naming it before the next read or write, by writing a new lease version with `state = "revoked"`; lineage is never erased.
11. **CAS.** `parent_digest` equals the recomputed digest of the stored predecessor and `version = previous.version + 1`.
12. **Array canonicalization.** `object_digests`, `receipt_digests` and `allowed_actions` are ascending lexicographic and duplicate-free before signing.

### Action vocabulary

`common.schema.json#/$defs/action` is the single vocabulary. `workspace-role.allowed_actions` grants from it and `team-receipt.operation_kind` records from it, so the previous gap — receipts for `context.export` / `context.delete` / `invite.issue` that no role could be granted — cannot recur. The 21 actions are:

```
checkpoint.create   conflict.resolve    context.delete      context.export
context.read        context.redact      context.submit      invite.consume
invite.issue        lease.grant         lease.revoke        membership.change
organization.create policy.change       promotion.decide    promotion.request
provenance.read     receipt.read        role.change         workspace.create
workspace.read
```

Checkpoint lifecycle uses `checkpoint_state` (`draft`, `sealed`, `superseded`, `expired`), deliberately disjoint from the object authority vocabulary (`personal`, `team_candidate`, `authoritative`, `historical`) so a checkpoint state can never be read as object authority. The authority of each object listed in a checkpoint is read from that object's own `authority_state`.

## Schemas

- `common.schema.json`: shared type library — identifiers, timestamps, digest preimage classes, roles, actions, scope, CAS lineage rule. Defines no record.
- `workspace.schema.json`: workspace identity, classification and tenant lifecycle.
- `organization.schema.json`: durable organization identity, classification and tenant lifecycle.
- `member.schema.json`: privacy-safe organization principal lifecycle.
- `membership.schema.json`: durable role and membership state with validity and revocation.
- `workspace-role.schema.json`: bounded, scoped, revocable role-to-action grant.
- `lease.schema.json`: short-lived revocable access lease derived from a membership and role binding.
- `invite.schema.json`: replay-safe, expiring invite lifecycle.
- `context-object.schema.json`: shared decisions, knowledge, gotchas, policies and performance profiles.
- `promotion.schema.json`: personal → candidate → authoritative → historical requests.
- `authority-decision.schema.json`: immutable, policy-bound, signed approval evidence.
- `policy.schema.json`: versioned, signed workspace policy metadata.
- `signature-envelope.schema.json`: detached Ed25519 signature over one record's canonical bytes.
- `checkpoint.schema.json`: workspace checkpoint manifest of object and receipt digests.
- `provenance.schema.json`: source, author, authority, validity and supersession lineage.
- `conflict.schema.json`: explicit contradictory branches and deterministic resolution evidence.
- `team-receipt.schema.json`: privacy-safe operation and outcome evidence.

The ten typed entities of the V4 model map onto these files as: Organization → `organization`; Workspace → `workspace`; Member → `member` (+ `membership`); WorkspaceRole → `workspace-role` (+ `lease`); ContextObject → `context-object`; ContextPromotion → `promotion`; AuthorityDecision → `authority-decision` (+ `signature-envelope`); Policy → `policy`; WorkspaceCheckpoint → `checkpoint`; TeamReceipt → `team-receipt`. `invite`, `provenance` and `conflict` are the supporting records the lifecycle requires.

## Reference resolution and validation

All `$id` values are relative, so the resolution base is the retrieval base URI. Validate from this directory, or pin the base explicitly, so relative references can never fall back to a network fetch:

```sh
cd docs/contracts/team-context-v1
check-jsonschema --check-metaschema ./*.schema.json
check-jsonschema --schemafile context-object.schema.json /path/to/context-object.json
```

If a validator is invoked from elsewhere, pass the directory as the base URI (`--base-uri file:///abs/path/docs/contracts/team-context-v1/`) rather than relying on the process working directory. Schema publication is immutable: incompatible changes require `team-context-v2`.

## Required negative acceptance cases

The committed conformance fixtures are dependency-free and run in both public
GitHub CI and the private GitLab merge-request gate:

```sh
python3 scripts/verify-team-context-v1.py
```

The verifier recomputes JCS bytes, digests, Ed25519 signatures, HMAC
pseudonyms, nonces, rules 1–12 and idempotency outcomes. It deliberately does
not claim JSON Schema, database, tenant-isolation or runtime enforcement;
those remain separate release gates.

`frozen-hashes.json` pins every schema, fixture, this README and the verifier.
The manifest excludes only itself to avoid a self-referential digest. Any
published v1 byte change therefore fails CI until an explicit reviewed manifest
update; incompatible changes require `team-context-v2`.

Before runtime release, tests must reject: cross-tenant replay; caller-supplied membership; role escalation; unauthorized approval; **self-approval**; **an approved promotion with no resolvable same-scope authority-decision**; **an authoritative object with no provenance or no authority-decision link**; **an unsigned or badly-signed authority-decision, policy or receipt**; **a genesis record carrying `parent_digest` and a non-genesis record omitting it**; duplicate invite consumption; idempotency-key byte drift; stale parents; unknown fields and unknown versions; **non-UTC or non-conforming timestamps and out-of-skew clocks**; **inverted or equal validity windows**; oversized bodies and arrays; invalid digests; **an unsalted identity hash in a pseudonym field**; **a conflict winner that is neither branch**; **terminal fields in a non-terminal lifecycle state**; unsorted canonical lists; and secret-bearing telemetry.

Two-user/two-device tests must prove promotion, contradiction, checkpoint recovery, revocation of membership and of active leases, redaction and deletion tombstones, and tenant isolation against a real durable backend.
