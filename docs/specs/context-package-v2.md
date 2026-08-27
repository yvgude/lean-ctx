# Research — .ctxpkg v2

> **Draft research, not a released package contract.** This file does not define
> an installable marketplace format, a hosted registry, a graph-native public
> API, or a LeanCTX ecosystem promise.

## Current status

LeanCTX has a signed local .ctxpkg substrate. First-class Context Kit semantics,
portable package identity, pinning, signer behavior, and the public
ContextKitV1 contract are still **Research**. The local substrate must not be
represented as a public publisher service or as a general package manager.

A `.ctxpkg` is not a live ContextWorkspace, a receipt, an evidence bundle, or
a Cloud account. Existing Kit TOML, legacy `.ctx.json` export, snapshots and
package substrate remain distinct until a versioned composition/migration
contract exists. See the internal
[Context Workspace & `.ctxpkg` Plan](../internal/vision/07-CONTEXT-WORKSPACE-CTXPKG-PLAN.md).

## Additive checkpoint layer

Research v2 reserves the explicit manifest layer `checkpoint` together with
`content.checkpoint` using envelope contract
`leanctx.ctxpkg-checkpoint/v1`. Both must be present together. The package kind
remains `context` and `manifest.schema_version` remains `2`.

Checkpoint-critical semantics live only inside authenticated `content` bytes:
the raw canonical content hash feeds package integrity, which is bound by the
existing Ed25519 signature message. Checkpoint ID, logical-state digest,
content hash, package digest and signature remain distinct identities.

Pre-extension typed readers reject the unknown `checkpoint` layer. Generic
package loading also rejects checkpoint packages; a checkpoint-aware Product
admission path must explicitly separate verification, signer trust, install,
seed and restore. Existing v1/v2 packages omit the new optional member and keep
their prior bytes and behavior.

## Research direction

Earlier work explored a richer package shape for reusable context assets,
including versioned manifests, integrity metadata, provenance, optional graph
relationships, and local verification. These are useful design questions, not
available behavior.

Any future contract must:

- preserve local-first use without an account or hosted dependency;
- define version, digest, provenance, and access boundaries explicitly;
- make recovery and verification behavior inspectable;
- earn compatibility, security, and support ownership before release;
- avoid turning Context Kits into an unsupported marketplace or agent platform.

## Out of scope until promoted

The following are not current LeanCTX product claims: a hosted registry,
publisher accounts, package billing, marketplace rankings, social discovery,
cross-vendor capability distribution, autonomous graph learning, or automatic
installation of third-party agent capabilities.

The canonical status map is [Product Architecture](../internal/vision/PRODUCT-ARCHITECTURE.md).
