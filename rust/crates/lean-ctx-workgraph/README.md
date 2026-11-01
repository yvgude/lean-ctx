# lean-ctx-workgraph

The bounded multi-agent work graph, budget cascade and outcome evaluation
model of the [LeanCTX Engine](https://github.com/yvgude/lean-ctx).

It holds pure data and rules: parent/child delegation with budget inheritance,
fan-out and depth limits, stop conditions, execution fences and receipts, plus
versioned outcome contracts, local signals and their deterministic evaluation.
Persistence, transport and policy stay in the engine.

This crate is an internal building block of `lean-ctx`. Its API follows the
engine's needs and may change between minor versions.

License: Apache-2.0
