# LeanCTX documentation

**Context Gateway for AI Systems**

**Control what your AI can see.**

LeanCTX sits between AI and the information it reads: **Select → Control → Learn → Prove**.
It prepares task-relevant context, applies supported access/content rules and
retains local task and knowledge records, and records observable evidence.
Your application, agent loop and model stay yours.

## Start with the product

1. [What is LeanCTX?](what-is-leanctx.md) — placement, inputs, outputs and editions.
2. [Where LeanCTX fits](where-leanctx-fits.md) — retrieval, gateways, guardrails and agents.
3. [Architecture](../ARCHITECTURE.md) — runtime and integration boundaries.
4. [Setup guides](guides/README.md) — use existing AI tools.
5. [LeanCTX SDK](https://github.com/Thinkery-AG/leanctx-sdk) — embed supported Engine capabilities.

The **LeanCTX Engine** is the runtime; the **LeanCTX SDK** is its application
integration surface. Context Intelligence is a capability inside the Gateway.

## Product status

| Status | Scope |
|---|---|
| **Available** | Community Engine local CLI/MCP, supported hook/proxy paths, context selection, structural views, compression/reuse/recovery, configured controls and local evidence. Standalone SDK Stable APIs follow the SDK release manifest. |
| **Preview** | Explicit SDK Preview APIs and other contracts individually labeled Preview; older in-tree wrapper experiments are not the standalone SDK's stable contract. |
| **Research** | Performance Profiles as a promoted product; first-class Context Kits; universal workspace/coordination, AutoTune, marketplace and Performance Benchmark directions not yet promoted through their release gates. |
| **Enterprise** | Commercial organization controls and deployment are scoped to the separately licensed Enterprise release and agreement; OSS Research labels do not determine Enterprise availability. |

## Technical references

- [Local Engine reference](reference/README.md)
- [Contracts and schemas](contracts/README.md)
- [MCP tool inventory](reference/generated/mcp-tools.md) and [configuration keys](reference/generated/config-keys.md)
- [SDK surface and compatibility](reference/sdk-surface.md)
- [Security](../SECURITY.md) and [contributing](../CONTRIBUTING.md)

Generated inventories enumerate mechanisms, not release guarantees. The
[canonical positioning](POSITIONING_CANONICAL.md) and
[public claims contract](contracts/public-product-claims-v1.md) govern public language.

## Historical and research records

Retained proposals have explicit status notices and point to the current
definition. The local signed `.ctxpkg` substrate does not establish a hosted
registry, marketplace or first-class Context Kit product. See
[package status](guides/publishing-packages.md).
