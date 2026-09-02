# LeanCTX as the local context layer

> **Give every agent a context system.**

LeanCTX is the **Context SDK for AI Agents**. It runs inside or alongside an
existing agent and makes the context path before inference explicit:

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
| Embed | Preview | Integrate natively into a custom host while the host retains ownership of its agent loop. |

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

Context Plans are **Preview**. Performance Profiles, Context Kits, broad
provider composition, and organization-scale operation are **Research** according to the
Product Architecture (`docs/internal/vision/PRODUCT-ARCHITECTURE.md`, internal — not in this repository). They must
be status-labelled where they appear.

## Evidence

A context reduction or calculated cost is an observation of one declared
workload. A valid performance claim needs comparable baseline and treatment,
a declared quality threshold, visible methodology, and inspectable evidence.
