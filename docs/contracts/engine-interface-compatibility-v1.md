# Engine Interface and Compatibility Policy v1

> **Status: Phase-0 boundary policy.** This document records the supported
> local Engine interfaces and the conditions for a future SDK compatibility
> commitment. It does **not** publish a new SDK API or promote implementation
> types to stable public interfaces.
>
> **Authority:**
> Engine, SDK & Cloud Plan (`docs/internal/vision/06-ENGINE-SDK-CLOUD-PLAN.md`, internal — not in this repository)
> defines the delivery boundary; the
> internal README (`docs/internal/README.md`, internal — not in this repository) defines status and claims. When
> source-level truth disagrees, source wins.

## 1. Scope and decision

LeanCTX Engine is deterministic local context infrastructure. It executes
source access, search, shaping, cache, recovery, integrity, and safety
mechanisms. It does not decide an agent's task, scheduling, retries,
permissions, business policy, or user experience.

This policy makes the current local boundary explicit before a production SDK
repository or façade is created:

```text
Engine → published local mechanisms and versioned wire contracts
SDK    → future semantic context decisions; consumes Engine contracts
Cloud  → Research control plane; consumes published SDK/receipt contracts
```

The Engine never imports SDK or Cloud. A future SDK must not require Cloud for
local operation. Existing Engine mechanisms remain Apache-2.0 in this
repository; any later SDK legal decision must not reinterpret that Engine
boundary.

## 2. Current local Engine boundary

“Supported” below means a current local Engine surface with a user path. It
does not mean a general language SDK, hosted service, or permanent ABI.

| Boundary | Current status | Compatibility source | Explicit limit |
| --- | --- | --- | --- |
| `lean-ctx` CLI | Available local Engine boundary | Release notes and supported command documentation | CLI commands are not a language-neutral SDK API. |
| Local MCP stdio tools | Available local Engine boundary | [HTTP/MCP contract](http-mcp-contract-v1.md) | It is local runtime transport, not hosted MCP or a generic agent platform. |
| Coding-agent Attach through supported local hooks/setup | Available | Internal status map and integration guides | The host still owns agent control flow, prompts, retries, permissions, and UX. |
| Loopback local proxy | Available local Runtime boundary | Runtime configuration and local proxy documentation | Provider transport support is not a general SDK compatibility promise. |
| Local evidence/receipt artifacts and standalone V2 verification | Available local proof boundary | [V2 contract](evidence-bundle-v2.md) and [verification contract](evidence-bundle-v2-verification-v1.md) | A receipt or verifier pass is scoped evidence; it is not a universal quality or savings claim. |
| `lean-ctx-sdk` Rust workspace crate | Preview substrate | Checked-in crate, `publish = false`, and its README | Its in-process `Engine`/`EngineBuilder` façade is not a supported stable public Rust SDK. |
| Rust `core`, Agent Bus, OCLA, provider, and registry internals | Internal implementation substrate | Source only, unless a named contract above says otherwise | They are not an SDK façade and may not be imported as one. |

The checked-in Rust preview façade currently exposes local tool-shaped
operations such as read, search, symbol lookup, outline, tree, and gated raw
tool dispatch. This inventory is descriptive only. It does not commit those
method names, their return types, or the registry to a future SDK contract.

## 3. What this policy does not publish

The following names describe planned or internal work; they are not introduced
as a new public API by this policy:

- `ContextSession`, `ContextSource`, `ContextView`, `ContextPlan`, and
  `ContextReceipt` are the proposed small stable SDK primitive set. Their wire
  schema, lifecycle, fixtures, and reference integration remain prerequisites.
- Existing protocol and in-process session types are compatibility inputs, not
  a declaration that a general SDK object model is available.
- Context Profiles, first-class Context Kits, Project Context, policy, handoff,
  adaptive planning, broad framework adapters, and Cloud remain Research or
  later phases as stated in the internal authority.

## 4. Versioning and compatibility posture

### 4.1 Engine releases and published contracts

Every Engine boundary promoted beyond local implementation must identify all of
the following before release:

1. a named versioned contract or schema;
2. an owning package and maintained fixtures;
3. the minimum and tested Engine release range;
4. failure, safety, and local-degradation behavior; and
5. an explicit deprecation/migration path.

For a published wire contract, backward-compatible fields may be added only
when its schema and fixtures make their interpretation unambiguous. Breaking
wire semantics require a new versioned contract and migration path; they must
not be hidden behind a reused `v1` name or silently coerced at decode time.

The [contract deprecation process](DEPRECATION.md) supplies the current minimum
posture: announce deprecation, provide a migration window (six months for a
major contract version and three months for a minor one), then archive the old
schema. Security fixes may use a compatible patch release with an advisory.

Supported local Engine paths must not be removed without a replacement and a
documented migration window. Internal substrate and Preview interfaces do not
thereby acquire a stable ABI promise.

### 4.2 Future SDK-to-Engine compatibility

No general stable cross-language SDK compatibility matrix exists today. Before
calling an SDK stable, its release must publish a matrix that maps each SDK
release to:

- minimum, tested, and unsupported Engine versions;
- required protocol schema/contract revisions;
- supported local execution modes and Cloud-absent degradation behavior;
- fixture/conformance versions; and
- supported upgrade and deprecation paths.

The matrix is a release artifact, not an inference from a shared source tree.
SDK compatibility tests must exercise the named Engine contract rather than
reach into `rust/src/core` or an unversioned tool registry.

### 4.3 Preview and Research posture

| Surface | Current posture | Compatibility commitment |
| --- | --- | --- |
| Python v1 and its declared OpenAI Agents reference wrapper | **Preview** | Narrow, documented limits and package tests; no claim of general framework coverage or stable SDK/Engine matrix. |
| Rust `lean-ctx-sdk` in-process façade | **Preview substrate** | Experiment only; `publish = false` and no external stable API commitment. |
| General SDK object model/custom-host Embed | **Preview target** | Requires the v1 primitive contracts, fixtures, compatibility matrix, and reference demo. |
| Cloud Receipt Board, shared state, governance, and fleet control | **Research** | No Cloud interface, remote dependency, or compatibility promise is created here. |

## 5. Package, repository, and legal decision

The existing public Engine repository and its runtime remain Apache-2.0. This
policy creates no new package, repository, license, contributor agreement, or
commercial entitlement.

The workspace crate currently named `lean-ctx-sdk` is a Preview in-process
experiment and is `publish = false`. Its name is not a decision to create or
reserve a production SDK package. Likewise, existing Python packaging remains
Preview and does not decide a future production SDK namespace.

Proposed BSL or commercial SDK terms are **not active**. They must not appear in
release notes, package metadata, installation guidance, or customer claims
until external legal authority has approved them in writing and a separate
repository has its own notices and release process.

## 6. Required promotion evidence

Before promoting a new Engine interface or stable SDK pair, maintainers must
provide:

- a feature-placement decision showing Engine rather than SDK/Cloud ownership;
- an exact contract, fixtures, compatibility tests, and named owner;
- a local reference integration that preserves host control of the agent loop;
- deterministic and negative-path tests for inputs, lineage, recovery, and
  degradation where applicable;
- a compatibility matrix and deprecation plan; and
- evidence-backed status wording reviewed against the internal authority.

No source module, prototype, benchmark, or package name substitutes for this
evidence.

## 7. Remaining external decision blocker

**Blocked pending written external legal authority:** select the production SDK
repository owner/location, package namespace and names, license/BSL or
commercial terms and their scope, and contributor model/agreements. Until that
single legal/package decision is recorded, this repository may document and
test Engine contracts but must not create, market, or ship a commercial or BSL
SDK surface.
