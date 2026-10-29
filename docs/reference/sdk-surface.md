# LeanCTX SDK surface

**Embed LeanCTX context control into your application.**

LeanCTX is the **Context Gateway for AI Systems**. **Control what your AI can see.**
The **LeanCTX SDK** connects your application to a compatible **LeanCTX Engine**;
you keep your model, workflow and UI.

In this consolidated checkout the SDK has one source tree, `sdk/`, with Python,
TypeScript, Go, Rust, JVM and .NET bindings. The Python package is
`thinkery-leanctx-sdk`. The consolidated checkout tests these bindings against
its own Engine. An Apache-only source export omits this separately licensed SDK
tree; existing published SDK packages remain at their current registry coordinates.

The standalone [SDK repository](https://github.com/Thinkery-AG/leanctx-sdk)
exists next to the in-tree source. For the SDK releases made from it, it holds
the stable interface manifest, supported package coordinates, language versions
and release compatibility, and it documents the Python, TypeScript, Go, Rust,
JVM and .NET interfaces. Registry availability is specific to each package release.

Package versions in this checkout describe release candidates, not proof of
registry publication. Engine companion wheels and the SDK must pass the
same-checkout compatibility gate before release. See the SDK's `LICENSE` and
`COMMERCIAL-LICENSE.md` for its usage rights; moving the source into this
checkout does not make it Apache-licensed or change existing agreements.

## Stable and Preview

- **Stable lifecycle:** `ContextSession`, `ContextSource`, `ContextView`,
  `ContextPlan` and `ContextReceipt`.
- **Stable Agent Tools:** `AgentContext` and its documented permission,
  execution-policy, result and metrics interfaces over the versioned Engine
  tool-session protocol.
- **Python Preview:** workspace, checkpoint, delta, handoff/fork and other
  interfaces explicitly placed in `leanctx_sdk.preview`.

SDK v1.1.0 is a published release. Use its tagged compatibility contract for that
artifact. Later main-branch alignment with an Engine version does not, by itself,
publish updated packages to every registry.

## Choose your path

| Building | Use |
|---|---|
| A coding agent | Agent Tools: supported reads, search, structure and explicit permissions |
| A copilot or other AI application | Stable context lifecycle operations; the host owns model calls |
| An embedded commercial product | SDK/OEM integration and the applicable written commercial agreement |
| An existing MCP-capable tool | Community Engine directly through a supported local integration |

## Artifact boundaries

| Artifact | Role |
|---|---|
| `lean-ctx` | Apache-2.0 Engine, CLI, MCP server, dashboard and supported proxy |
| `thinkery-leanctx-sdk` | Python package in the standalone, separately licensed LeanCTX SDK |
| `thinkery-leanctx-engine*` | Platform-specific Engine companion wheels |
| Standalone LeanCTX SDK packages | Separately licensed application integration |
| `lean-ctx-embed` | Unpublished in-process Rust facade; not the standalone SDK contract |
| `lean-ctx-client` | Internal Rust protocol/verification client |

The SDK's source-available license requires a signed written Thinkery AG
commercial agreement for production/OEM use and commercial redistribution.
Consult its LICENSE for the complete terms. Apache rights in the Engine remain
unchanged. Retired `lean-ctx-python` and npm `lean-ctx-sdk` artifacts are
compatibility history, not recommended new integration surfaces.

See [canonical positioning](../POSITIONING_CANONICAL.md) and
[What is LeanCTX?](../what-is-leanctx.md).
