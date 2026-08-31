# LeanCTX local Runtime reference

This reference describes the local Runtime. Its public boundary is governed by
[the internal canonical entry point](../internal/README.md) and
[Product Architecture](../internal/vision/PRODUCT-ARCHITECTURE.md).

LeanCTX is the Context SDK for existing agents. The agent, model, task logic,
and retry policy remain the integrator's responsibility.

## Current paths

| Area | Use it for | Status |
| --- | --- | --- |
| [Setup & onboarding](01-setup-and-onboarding.md) | Local CLI and coding-agent setup | Available |
| [Daily use](02-daily-use.md) | Context-aware reads, search, and shell output | Available |
| [Memory & knowledge](03-memory-and-knowledge.md) | Local project/session context | Available, within documented limits |
| [Code intelligence](04-code-intelligence.md) | Structural views and impact exploration | Available |
| [Advanced integrations](05-advanced.md) | Local proxy, providers, and plugin substrate | Available only where the individual contract says so |
| [Lifecycle](06-lifecycle.md) | Install, diagnose, update, and remove | Available |
| [Context engineering](07-context-engineering.md) | Local context decisions and verification primitives | Available / Preview by feature |
| [Customization & governance](10-customization-and-governance.md) | Local configuration and safety rules | Available |

## Status-qualified records

These files are retained to make the repository auditable. They are not
navigation to an additional public product.

| Record | Status | Boundary |
| --- | --- | --- |
| [Multi-agent collaboration](08-multi-agent.md) | Historical / Research | LeanCTX is not a multi-agent platform or orchestration product. |
| [Team, Cloud & CI](09-team-cloud-ci.md) | Historical / Research | No hosted/team/cloud service is publicly available from this repository. |
| [Analytics](11-analytics-and-insights.md) | Available local observations | Local counters are not universal savings or business-outcome proof. |
| [Proof & audit](16-signed-savings-ledger.md) | Available local primitives | Integrity evidence is not a standalone savings claim. |
| [Adaptive learning](18-adaptive-learning.md) | Research | LeanCTX does not offer AutoTune or autonomous promotion. |
| [Hermes context engine](20-hermes-context-engine.md) | Preview reference integration | Not a general agent framework or stable Embed contract. |

## Generated inventories

- [MCP tools](generated/mcp-tools.md)
- [Configuration keys](generated/config-keys.md)
- [CLI command map](appendix-cli-map.md)
- [SDK surface](../internal/SDK-SURFACE.md) — what each published artifact is

Generated inventories enumerate implementation surfaces; they do not promote a
surface to a stable public API. Check the status map before depending on one.
