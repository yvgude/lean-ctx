# Requirements Traceability Matrix — v1

Maps design requirements (Pillars, Waves, Gates) to delivered implementation.

## Pillar → Phase Mapping

| Pillar | Description | Delivered By | Evidence |
|---|---|---|---|
| P0 | IST-Hygiene | P0 | BanditStore, Gotcha, Double-Pull fixed |
| P1 | Canonical Token Envelope | E12 | Payload types, Wire, Golden Traces |
| P2 | Context Kernel | E13 | `ContextPolicy::candidate_violation_at` (admission before selection), ReceiptV1 |
| P3 | Quality Lab + Policy | E7, E10, E15 | CLI/API/MCP surface, PDP/PEP, Fail Matrix |
| P4 | Trait Adoption | E11 | `gateway_server` + `http_server` Strangler |
| P5 | Unified Ledger | E2, E14 | Attribution, `reconcile_strict`, export/verify |
| P6 | OSS Crate Separation | E18 | Evaluated → decided no split (`p6-evaluation.md`) |
| P7 | Wire & SDKs | E11, E12, E17, E18 | SDKs synced, Contract Pack, Certification |
| P8 | Model Router | E15 | Policy Override, PEP enforced |
| P9 | Forward-Path Activation | E9 | Shaping, WebApp, HardenedClient |
| P10 | AI Value Gate | — | Deferred to E21 (`lean-ctx-enterprise`) |
| P11 | A2A & Agent Fabric | E4, E16 | RemoteTransport, Chain Budgets, Capsule |

## Wave → Completion Status

| Wave | Name | Completion | Key Phases |
|---|---|---|---|
| W0 | Reality Baseline & Governance | ~60% | P0, E5, E19 (in progress) |
| W1 | Contract Kernel & Envelope | ~90% | E11, E12 |
| W2 | Observe Bus & Evidence | ~85% | E2, E13, E14 |
| W3 | Compression Safety & Quality Lab | ~75% | E7, E9, E10, R16 |
| W4 | Universal Data Plane | ~85% | E3, E8, E9, E13, R16 |
| W5 | Identity, Policy & Control Plane | ~60% | E15, R16 |
| W6 | Wire, SDKs & Ecosystem | ~78% | E17, E18 |
| W7 | Control Dimensions | ~78% | E4, E14, E15, E16, R16 |
| W8 | Enterprise Security & Operations | ~25% | E8 (partial) |
| W9 | AI Value Gate & Commercial | ~15% | — |
| W10 | Pilot & GA | ~5% | — |

## Gate → Readiness Status

| Gate | Status | Blocking Issues |
|---|---|---|
| G0 | Partial | Secret Rotation (E19), Cloud-CI |
| G1 | PASS | Verifier 18/18 (E18) |
| G2 | Partial | E2E Trace universal |
| G3 | Near | Quality-Budget CI |
| G4 | Near | G4 metrics measurement |
| G5 | Partial | Proxy PEP wiring |
| G6 | Near | Go Conformance in CI |
| G7 | Partial | Combined Attribution E2E |
| G8 | Not ready | PRR, Chaos/DR |
| G9 | Not ready | Invoice→Evidence |
| G10 | Not started | 2nd deployment |
