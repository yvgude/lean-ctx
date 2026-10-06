# ADR-013: Control-Plane Contract Ownership

**Status:** Accepted for contract-ownership hierarchy; superseded for the
historical multi-SDK support inventory below.
**Date:** 2026-08-09
**Authors:** Architecture Team

## Context

lean-ctx already exposes several representations of the same public contract.
Canonical OCLA traits, structures, and validation live in the standalone
`rust/crates/lean-ctx-ocla/src/` crate; language-neutral protocol contracts live
in `rust/crates/lean-ctx-protocol/`. The engine-facing paths
`rust/src/core/ocla/traits.rs` and `rust/src/core/ocla/types.rs` are compatibility
reexports only. The version registry remains in `rust/src/core/contracts.rs`,
and the OpenAPI projection is assembled in
`rust/src/core/openapi.rs`; committed JSON Schemas include
`docs/contracts/ocla-wire-v1.schema.json` and
`docs/contracts/ocla-agent-envelope-v1.schema.json`. The binary transport
projection is `contracts/ocla/v1/ocla.proto`, compiled by
`packages/ocla-grpc/build.rs`. The current SDK projections are the Rust client
and Python SDK v1 in `packages/python-lean-ctx/`; earlier Python, TypeScript,
and Go prototypes are archived migration material, not supported surfaces.

Without an ownership hierarchy, a hand-edited schema, generated Protobuf
field, or SDK type can become an accidental second source of truth. That makes
additive evolution unsafe, causes clients to disagree about optionality and
unknown fields, and makes signed bytes depend on the language or serialization
path used by the producer.

The control plane needs one semantic authority, one language-neutral contract
projection, deterministic bytes for signatures, and a repeatable generation
and migration process for every supported SDK.

## Decision

### Source-of-truth hierarchy

The authority chain is strict and one-directional:

```text
Rust semantic types and invariants
        ↓
JSON Schema / OpenAPI projection
        ↓
Protobuf transport projection
        ↓
generated SDKs and language bindings
```

1. **Rust types are the semantic source of truth.** Shared control-plane
   types, validation rules, version constants, enum meanings, and field
   ownership are defined in the canonical Rust crates: OCLA capability traits
   and runtime types in `rust/crates/lean-ctx-ocla/src/traits.rs` and
   `rust/crates/lean-ctx-ocla/src/types.rs`, and language-neutral protocol
   contracts in `rust/crates/lean-ctx-protocol/`. During the transition, the
   version registry in `rust/src/core/contracts.rs` remains the implementation
   reference for its already-published contracts. The engine paths
   `rust/src/core/ocla/traits.rs` and `rust/src/core/ocla/types.rs` are
   compatibility reexports only; they do not own semantics. A projection
   cannot introduce a field or invariant that has no Rust definition.
2. **JSON Schema and OpenAPI are the language-neutral contract projection.**
   The committed schemas under `docs/contracts/`, the OCLA schema projection in
   `rust/src/core/ocla/wire.rs`, and the endpoint projection in
   `rust/src/core/openapi.rs` describe the accepted JSON shape. JSON Schema
   carries requiredness, bounds, version constants, extensibility, and public
   identifiers in a form every client can validate.
3. **Protobuf is a transport projection, not a competing authority.**
   `contracts/ocla/v1/ocla.proto` mirrors the JSON contract with stable field
   numbers and transport-oriented messages. `packages/ocla-grpc/build.rs`
   generates bindings from that file. Protobuf may add transport concerns such
   as service methods, but it may not change the semantic meaning, requiredness,
   or security interpretation of a Rust/JSON field.
4. **SDKs are generated projections or conformance-checked compatibility
   layers.** A client type may improve ergonomics for its language, but it may
   not remove a wire field, change its wire name, narrow its allowed values, or
   invent a new semantic field.

The hierarchy applies to Task, Capability, Plan, Receipt, Outcome, policy,
lineage, and evidence contracts. Existing OCLA v1 artifacts remain valid at
their current version while new control-plane contracts converge on the
protocol crate.

### Compatibility policy

Within a schema version, changes are additive only:

- new fields are optional or have a wire-defined default;
- new enum values are allowed only for enums explicitly marked extensible;
- existing field names, types, meanings, numbers, and requiredness do not
  change;
- fields are never removed or renamed;
- Protobuf field numbers and enum discriminants are never reused; removed
  numbers are reserved permanently;
- a semantic or requiredness break creates a new schema version and a new
  projection file.

Readers preserve unknown fields. A JSON reader retains unrecognized object
members in an extension map or the original raw object and forwards them when
it acts as a proxy. A Protobuf reader retains unknown wire fields and forwards
them through supported runtimes. A writer changes only fields it owns and
does not round-trip an object through a typed model that would silently drop
unknown members.

The existing OCLA verifier intentionally rejects unknown nested fields for the
published OCLA v1 conformance profile, as documented in
`docs/contracts/ocla-verifier-conformance-v1.md`. That behavior is frozen for
that contract. It is not a license to make new control-plane contracts
strict-by-loss; future control-plane versions must declare their extensibility
policy and preserve fields wherever forward compatibility is promised.

### Canonical serialization and signatures

Every signable control-plane document has one canonical JSON byte form. The
canonical serializer is part of the Rust protocol implementation and applies
these rules:

- UTF-8 encoding with no byte-order mark;
- object keys sorted lexicographically by their wire names;
- no insignificant whitespace;
- arrays serialized in their contract-defined order;
- explicit `null` versus omission preserved exactly as the schema defines;
- integers emitted in their shortest non-negative or negative decimal form;
- no floating-point values in signed control-plane economics or identity
  fields; decimal quantities use the protocol's exact representation;
- all known fields and preserved unknown fields included in the signed object;
- the signature covers `contract_id`, `schema_version`, and the canonical
  document bytes, not a language-specific in-memory layout.

The signed payload is therefore the output of one deterministic serializer,
not the result of whichever SDK happened to emit the document. JSON Schema
validation occurs before signing and after verification. Protobuf transport
uses deterministic serialization when a binary digest or transport signature
is required, but the Protobuf encoding does not replace canonical JSON as the
cross-language semantic signing form.

Canonical fixtures are checked into the public contract pack at
`docs/contracts/ocla-contract-pack-v1.json` and
`clients/rust/lean-ctx-client/tests/fixtures/`. A change that alters signed
bytes is a compatibility event even when the decoded fields appear unchanged.

### SDK generation and synchronization

The release pipeline follows this order:

1. Change the Rust protocol type, invariant, or version constant.
2. Regenerate the committed JSON Schema and OpenAPI projection.
3. Regenerate or update the Protobuf projection in
   `contracts/ocla/v1/ocla.proto` and build bindings through
   `packages/ocla-grpc/build.rs`.
4. Generate or update only the currently supported Rust and Python projections
   from the schema, preserving unknown-field storage and wire names. A new
   language binding requires an explicit product-scope and evidence decision.
5. Regenerate canonical fixtures and the content digests in the contract pack.
6. Run the schema, SDK, and verifier conformance checks before publishing any
   version.

The current projection locations are:

- Rust protocol and client: `rust/crates/lean-ctx-protocol/` and
  `clients/rust/lean-ctx-client/`;
- Python SDK v1: `packages/python-lean-ctx/`;
- Historical Python, TypeScript, and Go prototypes: `_archive/`;
- gRPC bindings: `packages/ocla-grpc/`.

`scripts/check-sdk-versions.py` verifies canonical Python SDK metadata; protocol
and verifier checks remain required where their contracts change. A future
binding must be introduced with its own generated projection and conformance
evidence. The hand-maintained status of a current SDK never makes that SDK an
authority.

Each supported SDK release records the schema and contract-pack version it
supports. The Rust client, Python SDK, and OCLA verifier must run against the
applicable shared fixture set. A language-specific test that passes while the
shared fixtures fail is not a valid contract result.

### Version policy and migration gates

Every control-plane document carries an integer `schema_version` field and a
stable contract identifier. Version `1` means the v1 field meanings and
compatibility rules; it is not a package or SDK version.

An additive change stays within the current schema version. A breaking change
creates a new schema file, Rust type/version constant, Protobuf package or
message version as required, fixture set, and SDK projection. Old readers and
writers remain supported for the published migration window described by
`docs/contracts/DEPRECATION.md`; production readers support the current and
immediately previous schema before a producer is allowed to emit the new one.

Every migration passes these gates in order:

1. Rust validation and invariant tests pass for both the current and previous
   schema.
2. JSON Schema, OpenAPI, Protobuf, and canonical-byte fixtures are regenerated
   with no unreviewed drift.
3. Protobuf numbers are checked for reuse and JSON field names for accidental
   renames.
4. All supported SDKs can read the new document, preserve unknown fields, and
   still read the previous document.
5. Verification tests cover valid, invalid, unsupported-version, duplicate,
   non-canonical, and unknown-field cases appropriate to the contract.
6. New readers are deployed before new writers; a producer is enabled only
   after the compatibility matrix and signed-fixture check are green.
7. An incompatible version is rejected with an explicit migration error. The
   system never silently downgrades a signed or policy-bearing document.

Schema version changes are reviewed as architecture changes, even when the
implementation is generated. The migration record identifies the old version,
new version, supported window, adapter, fixture digest, and SDK release set.

## Consequences

The hierarchy gives maintainers one place to decide meaning and makes every
other representation auditable. Generated clients, schema digests, and golden
fixtures reduce language drift. Deterministic bytes make signatures portable
across Rust, Python, TypeScript, Go, and Protobuf transports.

Additive-only evolution and N/N-1 migration support make upgrades safer for
offline Runtime instances and Cloud services, but they increase release
overhead. Breaking improvements require parallel types, adapters, fixtures,
and a migration window. Unknown-field preservation requires raw-field storage
and careful proxy behavior rather than a simple typed deserialize/serialize
round trip.

Rust remains the semantic authoring language, so non-Rust contributors need a
clear generator and conformance workflow. Protobuf cannot express every JSON
extension equally well, and binary deterministic serialization is an
additional test surface. Those costs are accepted to keep the contract
interoperable and signatures reproducible.

The existing strict OCLA v1 verifier is not weakened by this ADR. Its frozen
behavior is versioned separately; the new policy prevents future contracts
from losing fields accidentally and makes any stricter behavior explicit.

## Alternatives Considered

Making JSON Schema the primary source was rejected because schemas express
shape and bounds well but do not own the Rust validation, semantic invariants,
or implementation-level migration logic already enforced by the Runtime.

Making Protobuf the primary source was rejected because it would make the
binary transport toolchain and field-number model govern JSON, SDK, and
offline document semantics. It would also make human inspection and extension
policy less direct for the existing contract portal.

Allowing each SDK to define its own types was rejected because optionality,
unknown fields, enum behavior, and signature bytes would drift across
languages. Hand-maintained client types remain only as checked projections
during the generator transition.

Signing whatever bytes a caller supplied was rejected because semantically
identical objects could have different key order, whitespace, numeric spelling,
or unknown-field handling. It would make verification depend on the producer's
serialization library.

Breaking every change in place was rejected because offline Runtime and Cloud
instances need a bounded compatibility window. Conversely, silently accepting
breaking changes under an old version was rejected because policy, identity,
and evidence documents must fail closed when their meaning is not understood.
