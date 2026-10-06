# Local Engine context planning v1

`lean-ctx engine context-plan --project-root /absolute/project --json-file request.json`
is an additive local operator interface. Existing context-view, recovery, host receipt
and Agent Tools v1 operations are unchanged. Public DTOs live in `lean-ctx-protocol`.

```json
{"schema_version":1,"transport_version":1,"engine_interface_version":"1.0.0","task_id":"external-task","query":"invoice ledger","budget_tokens":512,"max_candidates":20}
```

The response contains the same three version fields and `plan`, the existing
`ContextPlanProjectionV1` with task ID, canonical projection digest, selected/excluded/
deferred sources, reasons, provider accounting and estimated context-token budget.
Selection runs through `AutopilotController` and the canonical Context Kernel using
fixed public reference weights; there is no second planner and no private dependency.
This is planning evidence, **not** a signed admission, execution receipt or outcome.
Selected reasons describe the compiler decision; they never copy candidate content.
Previously generated projections remain readable and their recorded digests remain valid;
fresh projections use metadata-only selected reasons and compute their own new digest.

The operator supplies the project root outside the request. Task IDs are correlation,
not authenticated identities. The host's existing kernel policy controls sensitivity,
source permissions, retention and budget ceiling; invalid policy fails closed.
This surface always enforces policy, even if the legacy supplement is in shadow mode.
Retention uses host time. A request cannot supply authorization or override policy.
This is not a remote multi-tenant authorization boundary or an Enterprise deployment claim.

Requests are strict JSON with unique known fields: at most64KiB, query at most16KiB,
budget1..1048576 estimated tokens and candidates1..256 per provider. The plan uses
the requested budget (subject to host policy), not the legacy150-token supplement cap.
The public provider set remains project knowledge/session/episodic/procedural/ledger.
This operation neither invokes a model nor promises compiled output/recovery of every
candidate; use the existing context-view/recover contract for its supported file views.

Failures return exit2, no JSON output and `engine: <code>` on stderr. Existing version
errors remain stable; new failures are `context_policy_unavailable` and
`context_planning_failed`. Invalid payload is `invalid_request`; unavailable/nonregular
request files are `request_file_unavailable`; an invalid root is `unsafe_root`.

Behavioral gate: `cargo test --locked --test engine_context_plan_transport --test
engine_transport_compatibility`. The first test seeds real local knowledge with the CLI,
requests a digest-bound plan and changes host policy; the second target protects the
existing receipt/recovery path. These are component checks, not release acceptance.
