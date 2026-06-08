# Conformance & Reproducibility — `conformance-v1`

Status: stable · EPIC 12.17 · Code: [`rust/src/core/conformance.rs`](../../rust/src/core/conformance.rs)

A self-check any user or CI can run to prove a lean-ctx instance honors its own
contracts and that its extension surface behaves. It is the trust anchor for the
Context OS: third-party extensions, SDKs, and domain packs can rely on these
invariants holding on every build.

## Running it

```bash
lean-ctx conformance          # human-readable scorecard
lean-ctx conformance --json   # machine-readable (CI / dashboards)
```

Exit code is non-zero if any check fails, so it gates cleanly in CI. The same
suite runs as `tests/conformance_suite.rs`.

## What it checks

| Category | Check | Invariant |
|----------|-------|-----------|
| `contracts` | `contract_versions_present` | `versions_kv()` exposes ≥1 machine-verified contract version. |
| `reproducibility` | `capabilities_deterministic` | `GET /v1/capabilities` yields identical bytes across builds. |
| `reproducibility` | `openapi_deterministic` | `GET /v1/openapi.json` yields identical bytes across builds. |
| `extensions` | `compressor:<name>` | Deterministic for equal input; output never exceeds a byte budget; never splits a UTF-8 char. |
| `extensions` | `chunker:<name>` | Deterministic; empty input ⇒ no chunks; non-empty input ⇒ ≥1 non-empty chunk. |
| `extensions` | `read_mode:<name>` | Deterministic; `full` round-trips source verbatim. |

The extension checks run against **every registered** compressor / chunker /
read-mode — built-in *and* extension-provided (`extension-registry-v1`) — so an
extension that registers a non-deterministic or budget-violating transform fails
conformance immediately.

## Scorecard shape (`--json`)

```json
{
  "version": 1,
  "passed": 9,
  "total": 9,
  "all_passed": true,
  "checks": [
    { "name": "contract_versions_present", "category": "contracts", "passed": true, "detail": "" },
    { "name": "compressor:identity", "category": "extensions", "passed": true, "detail": "" }
  ]
}
```

## Versioning

`conformance-v1` is additive: new checks/categories may be added in a minor
revision. Removing a check or weakening an invariant is a breaking change
requiring `-v2`. The corpus the extension invariants run against is an
implementation detail and may grow.
