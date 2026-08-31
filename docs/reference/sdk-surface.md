# LeanCTX SDK surface

LeanCTX has one supported SDK for building custom agents:
[`thinkery-leanctx-sdk`](https://github.com/Thinkery-AG/leanctx-sdk), published
on [PyPI](https://pypi.org/project/thinkery-leanctx-sdk/).

The SDK owns the Python `AgentContext` API. Beginning with SDK 1.1.0, it starts
the local LeanCTX Engine through the versioned `lean-ctx engine tool-session`
protocol and exposes the Engine's read, search, tree, glob, compose, symbol,
patch, and structured-shell capabilities. Agent orchestration, model calls,
retry policy, and application logic remain under the integrator's control.

SDK 1.1.0 is published only after Engine 3.10.1 and its signed artifacts pass
verification. Until that SDK release completes, PyPI 1.0.0 remains the stable
context-lifecycle SDK and does not provide the `AgentContext` agent-tools API.

## Other artifacts

| Artifact | Purpose | SDK? |
| --- | --- | --- |
| `lean-ctx` | Local Engine, CLI, MCP server, dashboard, and proxy | No |
| `thinkery-leanctx-engine*` | Platform-specific Engine companion wheels consumed by the SDK | No |
| `lean-ctx-embed` | Unpublished in-process Rust embedding facade | No |
| `lean-ctx-client` | Internal Rust protocol and verification client | No |

The previously published `lean-ctx-python` and npm `lean-ctx-sdk` packages are
retired surfaces. Existing registry artifacts may remain installable for
compatibility, but new integrations should use `thinkery-leanctx-sdk`.

## Selection rule

- Building a Python agent from scratch or with an agent framework: use
  `thinkery-leanctx-sdk`.
- Connecting an existing MCP-capable agent: run the `lean-ctx` Engine directly.
- Embedding context compression inside Rust: evaluate `lean-ctx-embed`; it is a
  facade, not the cross-language Agent SDK.
