# ADR: v4 VIA resolution

**Status:** Accepted
**Date:** 2026-09-03
**Decision:** VIA survives as a protocol name with one bounded,
non-overlapping responsibility

## Authority and evidence

- Governing input: `LEANCTX_V4_ULTIMATE_MASTER_PROMPT_FINAL.md`, SHA-256
  `90fe65b50a80d8a802c066be060ac0771ddf0fa697758a3f09ec79e2ecc62f9b`.
- Engine cut:
  `b4429b80f3bc75bab4c2d7a959c9ae81ba5a2666`.
- Accepted private Phase-0 baseline (not published), SHA-256
  `17da230ccdd8faac41c190f703522d23e5fa0558914cf2421c6eace24b6d588c`.
- Accepted private Phase-0 capability census (not published), SHA-256
  `1300c675fce0c6d980e91add46bc222ac930d312a70edb89b070b6775ed5785d`.
- Owning architecture:
  [v4 Decision Spine consolidation](ADR-v4-decision-spine-consolidation.md).

### Required provenance revalidation

The consolidation candidate `1c8e4770becec6d79e7dd47af8bd4092a732eefc`
recorded a later Engine cut, `802db61a5ca3484e5693e512f12888ee87e9075b`,
and flagged private provenance and source-of-truth issues. Neither that
historical cut nor this ADR proves the current private snapshot is verified.
Before release or a maturity claim:

- Verify the confidential protocol-provenance manifest's individual source
  digests and aggregate digest against the canonical private snapshot; reissue
  the manifest if any digest differs. Acceptance of this architectural boundary
  does not accept stale provenance.
- Identify one canonical tracked source for the Edge protocol and executable,
  and bind its exact revision in the integration evidence. Multiple working
  copies or an untracked implementation do not establish release provenance.

Observed current behavior is distinguished below from target migration work.
Exact private topology remains in the confidential census.

## Context

"VIA" has been used historically for an agent-network direction and more
recently for an Edge-to-private-service context-planning implementation. The
accepted Phase-0 census found no canonical VIA namespace in the clean Engine
base, but it did find:

- historical planning language that overlaps Agent Bus, Work Graph, A2A,
  AgentGateway, remote execution, and control-plane responsibilities;
- a later Edge↔VIA protocol snapshot containing bounded event streaming,
  content manifests, explicit content-on-demand, typed plans, and scoped agent
  identity references;
- a private Brain implementation that reconstructs a bounded metadata graph
  and returns deterministic typed reuse plans without accepting provider
  credentials;
- owner-controlled private deployment and recovery infrastructure;
- newer unaccepted working-copy changes that remain outside the evidence cut.

Exact private repository identities and accepted SHAs remain in the
confidential Phase-0 census. This public ADR states the architectural boundary
without publishing private topology or operational details.

Leaving VIA undefined would invite a second bus or orchestration stack. Deleting
the name outright would discard an implemented, provenance-recorded protocol
whose context-planning boundary is not the same as Agent Bus or A2A.


## Exhaustive VIA search inventory

The Phase-1 review searched every locally accessible evidence class required by
the governing prompt. It scanned current tracked trees and every unique blob
reachable from all local refs, branch and tag names, public Engine issues and
pull requests, private issue and merge-request results, and the dirty Engine
working tree separately. Product-specific identifiers were used to avoid
treating the English preposition "via" as architecture.

| Evidence class | Scope and result |
| --- | --- |
| Clean Engine tree and all reachable blobs | The accepted cut contains no canonical VIA product/protocol namespace. Apparent `via_` hits are ordinary wording or variable names. No product-specific identifier, VIA-named branch, or VIA-named tag was found across reachable history. |
| Engine dirty/quarantined state | Untracked VIA edge/protocol/operator/proxy seams exist in the separate dirty shared tree. They are direct working-tree evidence only, excluded from the accepted baseline, and cannot establish accepted history, release, or product maturity. |
| Public issues and pull requests | Exact GitHub searches for `LeanCTX Via`, `Edge VIA`, `edge_via`, `Via Brain`, and `VIA protocol` returned zero Engine issues and zero pull requests. Search semantics are broader than case-sensitive text, so candidates were manually classified. |
| Official SDK and Cloud cuts | Current trees and all reachable blobs contain no product-specific identifier and no VIA-named ref; neither surface defines the VIA contract. |
| Enterprise/control-plane cut | Current tree and all reachable blobs contain no product-specific identifier or VIA-named ref. Capitalized "Via" matches are English prose or UI labels, not product evidence. Private issue/MR results were likewise ordinary-language false positives. |
| Private Brain cut | All reachable blobs confirm a bounded context-planning service, an `edge_via` protocol snapshot, `ViaProtocol*` contracts, and explicit closure of the broader `Via+`/Mesh scope. Private issue/MR search returned zero VIA results. |
| Private Infra cut | All reachable blobs confirm owner staging, TLS/service authentication, isolated execution, deployment, recovery, and observability—not bus, graph, scheduler, identity, or policy ownership. Private search found two relevant merged MRs for the ordinary isolated runner and restricted staging ingress/egress. |
| Website cut | The accepted revision contains no product-specific VIA identifier and supplies no architecture authority. |

This inventory supports only the implemented context-planning meaning. Generic
agent-network, bus, and mesh language has no tracked Engine or public
issue/pull-request contract and is superseded. A broader VIA meaning requires a
new ADR and new accepted evidence.

## Decision

VIA survives **only as the protocol name for bounded Edge↔remote context
planning**.

VIA is not a customer-facing generic agent network, not the Local or Workspace
Agent Bus, not Work Graph, not A2A transport, not an OCLA capability registry,
not AgentConnector, and not the overall Control Plane. Customer-facing product
language is Context Autopilot or Execution Autopilot as appropriate.

The Edge owns trusted ingress, product entitlement, hard policy, source access,
provider credentials, provider request construction, execution, and final
outcome evaluation. A VIA planner may consume admitted metadata and explicitly
authorized bounded content, then return a typed context-plan proposal. The
Decision Spine validates and either accepts, modifies, or rejects that proposal
before execution.

```text
TaskEnvelopeV1 + admitted policy/capability refs
                  |
                  v
         Edge context candidate adapter
                  |
        bounded Edge↔VIA protocol
                  |
                  v
       remote context-plan proposal
                  |
                  v
 Decision Spine validation and local policy
                  |
                  v
 ContextPlan projection -> ExecutionPlanV1
```

This is the precise non-overlapping contract required for the "VIA survives"
option.

## Owned and excluded responsibilities

| VIA owns | VIA explicitly does not own |
| --- | --- |
| version negotiation for its Edge↔planner contract | product capability classification or entitlement |
| bounded scoped event/cursor exchange for context planning | general agent-to-agent messaging |
| content-reference manifests and digests | arbitrary project crawling or ambient source access |
| explicit, bounded, expiring content-on-demand requests | provider credentials, request headers, cookies, or opaque provider payloads |
| typed context-plan proposals and their expiry | final ContextPlan authority or execution scheduling |
| planner acknowledgement, resume, backpressure, and replay semantics | Work Graph topology, task admission, or accepted outcome |
| metadata graph and context-reuse intelligence in the private planner | durable user, agent, device, or workspace identity authority |
| plan/evidence references required to audit a proposal | general telemetry, billing, fleet control, or governance |

A request crossing this boundary must be attributable to a canonical
`TaskEnvelopeV1`. VIA-scoped event/request IDs remain protocol-local and
cannot replace `task_id`, `trace_id`, or durable agent identity.

## Relationship to canonical v4 layers

| Layer | Relationship to VIA |
| --- | --- |
| Decision Spine | Owns admission and final context/execution decision. VIA is one optional context-planning strategy provider. |
| Context Kernel | Owns local candidate gathering, policy, budget compilation, plan semantics, receipt, and local deterministic fallback. VIA consumes/returns versioned projections. |
| OCLA | Describes and invokes capabilities. A VIA planner adapter may be represented by an OCLA manifest, but VIA does not own the catalogue or scheduler. |
| ControlPlaneContract | May select or recommend strategy/provider resources. VIA is not the aggregate control plane and cannot bypass its hard policy. |
| Agent Bus | Carries authorized local/workspace coordination messages. VIA carries context-planner protocol frames only. |
| Work Graph | Owns bounded multi-agent delegation and result fusion. VIA neither creates nor schedules graph nodes. |
| A2A | Provides signed bounded agent-to-agent transport and retry/DLQ behavior. VIA does not become a second general A2A transport. |
| AgentGateway | Adapts admitted agent requests to the selected transport. It is not routed through VIA unless the payload is specifically an authorized context-plan exchange. |
| AgentConnector | Executes local agent processes. VIA never launches a connector or receives its credentials. |
| OCP | Defines open context/governance/evidence exchange. VIA may reference or project OCP objects but cannot fork their semantics. |
| Delivery Registry | Supplies content-addressed authorized delivery/dedup. VIA content references may use it; VIA does not own the delivery store. |
| Identity registries | Supply durable identity and ephemeral presence references. VIA validates references and scope; it does not mint identity. |

## Protocol shape

The accepted Edge↔VIA v1 direction contains the following capability classes.
They remain protocol projections and must converge on canonical Decision Spine
contracts.

| VIA concept | Canonical meaning | Required migration |
| --- | --- | --- |
| scope | task/project/workspace/session/agent references plus data classification | Require canonical task/trace refs and authenticated tenant/workspace scope; prohibit identity invention. |
| protocol hello | bounded version and feature negotiation | Preserve additive compatibility; unknown required features fail closed. |
| event stream | context-planning observations with ordered acknowledgement and cursor resume | Adapt to the typed learning/observation envelope; never treat delivery as accepted outcome. |
| content manifest | bounded source references, digests, classification, and token estimates | Project Context Kernel candidates; no raw content or access grant is implied. |
| content on demand | explicit short-lived request for named content chunks | Require prior policy, data-class match, item/byte/token/TTL limits, audit, and local refusal path. |
| typed plan | expiring ordered source selection/reuse proposal | Project to the canonical ContextPlan wire form; Decision Spine revalidates it. |
| multi-agent identity reference | reference to the agent/task responsible for the exchange | Replace VIA-local ownership claims with canonical durable/presence/task references. |
| invocation context binding | signed linkage among task, plan, admission, capability, policy, and source refs | Keep as evidence binding; align with protocol Decision/Execution Receipt refs. |
| invocation evidence manifest | digests and receipt references proving the inputs to an invocation | Keep as specialized evidence referenced by the canonical receipt chain. |

No VIA frame may contain an arbitrary JSON patch for a provider request. No
frame grants authority to rewrite prompts, tools, headers, credentials, model,
provider, entitlement, or policy.

## Edge and service authority

The Edge remains the policy-enforcement and data-access point:

1. establish or validate `TaskEnvelopeV1`;
2. resolve the explicit product capability and entitlement;
3. apply data, region, model, egress, and tenant policy;
4. gather local candidate metadata;
5. decide whether remote planning is allowed;
6. redact and bound the VIA request;
7. authenticate the service and verify response integrity;
8. validate plan schema, expiry, capability versions, scope, and budget;
9. compile or reject the proposal locally;
10. execute through the canonical Decision Spine and record receipts/outcome.

The remote Brain owns planner availability, bounded event journal, metadata
graph, and adaptive plan proposal. It has no authority to execute code, contact
a model provider with user credentials, mutate local configuration, access
unrequested content, or declare an outcome accepted.

When VIA is absent, unreachable, invalid, expired, or denied, explicitly
classified Community capabilities continue through the deterministic local
Context Kernel. Paid VIA-dependent capabilities follow their signed bounded
grace/offline policy; they do not silently become Community.

## Data and security boundary

Default VIA exchange is metadata-first. Allowed data is limited to
schema-listed identifiers, digests, classifications, bounded token/count
metadata, capability/version refs, policy/admission refs, timing, cursor, and
content-free outcome signals.

Raw content crosses only through an explicit content-on-demand request that is:

- authorized for the exact task, source reference, recipient, and purpose;
- bounded by item, chunk, byte, token, and short TTL limits;
- encrypted in transit and authenticated in both directions;
- denied for local-only/secret classifications unless an explicit higher-level
  policy permits that exact transfer;
- recorded through content-free evidence and deletion/retention policy;
- revocable without disabling the deterministic Community path.

Provider API keys, service credentials, cookies, authorization headers,
arbitrary environment variables, raw shell commands, opaque tool payloads, and
unbounded prompt/result bodies are prohibited.

The service must default to loopback or an authenticated private ingress.
Production exposure requires TLS, service authentication, replay defense,
rate/backpressure limits, bounded journal retention, restore evidence, secret
management, and content-free observability. Existing private deployment
evidence is not a general availability claim.

## Protocol ownership and licensing

The shared Edge↔VIA contract belongs in the canonical protocol source governed
by the Engine's contract-ownership ADR. A private Brain may vendor an exact
Apache Trust Core snapshot with recorded origin and integrity, but it may not
silently change shared field meaning, signing bytes, bounds, or fixtures.

Private planner implementation, learned models, operations, and deployment
automation remain private service concerns. Shared protocol types and
conformance fixtures retain their explicit open classification. Repository
legal ownership/provenance remains a separate release gate; this ADR does not
invent legal certainty.

## Historical-name resolution

The following historical meanings are retired:

| Historical VIA meaning | Canonical replacement |
| --- | --- |
| generic local agent network | Pro Local Agent Bus |
| shared multi-user agent network | Team Workspace Agent Bus |
| agent task graph or scheduler | Bounded Work Graph under Decision Spine |
| generic remote agent transport | A2A |
| capability/plugin network | OCLA capability fabric |
| local child-agent execution | AgentConnector |
| enterprise identity/fleet policy | Enterprise governed identity and Control Plane |
| context delivery/cache | Delivery Registry plus Context Kernel stores |
| remote pairing/session establishment | durable identity plus Control Plane policy, carried through AgentGateway/A2A |
| remote task execution | Decision Spine admission → Work Graph scheduling → AgentConnector execution → AgentGateway/A2A transport, governed by Control Plane policy |
| Via+ or Mesh umbrella | retired/closed names unless a later ADR assigns a new non-overlapping contract |

Documentation, code comments, schemas, and marketing must use "VIA" only for the
bounded context-planner protocol. Old materials with another meaning move to an
explicit archive and carry a superseded notice.


### Remote pairing and execution

Remote pairing authenticates durable agent/service identities, checks
workspace/tenant scope through Control Plane policy, and negotiates a bounded
AgentGateway/A2A channel. It does not use VIA event or plan IDs as identity.

Remote execution follows one exact ownership chain:

```text
Decision Spine admission and policy
  -> BoundedWorkGraph task/node and budget
  -> AgentConnector executor
  -> AgentGateway over bounded A2A transport when remote
  -> ExecutionReceiptV1 and AcceptedOutcomeV1
```

VIA may propose the context portion before that chain executes. It does not
pair agents, create graph nodes, launch processes, carry generic task messages,
or govern the remote executor.

## Migration plan

### Stage 1: freeze and map

- freeze accepted Edge↔VIA v1 bytes and conformance fixtures;
- record exact private snapshot provenance in the confidential evidence pack;
- label all historical bus/network/mesh documents superseded;
- prohibit new generic VIA modules or endpoints.

### Stage 2: canonical lineage and admission

- add canonical task, trace, policy/admission, capability, and data-class refs
  to the VIA projection using additive/versioned evolution;
- make entitlement and hard-policy evidence mandatory for paid remote planning;
- reject unknown capability/version and scope mismatches.

### Stage 3: ContextPlan convergence

- define the stable Context Kernel ContextPlan wire projection in Phase 4;
- adapt VIA typed plans to that projection;
- retain a compatibility reader for accepted VIA v1;
- reject plans that exceed local budget, classification, expiry, or source set.

### Stage 4: observation and evidence convergence

- project VIA events into the typed Decision Spine observation envelope;
- reference invocation context binding/evidence manifests from
  `ExecutionReceiptV1`;
- keep transport success distinct from outcome acceptance;
- feed learning only after canonical outcome evaluation.

### Stage 5: naming and code cleanup

- rename generic VIA agent-bus/scheduler references to their canonical layer;
- keep a narrowly named `edge_via` compatibility adapter while v1 is supported;
- archive experimental Via+/Mesh material;
- delete duplicate identity, transport, scheduling, or delivery state only
  after replay/conformance parity and rollback evidence.

## Failure and rollback behavior

- malformed, oversized, unknown-version, expired, replayed, or
  classification-incompatible VIA messages fail closed;
- a rejected remote proposal cannot mutate local context or execution state;
- partial event delivery resumes from an acknowledged cursor and remains
  idempotent;
- content requests expire and cannot be broadened by the service;
- the Edge can disable the remote adapter without migrating local source data;
- rollback selects the deterministic local planner or an earlier compatible
  protocol adapter and records the decision;
- service failure never authorizes a paid/unknown capability.


## Validation evidence

- Current trees and every unique Git blob reachable from all local refs were
  searched with product-specific identifiers. Engine, SDK, Cloud, and
  Enterprise/control-plane contain no match or VIA-named ref; Brain and Infra
  contain only the bounded planning/protocol and deployment responsibilities
  described above.
- Exact public Engine searches returned zero VIA issues and pull requests.
  Private search returned zero Brain result, two relevant merged Infra MRs, and
  only ordinary-language false positives in Enterprise/control-plane and Cloud.
- Unmerged/quarantined Engine VIA seams are direct dirty-working-tree evidence,
  not accepted product behavior.
- The accepted private Brain cut was inspected by exact commit through Git
  object reads. Its README limits scope to private P1 owner staging with
  synthetic/founder data, and its Edge↔VIA contract states that Edge validates
  and executes typed plans.
- The accepted protocol exposes bounded version negotiation, scoped event
  frames, acknowledgements/cursors, manifests, explicit content-on-demand,
  typed plans, classifications, and identity references. It prohibits opaque
  provider-request rewriting and provider credentials.
- The accepted private Infra cut was inspected separately and owns deployment,
  TLS/service authentication, runner isolation, backup/restore, and synthetic
  proof—not product scheduling or agent messaging.
- Exact accepted cuts are recorded in the private census whose hash appears
  above. Newer dirty private state and unmerged Engine seams remain excluded.
- The layer-by-layer table above was reconciled with the Decision Spine source
  map and all named VIA neighbors.

## Unresolved implementation gates

This ADR narrows the name; it does not make VIA generally available. Later
phases must still provide:

- a merged Engine adapter based on canonical TaskEnvelope, ContextPlan,
  decision, receipt, outcome, identity, and capability references;
- the Phase-2 capability/entitlement classification and fail-closed admission;
- the Phase-4 ContextPlan projection and compatibility/conformance fixtures;
- authenticated service discovery, TLS, replay defense, tenant/project scope,
  egress controls, bounded retention/deletion, and disaster-recovery proof;
- deterministic local fallback and signed bounded paid offline/grace behavior;
- accepted remote end-to-end outcome evidence and public-claims review;
- an archive/naming scan proving generic VIA bus/network/mesh meanings no
  longer appear as live architecture.

## Consequences

The existing VIA protocol and private planner investment remains usable without
creating another agent platform. The public product taxonomy stays coherent:
Agent Bus coordinates, Work Graph delegates, A2A transports, OCLA invokes, the
Context Kernel plans, and the Decision Spine decides.

The narrower contract requires migration of VIA-local identity and plan fields
to canonical references. Historical "agent network" language must be retired,
and private deployment proof must not be advertised as Team/Enterprise GA.

## Alternatives rejected

### VIA as an umbrella for the Agent Bus stack

Rejected because the implemented Edge↔VIA context-planning protocol has a
different data model, trust boundary, and availability model from local or
workspace messaging. Renaming the entire bus stack VIA would preserve ambiguity
rather than remove it.

### VIA as a second orchestration or control plane

Rejected because Task Spine, Decision Spine, OCLA, Work Graph, A2A, and
ControlPlane already own those responsibilities. A second authority would make
policy, scheduling, and receipt lineage inconsistent.

### Retire every VIA artifact immediately

Rejected because the bounded protocol and private metadata planner provide a
distinct context-planning function with accepted provenance. Immediate removal
would discard working assets before canonical ContextPlan migration exists.

## Phase-1 acceptance gate

This decision is accepted only with the Decision Spine ADR. Review must confirm:

- exactly one VIA option is chosen;
- the retained contract is limited to remote context-plan proposal;
- Edge and Decision Spine retain admission, policy, execution, and outcome
  authority;
- VIA is explicitly separated from OCLA, Agent Bus, Work Graph, A2A,
  AgentGateway, AgentConnector, Control Plane, OCP, identity, and delivery;
- private topology and secrets are absent from this public artifact;
- historical generic VIA meanings have a migration/archive plan;
- Phase 2 and Phase 4 cannot create a parallel VIA capability or plan model.
