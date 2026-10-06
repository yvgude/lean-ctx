# Agent Identity and Presence

Status: canonical V4 identity model; local durable registry and presence bridge
exist, while Team/Enterprise authority and end-to-end enforcement remain
incomplete.

## Purpose

V4 uses distinct identifiers for distinct security and lifecycle scopes. The
phrase "agent ID" alone is insufficient and must not mean a durable principal,
running process, logical session, human account and device interchangeably.

The primary distinction is:

> Agent Identity is a durable security principal. Agent Presence is an
> ephemeral running instance or session.

## Canonical identity model

| Concept | Lifetime and authority | Canonical relationship |
| --- | --- | --- |
| Durable agent identity | Persists until decommissioned; may authenticate, hold roles/capabilities and be revoked. | One durable identity may have many presence records over time. |
| Agent presence | Expires with heartbeat/session/process liveness. | Optionally binds explicitly to one active durable identity. |
| Process identity | OS-specific PID plus immutable start marker or equivalent. | Proves that a presence still refers to the same process; never becomes durable identity. |
| Logical session | Editor/CLI/agent session spanning requests and possibly processes. | Groups task attempts and presence, but does not authorize by itself. |
| User/account identity | Human or service account used for ownership, billing and login. | Owns or operates agents according to membership and policy. |
| Device identity | Registered execution device or installation. | Hosts presences and may carry device policy/attestation; not an agent or user. |
| Workspace/organization identity | Shared authorization and governance scope. | Constrains users, agents, devices, tasks, messages and data. |
| Task identity | One logical unit of work under ADR-012. | References executing identity/presence but remains stable across retries or executor changes. |

IDs are opaque, bounded and namespaced by type. Equality across different ID
types never creates a relationship. Joins require an explicit validated binding.

## Durable governed identity

Every governed agent record contains or references:

- canonical `agent_id`;
- accountable human owner or service principal;
- agent/connector type;
- Ed25519 public key and key identifier;
- role and capability claims;
- lifecycle status and creation record;
- allowed tenant, organization and workspace scopes;
- supported attestation and drift evidence; and
- key rotation, suspension, revocation and decommission history.

The lifecycle is:

```text
registered → active ⇄ suspended → decommissioned
```

Decommissioning is terminal. The record remains for audit and identifier reuse
is forbidden. Key rotation preserves the durable identity and creates explicit
old/new key history; it never silently replaces ownership or scope.

An active status is necessary but not sufficient to execute. Entitlement,
workspace membership, role/capability, task, policy, classification, region,
provider/model and budget checks still apply at admission.

## Presence and process liveness

A presence may report:

- its generated presence ID;
- optional durable identity binding;
- connector/agent type and role hint;
- project and workspace references;
- logical session and current task references;
- process identity;
- start, heartbeat and last-active times; and
- active, idle or finished status.

Presence expires and stale entries are reaped. PID alone is unsafe because
operating systems reuse it; liveness requires the recorded process start marker
or a stronger platform handle. A generated local presence ID that textually
matches a durable identity remains unrelated unless the explicit binding was
validated.

Suspended, decommissioned, missing or mismatched durable bindings make the
presence non-routable on enforced paths. Finishing a presence does not
decommission its durable identity. Losing a heartbeat does not revoke identity;
it only removes current routability.

## Accountability and offboarding

Every Enterprise agent has an accountable owner or service principal. When
SCIM or another authoritative identity provider deactivates an owner, all
dependent active agents are suspended before further execution is admitted.
Their historical records, receipts and audit trail remain available according
to retention policy.

Resume requires current owner/workspace status and explicit authorization.
Decommission is audited and irreversible. Compromised keys or identities can be
revoked independently of process cleanup.

## Attestation and drift

The current local registry records best-effort hashes of the running binary and
active role configuration at registration/heartbeat. This detects some drift
but does not resist an attacker controlling the host. It is software evidence,
not hardware attestation.

V4 attestation claims must name the measured subject, algorithm, signer,
timestamp/nonce, boot/runtime context and verification policy. Unknown,
expired, mismatched or revoked evidence fails closed where policy requires
attestation. Hardware-backed claims require measured proof on each supported
deployment target.

## Product boundaries

### Community

Community retains open task, A2A, AgentGateway and signature contracts plus
manual handoff. Local process labels and explicit keys may support those
contracts, but no Enterprise governance claim is implied.

### Pro

Pro adds local multi-agent presence and coordination for one account. Local
identity may remain device-scoped, but process reuse, stale presence and
explicit binding rules still apply.

### Team

Team adds workspace identity, membership, multiple users, shared presence,
roles, seats, cross-device/machine routing and durable workspace history.
Personal agents or context require explicit promotion into workspace scope.

### Enterprise

Enterprise adds mandatory accountable ownership, durable Ed25519 identities,
RBAC/capability claims, SSO/SCIM, offboarding, lifecycle governance, attestation,
drift policy, audit, tenant/region/classification boundaries and supported
VPC/on-premise/air-gap identity operation.

Required capability equivalents include `pro.agent_presence.local`,
`team.agent_presence.shared`, `enterprise.agent_identity.governed` and
`enterprise.agent_attestation`.

## Current implementation map

| Surface | Current role and limitation |
| --- | --- |
| `core::agent_registry` | Durable local records with owner, role, Ed25519 key, lifecycle, best-effort attestation, heartbeat, offboarding suspension and audit. It is not yet proven as Team/Enterprise authority. |
| `core::agent_identity` | Local key creation/import, signing and verification with bounded/path-safe agent IDs. Recovery-phrase and key custody require product/security review. |
| `core::agents` | Ephemeral presence, logical sessions, scratchpad, heartbeat and reaping. |
| `core::agents::bridge` | Explicit join of presence to durable identity; textual ID collisions do not join. |
| `ipc::process::ProcessIdentity` | Platform-specific process-start identity used to prevent PID-reuse resurrection. |
| `ctx_agent` and CLI agent commands | Reachability adapters; public availability is not proof of correct tier or enforcement. |
| Task/A2A/connector contracts | Consumers of identity references; they must validate current authority at every trusted ingress. |

Diary and scratchpad belong to collaboration state, not the durable identity
record. Scent identities, task IDs, session IDs, trace IDs and connector labels
remain separate types.

## Security and privacy invariants

- No implicit join by matching strings, PID, role name, hostname or account
  display name.
- Private keys never leave their approved local/secret-management boundary.
- Public keys, rotations and revocations are versioned and auditable.
- Every security-sensitive operation rechecks current lifecycle and scope;
  cached active status has a bounded lifetime.
- Identity, workspace and task authorization precede messages, handoff,
  delegation, context access and execution.
- Cross-tenant and cross-workspace identity lookup fails without existence
  disclosure.
- Audit and telemetry omit prompts, source, paths, secrets, message payloads and
  raw evidence.
- Presence expiry cannot reactivate suspended identity; identity resumption
  cannot resurrect stale presence.
- Offline identity/entitlement state is signed, bounded and reconciled on
  reconnect.

## Migration

1. Inventory every field currently named `agent_id`, process/session identifier,
   registry and key store.
2. Classify each as durable identity, presence, process, session, user, device,
   workspace or task reference.
3. Add explicit typed/versioned bindings before changing persisted records or
   public schemas.
4. Preserve legacy readers and aliases without treating untrusted legacy IDs as
   canonical principals.
5. Back up and test idempotent migration, rollback, stale-process cleanup and
   owner offboarding.
6. Remove duplicate registries only after every producer and consumer uses the
   canonical owner and old data passes compatibility gates.

## Acceptance gates

1. One authoritative durable registry and one presence registry are proven;
   every other identity representation is a typed adapter.
2. Registration, explicit binding, signing, rotation, suspension, resume,
   decommission, owner offboarding and revocation pass end-to-end tests.
3. PID reuse, stale heartbeat, ID collision, forged binding, key tamper,
   workspace mismatch and cross-tenant lookup fail closed.
4. Team proves shared presence across two users and machines with membership and
   seat enforcement.
5. Enterprise proves SSO/SCIM offboarding, RBAC/capability policy, drift denial,
   audit and supported deployment/offline behavior.
6. Packaging, migration, backup/restore, cross-platform, security, telemetry,
   entitlement and licensing gates pass.

Until those gates pass, existing registry and bridge tests are substrate
evidence, not proof of a completed governed identity product.

## Related architecture

- `docs/adrs/ADR-012-task-identity-and-lineage.md`
- `docs/architecture/decision-spine.md`
- `docs/architecture/agent-bus.md`
- `docs/architecture/subagent-contract.md`
- `docs/architecture/a2a-transport.md`
