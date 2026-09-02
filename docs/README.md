# LeanCTX repository documentation

This directory documents the local LeanCTX Runtime and the work needed to
converge its public contracts.

LeanCTX is the **Context SDK for AI Agents**. It sits inside or alongside an
existing agent loop and controls how context is selected, shaped, reused, and
recovered before inference; it can expose evidence about those decisions. It does not replace an agent,
orchestrate a fleet, choose a model, or host an agent platform.

## Product status

| Status | Public meaning |
| --- | --- |
| **Available** | Local Runtime: CLI, MCP, proxy paths, context selection/compression/reuse, and local receipt or offline-verification primitives. |
| **Preview** | Python SDK v1 and its declared OpenAI Agents reference-wrapper scope; common session and receipt convergence; explicit capability and degradation matrices. |
| **Research** | Performance Benchmark; Performance Profiles; first-class Context Kits; canonical evidence bundle; automated tuning; managed/cloud operation; marketplaces; organization controls; public rankings; and external-capability composition. |

Read the status boundary before relying on a document:
internal product authority (`docs/internal/README.md`, internal — not in this repository) and the
Product Architecture (`docs/internal/vision/PRODUCT-ARCHITECTURE.md`, internal — not in this repository).

## Start here

- Project overview: [`README.md`](../README.md)
- Contributing: [`CONTRIBUTING.md`](../CONTRIBUTING.md)
- Security: [`SECURITY.md`](../SECURITY.md)
- Architecture: [`ARCHITECTURE.md`](../ARCHITECTURE.md)
- Current tool and configuration inventories: [MCP tools](reference/generated/mcp-tools.md) · [config keys](reference/generated/config-keys.md)

## Current local Runtime

- Core binary and MCP server: [`rust/`](../rust/)
- Integration setup: [guides](guides/README.md)
- Local Runtime reference: [reference](reference/README.md)
- Local contracts and schemas: [contracts](contracts/README.md)
- The SDK: [`thinkery-leanctx-sdk`](https://github.com/Thinkery-AG/leanctx-sdk) (external repo — see [SDK surface](reference/sdk-surface.md))

## Context Kits and package material

The signed `.ctxpkg` substrate exists locally. First-class Context Kit
semantics and any hosted distribution are **Research**, not a
public registry, marketplace, or enterprise service. See
[package-status notes](guides/publishing-packages.md) and
[the v2 research record](specs/context-package-v2.md).

## Historical and research material

Some repository documents preserve experiments, implementation sketches, or
retired commercial concepts. They are not installation instructions or public
availability claims. Each such document must carry a prominent status header;
when it conflicts with the internal product authority, the authority wins.
