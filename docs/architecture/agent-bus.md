# Agent Bus

Status: V2 target architecture; current code and capability IDs require migration
and end-to-end acceptance. This document is not a release availability claim.

## Purpose

The Agent Bus coordinates context and bounded work among supported agents. It
serves LeanCTX's context lifecycle; it is not a generic chat system, workflow
builder, agent creator or replacement for A2A, Work Graph or OCLA.

V4 keeps five coordination styles distinct:

1. message coordination through targeted/broadcast messages, scratchpad and
   handoff;
2. stigmergic coordination through low-token Scent Field signals;
3. ownership coordination through task/path/symbol claims and leases;
4. context coordination through handoff, shared knowledge, deduplication and
   Delivery Registry references; and
5. execution coordination through the bounded Work Graph.

The Bus may connect those styles, but does not collapse them into one message
queue or become their semantic owner.

## Boundaries

| Surface | Product tier | Boundary |
| --- | --- | --- |
| OCLA event bus | Community / Trust Core | Internal bounded runtime events for observability and interoperability; not the commercial Agent Bus. |
| AgentGateway and A2A contracts | Community / Trust Core | Open typed interoperability and secure task/context/evidence transport; not live collaboration by themselves. |
| Manual handoff/export/import | Community | Honest point-to-point portability, signed bundles and explicit context movement. |
| Local Agent Bus | Free | Complete bounded local coordination without payment. |
| Workspace Agent Bus | Free Team / Cloud Scale | Complete small-team collaboration within configurable limits; paid managed scale beyond the allowance. |
| Governed Agent Bus | Enterprise | Organization-authorized identity, policy, audit and deployment control. |

Historical Apache-licensed v3 collaboration remains available under its
published terms. V4 monetization comes from managed scale, hosted operations, organizational
governance and SDK/OEM rights;
it does not revoke historical rights.

## Community surfaces

Community retains:

- the open OCLA event contract and required runtime bridge;
- AgentGateway, AgentEnvelope, AgentCard and bounded A2A schemas;
- explicit `ctx_handoff` and context/session export/import;
- manual `.ctxpkg` and snapshot transfer; and
- public conformance types needed for interoperability and audit.

Free users also receive the complete useful bounded Agent Bus experience.
Public primitives and optional private/free intelligence may jointly supply it;
free use does not imply public source for every optimization.

## Free Local Agent Bus

Local coordination is free and supports multiple agents concurrently. It includes:

- discovery, registration, ephemeral presence, heartbeat, status and health;
- targeted and broadcast messages, bounded scratchpad, read/poll and sync;
- automatic handoff, briefing packs and structured sub-agent returns;
- local diary and cross-agent knowledge sharing;
- task/path/symbol claims and ownership leases;
- Scent Field claim/done/friction/decay signals;
- Delivery Registry deduplication, reference serving and safe expansion;
- supported local A2A send/get/cancel and signed capsule relay;
- bounded Work Graph integration, child receipts and result fusion; and
- agent-chain cost, accepted-path and waste attribution.

Technical limits protect capacity, latency, cost and safety. They must not
artificially reduce Free to one agent at a time or make payment a prerequisite.

## Team Workspace Agent Bus

Free Team extends the local primitives to a securely authenticated workspace.
The initial configurable target is one workspace and up to five human members;
it is an allowance default, not a permanent license restriction. It includes:

- multiple human users, membership, basic roles and configurable allowance enforcement;
- workspace identity and shared authoritative context;
- hosted or self-hosted durable channels and presence;
- cross-device, cross-machine and cross-user coordination;
- shared task graph, leases and worktree/branch ownership metadata;
- personal-to-team context promotion with explicit policy;
- shared agent/task history and scheduler observations;
- team-wide cost/outcome attribution; and
- a Team dashboard.

Workspace scope is authenticated on every read, write, message, claim, lease,
handoff, reference and graph operation. Local-account records do not become
team-visible merely because their content hashes match.

## Enterprise Governed Agent Bus

Enterprise adds organization authority:

- durable Ed25519 agent identity with accountable human/service owner;
- capability/role claims, revocation, drift detection and supported attestation;
- SSO/SCIM, offboarding and RBAC;
- tenant, classification, region, model/provider and retention policy;
- policy-controlled messaging and delegation;
- compliance audit and organization-wide cost/outcome attribution;
- VPC, on-premise, air-gapped and governed relay deployment; and
- offline entitlements and organization scheduler/control-plane integration.

Hardware attestation is never claimed unless measured on the deployed target.
Enterprise transport does not weaken the open A2A contract or local fail-closed
policy.

## Canonical implementation roles

Current implementation families are inputs to consolidation, not proof that
the product boundary is complete:

| Current family | Canonical role |
| --- | --- |
| `core::agents` and `ctx_agent` | Local registration, presence, messages, scratchpad, diary, sync and task-facing bridge. |
| `core::scent_field` | Bounded decaying stigmergic signal store; payload-light hints, not authoritative messages. |
| `core::agent_lease` | Bounded payload-free local resource ownership; not durable Team authority. |
| `core::subagent_contract` | Deterministic briefing and structured-return contract. |
| OCLA Delivery Registry | Cross-agent delivery/dedup primitive under project/account/classification policy. |
| `core::ocla_bus` | Community runtime event backbone, explicitly separate from the collaboration Bus. |
| A2A and AgentGateway | Signed transport and interoperability boundary. |
| Work Graph | Bounded execution topology, budgets, cancellation and fusion. |
| Durable identity registries | Agent/device/workspace identity references and revocation; presence is not identity. |
| Control Plane | Team/Enterprise entitlement, policy and hosted coordination authority. |

The public `ctx_agent` tool must not be treated as proof of correct V4 tiering.
Reachability, entitlement checks, storage scope, migration and packaging must be
verified at every entry point.

## Existing capability IDs requiring V2 migration

The following legacy IDs must be reconciled against the executable registry.
The `pro.*` prefix is not evidence that payment is required under V2. Preserve
compatibility through an explicit mapping and classify price, source visibility,
and runtime delivery independently:

```text
trust.ocla_event_bus
trust.a2a_contracts
trust.manual_handoff

pro.agent_bus.local
pro.agent_presence.local
pro.agent_handoff.automatic
pro.agent_knowledge.local
pro.agent_leases.local
pro.work_graph.local
pro.result_fusion
pro.execution_attribution

team.agent_bus.workspace
team.work_graph.shared
team.agent_presence.shared
team.context.shared
team.leases.shared

enterprise.agent_identity.governed
enterprise.agent_attestation
enterprise.agent_bus.governed
enterprise.execution_policy
```

If the repository's canonical naming differs, an explicit stable mapping is
required. Entitlement and CI classification gates must reject mis-tiered paths.

## Security and reliability invariants

- Authenticate sender, recipient, tenant/workspace and durable identity where
  the trust boundary requires it.
- Enforce classification, region, retention, capability, budget and egress
  policy before delivery or delegation.
- Bound message size, queue size, fan-out, polling, history, TTL and retries.
- Preserve idempotency, replay protection and task/trace lineage.
- Keep presence ephemeral and identity durable; never promote a process PID to
  organization identity.
- Claims and leases expire or release safely and cannot authorize unrelated
  mutations.
- Content hashes never permit cross-project, cross-account or cross-tenant
  disclosure.
- Unknown external effects remain pending/unknown until reconciled.
- Receipts prove execution evidence; accepted outcome requires its own contract.
- No telemetry contains prompts, source, paths, secrets or message payloads.

## Acceptance gates

This architecture is complete only when:

1. Free/Free Team/Cloud Scale/Enterprise reachability matches the capability registry
   in CLI, MCP, HTTP, packages and services.
2. Free proves two supported local agents coordinating messages, ownership,
   context dedup and bounded work without payment.
3. Free Team proves two users on distinct machines sharing one authorized workspace,
   with seats, roles, durable channels, leases and task history.
4. Enterprise proves identity revocation, owner offboarding, RBAC, policy denial,
   audit and at least one supported governed deployment topology.
5. Replay, duplicate delivery, stale presence, lease expiry, cancellation,
   disconnect, restart, partition and storage-capacity failures remain bounded
   and fail closed.
6. Historical migration and paid-to-free downgrade preserve export and the
   bounded Free experience without exposing cross-tenant state.
7. Cost/outcome attribution, telemetry, packaging, licensing, security and
   cross-platform gates pass.

Until those gates pass, the current local collaboration code is implementation
substrate, not evidence that the V2 Agent Bus is release-ready.

## Related architecture

- `docs/architecture/decision-spine.md`
- `docs/architecture/ADR-v4-via-resolution.md`
- `docs/architecture/a2a-transport.md`
- `docs/architecture/work-graph.md`
- `docs/architecture/stigmergic-coordination.md`
- `docs/architecture/cross-agent-delivery.md`
- `docs/architecture/subagent-contract.md`
- `docs/architecture/agent-identity.md`

## V2 intelligence and paid-scale boundary

The public layer owns message schemas, basic transport, claims, leases,
reference coordination and receipts. New learned delegation, agent selection,
handoff timing and redundancy prediction may live in the private/free runtime.
Public behavior must remain deterministic and useful when that runtime is absent.

Free Team includes shared context, decisions, knowledge, presence, bounded
work, basic history, provenance, conflict handling, a basic value dashboard and
export. Paid conversion follows additional workspaces/members, larger hosted
storage/index, remote execution, longer retention, managed reliability and support.
