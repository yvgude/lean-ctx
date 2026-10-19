# Integrating the LeanCTX Context Gateway

> **Context Gateway for AI Systems. Control what your AI can see.**

LeanCTX sits between AI and the systems it reads: **SELECT → CONTROL → LEARN → PROVE**.
LeanCTX Engine implements context selection, supported controls, local task and
knowledge continuity, and bounded evidence. LeanCTX SDK connects applications to
that runtime. Learn means context continuity, not model training.
Within the Engine, the technical context lifecycle is:

    select → shape → reuse → recover

The agent, task logic, model choice, tools, and retry policy remain with the
integrator.

## Available local Runtime

LeanCTX provides local paths for:

- selecting and representing repository and tool context;
- structural views, exact-source recovery, compression, and reuse;
- CLI, MCP, hooks, proxy, and supported coding-agent integration paths;
- local Receipts and offline-verification primitives.

The Runtime is local-first. An integration must state what it can see; it
cannot claim facts about host prompts, decisions, retries, quality, or outcomes
outside that visibility.

## Integration depths

| Depth | Status | Role |
| --- | --- | --- |
| Attach | Available | Operate around a supported coding agent through local CLI, MCP, hooks, or proxy paths. |
| Wrap | Preview | Use the declared Python SDK v1 reference-wrapper scope. |
| Embed with the standalone LeanCTX SDK | Available stable contract | Use the released context lifecycle and Agent Tools interfaces; the host retains ownership of its agent loop. Preview namespace operations remain Preview. |

## Research: shared project context

The local Runtime includes session, project knowledge, handoff, and
agent-presence substrate. It can inform future **shared, scoped project
context**: agents reuse bounded decisions, findings, and source references
instead of passing complete transcripts.

This is **Research**, not a supported agent-orchestration product. The host
still assigns work, schedules agents, retries failures, and controls execution.
LeanCTX does not yet provide a public cross-agent coordination, privacy,
freshness, or interoperability contract.

## Scope boundaries

LeanCTX is not a hosted RAG service, universal retrieval layer, multi-agent
platform, marketplace, model router, or generic agent framework. A source
connector, index, provider, plugin, or implementation module does not itself
create a public product promise.

The standalone SDK's released ContextPlan belongs to its stable lifecycle.
Older planning proposals do not set that package's status. Performance Profiles,
first-class Context Kits, and cross-agent product directions remain Research.
Commercial Enterprise organization capabilities have a separate contract and
license; they are not promoted or demoted by this Engine guide. See
[current positioning](../POSITIONING_CANONICAL.md) and [SDK boundaries](../reference/sdk-surface.md).

## Evidence

A context reduction or calculated cost is an observation of one declared
workload. A valid performance claim needs comparable baseline and treatment,
a declared quality threshold, visible methodology, and inspectable evidence.
