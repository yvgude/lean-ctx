# Context Policy Packs

Pin your team's context-governance expectations in one reviewable TOML file:
which tools agents may call, the default read mode, redaction patterns for
sensitive data, an audit-retention expectation and a context-budget cap.
Policies live in your repo, go through code review, and inherit from curated
baselines — **Policies as Code**.

```bash
lean-ctx policy list        # see what ships built in
lean-ctx policy show finance-eu
```

## Quick start

Pick the built-in closest to your posture and copy it into your repo:

```bash
mkdir -p .lean-ctx
lean-ctx policy show baseline --toml > .lean-ctx/policy.toml
lean-ctx policy validate
```

Commit `.lean-ctx/policy.toml`. From now on, governance changes are diffs.

## Built-in packs

| Pack | For |
|---|---|
| `baseline` | Any team — secret redaction (private keys, AWS, credentials, bearer tokens), 90-day audit expectation |
| `strict-redaction` | Teams handling customer data — adds JWT, GitHub/GitLab/Slack tokens, OpenAI/Anthropic/Stripe keys, DB connection strings; compact `map` reads |
| `open-source` | Public repos — permissive, but secrets stay covered |
| `finance-eu` | EU financial services — adds IBAN, payment cards, EU VAT, SWIFT/BIC; denies web fetches; 1-year audit expectation |
| `healthcare` | HIPAA-aligned — adds SSN, MRN, member ids, DOB, NPI; denies web fetches; 6-year audit expectation |
| `soc2-context` | SOC 2 TSC alignment (CC6.1 access, CC6.6 boundary, C1.1 confidentiality) |
| `iso42001-aligned` | ISO/IEC 42001 Annex A (A.7.4 data filtering, A.9.2/A.9.4 use control) |
| `eu-ai-act-deployer` | EU AI Act deployer (Art. 10(5) data, 14(4)(e) cap, 26(6) retention) |

The five regulated packs (`finance-eu`, `healthcare`, `soc2-context`,
`iso42001-aligned`, `eu-ai-act-deployer`) ship live `[filters]` (PII redaction +
prompt-injection handling) and `[egress]` DLP (`block_secrets`, write-rate cap)
on top of their redaction — the runtime filter matches the posture they
advertise. Inspect any pack resolved (`lean-ctx policy show healthcare`) or raw
(`--toml`).

## Writing your own pack

Extend a built-in and override only what differs:

```toml
name = "acme-platform"
version = "1.0.0"
description = "ACME platform team — strict redaction plus internal identifiers"
extends = "strict-redaction"

[context]
default_read_mode = "map"
deny_tools = ["ctx_url_read"]
max_context_tokens = 16000

[redaction]
employee_id = 'EMP-\d{6}'
internal_host = '\b[a-z0-9-]+\.corp\.acme\.com\b'

[filters]
pii = "redact"            # off | warn | redact | block
classification = "block"  # refuse files marked confidential/secret
injection = "redact"      # mask prompt-injection lines (OWASP LLM01)

[egress]
forbidden_patterns = ['\.prod\.acme\.internal']  # block writes/actions hitting prod
block_secrets = true      # refuse writes/actions carrying secrets or PII
max_writes_per_min = 30   # rate-limit agent writes/actions
```

Validate before committing:

```bash
lean-ctx policy validate            # checks .lean-ctx/policy.toml
lean-ctx policy show project        # the resolved, effective policy
```

### Inheritance rules (predictable on purpose)

- **Scalars** (`default_read_mode`, `max_context_tokens`,
  `audit_retention_days`): your value wins when set.
- **`deny_tools`, `[redaction]`, `filters.blocked_labels` and
  `egress.forbidden_patterns`**: accumulate down the chain — you can add
  restrictions, never silently drop a parent's. A redaction entry with the
  same name re-points that pattern.
- **`allow_tools`**: setting it replaces the parent's list (an allowlist is a
  deliberate posture choice). A tool can never end up both allowed and denied
  — that's a validation error.

### Validation catches

- unknown/typo'd keys (`alow_tools` → hard error)
- bad names/versions, empty descriptions
- unknown read modes (must be one of the documented `ctx_read` modes)
- regexes that don't compile (with the pattern name in the error)
- `extends` to unknown packs, cycles, chains deeper than 8
- allow/deny overlaps

## Personal Pro rules (staging)

The installed Pro runtime prepares personal mask/block rules; their resulting
policy stays in the existing `.lean-ctx/policy.toml`. Existing OSS regex rules
remain available. Published protection keeps working after a license expires;
preparing or publishing a new Pro rule set requires `personal_protection`.

```json
{"schema_version":1,"rules":[
  {"id":"customer","action":"mask","matcher":{"kind":"field","names":["customer_number","Kundennummer"]}},
  {"id":"restricted","action":"block","matcher":{"kind":"pattern","pattern":"RESTRICTED-[0-9]{6}"}}
]}
```

```bash
lean-ctx policy personal preview rules.json
lean-ctx policy personal preview rules.json --stdin < sample.txt
lean-ctx policy personal status
lean-ctx policy personal apply rules.json --expected-sha256 <current-policy-sha256>
```

Omit `--expected-sha256` only when creating the first project policy. Updates
replace only previously recorded managed personal rules and preserve other local
settings, including older hand-authored `personal_*` rules. A name collision
with an unmanaged rule is refused; organizational restrictions still apply. The reported SHA-256 is
the publication revision. Concurrent cooperative publishers serialize, and a
stale revision refuses the update. Files remain private and publication is atomic.
The project and policy directory must belong to the current user and must not be
writable by other users; unsafe existing permissions are refused, never changed
implicitly.

Field selectors match literal labels in single-line JSON, key=value and colon
text, including quoted values with escaped quotes. They mask the whole
assignment. They do not decode escaped JSON keys, XML, multiline values or other
document structures; masked output is context text, not necessarily valid JSON.
Use explicit patterns for other text conventions. Rule sets contain at most32
rules and use bounded regex compilation; no programs or project tests are run.

The shared open policy format also supports `[filters.blocked_patterns]`:
named regexes block original content before any redaction. Inherited and
organization patterns accumulate, including same-name collisions. Exceeding
resolved rule bounds refuses protected output. Preview with `--stdin` returns
only filtered sample text or a block decision. No source content goes to the
private compiler, network, product analytics or model through this command.
Preview also retains the current policy until publication; previewing a weaker
draft does not release currently blocked material, and a policy change while
reading the sample refuses the result.

Direct `lean-ctx read` calls under an active project policy check access and
original source content before rendering any mode, including `raw` and line
ranges. They reacquire the source rather than reuse legacy daemon/disk results
without a caller-policy receipt, and recheck policy authority before output.
Compression fallbacks receive only admitted text. A read outside the project,
uninspectable content or a failed policy check returns no content. These reads
measure compression from the admitted text; they do not count removed sensitive
bytes as compression savings. This CLI read behavior does not qualify every
legacy CLI command or replace the separate protected Codex/SDK release gates.

## Automated CGB coverage

```bash
lean-ctx policy coverage              # project pack (.lean-ctx/policy.toml)
lean-ctx policy coverage finance-eu   # any built-in or .toml path
lean-ctx policy coverage --json       # machine-readable, CI-friendly
```

`policy coverage` runs an automated **partial** assessment of a resolved
pack against the [Context Governance Benchmark](../compliance/cgb-self-assessment.md)
(v1.0-draft). It checks what a static pack analysis can honestly check —
credential redaction against synthetic fixtures (CGB-1.1), declarative rules
(1.2), regulated-identifier classes (1.3), budget cap (3.2), retention
expectation (4.3), tool posture (5.4) and egress restriction (5.5) — and
reports `PASS`/`FAIL`/`INCONCLUSIVE` per aspect.

It deliberately **never prints a maturity grade**: 7 of 32 controls are
statically touchable; the rest need the manual assessment (spec repo,
`assessment/TEMPLATE.md`). Exit code is non-zero when any check fails, so
you can gate CI on it.

## How enforcement works (#673)

Once `.lean-ctx/policy.toml` exists, the resolved pack is enforced for every
agent tool call:

- **Tool gating** — a tool in `deny_tools` (or absent from an `allow_tools`
  allowlist) is refused with a `[POLICY DENIED]` message and recorded in the
  audit trail. The agent sees the refusal and moves on.
- **Redaction** — every `[redaction]` pattern (plus the built-in secret rules)
  is applied to tool output *before the model sees it*, replacing matches with
  `[REDACTED:<name>]`.
- **Default read mode** — when an agent calls `ctx_read` without a `mode`, your
  `default_read_mode` is used. An explicit `mode` always wins.
- **Token cap** — `max_context_tokens` lowers the session token budget; the
  agent hits the usual budget warning/exhausted path at your ceiling.

Guarantees that keep this safe:

- **Opt-in** — no `.lean-ctx/policy.toml`, no enforcement.
- **Never locks you out** — `ctx`, `ctx_session` and `ctx_policy` are always
  allowed, so you can inspect or switch policy even under a strict allowlist.
- **Fails open** — a pack that doesn't parse is logged and ignored rather than
  blocking work; fix it with `lean-ctx policy validate`.
- **Local-Free** — only what the *agent* does is governed. Your own reads, edits
  and `lean-ctx -c` shell commands are never gated.
- The pack is cached after first use; restart the session/daemon to pick up
  edits.

What `policy show` resolves is exactly what gets enforced.

### Test enforcement without the server

`lean-ctx policy enforce` runs the **exact same guards** as the live agent
pipeline — role/policy gating, egress DLP, output redaction and filters — for a
single tool call, **without starting the MCP server**, and records the same
audit entries. Use it to prove what a pack does before you roll it out, or to
produce enforcement evidence in CI:

```bash
lean-ctx policy enforce ctx_url_read --project-root .            # → DENIED (deny_tools)
lean-ctx policy enforce ctx_shell    --project-root . \
    --json '{"command":"echo TOKEN=sk-live-…"}'                  # → BLOCKED (egress)
lean-ctx policy enforce ctx_search   --project-root . \
    --json '{"pattern":"IBAN","path":"."}' --as-json            # → ALLOWED, redactions + filters
```

It honors the active policy (project pack ⊕ trusted org floor), so the verdict
is exactly what an agent on this endpoint would hit. `scripts/demo-great-filter.sh`
chains these into the full CISO flow: sign → install → enforce → signed
compliance report → offline verify.

## Input filters (#675)

The `[filters]` section adds net-new detectors that scan tool output **before it
reaches the agent** — the input side of the filter regulated teams ask for. Each
takes an action: `off`, `warn` (let through + audit), `redact` (mask matches), or
`block` (refuse the content).

```toml
[filters]
pii = "redact"                       # Swiss AHV, IBAN, payment cards, email
classification = "block"             # gate confidential/secret-marked files
injection = "block"                  # OWASP LLM01 prompt-injection
blocked_labels = ["CONFIDENTIAL", "TS//SCI"]   # optional: your own label set
```

- **PII** is checksum-validated (Luhn for cards, mod-97 for IBAN, EAN-13 for
  AHV), so a random 16-digit order number is not mistaken for a card.
- **Classification** only fires on an actual *marking* — a banner line
  (`CONFIDENTIAL` on its own line) or a `Classification:`/`Sensitivity:` field —
  not the word used in a sentence.
- **Injection** masks (or blocks) lines carrying known role-override /
  token-smuggling patterns, leaving the rest of the file intact.

Every decision is audit-logged **without leaking the data**: only the detector
class and a count are recorded (e.g. `pii:iban×2`), never the matched value. A
`block` returns a `[POLICY BLOCKED]` message in place of the content. Filters
inherit like the rest of the pack — actions override, `blocked_labels`
accumulate — and obey the same opt-in / fail-open / Local-Free guarantees.

## Egress / output DLP (#676)

Where `[filters]` scans what reaches the agent, `[egress]` scans what the agent
*writes and runs* — the output side. It checks the payload of `ctx_edit` writes
and `ctx_shell`/`ctx_execute` actions **before they execute**, so a blocked write
never touches disk and a blocked command never runs.

```toml
[egress]
forbidden_patterns = ['\.prod\.acme\.internal', 'DROP\s+TABLE']
block_secrets = true        # refuse content carrying detected secrets or PII
max_writes_per_min = 30     # sliding-window rate limit on agent writes/actions
```

- **`forbidden_patterns`** — if any regex matches the write body or command, the
  action is refused (e.g. stop the agent editing a prod connection string or
  running a destructive query).
- **`block_secrets`** — reuses your `[redaction]` patterns and the #675 PII
  detectors to stop the agent from *writing out* a secret or personal data.
- **`max_writes_per_min`** — caps how many writes/actions the agent may perform
  per minute; the next one inside the window is refused until it ages out.

A blocked egress returns a `[POLICY BLOCKED]` message and is audited
(`ToolDenied`) with a non-sensitive reason (`forbidden-pattern:…`, `secret`,
`pii:…`, `rate-limit`) — never the matched content. Egress is opt-in (no
`[egress]` section ⇒ nothing gated) and Local-Free: only the agent's tool-driven
writes/actions are checked, never your own manual edits.

Full contract: `docs/contracts/context-policy-packs-v1.md`.

## Compliance report (#677)

Policy packs *do* the governance; the compliance report *proves* it. One command
folds the engine's evidence surfaces into a single **Ed25519-signed** artifact
for a date range — the thing a CISO or auditor actually signs off on:

```bash
lean-ctx compliance report \
  --from 2026-01-01T00:00:00Z --to 2026-03-31T23:59:59Z \
  --framework eu-ai-act --framework iso42001 \
  --pack regulated-eu --format pdf --out q1-report.pdf
# → writes q1-report.json  (signed, always — the verifiable deliverable)
#   and     q1-report.pdf   (human rendering)
# Without --out, the signed JSON lands in
#   ~/.local/share/lean-ctx/compliance/report-v1_<timestamp>.json
```

The artifact bundles, for the period:

- **OWASP Top-10-for-Agents alignment** — how the active controls map to the
  agentic threat list.
- **Framework coverage** — EU AI Act / ISO 42001 / SOC2 rows, verified *live*
  against the resolved pack (not a static claim).
- **Enforcement evidence** — what was **blocked** (`ToolDenied`) and **redacted**
  (`SecretDetected`), folded from the append-only audit chain; the segment's
  `head_hash` is bound into the signed payload.
- **Retention posture** — the pack's `audit_retention_days` intent vs. your
  plan entitlement.

Honest by construction: a quiet quarter reports **zero** blocks rather than
inventing activity, and a broken local audit chain is surfaced
(`chain_valid = false`), never hidden. The signed JSON is offline-verifiable with
no audit trail and no LeanCTX install:

```bash
lean-ctx compliance verify q1-report.json
# → VALID — signature verifies (Ed25519, offline)
#     Signer key: <key-id> · Period: … · Audit head: <sha-256>
```

`--format json` (default) writes only the signed artifact; `--format csv|pdf`
additionally emits that human rendering — the PDF is a real, dependency-free
PDF 1.7. Full contract: `docs/contracts/compliance-report-v1.md`.

## Central org policy (#674)

Everything above is *project-local*: one machine, one `.lean-ctx/policy.toml`.
`policy org` makes one policy **central and un-bypassable** across a fleet. An
admin signs a pack into a distributable artifact; each endpoint pins the org's
public key once, then installs artifacts that the runtime folds in as a **floor**
beneath the local pack — the local pack can only *tighten* it, never weaken it.

**Admin — sign and distribute:**

```bash
lean-ctx policy org key --org acme            # show/create the org key + its public key
lean-ctx policy org sign acme-floor.toml --org acme -o acme.signed.json
# → distribute acme.signed.json AND the printed public key
```

**Endpoint — pin once, then install:**

```bash
lean-ctx policy org trust <pubkey-hex> --org acme   # pin the org key (out-of-band)
lean-ctx policy org install acme.signed.json        # verify + install; enforced on next call
lean-ctx policy org status                          # see the effective floor ⊕ local pack
```

How the floor merges with the local pack — always toward the **stricter** side:

- `deny_tools` union; `allow_tools` intersect (an allowlist can only narrow);
- `redaction` union with **org patterns winning** a name clash;
- `filters` keep the stronger action (`block` > `redact` > `warn` > `off`);
- `egress` patterns union, `block_secrets` true wins, the tighter rate limit wins;
- the smaller `max_context_tokens`, the longer `audit_retention_days`.

So a user editing `.lean-ctx/policy.toml` cannot drop an org deny, replace an org
redaction pattern, or raise a cap above the org limit. Two independent checks gate
application: the artifact's **Ed25519 signature** must verify *and* its signer key
must be **pinned** here — a forged or untrusted artifact is ignored, never
enforced, and never bricks the agent (fail-open). With no key pinned, nothing is
enforced (opt-in). Sign with `--advisory` to distribute a policy for preview
(`policy org show`) without enforcing it yet. Verify any artifact offline:

```bash
lean-ctx policy org verify acme.signed.json
# → VALID — signature verifies (Ed25519, offline) · trust: TRUSTED
```

Full contract: `docs/contracts/org-policy-v1.md`.
