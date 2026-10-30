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
| Recovery (`ctx_expand`, archive, tee, CCR, session cache, handoffs, context packages) re-admits under the current policy | listed paths | Scenarios 29–36; bypass paths in the registry |
| Derived stores (BM25, knowledge, graph, session memory, provider artifacts) never hold raw secrets or restricted files | listed stores | Scenarios 37–40; `e3_*` store tests; graph and memory bypass tests |
| Every bypass path of the gateway contract is attacked by a named test: nested dispatch, all read modes, recovery paths, stores, provider cache, MCP bridge, handoffs, snapshots, in-band expansion, and the OpenAI, Anthropic, Gemini and WebSocket rails | 26 paths | `docs/contracts/gateway-proof-v1/registry.json`; CI job `gateway-proof` |
| A clean round shows a compact metric (`LeanCTX 🛡 X → Y ↓Z%`) computed from its receipt | `lean-ctx inspect` | Scenario 52 |
| One decision receipt per round, with policy digest, measured tokens, security counts (never values) and the delivered-context digest; tampering is detected | MCP rounds and proxy requests | Scenarios 44, 47–51, 59 |
| `lean-ctx inspect` explains the current round: why something was delivered, redacted or withheld | current round | `cli::inspect_cmd` tests |
| A receipt proves what the gateway **prepared**: the decisions, the policy digest and the digest of exactly the bytes released to a channel, stored before a byte leaves. It does not prove a hand-over; a V1 `delivered` outcome is shown as recorded at preparation | MCP, CLI, proxy | `cli::inspect_cmd::delivered_bytes_lead_back_to_their_receipt` |
| Where protection is active (policy pack, `governed`/`sovereign`, unloadable configuration; for the proxy also a destination policy) the receipt is mandatory: every result with bytes is receipted, with a trace sidecar and one signed audit-trail entry for the pair, or it is not released (`receipt.not_persisted`); a receipt whose sidecar or anchor failed is shown as a failed preparation, never as a delivery | MCP results, CLI output through the shared tool run, proxy egress | `server::call_tool::gateway_receipt` test; `cli::common` and `cli::inspect_cmd` tests |
| What the Engine observed after preparation is a separate record: `handed_over` after a successful write and flush of exactly the prepared bytes (CLI), `returned_to_transport` (MCP), `sent_upstream` with the response status (proxy); otherwise `unconfirmed`. The observation is a local, unsigned record bound to its own receipt — never a signed delivery proof, never matched across calls by content | CLI output written through the shared observed writer; MCP; proxy egress lifecycle | `cli::common::a_protected_cli_release_is_receipted_with_the_written_bytes` |
| One policy snapshot per call, taken at its entry, decides release, receipt duty and trace of that call | MCP calls, CLI tool runs | `cli::common` test (configuration changed mid-run) |
| The trace sidecar adds what a V1 receipt cannot say: real transport and endpoint, counts by class, the project-relative path of a delivered project file (a withheld source only as `withheld`), and the recovery ids the prepared bytes carry — no content | protected mode | `server::call_tool::gateway_receipt` and `cli::common` tests |
| An Engine-interface answer (`lean-ctx engine <operation>`) is one receipted delivery of the project its operation works on — named on the command line, the working directory the operation selects, or the receiving root of the operator's settings: that project's protection decides (a policy pack included), the receipt binds exactly the document's bytes and is listed under that project, and without a mandatory receipt nothing is written. Operations that work on no project are listed under `<engine>` | every Engine-interface operation; in every mode the sources of checkpoint and personal-sync documents and an expanded presented copy are admitted too — a document the host produces carries the admitted (masked) text, an already bound or foreign one is unchanged or refused whole, foreign content before any local store | `cli::engine_cmd` tests; `checkpoint_transfer::content::tests::sources_are_admitted_field_by_field_or_the_bound_document_is_refused` and `developer_mode_admits_clean_refuses_bound_and_masks_fresh_documents`; `archive::recovery::tests::a_presented_copy_is_readmitted_in_the_call_that_expands_it` (`an_engine_document_is_receipted_for_its_project_or_not_written`, `a_host_operation_is_scoped_to_the_project_its_settings_name`) |
| One receipt index key per project root (canonical path), never promoted to the repository root; `lean-ctx inspect` and the dashboard list by repository with one resolver, within stated bounds, and still read indexes written under an older spelling | MCP, CLI, tool session, Engine interface, host egress; sub-directory roots registered since this version | `dashboard::routes::gateway_receipts::a_repository_lists_the_receipts_of_its_roots_and_a_legacy_index` |
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

- Semantic detectors behind the detector seam (contract
  `pro.runtime.semantic_detectors@1.1.0`): a local ONNX model backend in the
  LeanCTX Hive runtime, asked by the warm worker of one long-lived process, exists in
  source and has been exercised with a TEST package only. Not shipped, and
  nothing is claimed for it: the one model set tried examines prompt injection
  only (personal data and secrets are reported as not inspected by it), a
  short-lived command ends before the worker is warm and is reported as not
  inspected, and detection quality is unmeasured. Prompt Guard and Laya were
  evaluated on paper and are not in the tree.
- Org-wide policy simulation and shadow mode.
