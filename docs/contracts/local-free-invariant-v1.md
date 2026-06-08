# Local-Free Invariant & Plane Separation — v1

Status: **stable (v1)** · EPIC 12.19 · RFC §4, §6

lean-ctx is **local-first and free**. Commercialization comes from *additive*
Team/Cloud features over a clean process/service boundary — never from gating the
local experience. This document defines the boundary and the CI gate that
enforces it.

## The two planes

| Plane | What it is | How it ships | Gating |
|-------|-----------|--------------|--------|
| **Personal (local)** | The full single-developer experience: every MCP tool, all read modes, compression, caching, knowledge, sessions, gateway, sensitivity floor, savings ledger, audit trail, personas, plugins, extensions. | Always compiled into the binary (some capabilities are optional **compile** features, never license features). | **None.** No account, license, or plan — ever. |
| **Team / Cloud (commercial)** | Cross-machine sync, shared knowledge graph, RBAC/SSO/SCIM, hosted ingestion, marketplace, domain packs, billing. | Opt-in `team-server` / `cloud-server` Cargo features + separate services. | Account / plan, by design — but strictly **additive**. |

`GET /v1/capabilities` reports the active `plane` (default `personal`).

## The invariant

> Any capability available locally to a single developer MUST remain available
> without an account, license, or plan. Commercial features may only **add**
> capabilities (sync, collaboration, governance, hosting) — never remove,
> degrade, or gate a local one.

## Feature classification (single source of truth)

`core::server_capabilities` classifies every advertised feature:

- `LOCAL_ALWAYS_ON_FEATURES` — free, ungated, in every build.
- `LOCAL_OPTIONAL_FEATURES` — free, gated by **compilation** only (Cargo features).
- `COMMERCIAL_PLANE_FEATURES` — additive, opt-in (`team_server`, `cloud_server`).

Every feature flag must belong to exactly one of these sets.

## CI conformance gate

`rust/tests/local_free_invariant.rs` fails the build if:

1. the default `plane` is not `personal`,
2. any `LOCAL_ALWAYS_ON` capability is not unconditionally `true`,
3. a commercial feature is misclassified as local, or
4. any local capability changes based on a `LEAN_CTX_LICENSE` / `LEAN_CTX_PLAN` /
   `LEAN_CTX_ACCOUNT` environment variable.

A complementary unit test (`feature_keys_partition_into_local_and_commercial`)
fails if a new feature flag is added without being classified — so the invariant
can never silently drift.
