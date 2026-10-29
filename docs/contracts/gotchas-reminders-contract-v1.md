# Gotchas/Reminders Contract v1

**Status**: Stable  
**Version**: `GOTCHAS_REMINDERS_V1_SCHEMA_VERSION = 1`  
**Runtime source**: `rust/src/core/gotcha_tracker/model.rs`

## Purpose

Formalizes the Gotcha tracking system: structured provenance, provenance-based decay, a per-project bound, and time-bounded reminders.

## Gotcha Schema

```rust
struct Gotcha {
    id: String,
    category: GotchaCategory,       // Build, Test, Config, Runtime, Dependency, Platform, Convention, Security
    severity: GotchaSeverity,        // Critical, Warning, Info
    trigger: String,
    resolution: String,
    file_patterns: Vec<String>,
    occurrences: u32,
    session_ids: Vec<String>,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    confidence: f32,
    source: GotchaSource,
    prevented_count: u32,
    tags: Vec<String>,
    provenance: Vec<ProvenanceRef>,  // NEW v1
    expires_at: Option<DateTime<Utc>>,  // NEW v1
    decay_rate_override: Option<f32>,   // NEW v1
}
```

## ProvenanceRef

Structured origin tracking replacing the previous unstructured `source: String`:

```rust
struct ProvenanceRef {
    kind: String,           // "issue", "commit", "tool_call", "agent", "manual"
    url: Option<String>,    // e.g. "https://gitlab.com/group/project/-/issues/123"
    commit_hash: Option<String>,
    tool_call_id: Option<String>,
    session_id: Option<String>,
}
```

## GotchaPolicy

Configurable via `[memory.gotcha]` in `config.toml` (env `LEAN_CTX_GOTCHA_MAX_PER_PROJECT`):

```toml
[memory.gotcha]
max_gotchas_per_project = 100       # lowest-confidence gotchas are dropped first
```

Decay depends on each gotcha's `source` (promoted 0.01, agent-reported 0.02,
cross-session 0.03, auto-detected 0.05 per week, damped by occurrences). The
policy keys `retrieval_budget_per_room`, `default_decay_rate`,
`category_decay_overrides` and `auto_expire_days` were documented here but never
applied; they were retired on 2026-10-16 and are ignored (and removed by the
automatic config migration) when an older config loads.

## Expiration

- `expires_at`: absolute time after which the gotcha is removed when the store loads
- `decay_rate_override` per gotcha: replaces the source-based decay rate

## Tool Actions

Available via `ctx_knowledge action=gotcha`:
- **Add**: `trigger`, `resolution`, `severity`, `category`
- **List**: all gotchas for current project
- **Forget**: remove by trigger

## Drift Gate

`rust/tests/contracts_md_up_to_date.rs` verifies that `GOTCHAS_REMINDERS_V1_SCHEMA_VERSION` in `contracts.rs` matches `CONTRACTS.md`.
