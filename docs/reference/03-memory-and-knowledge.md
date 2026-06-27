# Journey 3 — Memory & Knowledge

> You start a new chat in the same project tomorrow. Will your AI remember what
> it learned today? This journey covers how lean-ctx persists context across
> sessions: the Cross-Session Context Protocol (CCP), the project knowledge base,
> and how they get recalled automatically.

Source files referenced here:
- `rust/src/cli/session_cmd.rs` — `session` / `sessions`
- `rust/src/cli/knowledge_cmd.rs` — `knowledge`
- `rust/src/tools/registered/ctx_session.rs`, `ctx_knowledge.rs` — MCP tools
- `rust/src/core/session/` — CCP storage
- `rust/src/cli/overview_cmd.rs` — `overview`

---

## 0. Two kinds of memory

| Layer | Scope | Lives in | Recalled |
|-------|-------|----------|----------|
| **Session (CCP)** | one working session | `sessions/<id>.json` | auto on new session in same project |
| **Knowledge** | the whole project, forever | `knowledge/<project-hash>/` | on demand + auto at session start |

Think of CCP as "what I was doing" and knowledge as "what's true about this
project." Both are keyed by project, so a different repo gets its own memory.

---

## 1. Sessions — `ctx_session` / `lean-ctx session`

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
write (`.tmp` → rename) and updates `sessions/latest.json` as the pointer. The
MCP tool supports more actions: `snapshot`, `restore`, `resume`, `diff`,
`verify`, `episodes`, `procedures`.

### Auto-restore — the key feature

When you start a new session in the same project, lean-ctx injects the prior
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
knows where we were" work. If it *doesn't* work, see Journey 6 →
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

---

## 2. Knowledge — `ctx_knowledge` / `lean-ctx knowledge`

**What it does:** A persistent, project-scoped knowledge base. Facts you (or your
AI) store survive forever and are recalled across sessions — including by
semantic search.

```bash
lean-ctx knowledge remember "Payments use Stripe; webhook secret in env STRIPE_WH"
lean-ctx knowledge recall "how do payments work"
lean-ctx knowledge search "stripe"
lean-ctx knowledge status          # counts, capacity
lean-ctx knowledge health          # integrity check
lean-ctx knowledge consolidate     # import session + run lifecycle
lean-ctx knowledge consolidate --all
lean-ctx knowledge export --output kb.json
lean-ctx knowledge import kb.json --merge
```

**Categories & confidence:** facts carry a `--category`, optional `--key`, and a
`--confidence`. High-confidence facts can be promoted to agent rules via
`lean-ctx export-rules` (Journey 5).

**Recall modes** (`--mode`): `exact`, `semantic`, `hybrid`, or `auto`. Semantic
recall uses the knowledge embeddings (`knowledge/<hash>/embeddings.json`).

**Under the hood:** stored under `knowledge/<project-hash>/knowledge.json`.
The MCP tool adds richer actions: `relate`/`relations` (link facts),
`consolidate`, `timeline`, `rooms`, and `wakeup` (the session-start recall
bundle). `ctx_knowledge action=consolidate` and
`lean-ctx knowledge consolidate` call the same implementation: if a latest
session exists, findings/decisions/history are imported first; if not, session
import is skipped. Both paths then run the memory lifecycle over all project
knowledge and report `run_memory_lifecycle` stats: decayed, consolidated,
archived, compacted, and remaining facts. Every store — facts, history,
procedures and patterns — is capacity-bounded (`[memory.knowledge] max_facts` /
`max_history` / `max_patterns` and `[memory.procedural] max_procedures`) and
reclaimed **losslessly**: when a store reaches its cap, the lowest-value tail is
archived under `memory/archive/<store>/` (restorable) and the store settles at a
working-room target — 75% by default (`[memory.lifecycle] reclaim_headroom_pct`).
A reclaim uses hysteresis: it triggers only when a store hits its cap, never on
every write. The reclaim is **on by default**; set
`[memory.lifecycle] reclaim_enabled = false` to trim only the overflow instead.
Eviction is archived either way, so nothing is ever hard-dropped. Near capacity,
`doctor` warns and `consolidate` reclaims space.
The CLI `--all` flag scans stored project knowledge roots and invokes that same
per-project consolidation function for each one.

### Gotchas — auto-learned mistakes

lean-ctx auto-detects recurring error patterns and stores them as **gotchas**
(`knowledge/<hash>/gotchas.json`). View with `lean-ctx gotchas list` (alias
`bugs`). Universal (cross-project) gotchas live in
`knowledge/universal-gotchas.json`, gated by `[boundary_policy]`.

---

## 3. Starting a session right — `lean-ctx overview` / `ctx_overview`

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

---

## 4. Context checkpoints — `ctx_compress`

For long conversations, `ctx_compress` creates a checkpoint that condenses what's
been seen so far, freeing context budget without losing the thread. The CLI
equivalent is `lean-ctx compress` (`--signatures` keeps API surfaces). When you
see `[CHECKPOINT]` in tool output, that's lean-ctx prompting you to record
progress via `ctx_session(action="task")`.

---

## UX notes captured during this walkthrough

- "Does it remember?" is the #1 retention question. The auto-restore mechanism
  works without any user action, but it's invisible — `sessions doctor` is the
  documented way to *prove* it's working.
- `session` vs. `sessions` is a genuine naming hazard; both the help text and
  this journey now call out the singular/plural distinction explicitly.
