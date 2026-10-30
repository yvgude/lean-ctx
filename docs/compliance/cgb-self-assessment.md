# CGB Self-Assessment — LeanCTX

| | |
|---|---|
| **Spec version** | Context Governance Benchmark v1.0-draft |
| **Product & version** | LeanCTX 3.7.x, OSS engine @ `main` (2026-06-10) |
| **Assessment type** | Self-assessment |
| **Assessor** | LeanCTX maintainers |
| **Date** | 2026-06-10 |
| **Scope notes** | Self-hosted CLI + MCP server + team server. Cloud control plane and website out of scope. |

Spec: [context-governance-benchmark](https://gitlab.pounce.ch/root/context-governance-benchmark)
(v1.0-draft, **pre-review** — this grade is against a draft and will be
re-assessed at v1.0-final).

Statuses follow each control's own measurement method. Where we could not
hard-verify a claim against `main` today, the control is graded **down**
(see CGB-3.1, CGB-6.5) — initiator self-assessments must over-prove, not
over-claim.

## Result

> CGB v1.0-draft: **C2 — Managed**. Basic 96% · Hardened 80% · Audited 50%.
> Self-assessment, not independently verified.

## Per-control findings

### Domain 1 — Sensitivity & Redaction

**CGB-1.1 (Basic) Credential material never reaches the model — Met.**
`core/redaction.rs` + `core/sensitivity/` redact private keys, API/bearer
tokens, cloud keys, credential assignments on the read path. Evidence:
redaction + sensitivity test modules in `rust/src/core/`.

**CGB-1.2 (Basic) Declarative, reviewable rules — Met.**
Policy packs: named TOML patterns, versioned in-repo, built-ins
(`baseline`, `strict-redaction`, `open-source`). Evidence: `docs/contracts/context-policy-packs-v1.md`,
`lean-ctx policy show strict-redaction --toml`.

**CGB-1.3 (Hardened) Classification beyond secrets — Met.**
Sensitivity classes in the Community PII filter (IBAN, payment cards,
e-mail, phone, US SSN, Swiss AHV; checksummed where the format allows) plus
user-defined pack patterns. Evidence: `rust/src/core/input_filters/pii.rs`
and its tests. Curated domain packs are LeanCTX Hive.

**CGB-1.4 (Hardened) Fail-closed coverage of content paths — Partial.**
Read/shell/search share the redaction pipeline, but there is no structural
single-enforcement-point proof and no CI gate that fails when a new tool
bypasses the stage. Gap: coverage is conventional, not structural.

**CGB-1.5 (Hardened) Regression-tested efficacy — Met.**
Redaction/sensitivity tests run in `cargo test` in CI; corpus includes
split-token and noise variants. Improvement noted: encoding-variant
coverage is thin.

**CGB-1.6 (Audited) Independent verification — Not met.**
No red team or external assessor has exercised the redaction stage.
**Declared gap #1.**

### Domain 2 — Provenance & Integrity

**CGB-2.1 (Basic) Source attribution — Met.**
Reads/search results carry path + mode metadata; session findings store
source and timestamp. Evidence: tool output headers, session store schema.

**CGB-2.2 (Basic) Transformations disclosed — Met.**
Compressed reads are framed with mode banners; the mode is recorded in the
ledger. Evidence: read-mode output framing, ledger entries.

**CGB-2.3 (Hardened) Diagnostics never lossily transformed — Met.**
Error-preservation in shell compression keeps compiler/test/stack output
verbatim (`core/compression_safety.rs`). Evidence: shell pattern tests.

**CGB-2.4 (Hardened) Staleness detected — Partial.**
Cache validates via mtime+size and `fresh=true` forces re-reads; cache hits
are disclosed. The control requires validation beyond modification-time
heuristics (content hash); same-second double-writes are a blind spot. Gap:
no content-hash validation on every hit.

**CGB-2.5 (Audited) Tamper-evident assembly — Partial.**
`core/audit_trail.rs` hash-chains audit events (verified on `main`), but
context-assembly records are not chained end-to-end. Gap: chain covers
audit events, not full assembly.

### Domain 3 — Budget & Resource Control

**CGB-3.1 (Basic) Consumption measured — Partial.**
Token accounting per call + session ledger exist, with documented
divergence notes. The control requires the **same tokenization basis the
provider bills on**; LeanCTX measures with its own tokenizer and documents
divergence. Graded down to Partial on the basis-match requirement.

**CGB-3.2 (Basic) Hard budgets bind — Met.**
`core/budgets.rs` (policy-pack budgets) enforces hard caps with refusal
on exhaustion; role token/shell/cost limits were removed in October 2026. Evidence: budget tests.

**CGB-3.3 (Hardened) Attribution per principal/workload — Met.**
Per-member drilldown (team server), agent identities
(`core/agent_identity.rs`), per-session ledgers. Evidence: team ROI
drilldown API + UI.

**CGB-3.4 (Hardened) Self-inflicted overhead disclosed — Met.**
`gain --json` reports `injected_overhead_tokens_per_turn`;
`rules_injection=off` and `LEAN_CTX_MINIMAL` reduce/disable injection.
Evidence: performance-tuning guide; claims verified against `main`
(2026-06-09, tokbench follow-up).

**CGB-3.5 (Hardened) Fan-out bounded — Partial.**
`core/agent_budget.rs` bounds delegated consumption; recursion-depth and
concurrency limits for sub-agent spawning are not uniformly configurable.
Gap: depth bound is implicit, not policy.

### Domain 4 — Audit & Evidence

**CGB-4.1 (Basic) Admin actions logged — Met.**
Org audit log: membership/role changes, key issuance/revocation, plan and
policy events with actor/action/target/timestamp. Evidence:
`docs/contracts/org-audit-log-v1.md`.

**CGB-4.2 (Basic) Append-only — Met.**
No edit/delete surface in UI/API/CLI; retention sweep is system-initiated
and itself recorded. Evidence: audit API surface review.

**CGB-4.3 (Hardened) Retention policy-driven — Met.**
Plan-based retention windows, automatic sweeper, effective window visible
to org admins. Evidence: retention sweeper + account audit page.

**CGB-4.4 (Hardened) Open-format export — Met.**
CSV export for audit log, JSON for ledgers/ROI, schemas documented.
Evidence: `/api/account/org/audit/export.csv` contract.

**CGB-4.5 (Audited) Claims reproducible — Met.**
Pre-registered protocols, pinned task sets, self-hashing result artifacts;
agent-task harness uses the official SWE-bench evaluation. Evidence:
`bench/agent-task/PROTOCOL.md`, tokbench methodology.

**CGB-4.6 (Audited) Evidence integrity third-party verifiable — Partial.**
Benchmark artifacts embed SHA-256 self-hashes; audit/ledger exports are not
signed. Gap: no signature on operational evidence exports.

### Domain 5 — Access Scoping

**CGB-5.1 (Basic) Filesystem jailed — Met.**
`core/pathjail.rs`: workspace-rooted canonicalization, symlink/traversal
refusal, explicit allow-roots. Evidence: pathjail tests.

**CGB-5.2 (Basic) Command allowlist — Met.**
`core/shell_allowlist/`: allowlist semantics incl. indirect execution
(`sh -c`, interpreter one-liners, pipe-to-shell) on CLI and MCP surfaces.
Evidence: allowlist test suite.

**CGB-5.3 (Hardened) Destructive tier — Partial.**
Dangerous commands are distinguished, but destructive/cloud-mutation
tooling is not a separately gated tier bound to roles. Gap: single tier
beyond the deny set.

**CGB-5.4 (Hardened) Roles bind capabilities — Met.**
`core/roles.rs`: ToolPolicy, TeamScope, deny-by-default for
ungranted tools. Evidence: roles tests.

**CGB-5.5 (Hardened) Egress governed — Partial.**
URL tools deniable per policy pack; telemetry consent-based. No single
operator command enumerates the full effective egress surface. Gap:
enumerability. **Declared gap #2.**

### Domain 6 — Lifecycle & Retention

**CGB-6.1 (Basic) Stores enumerable — Met.**
`~/.lean-ctx/` layout documented; `lean-ctx status`/`doctor` list stores
and sizes. Evidence: docs + doctor output.

**CGB-6.2 (Basic) Complete local erasure — Met.**
`lean-ctx uninstall` removes stores, hooks, LaunchAgent, binary;
`lean-ctx stop` halts processes (verified on `main`,
`cli/dispatch/network.rs`). Evidence: uninstall path + process-management
docs.

**CGB-6.3 (Hardened) Memory lifecycle semantics — Met.**
`core/memory_lifecycle.rs` + `core/memory_policy.rs`: decay, supersession
with history, eviction rules; knowledge timeline exposes history. Evidence:
lifecycle tests.

**CGB-6.4 (Hardened) Shared state honors boundary rules — Partial.**
Team sync applies redaction before upload; member/device revocation
exists. Recipient-side effect of revocation is not verified end-to-end.
Gap: revocation effect verification.

**CGB-6.5 (Audited) Local operation without vendor services — Partial.**
Core engine is offline-by-design (local stores, no required cloud calls,
consent-based telemetry), but no *documented network-isolation test*
demonstrates Domains 1–5 under blocked vendor endpoints. Graded down until
that test exists.

## Declared gaps

1. **No independent verification of the redaction stage** (CGB-1.6, Not
   met) — planned as a paid external engagement once revenue allows.
2. **Egress surface not operator-enumerable in one step** (CGB-5.5,
   Partial) — a `doctor`-style egress inventory is missing; tracked as a
   follow-up feature.
3. Further Partials, tracked for the C3 path: structural fail-closed gate
   (CGB-1.4), content-hash staleness validation (CGB-2.4), assembly-level
   chaining (CGB-2.5), billing-basis token accounting (CGB-3.1), fan-out
   depth policy (CGB-3.5), signed evidence exports (CGB-4.6), destructive
   tiering (CGB-5.3), revocation verification (CGB-6.4), isolation test
   (CGB-6.5).

## Score calculation

| Level | Met | Partial | Not met | N/A | Score |
|---|---|---|---|---|---|
| Basic (12) | 11 | 1 (3.1) | 0 | 0 | 11.5/12 = **96%** |
| Hardened (15) | 9 | 6 (1.4, 2.4, 3.5, 5.3, 5.5, 6.4) | 0 | 0 | 12/15 = **80%** |
| Audited (5) | 1 (4.5) | 3 (2.5, 4.6, 6.5) | 1 (1.6) | 0 | 2.5/5 = **50%** |

Grade per SCORING.md: C1 ✓ (96% ≥ 75%) · C2 ✓ (96% ≥ 90%, 80% ≥ 50%) ·
C3 ✗ (Basic ≠ 100%, blocked by CGB-3.1) → **C2 — Managed.**

## Reassessment

- At CGB v1.0-final (after external review of the spec), and
- after closing CGB-3.1 (provider-tokenizer accounting), the single Basic
  control blocking the C3 path.
