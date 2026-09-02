# Historical / Research — Multi-agent collaboration

> **Not a public LeanCTX product surface.** LeanCTX is a Context SDK for
> existing agents, not an agent platform, work orchestrator, team task board, or
> general agent-to-agent communication layer.

## Current product boundary

An integrator owns the agent loop, task assignment, model choice, tools, retry
policy, and any coordination between agents. LeanCTX may run inside or alongside
each of those agents to control how that agent selects, shapes, reuses, and
recovers context before inference, with local evidence about those decisions.

The public integration depths remain:

- **Attach — Available:** local CLI, MCP, hooks, or proxy around a supported
  coding agent.
- **Wrap — Preview:** the declared Python SDK v1 reference-wrapper scope.
- **Embed — Preview:** native use inside a custom host, without turning LeanCTX
  into an agent framework.

## Historical implementation record

The Runtime contains local implementation substrate for session state,
project-scoped knowledge, handoff artifacts, and agent-presence experiments.
Those code paths are useful engineering evidence, but they do **not** establish
a supported coordination protocol, cross-agent cache contract, hosted team
service, or customer-facing orchestration product.

Use the generated tool inventory and the relevant local contract to inspect a
specific implementation surface. Do not infer product availability from a tool
name or implementation directory.

## Promotion rule

A future coordination capability needs an explicit product decision, a bounded
contract, security and privacy ownership, observable evidence limits, and the
status gate in Product Architecture (`docs/internal/vision/PRODUCT-ARCHITECTURE.md`, internal — not in this repository).
Until then, agents integrate LeanCTX for their own context path; they do not
depend on LeanCTX to coordinate the work itself. The Research direction may be
described internally as **shared, scoped project context**, never as an agent
bus, scheduler, or agent-to-agent communication platform.
