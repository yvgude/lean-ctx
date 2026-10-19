# Embed LeanCTX in your application

**LeanCTX SDK — embed context control into your application.**

Keep your model, workflow and UI. LeanCTX is the **Context Gateway for AI Systems**:
**Control what your AI can see.** The SDK connects your code to supported
LeanCTX Engine interfaces within **Select → Control → Learn → Prove**.
Learn describes local task and knowledge continuity; each SDK surface exposes
only its documented capabilities.

## Supported integration

Use the [standalone SDK](https://github.com/Thinkery-AG/leanctx-sdk) and its
published compatibility matrix. Stable lifecycle primitives and Agent Tools are
separate supported surfaces; Python `leanctx_sdk.preview` APIs retain their own limits.

- Coding agents can use Agent Tools for reads, search, structural tools and
  explicit permissions.
- Applications and copilots can use supported lifecycle operations for context
  preparation, recovery and receipts.
- Production/OEM use and commercial redistribution require a signed written
  Thinkery AG commercial agreement under the SDK license.
  The Apache-2.0 Engine and the source-available SDK have different terms.

The host owns model calls, orchestration, retries and user-visible behavior.
A context-only receipt cannot assert that the model received an unseen later
prompt. Record host delivery facts only through a documented integration.

## Preview and historical adapters

Status: Preview — older in-tree embedding experiments and reference wrappers.

An internal crate or old `wrap()` example is not the standalone SDK's Stable
contract. Workspace, checkpoint, delta and handoff interfaces explicitly in
Python's `leanctx_sdk.preview` namespace may change. Consult the [SDK surface](../reference/sdk-surface.md)
before choosing a package or adapter.

## Evaluate your workload

Compare a named baseline and treatment with a quality gate. Context-token
reduction and calculated cost differences do not establish a universal saving
or accepted outcome. Recovery requires an authorized retained source/archive.

See [canonical positioning](../POSITIONING_CANONICAL.md).
