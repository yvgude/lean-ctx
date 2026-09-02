# LeanCTX and codebase-memory-mcp

> **Status: historical comparison note — not canonical product copy.** Current
> LeanCTX scope and status are governed by
> `docs/internal/README.md` (internal, not in this repository). This note makes no
> performance, coverage, feature-count, or compatibility promise.

codebase-memory-mcp and LeanCTX may both be evaluated for codebase-oriented
agent workflows, but their implementation choices should be tested against the
same repository and task. LeanCTX's current product boundary is a local context
Runtime that helps an existing agent select, shape, reuse, and inspect context.

| Question | Evaluation approach |
|---|---|
| Do you need a particular structural query? | Check the current tool documentation and run it on representative code. |
| Do you need context shaping before inference? | Compare a declared read mode against an unshaped baseline. |
| Do you need a dependable result? | Hold the task and quality threshold constant; inspect the evidence. |

## Evidence boundary

Past versions of this page promoted language counts, timing, token reductions,
and broad workflow benefits. Those observations are not universal or current
product guarantees. A result applies only to the measured configuration.

## Current boundary

LeanCTX is **The Context SDK for AI Agents**, not a generic agent platform,
team service, or hosted graph product. See the internal
Product Architecture (`docs/internal/vision/PRODUCT-ARCHITECTURE.md`, internal — not in this repository) for the
Available, Preview, and Research boundary.
