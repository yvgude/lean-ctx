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
references**: an eligible unchanged read can return a compact reference when the
session already holds the content. Restoring saved context depends on retained,
authorized records and the host integration. If recovery does not work, see Journey 6 →
`lean-ctx sessions doctor`.

### Managing saved snapshots — `lean-ctx sessions`

```bash
lean-ctx sessions list             # all saved snapshots
lean-ctx sessions show [id]        # inspect one
lean-ctx sessions delete <id>      # delete one snapshot
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
lean-ctx knowledge export --format okf --output ./kb-okf   # portable Markdown bundle
lean-ctx knowledge import kb.json --merge
lean-ctx knowledge import ./kb-okf --merge append          # a directory = OKF bundle
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
archived, compacted, and remaining facts. **Facts have no limit**: they leave
the active store only by quality — confidence decay, merging near-duplicates,
and archiving low-confidence or stale facts — never by count. The old
`[memory.knowledge] max_facts` setting is deprecated and ignored (an existing
value still loads). The other stores — history, procedures and patterns — are
capacity-bounded (`[memory.knowledge] max_history` / `max_patterns` and
`[memory.procedural] max_procedures`) and
reclaimed **losslessly**: when such a store reaches its cap, the lowest-value tail is
archived under `memory/archive/<store>/` (restorable) and the store settles at a
working-room target — 75% by default (`[memory.lifecycle] reclaim_headroom_pct`).
A reclaim uses hysteresis: it triggers only when a store hits its cap, never on
every write. The reclaim is **on by default**; set
`[memory.lifecycle] reclaim_enabled = false` to trim only the overflow instead.
Eviction is archived either way, so nothing is ever hard-dropped. Near capacity,
`doctor` warns and `consolidate` reclaims space.
The CLI `--all` flag scans stored project knowledge roots and invokes that same
per-project consolidation function for each one.

### Portable knowledge — OKF export/import

Beyond the native JSON export, lean-ctx can render its knowledge as an **Open
Knowledge Format (OKF)** bundle — a directory of Markdown files, one concept per
file, with relations as Markdown links. It's vendor-neutral, git-diffable, and
hand-editable, so your project knowledge is never locked in.

```bash
lean-ctx knowledge export --format okf --output ./kb-okf   # → directory of .md
lean-ctx knowledge import ./kb-okf --merge append          # a dir is read as OKF
```

The MCP tool exposes the same via `ctx_knowledge(action="export", format="okf",
path=…)` and `ctx_knowledge(action="import", path=…, merge=…)`. Exports are
**deterministic** (byte-identical for the same knowledge, #498), facts round-trip
losslessly via producer-owned `leanctx_*` frontmatter keys, and imported edges
are guarded so both endpoints must be current facts. OKF and the signed `ctxpkg`
bundle both render from one shared `KnowledgeSnapshot`, so they never disagree on
what the project knows. OKF is a **local feature, free on every plan**. See the
[OKF interop guide](../guides/okf-interop.md) and
[Knowledge Formats — which one, when](../guides/knowledge-formats.md).

### Restore limits — backups, archives, `cloud pull` and `sync`

Three ways bring knowledge back: copying a backup of the data directory,
`lean-ctx knowledge restore` (from the reclaim archive), and
`lean-ctx cloud pull` (LeanCTX Hive, from your account). `lean-ctx snapshot restore`
only resumes a saved session slice and, with `--git`, checks out its commit
(never over a dirty tree); it does not touch knowledge or account access.

- **What a restore brings back:** the facts, history, patterns and procedures
  in the backup, archive or cloud copy.
- **What it never brings back:**
  - Access you have lost. Revoked plans, seats and grants stay revoked. A
    restored entitlement file is not trusted: the record of what your account
    accepted lives in the OS credential store, not in the data directory.
    Until the next check against your account, a restored file grants nothing
    paid. After that check, you have only what the account grants today.
  - Sources you can no longer read. Archived items are checked again before
    they return.
  - Newer values. `knowledge restore` never puts an older value on a key that
    a current fact owns. `cloud pull` never overwrites a local fact and never
    writes to the cloud.
- **How conflicts show:** `cloud pull` reports
  `Conflicts: N kept local, cloud value differs: category/key, …`. Your local
  value stays, the cloud copy stays unchanged, and you decide which one is
  right (see the sync rules below).
- **Sync never replaces newer cloud values unasked.** `lean-ctx sync` (and
  the background auto-sync) reads the cloud copy first, then uploads your
  local knowledge as the new cloud copy.
  - Keys that only the cloud holds (another device's facts) are kept: the
    upload carries them along, so a sync never deletes another device's
    knowledge. A key you remove only on this machine therefore stays in the
    cloud.
  - For a key both sides hold, the upload is refused (exit code 1,
    `Knowledge upload refused: the cloud copy has newer values for: …`) when
    the cloud value differs from every value this machine holds for that
    key, current or superseded, and either:
    - the cloud entry has a timestamp newer than the key's local
      `last_confirmed`, or
    - the cloud entry has no timestamp. The encrypted cloud copy carries
      none, so a differing value this machine never saw counts as a
      conflict.

  Other surfaces (stats, commands, gotchas) still sync. Run
  `lean-ctx cloud pull` to see the cloud values. To take a cloud value,
  `knowledge remember` it and sync again. To keep your local values instead,
  `lean-ctx sync --force` replaces them in the cloud copy; keys only the
  cloud holds are still kept. If the account has no cloud copy yet, sync
  uploads. If the cloud copy can't be fetched, sync refuses the upload, and
  `--force` replaces it with the local copy. A cloud copy this device cannot
  decrypt is never replaced, not even with `--force` (see below).

### Encrypted vault — one account key on every device

Knowledge and gotchas are end-to-end encrypted under one random **account
vault key**. The service keeps only a copy wrapped under your **recovery
code**, which it never sees.

- **First device:** the first `lean-ctx sync` creates the key and shows the
  recovery code once (`XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX`). Store it in your
  password manager; LeanCTX cannot recover it.
- **Every other device, once:** `lean-ctx cloud vault unlock` asks for the
  code at a hidden prompt (or `--code-file <owner-private file>`; never on the
  command line). A typo is caught locally before anything is sent.
- **New code:** on an unlocked device, `lean-ctx cloud vault new-code` replaces
  the code; the key and your data stay the same.
- **Status:** `lean-ctx cloud status` (or `cloud vault status`) shows
  `Vault: unlocked (key 1a2b3c4d…)`, `locked on this device — run lean-ctx
  cloud vault unlock`, or `not set up`.
- **Locked device:** sync and auto-sync keep the cloud copy untouched and say
  `Personal Cloud vault locked: …` with the command that fixes it.
- **Upgrade from 3.11.4 or earlier:** vaults sealed under the old
  per-device key still open on the device that wrote them. That device
  creates the account key on its next sync and re-seals the cloud copy. If two
  devices race to create it, the first one the service stores wins; the other
  is told to unlock.

### Cloud Cockpit metrics (LeanCTX Hive)

On a signed-in LeanCTX Hive account each device also uploads the numbers its Cockpit
shows, per project and UTC day, so the Cloud Dashboard adds up all your
devices (contract `cloud-cockpit-metrics-v1`). Free accounts upload nothing.

- **What is sent:** savings and tool calls, model spend at list price, ledger
  entries by kind, guard counts, agents per host, knowledge counts and
  receipts. Never paths, file names, commands, URLs, fact keys or values.
- **Who is who:** a project is a salted id of its identity (`.lean-ctx-id`,
  else git origin, else manifest name), so one repository on two devices is
  one project. Its label is the folder name; `cloud.metrics_project_labels =
  false` sends none. The device is a random id in `<data>/cloud/device-id`
  (delete it to appear as a new device) labelled with the hostname.
- **When:** after the first tool call and on `lean-ctx sync`, at most once an
  hour, covering today and the two days before. `lean-ctx sync
  --metrics-backfill` uploads the last 90 days once. Failures retry silently.
- **Off switch:** `cloud.metrics_upload = false`. `lean-ctx cloud status`
  shows whether uploads are on and when the last one ran.

### Gotchas — auto-learned mistakes

lean-ctx auto-detects recurring error patterns and stores them as **gotchas**
(`knowledge/<hash>/gotchas.json`). View with `lean-ctx gotchas list` (alias
`bugs`). Universal (cross-project) gotchas live in
`knowledge/universal-gotchas.json`, gated by `[boundary_policy]`.

---

## 3. Starting a session right — `lean-ctx overview` / `ctx_compose`

**What it does:** A task-relevant map of the project — the ideal first call in a
new session. Combines structure, recent knowledge, and (optionally) task focus.

```bash
lean-ctx overview                       # general project map
lean-ctx overview "fix the login bug"   # task-contextualized
lean-ctx overview --json
```

Over MCP, `ctx_compose(task=…)` is the first call: ranked files, symbols and
structure for the task. The knowledge `wakeup` bundle comes with the first
`ctx_session(action="status")` or `"load"` of a session when
`enable_wakeup_ctx = true` (default), as an `--- AUTO CONTEXT ---` block. Other
tool replies never carry it, so they stay byte-stable for prompt caching.

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
