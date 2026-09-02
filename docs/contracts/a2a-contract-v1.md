# A2A Contract v1

> **Status: research contract — not an available multi-agent product surface.**
> The document records an internal coordination design. LeanCTX is **The Context
> SDK for AI Agents**, an available local context layer for existing agents;
> generic orchestration, shared project context, and agent-building are
> deferred. Current scope and status are governed by
> `docs/internal/README.md` (internal, not in this repository).

GitLab: `#2319`

## Ziel

Ein **versionierter Agent↔Agent Vertrag** (A2A), der Multi‑Agent‑Koordination als Infrastruktur absichert:

- **bounded**: Outputs/Stores wachsen nicht unkontrolliert.
- **deterministisch**: stable ordering + klare Semantik für Sichtbarkeit/TTL/Transitions.
- **privacy-safe**: Private Messages sind nur Sender/Empfänger sichtbar; Exports sind redaction-safe.
- **auditable**: Export‑Artefakte werden im Evidence Ledger referenziert.

## Version (SSOT)

- `leanctx.contract.a2a_snapshot_v1.schema_version=1`
  - SSOT: `CONTRACTS.md`
  - Runtime: `rust/src/core/contracts.rs`

## Datenmodelle (Runtime)

### Messages / Scratchpad

- **Runtime structs**:
  - `rust/src/core/a2a/message.rs` (`A2AMessage` — Privacy/Priority/TTL Semantik)
  - `rust/src/core/agents.rs` (`ScratchpadEntry` als persisted Scratchpad)

**Type Alignment** (seit v1.1):

`ScratchpadEntry` und `A2AMessage` sind feld-kompatibel mit bidirektionalen `From`-Konvertierungen:

| ScratchpadEntry | A2AMessage | Mapping |
|---|---|---|
| `message: String` | `content: String` | `message` ↔ `content` |
| `category: String` | `category: MessageCategory` | `String` ↔ `MessageCategory::parse_str()` / `.to_string()` |
| `task_id: Option<String>` | `task_id: Option<String>` | direkt |
| `project_root: Option<String>` | `project_root: Option<String>` | direkt |
| `expires_at: Option<DateTime>` | `expires_at: Option<DateTime>` | direkt |

Alle neuen Felder sind `#[serde(default)]` für backward compatibility.

**MUST Semantik**:

- **Privacy**:
  - `public` / `team` ⇒ sichtbar (subject to routing)
  - `private` ⇒ sichtbar nur für Sender oder expliziten Empfänger
  - Private Messages **müssen** ein `to_agent` besitzen (keine private broadcasts).
- **TTL**:
  - `ttl_hours>=1` ⇒ `expires_at` wird gesetzt.
  - Expired Messages sind nicht sichtbar und werden bei Reads/Posts deterministisch bereinigt.
- **Project Scope**:
  - Wenn ein Project Root im Tool‑Kontext aktiv ist, sind nur Messages mit exakt passendem `project_root` sichtbar.

### Tasks

- **Runtime**: `rust/src/core/a2a/task.rs`
- **MUST**:
  - `TaskState` ist eine State Machine; ungültige Transitions werden abgelehnt.
  - Terminal states: `completed|failed|canceled`.

### Cost Attribution (local-first)

- **Runtime**: `rust/src/core/a2a/cost_attribution.rs`
- **MUST**:
  - Costs werden pro Agent und pro Tool aggregiert.
  - `cached_tokens` werden unterstützt (separat aggregiert und im Pricing berücksichtigt, wenn vorhanden).

### Rate Limiting (fairness)

- **Runtime**: `rust/src/core/a2a/rate_limiter.rs`
- **MUST**:
  - token-bucket Limits auf **global**, **agent**, **tool** Ebene.
  - Bei Limit wird `retry_after_ms` zurückgegeben.

**Env Overrides** (optional):

- `LEAN_CTX_RATE_LIMIT_GLOBAL_PER_MIN` (alias: `LCTX_RATE_LIMIT_GLOBAL_PER_MIN`)
- `LEAN_CTX_RATE_LIMIT_AGENT_PER_MIN` (alias: `LCTX_RATE_LIMIT_AGENT_PER_MIN`)
- `LEAN_CTX_RATE_LIMIT_TOOL_PER_MIN` (alias: `LCTX_RATE_LIMIT_TOOL_PER_MIN`)

## Tool Surface

### `ctx_agent`

- **Runtime**: `rust/src/tools/ctx_agent.rs`
- **Dispatcher**: `rust/src/server/dispatch/session_tools.rs`

**Post (Messages)**:

- `action=post`
- Optional: `privacy=public|team|private`, `priority=low|normal|high|critical`, `ttl_hours=<u64>`
- `privacy=private` erfordert `to_agent`.

**Read**:

- `action=read` liest nur project-scoped, privacy‑enforced unread Messages.

**Export (A2A Snapshot v1)**:

- `action=export`
- `format=text|json` (default: json)
- `write=true` schreibt `.lean-ctx/proofs/a2a-snapshot-v1_<ts>.json`
- `privacy=redacted|full`
  - `full` ist nur möglich, wenn Redaction für die aktive Role deaktiviert ist (Admin‑Semantik).

### `ctx_task`

- **Runtime**: `rust/src/tools/ctx_task.rs`
- Actions: `create|update|list|get|cancel|message|info`

## Export Artefakte & Evidence

- **Artefakt**: `.lean-ctx/proofs/a2a-snapshot-v1_<ts>.json`
- **Evidence ledger key**: `proof:a2a-snapshot-v1`
- **Redaction**: Exports sind standardmäßig redacted.

## Selektives Routing (Phase 1)

- **TopicFilter**: Agents können Events nach `kinds`, `actors`, `min_consistency`, und `agent_id` filtern.
- **Directed Events**: Events mit `target_agents` sind nur für gelistete Agents sichtbar.
- **Filtered Subscriptions**: `subscribe_filtered()` liefert nur passende Events (spart Tokens).
- **Runtime**: `rust/src/core/context_os/context_bus.rs` (`TopicFilter`, `FilteredSubscription`)
- **Poll-Endpoint**: `ctx_agent(action=poll_events)` — cursor-basiertes Polling mit Filtern.

## Transport Layer (Phase 2)

- **TransportEnvelopeV1**: Versioniertes, signiertes Wrapper-Format für Cross-Machine Transport.
  - HMAC-SHA256 Signatur für Integrität
  - `AgentIdentityV1` mit daemon_fingerprint
  - Content Types: `handoff_bundle`, `context_package`, `a2a_message`, `a2a_task`
- **Runtime**: `rust/src/core/a2a_transport.rs`
- **CLI**: `lean-ctx pack send/receive` für file- und HTTP-basiertes Senden/Empfangen.
- **HTTP Endpoints**:
  - `POST /v1/a2a/handoff` — Empfängt TransportEnvelope
  - `GET /v1/a2a/agent-card` — Agent Card für Discovery
  - `GET /.well-known/agent.json` — Standard-Pfad (A2A v1.0)
  - `POST /a2a` — JSON-RPC 2.0 Endpoint

## Google A2A Kompatibilität (Phase 3)

- **Agent Card v1.0**: Publiziert unter `/.well-known/agent.json` mit `provider`, `documentationUrl`, `skills` mit `inputModes/outputModes`.
- **JSON-RPC 2.0**: `tasks/send`, `tasks/get`, `tasks/cancel` gemapped auf interne TaskStore.
- **Runtime**: `rust/src/core/a2a/a2a_compat.rs`

## Security & Privacy

- Redaction‑Pipeline gilt für alle Exports/Proof‑Artefakte.
- A2A Snapshot ist bounded (agents/messages/tasks/diary capped) und project-scoped.
- TransportEnvelope unterstützt HMAC-SHA256 Signatur für Integrität.
- Private Messages erfordern `to_agent` (keine private broadcasts).
- Directed Events sind nur für gelistete Agents sichtbar.
