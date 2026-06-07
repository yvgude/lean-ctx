You let the agent run `ctx_shell`, `ctx_search` and `ctx_tree` freely. Then one `rg` across a monorepo, one noisy build log, and **30k tokens of output** lands in the window — pushing out the code the agent was actually editing. This journey is the guardrail that keeps a single runaway command from evicting your working set.

---

## 1. You do nothing — the firewall is on by default

When a firewallable tool's output crosses the token threshold, LeanCTX stores the
full output out-of-band and returns a compact, deterministic **digest** instead:

```text
[ctx_search output: 31,402 tokens stored]
… head (20 lines) …
… tail (8 lines) …
Retrieve in full: ctx_expand(id="a1b2c3", search="TODO", start_line=…, end_line=…)
```

## 2. Drill into the exact slice — zero loss

The agent keeps a small, navigable footprint and can recover the *exact* slice it
needs with `ctx_expand` — by line range or full-text search across the archive.
Nothing is ever lost, only deferred.

## 3. Under the hood — `rust/src/core/firewall.rs`

- Scope is deliberately narrow: `ctx_shell`, `ctx_execute`, `ctx_search`,
  `ctx_tree`. **Explicit file reads are never firewalled** — `is_protected_read()`
  makes `ctx_read` / `ctx_multi_read` / `ctx_smart_read` the single source of truth
  for "a read always returns content the agent can edit against," honoured by both
  the firewall and the `reference_results` path.
- The digest is built **without an LLM** (head/tail, or a char-bounded excerpt for a
  single giant line), so it is reproducible and cheap.

## 4. Config

```toml
[archive]
ephemeral = true             # default on. Env: LEAN_CTX_EPHEMERAL
ephemeral_min_tokens = 4000  # threshold. Env: LEAN_CTX_EPHEMERAL_MIN_TOKENS
```

## Payoff

Runaway outputs can no longer evict the working set, with **zero loss** — the raw
output is one `ctx_expand` away.
