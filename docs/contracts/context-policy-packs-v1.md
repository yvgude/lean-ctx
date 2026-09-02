# Context Policy Packs v1 (GL #489)

> **Status: Research contract — unavailable as a public governance product.**
> This document records an experimental local format and implementation notes.
> It does not announce organization policy distribution, compliance coverage,
> hosted retention, or CISO workflow availability. Current product scope and
> status are governed by `docs/internal/README.md` (internal, not in this repository).

Declarative, versioned governance presets — "Context-Policies as Code". A pack
pins a team's context-governance expectations in reviewable TOML: default read
mode, allowed/denied tools, redaction patterns, an audit-retention expectation
and a context-budget cap. The reduced, solo-viable slice of #377/#403/#404.

The implementation notes below describe a proposed format, validation, and
local enforcement experiments. References to curated packs, organization-policy
distribution, retention, regulation, or compliance flows are historical design
material, not a public product claim.

## Format

A pack is one TOML file. The project pack lives at `.lean-ctx/policy.toml`.

```toml
name = "acme-internal"          # lowercase letters, digits, hyphens
version = "1.0.0"               # MAJOR.MINOR.PATCH (digits only)
description = "ACME engineering baseline"
extends = "strict-redaction"    # optional: single inheritance, built-in parent

[context]                       # all fields optional
default_read_mode = "map"       # auto|full|map|signatures|diff|task|reference|aggressive|entropy
allow_tools = ["ctx_read", "ctx_search"]   # when set: only these
deny_tools = ["ctx_url_read"]   # always additive down the chain
max_context_tokens = 12000      # > 0
audit_retention_days = 365      # governance intent (hosted plane enforces its plan window)

[redaction]                     # name -> regex, matched before content enters context
employee_id = 'EMP-\d{6}'

[filters]                       # inbound content detectors (GL #675); action = off|warn|redact|block
pii = "redact"                  # Swiss AHV, IBAN, payment cards, email (checksum-validated)
classification = "block"        # gate files marked confidential/secret
injection = "redact"            # OWASP LLM01 prompt-injection in file content
blocked_labels = ["TS//SCI"]    # optional: override the default confidential/secret label set

[egress]                        # output DLP on agent writes & actions (GL #676)
forbidden_patterns = ['prod\.db\.internal']  # regexes that block a write/action
block_secrets = true            # refuse writes/actions carrying detected secrets or PII
max_writes_per_min = 30         # sliding-window rate limit on agent writes/actions
```

Unknown keys are **rejected** (`deny_unknown_fields`) so a typo like
`alow_tools` fails validation instead of silently weakening a policy.

## Inheritance (`extends`)

Single inheritance against the built-in registry, max depth 8, cycles
rejected. Semantics are security-first and predictable:

| Field | Rule |
|---|---|
| `default_read_mode`, `max_context_tokens`, `audit_retention_days` | child **overrides** when set |
| `deny_tools` | **accumulates** (parent restrictions can never be dropped) |
| `[redaction]` | **accumulates**; a child entry with the same name re-points that pattern |
| `allow_tools` | child **overrides** when set (an allowlist is a posture choice, not a set union) |
| `[filters]` actions (`pii`/`classification`/`injection`) | child **overrides** when set |
| `filters.blocked_labels` | **accumulates** (a child may add labels, never drop them) |
| `egress.forbidden_patterns` | **accumulates** (a child may add patterns, never drop them) |
| `egress.block_secrets`, `egress.max_writes_per_min` | child **overrides** when set |

After folding, a resolved `allow_tools` colliding with an accumulated deny is
an error (`AllowDenyOverlap`) — a pack cannot both allow and deny a tool.

## Built-in packs

| Pack | Extends | Posture |
|---|---|---|
| `baseline` | — | secret redaction (PEM keys, AWS, credential assignments, bearer tokens), `auto` mode, 90-day audit expectation |
| `strict-redaction` | baseline | + JWT/GitHub/GitLab/Slack/OpenAI/Anthropic/Stripe/DB-URL coverage, `map` mode, 180 days |
| `open-source` | baseline | permissive, keeps secret coverage, 30 days |
| `finance-eu` | strict-redaction | + IBAN/payment-card/EU-VAT/SWIFT, denies `ctx_url_read`, 12 k cap, 365 days, **PII filter + egress DLP** |
| `healthcare` | strict-redaction | + SSN/MRN/member-id/DOB/NPI (HIPAA-aligned), denies `ctx_url_read`, 12 k cap, 2 190 days, **PII filter + egress DLP** |
| `soc2-context` | strict-redaction | SOC 2 TSC slice (CC6.1/CC6.6/C1.1), denies `ctx_url_read`, 16 k cap, 365 days, **PII filter + egress DLP** |
| `iso42001-aligned` | strict-redaction | ISO/IEC 42001 Annex A (A.7.4/A.9.2/A.9.4), denies `ctx_url_read`, 16 k cap, 365 days, **PII filter + egress DLP** |
| `eu-ai-act-deployer` | strict-redaction | EU AI Act deployer (Art. 10(5)/14(4)(e)/26(6)), denies `ctx_url_read`, 12 k cap, 365 days, **PII filter + egress DLP** |

Built-ins are embedded at compile time (`include_str!`) and covered by tests:
every pack must parse, validate, resolve and retain the baseline secret
coverage; the regulated packs must deny web fetches and pin budgets. The five
regulated packs additionally ship `[filters]` (PII redaction, prompt-injection
handling) and `[egress]` (`block_secrets`, write/action rate limit) so their
runtime DLP matches the compliance posture they advertise — additive to the
static framework-coverage claims (the coverage assessment reads redaction /
tool / budget / retention only, so these sections never inflate a claim).

## CLI

```
lean-ctx policy list                  # built-ins + project pack (if any)
lean-ctx policy show <name> [--toml]  # resolved effective policy / raw TOML
lean-ctx policy show project          # the .lean-ctx/policy.toml pack
lean-ctx policy show ./custom.toml    # any pack file
lean-ctx policy validate [path]       # lint (default .lean-ctx/policy.toml); exit 1 on INVALID
lean-ctx policy coverage [name] [--benchmark cgb] [--json]
                                      # automated PARTIAL CGB assessment; exit 1 on any FAIL
lean-ctx policy enforce <tool> --project-root <p> [--json '<args>'] [--as-json]
                                      # evaluate one tool call against the active
                                      # policy (deny/egress/redact/filter) + audit
lean-ctx policy org <key|sign|verify|trust|install|status>
                                      # central signed org policy (see org-policy-v1.md)
```

`coverage` statically checks a resolved pack against the Context Governance
Benchmark v1.0-draft: credential fixtures vs. redaction patterns (CGB-1.1),
named declarative rules (1.2), regulated-identifier classes (1.3), budget
cap (3.2), retention expectation (4.3), tool posture (5.4), egress
restriction (5.5). It prints PASS/FAIL/INCONCLUSIVE per aspect and an
honesty line — never a maturity grade (7 of 32 controls are statically
checkable; see `docs/compliance/cgb-self-assessment.md`).

`show --toml` prints the **unresolved** pack definition — the natural starting
point for an org-specific pack:

```
lean-ctx policy show baseline --toml > .lean-ctx/policy.toml
```

## Error vocabulary

`PolicyError` names the offending field and value; the CLI prints it verbatim:
`Toml`, `InvalidName`, `InvalidVersion`, `EmptyDescription`,
`UnknownReadMode`, `BadRegex{pattern_name}`, `ZeroMaxTokens`,
`AllowDenyOverlap`, `UnknownParent`, `ExtendsCycle`, `ExtendsTooDeep`,
`UnknownFilterAction{field}`.

## Enforcement (#673)

The resolved project pack (`.lean-ctx/policy.toml`) is applied at the MCP
server hot path. Enforcement is **opt-in**: with no project pack present nothing
is gated and behavior is identical to a pack-less install.

| Field | Where it is enforced | Effect |
|---|---|---|
| `deny_tools` / `allow_tools` | `server::policy_guard` in `call_tool_guarded`, right after the role guard | a denied tool returns a `[POLICY DENIED]` result and is audited (`ToolDenied`); an `allow_tools` allowlist is exclusive |
| `[redaction]` | `call_tool_guarded`, before the result reaches the model and the out-of-band archive | each match becomes `[REDACTED:<name>]`, on top of the built-in secret rules |
| `default_read_mode` | `ctx_read`, only when the caller omits `mode` | the pack default replaces auto/profile selection (an explicit `mode` always wins; line windows may still narrow it) |
| `max_context_tokens` | `core::budget_tracker::check` | tightens (never loosens) the per-session token ceiling; the agent hits the normal budget warning/exhausted path |
| `[filters]` (#675) | `call_tool_guarded`, same outbound chokepoint as `[redaction]` | each detector (`pii`/`classification`/`injection`) can `warn`/`redact`/`block`; a `block` replaces the content with a `[POLICY BLOCKED]` refusal so it never reaches the model |
| `[egress]` (#676) | `call_tool_guarded`, **before dispatch** of `ctx_edit` writes and `ctx_shell`/`ctx_execute` actions | a forbidden pattern, a detected secret/PII (`block_secrets`) or an exceeded `max_writes_per_min` returns a `[POLICY BLOCKED]` result and is audited (`ToolDenied`) — the write never touches disk, the command never runs |

The same guard sequence runs **without the MCP server** via `lean-ctx policy
enforce <tool> --project-root <p> [--json '<args>']`: role + policy gating,
egress DLP and output redaction/filters against the active policy (project pack
⊕ trusted org floor), recording the identical audit entries. It is the headless
path for policy testing and for producing enforcement evidence in CI — and what
`scripts/demo-great-filter.sh` drives end to end.

### Inbound content filters (#675)

`[filters]` adds net-new detectors that run **before** tool output reaches the
agent, on the same chokepoint as `[redaction]`:

- **`pii`** — Swiss AHV (EAN-13), IBAN (mod-97), payment cards (Luhn) and email,
  each checksum/shape-validated to keep false positives low. `redact` →
  `[REDACTED:<class>]`; `block` → refuse.
- **`classification`** — gates content *marked* confidential/secret (banner
  lines or a `Classification:`/`Sensitivity:` field), not prose mentions.
  `block` (the default-meaningful action) refuses; `warn` annotates.
  `blocked_labels` overrides the built-in label set.
- **`injection`** — OWASP LLM01 prompt-injection via
  `core::output_sanitizer::detect_injection`; `redact` masks the offending
  lines, `block` refuses.

Decisions are audited **privacy-preservingly** — only `(class, count)` pairs
(e.g. `pii:iban×2`), never the matched values. A `block` emits a policy
violation event (`ToolDenied`); redactions record `SecretDetected`.

### Egress / output DLP (#676)

Where `[filters]` governs what *reaches* the agent, `[egress]` governs what the
agent *emits*. It runs **before dispatch** of write/action tools (`ctx_edit`,
`ctx_shell`, `ctx_execute`), so a blocked write never touches disk and a blocked
command never runs:

- **`forbidden_patterns`** — regexes inspected against the write body / command;
  a match blocks with reason `forbidden-pattern:<source>` (the regex source, a
  non-sensitive label — never the matched text).
- **`block_secrets`** — refuses content carrying detected secrets (the active
  pack's `[redaction]` patterns, reason `secret`) or PII (the #675 checksum-
  validated detectors, reason `pii:<class>`).
- **`max_writes_per_min`** — a per-process sliding-window rate limit; the
  `max+1`-th write/action within any trailing 60 s is refused (`rate-limit`).

Blocked egress is audited `ToolDenied` with the privacy-preserving reason; the
matched content is never recorded. Egress checks are **opt-in** (no `[egress]`
section ⇒ no gating) and obey the **Local-Free Invariant** below — only the
agent's tool-driven writes/actions are gated, never a human's manual edits.

Invariants:

- **No self-lockout** — the meta tools `ctx`, `ctx_session`, `ctx_policy` can
  never be policy-denied, so an operator can always switch policy back out.
- **Fail-open on a broken pack** — an invalid `.lean-ctx/policy.toml` is logged
  and ignored (no enforcement), never bricking the agent; `lean-ctx policy
  validate` surfaces the same error.
- **Local-Free Invariant** — enforcement only constrains the *agent* pipeline
  (the tools the model calls); it never gates a human's own local reads or CLI.
- The active pack is loaded once and cached (`core::policy::runtime`); call
  `runtime::reload()` after editing the pack.

## Historical out-of-scope notes

1. **Registry/marketplace distribution** of packs (#403/MKT) — beyond the
   built-in registry and `extends`.
2. **Conformance scoring against live telemetry** — `policy coverage` (v1) is
   static pack analysis. Runtime evidence is *emitted* (denials audited as
   `ToolDenied`, redaction/filter counts as `SecretDetected`) and aggregated by
   `lean-ctx compliance report`; a continuous score is the follow-up.
3. Multi-file packs, non-built-in parents (`extends` against local files).

**Historical implementation notes:** earlier work recorded central signed
org-policy distribution (#674), inbound filters (#675), egress DLP (#676), a
server-free `policy enforce` evaluator, and a signed CISO compliance report.
They are not current public offerings or compliance claims.

## Module map

| Piece | Path |
|---|---|
| Types, parse, validate, resolve | `rust/src/core/policy/mod.rs` |
| Runtime view (load + cache active pack) | `rust/src/core/policy/runtime.rs` |
| Server-side tool gating + redaction + filter/egress audit | `rust/src/server/policy_guard.rs` |
| Inbound content filters (PII / classification / injection) | `rust/src/core/input_filters/` |
| Egress / output DLP (forbidden patterns, secret/PII block, rate limit) | `rust/src/core/egress.rs` |
| CGB coverage checks | `rust/src/core/policy/coverage.rs` |
| Built-in registry | `rust/src/core/policy/builtin.rs` |
| Built-in pack sources | `rust/src/core/policy/builtin/*.toml` |
| CLI | `rust/src/cli/policy_cmd.rs` (dispatch key `policy`) |
| Server-free enforcement evaluator (`policy enforce`) | `rust/src/cli/policy_enforce_cmd.rs` |
| Central signed org policy (sign/trust/install/floor merge) | `rust/src/core/policy/org/`, `rust/src/cli/policy_org_cmd.rs` ([org-policy-v1.md](org-policy-v1.md)) |
| Authoring guide | `docs/guides/policy-packs.md` |
