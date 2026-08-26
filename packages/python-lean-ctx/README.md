# lean-ctx-python — Preview

> **Status: Preview.** This package documents one declared reference path:
> Python v1 with the OpenAI Agents SDK. It is not a general agent SDK, an
> automatic adapter layer, or evidence of support for every framework. Current
> product scope and transition status are governed by the
> [P4 extraction report](../../docs/internal/P4-SDK-EXTRACTION-AND-ENGINE-REFACTOR-REPORT.md).

Security and correctness fixes continue here. Compatibility fixtures and the
documented Engine path remain maintained. Removal or deprecation requires a
released successor, migration window, rollback path, and the applicable
release-policy decision; none is implied by this classification. Future
production users should move to the separate Production SDK only after its
namespace, license and release gates are approved. Until then this package
remains the supported transition path.

The persistent successor head is private SDK staging commit `8c84e224`; ledger
`a7afd1f` binds immutable implementation `11f77debc2`, wheel `b54ac013…`, and
no public remote or release. Independent internal-RC evidence passed Python
3.9/3.14 (`19/19` each), exact CPython 3.9 macOS-arm64 offline closure
(`25/25`), clean wheel/install, real Engine
recovery, sealed receipts and provider-free OpenAI Agents 0.8.4 actual Runner
success/abort. BSL 1.1 is the decided license family; exact
publication, namespace and license parameters remain pending. The integrated
P3 Preview seam remains the compatibility baseline. Historical `fbfec0b`
references remain quarantined. No provider-backed production-support claim is
made here; live-provider smoke is unverified.

LeanCTX is **The Context SDK for AI Agents**. The reference wrapper connects an
existing OpenAI Agents SDK agent to a local LeanCTX Runtime. It does not replace
the agent, model, provider, or task logic.

## Reference path: OpenAI Agents SDK

Install the Preview package in a clean Python 3.9+ environment with the named
OpenAI Agents extra. The Engine proof is credential-free; live provider calls
remain a separate concern.

```bash
python3 -m venv /tmp/leanctx-p3-venv
/tmp/leanctx-p3-venv/bin/python -m pip install -e \
  ".[openai-agents,test]"
```

```python
from agents import Agent, Runner
from lean_ctx import LeanCTX

agent = Agent(name="Assistant", instructions="Be concise and helpful.")
task = "Summarize the deployment plan."

result = Runner.run_sync(LeanCTX().wrap(agent), task)
print(result.final_output)
```

This is a **Preview reference wrapper**, not a claim that every agent shape or
provider transport is supported. A live OpenAI run also requires the relevant
provider credentials.

## Local Engine Embed proof

Embed is explicit host control: Python creates the task/session and plan, the
local Engine creates the factual view, the host calls its agent, and the host
explicitly records the outcome. The default executable is `lean-ctx`; set
`engine_binary` to an explicit built binary for a credential-free integration
test.

```python
from agents import Agent, Runner
from lean_ctx import ContextSource, LeanCTX

ctx = LeanCTX({"engine_binary": "lean-ctx"})
session = ctx.embed("Review the deployment plan", project_root=".")
plan = session.plan(ContextSource("README.md", project_root="."))
view = plan.execute()
# local_model implements agents.models.interface.Model; the deterministic,
# provider-free implementation used for this proof is in tests/test_agents_sdk.py.
agent = Agent(name="Reviewer", model=local_model)
result = session.run_openai(agent)
receipt = session.receipt
assert result.final_output == "approved"
assert receipt.verify()
exact_source = view.recover()
```

The Engine command is the versioned local process boundary:

```text
lean-ctx engine context-view --project-root ROOT --json-file REQUEST
lean-ctx engine recover --project-root ROOT --json-file REQUEST
```

Malformed observations, version or lineage mismatches, digest changes, policy
rejection, and receipt-link failures are never silently treated as success.
`fail_open=True` may continue only with an explicit degraded, unsealed Python
receipt; `fail_open=False` raises before the host call. No local code infers
usage, savings, coverage, or acceptance from text or delivery.

Run the package proof with:

```bash
python -m pytest -q packages/python-lean-ctx/tests
```

The real-binary integration test is opt-in via `LEAN_CTX_ENGINE_BINARY` and is
skipped when that variable is absent.

## Evidence boundary

The local Runtime can emit receipt and offline-verification primitives. A
receipt makes a declared artifact inspectable; it does not by itself prove task
quality, a cost result, or a performance result. Any gain claim needs a
comparable baseline and treatment, a declared quality threshold, and visible
methodology.

## Not public SDK surface

The repository may contain experimental or internal interfaces for generic
`ctx.wrap` behavior, automatic adapter selection, LangChain, LiteLLM,
LlamaIndex, `load_kit`, `ContextKit`, `TuningProfile`, sessions, or custom
agent embedding. They are **not supported public Python SDK integrations**.
Context Kits, Performance Profiles, broader Embed work, and the canonical
evidence bundle remain Research; do not build a production integration against
those interfaces from this README.

For the Available, Preview, and Research boundary, see the internal
[implementation roadmap](../../docs/internal/vision/08-COMPLETE-IMPLEMENTATION-ROADMAP.md).

## License

Apache-2.0
