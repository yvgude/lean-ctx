# Migrating to lean-ctx 1.0

**The short version: there is nothing to migrate.** That is the story of 1.0.

The 1.0 release is a *stability promise*, not a rewrite: the 29 protocol
contracts in [`CONTRACTS.md`](../../CONTRACTS.md) are classified
frozen/stable/experimental, the seven platform-promise contracts are
hash-frozen in CI, and the public `/v1` HTTP surface can only grow — never
shrink or mutate. Every claim below is enforced by a test you can run.

## Prove it on your machine

```bash
lean-ctx doctor --migrate-check
```

Four audits, read-only, exit 0 means ready:

| Check | What it verifies |
|---|---|
| Config schema | your `config.toml` parses and every key is a known schema key (typos and removed keys surface here) |
| Deprecations | nothing you use is on the deprecation register (`DEPRECATIONS.toml` — currently empty) |
| Data layout | your data directory is current; all on-disk formats (embedding index v1→v3, sessions, BM25 shards) self-migrate on first touch |
| Contracts | the build carries the frozen v1 contract set |

`--json` gives the machine-readable report for fleet rollouts.

## Breaking changes: none

| Surface | 0.x/3.x → 1.0 | Enforcement |
|---|---|---|
| CLI commands & flags | unchanged | `cli-contract-v1.md` (frozen) |
| MCP tools (72) | unchanged, additive only | `mcp-tools` drift tests |
| HTTP `/v1` API | additive only | `rust/tests/openapi_stability.rs` |
| config.toml keys | all 0.x keys remain valid | config schema + `doctor --migrate-check` |
| On-disk data | self-migrating, no manual steps | format version headers + auto-upgrade |
| Wire protocols (http_mcp, team-server, context-ir) | v1, hash-frozen | `rust/tests/contracts_frozen.rs` |

If you find a regression that contradicts this table, it is a release blocker:
[open an issue](https://github.com/yvgude/lean-ctx/issues) with the
`doctor --migrate-check --json` output.

## SDKs: 0.1.x → 1.0

The unified SDK family (`lean-ctx-client` on PyPI, npm, and crates.io) moves to
1.0 with the engine. No API changes — 1.0 marks the
conformance guarantee: every SDK release passes the 14-check conformance kit
against the engine it ships with
([matrix](../reference/sdk-conformance-matrix.md)), and the release pipeline
refuses to ship an engine an SDK cannot speak to
(`scripts/check-sdk-versions.py`).

```python
# 0.1.x code runs unchanged on 1.0:
from leanctx import LeanCtxClient
client = LeanCtxClient("http://127.0.0.1:7745", bearer_token="...")
client.call_tool_text("ctx_read", {"path": "src/main.rs"})
```

## When something *does* change later

The deprecation policy (CONTRACTS.md § Deprecation policy) guarantees:

1. announcement in `DEPRECATIONS.toml` ≥ 2 minor releases before removal,
2. a visible warning in `lean-ctx doctor`,
3. a documented replacement,
4. breaking a `frozen` contract is impossible — a v2 surface ships *next to* v1.
