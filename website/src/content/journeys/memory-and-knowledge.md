You start a new chat in the same project tomorrow. Will your AI remember what it learned today? This journey covers how LeanCTX persists context across sessions: the Cross-Session Context Protocol (CCP), the project knowledge base, and how they get recalled automatically.

## Two kinds of memory

| Layer | Scope | Lives in | Recalled |
|-------|-------|----------|----------|
| **Session (CCP)** | one working session | `sessions/<id>.json` | auto on new session in same project |
| **Knowledge** | the whole project, forever | `knowledge/<project-hash>/` | on demand + auto at session start |

Think of CCP as "what I was doing" and knowledge as "what's true about this
project." Both are keyed by project, so a different repo gets its own memory.

## Sessions — `ctx_session` / `lean-ctx session`

**What it does:** Tracks the current session's tasks, findings, and decisions,
and snapshots them so the next session in this project can resume.

```bash
lean-ctx session task "Refactor auth module [40%]"
lean-ctx session finding "JWT validation lives in auth/verify.rs"
lean-ctx session decision "Use session cookies, not JWT, for the web UI"
lean-ctx session status        # current session state
lean-ctx session save          # snapshot now
lean-ctx session load [id]     # restore a snapshot
lean-ctx session reset         # clear current session
```

**Under the hood:** `ctx_session` writes to `sessions/<id>.json` with an atomic
write and updates `sessions/latest.json` as the pointer. The MCP tool supports
more actions: `snapshot`, `restore`, `resume`, `diff`, `verify`, `episodes`,
`procedures`.

### Auto-restore — the key feature

When you start a new session in the same project, LeanCTX injects the prior
session's context (findings, decisions, touched files, progress) into the
system prompt as the `ACTIVE SESSION` block. **You don't call anything** — it
happens because the MCP server detects the project and loads `latest.json`.

**Golden output — the restored context block.** This is the real, compact
payload (~400 tokens) a new chat receives, also viewable on demand via
`ctx_session(action="status")`:

```text
SESSION v2610 | 354h 37m | 1953 calls | 90710600 tok saved
Task: Modified: 31 files changed, 1197 insertions(+), 780 deletions(-)
Root: …/Projects/lean-ctx
Findings (20): jetbrains.rs — deps: super::super::resolve_binary_path | setter.rs (227L) | schema.rs (1425L) | ctx_search.rs — pub struct CtxSearchTool;
Files (50): [F1 …/agents/jetbrains.rs map] [F33 …/config/setter.rs signatures] [F30 …/registered/ctx_read.rs full] [F26 …/core/protocol.rs full]
```

The `[F1 … map]` / `[F33 … signatures]` entries are **persistent file
references**: the next read of `F1` costs ~13 tokens because the agent already
holds its compressed shape. This is what makes "start a new chat, it already
knows where we were" work. If it *doesn't* work, see
[Lifecycle & Maintenance](/docs/journeys/lifecycle-and-troubleshooting/) →
`lean-ctx sessions doctor`.

### Managing saved snapshots — `lean-ctx sessions`

```bash
lean-ctx sessions list             # all saved snapshots
lean-ctx sessions show [id]        # inspect one
lean-ctx sessions cleanup [days]   # prune old snapshots
lean-ctx sessions doctor [--fix]   # diagnose/repair session restore
```

> **`session` vs. `sessions`:** `session` (singular) records *into* the current
> session. `sessions` (plural, alias `session-store`) *manages the store* of
> saved snapshots. `sessions doctor` is your first stop if recall breaks.

## Knowledge — `ctx_knowledge` / `lean-ctx knowledge`

**What it does:** A persistent, project-scoped knowledge base. Facts you (or your
AI) store survive forever and are recalled across sessions — including by
semantic search.

```bash
lean-ctx knowledge remember "Payments use Stripe; webhook secret in env STRIPE_WH"
lean-ctx knowledge recall "how do payments work"
lean-ctx knowledge search "stripe"
lean-ctx knowledge status          # counts, capacity
lean-ctx knowledge health          # integrity check
lean-ctx knowledge export --output kb.json
lean-ctx knowledge import kb.json --merge
```

**Categories & confidence:** facts carry a `--category`, optional `--key`, and a
`--confidence`. High-confidence facts can be promoted to agent rules via
`lean-ctx export-rules` (see
[Advanced & Integrations](/docs/journeys/advanced-and-integrations/)).

**Recall modes** (`--mode`): `exact`, `semantic`, `hybrid`, or `auto`. Semantic
recall uses the knowledge embeddings (`knowledge/<hash>/embeddings.json`).

**Under the hood:** stored under `knowledge/<project-hash>/knowledge.json`.
The MCP tool adds richer actions: `relate`/`relations` (link facts),
`consolidate` (merge duplicates), `timeline`, `rooms`, and `wakeup` (the
session-start recall bundle). Capacity is bounded by
`[memory.knowledge] max_facts` (default 200) — at capacity, `doctor` warns and
`consolidate` reclaims space.

### Gotchas — auto-learned mistakes

LeanCTX auto-detects recurring error patterns and stores them as **gotchas**
(`knowledge/<hash>/gotchas.json`). View with `lean-ctx gotchas list` (alias
`bugs`). Universal (cross-project) gotchas live in
`knowledge/universal-gotchas.json`, gated by `[boundary_policy]`.

## Starting a session right — `lean-ctx overview` / `ctx_overview`

**What it does:** A task-relevant map of the project — the ideal first call in a
new session. Combines structure, recent knowledge, and (optionally) task focus.

```bash
lean-ctx overview                       # general project map
lean-ctx overview "fix the login bug"   # task-contextualized
lean-ctx overview --json
```

`ctx_overview` is in the **standard** profile, so most setups expose it. It pulls
the knowledge `wakeup` bundle when `enable_wakeup_ctx = true` (default), so your
AI sees relevant prior facts immediately.

## Context checkpoints — `ctx_compress`

For long conversations, `ctx_compress` creates a checkpoint that condenses what's
been seen so far, freeing context budget without losing the thread. The CLI
equivalent is `lean-ctx compress` (`--signatures` keeps API surfaces). When you
see `[CHECKPOINT]` in tool output, that's LeanCTX prompting you to record
progress via `ctx_session(action="task")`.
