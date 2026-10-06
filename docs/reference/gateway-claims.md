# Information gateway — claims catalog

Every public statement about the information gateway is listed here with
its maturity and the evidence behind it. Website and docs may only say what
this catalog lists as **Available** or **Preview**, in the scope given.
Anything not listed is not claimed.

- **Available**: shipped, on by default where stated, proven by named tests,
  usually in the [gateway proof registry](../contracts/gateway-proof-v1/README.md).
- **Preview**: usable, but the contract may change in minor releases.
- **Research**: being evaluated, not offered.

## Available — Community (open source)

| Claim | Scope | Evidence |
|---|---|---|
| Secrets, checksum-verified PII and prompt-injection detectors run by default before context reaches the model | MCP reads, recovery paths and derived stores; proxy egress on the OpenAI, Anthropic and Gemini rails | Scenarios 6–22, 41; `case_*` tests in `core::context_admission` |
| A credential in a prompt is masked before it leaves through the proxy | proxy-routed hosts (see the host coverage matrix) | Scenario 10; `proxy::host_journey_tests` (Claude Code, Codex) |
| Restricted content never goes to a remote model | proxy egress and MCP reads | Scenarios 43 and 46 |
| Detector coverage is explicit: partial or failed inspection is never reported as clean; governed mode blocks it | all admission paths | Scenarios 19–22, 58 |
| Recovery (`ctx_expand`, `ctx_retrieve`, archive, tee, CCR, session cache, handoffs, context packages) re-admits under the current policy | listed paths | Scenarios 29–36; bypass paths in the registry |
| Derived stores (BM25, knowledge, graph, session memory, provider artifacts) never hold raw secrets or restricted files | listed stores | Scenarios 37–40; `e3_*` store tests; graph and memory bypass tests |
| Every bypass path of the gateway contract is attacked by a named test: nested dispatch, all read modes, recovery paths, stores, provider cache, MCP bridge, handoffs, snapshots, in-band expansion, and the OpenAI, Anthropic, Gemini and WebSocket rails | 26 paths | `docs/contracts/gateway-proof-v1/registry.json`; CI job `gateway-proof` |
| A clean round shows a compact metric (`LeanCTX 🛡 X → Y ↓Z%`) computed from its receipt | `lean-ctx inspect` | Scenario 52 |
| One decision receipt per round, with policy digest, measured tokens, security counts (never values) and the delivered-context digest; tampering is detected | MCP rounds and proxy requests | Scenarios 44, 47–51, 59 |
| `lean-ctx inspect` explains the current round: why something was delivered, redacted or withheld | current round | `cli::inspect_cmd` tests |
| Per-host coverage is reported honestly: `enforced`, `partial`, `not_observable` or `unsupported`, never `observed` | 38 hosts | `lean-ctx doctor`; `docs/reference/generated/host-coverage.md` |

## Preview

| Claim | Scope | Evidence |
|---|---|---|
| Egress admission and typed decision receipts (`ContextPrincipal`, `ContextDestination`, `ContextDecision`, `SecuritySignal`) in the Python, TypeScript, Go, Rust, JVM and .NET SDKs | `leanctx-gateway-preview` 0.1 | SDK conformance fixtures and the same live-Engine journeys, green in all six languages (JDK 21, .NET 8) |
| Reference journey "Fix the production login issue" with a measured HUD line | SDK example | `examples/gateway_login_journey.py`; reference run 7.4k → 1.6k, 2 credentials redacted, 1 source blocked, 7/21 sources used |

## Available — Enterprise (commercial)

| Claim | Evidence |
|---|---|
| Destination governance (classification × model × provider × region), fail-closed without the Engine | Enterprise CISO demo on real processes (Enterprise repository) |
| Approver release for exactly one request (four eyes, single use, time-bounded) without storing content | CISO demo steps 2–4 |
| Every governed egress decision is recorded in the tenant's hash-chained audit log before dispatch, with an exportable SIEM chain (NDJSON) | CISO demo step 7 |

## Not claimed (open)

No entry of the proof registry is open. Claims beyond the registry's scope
need their own evidence before they are made. For example, relevance for
plans over the whole repository rather than explicit sources: the floor
applies to explicit-source plans only.

## Research

- Semantic detectors (ONNX, Prompt Guard, Laya) behind the detector seam. Not
  shipped.
- Org-wide policy simulation and shadow mode.
