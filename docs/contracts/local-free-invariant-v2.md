# Local-Free Invariant — v2

Status: binding for the 3.12 line (LeanCTX Hive). Supersedes
[`local-free-invariant-v1.md`](local-free-invariant-v1.md), which stays as the
historical record.

## The invariant

> Everything the Engine and the cockpit do on one machine works without an
> account, licence or plan. A paid right is granted only by a LeanCTX Hive plan
> and only for what Hive delivers (filter packs and semantic detectors, shared
> team state, governance). A Hive plan adds capabilities; it never removes,
> degrades or gates a local one.

Consequences:

1. Every `free` capability in `product/capabilities.toml` has
   `account_required = false`. Former Pro capabilities
   (`pro.dashboard.knowledge_workflows`, `pro.runtime.adaptive_context`,
   `pro.runtime.memory_curation`, `pro.runtime.context_policy`) are `free`; the
   ids keep their historical prefix. Since 2026-10-29 the Engine never invokes
   the last three in a runtime: context ranking, memory selection and read
   strategy are always the local behaviour (a runtime may still declare them so
   it installs).
2. Paid capabilities use price class `hive` with `hive_plan = personal | team |
   enterprise`. Personal Cloud (`pro.cloud_sync`, `pro.hosted_index`) and
   `team.revenue_share` are `retired`: known lookup names, granted by no plan.
3. Plan, licence and entitlement checks live only in `rust/src/core/hive/`
   (the Hive connector). No other Engine module decides from a plan, a licence,
   a signed entitlement or the billing capability CLI.
4. When Hive is unreachable the Engine is unaffected; only Hive-delivered
   capabilities (filter packs) fail closed.

## Gates

| Gate | What it fails on |
|---|---|
| `rust/tests/local_free_invariant.rs` | the default `plane` is not `personal`; an always-on Community feature is not `true`; a Community feature, or the free capability set, changes under `LEAN_CTX_LICENSE`, `LEAN_CTX_PLAN`, `LEAN_CTX_ACCOUNT` or a forged entitlement cache; a free capability requires an account or is refused to Community; a Community feature is not classified free |
| `scripts/check-local-free.py` | a gating symbol in the code (comments excluded) of a tracked `rust/src/**/*.rs` file outside `rust/src/core/hive/` that is not listed in `product/local-free-allowlist.toml`; an allowlist entry without owner (strand B or C) or reason; a stale entry (no hit any more) |
| `scripts/check-capabilities.py` | a schema-3 record with a legacy price class, `hive_plan` missing or misplaced, a free capability with `account_required = true`, a paid tier without an instance limit |

Gating symbols: `entitlement` (any case), `require_pro`, `capability_admitted`,
`signed_entitlements`, `billing capability`, `LEAN_CTX_PLAN`,
`LEAN_CTX_LICENSE`, `Plan::Pro|Team|Enterprise`, `minimum_plan(`. Test-only
files (`*_tests.rs`, `tests/`, `fixtures/`) are not scanned.

`check-local-free.py` runs in `scripts/preflight.sh` (always on), `make check`
and the GitLab `checks` stage.

## Allowlist

`product/local-free-allowlist.toml` starts with every file that has a gating
symbol today, each with the strand that removes or moves it (B: engine cleanup,
C: Hive connector) and a one-line reason. The list may only shrink: an entry
whose file no longer has a hit fails the check until it is removed. Exit
criterion at G1: the allowlist is empty, except `rust/src/core/billing/plans.rs`
if the plan catalog is kept as data.

## Instance limits

Hive enforces connected Engine instances per plan (`product/tier-map.json`
`instance_limits`): without a plan 1 per account, Personal (map key `pro`) 2 per
account, Team 2 per seat, Enterprise 3 per seat (contract).
