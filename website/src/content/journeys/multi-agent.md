You're running more than one AI agent on the same project — a planner and a
coder, a dev and a reviewer, or several subagents working in parallel. This
journey documents everything LeanCTX provides to make agents share context,
coordinate, hand off work, and not step on each other.

## 0. The mental model

LeanCTX already gives every session a shared, project-scoped memory (knowledge +
CCP, Journey 3). Multi-agent builds **coordination** on top of that shared memory:

| Layer | Tool | Analogy |
|-------|------|---------|
| Presence | `ctx_agent` register/status/list | "who's online" |
| Messaging | `ctx_agent` post/read | a team chat channel |
| Long-term notes | `ctx_agent` diary | each agent's lab notebook |
| Fact sharing | `ctx_agent` share_knowledge | a shared whiteboard |
| Work transfer | `ctx_handoff`, `ctx_agent handoff` | a baton pass |
| Task tracking | `ctx_task` | a shared task board |
| Context transfer | `ctx_share` | "here, look at these files I already loaded" |

All of it is persisted under the data dir (`agents/`, `handoffs/`), so it survives
restarts and works whether agents run side-by-side or one after another.

**Golden output — where presence lives.** The roster is a single file,
`~/.lean-ctx/agents/registry.json`. On a fresh project it is the empty state
below; each `ctx_agent action=register` appends an entry to `agents`:

```json
{
  "agents": [],
  "scratchpad": [],
  "updated_at": "2026-05-30T13:32:14.977520Z"
}
```

These tools are in the **standard** (`ctx_agent`) and **power** (`ctx_task`,
`ctx_handoff`, `ctx_share`) profiles.

## 1. Presence — who is working

```text
ctx_agent action=register agent_type=cursor role=dev
ctx_agent action=status status=active message="implementing auth"
ctx_agent action=list                 # all registered agents + their state
ctx_agent action=info                 # details for the current agent
ctx_agent action=sync                 # full overview: agents + pending msgs + shared ctx
```

- `agent_type`: `cursor` | `claude` | `codex` | `gemini` | `crush` | `subagent`.
- `role`: `dev` | `review` | `test` | `plan` (free-form, used for routing).
- `status`: `active` | `idle` | `finished`.
- Stale agents are auto-pruned after 24h of inactivity, so the registry never
  fills with dead PIDs.

`ctx_agent action=sync` is the single best "what's the state of the team?" call —
agents, their statuses, unread messages, and shared contexts in one response.

## 2. Messaging — the shared bus

```text
ctx_agent action=post message="auth refactor done, see verify.rs" category=status
ctx_agent action=post to_agent=<id> message="can you review src/auth.rs?" category=request
ctx_agent action=read                 # poll messages addressed to you (+ broadcasts)
```

- Omit `to_agent` to broadcast; set it for a direct message.
- `category`: `finding` | `warning` | `request` | `status`.
- Messages carry a `priority` and a `privacy` level (`Team` by default) and are
  marked read per-agent, so each agent sees each message once.

## 3. Diaries — persistent per-agent memory

A diary is an agent's own log, persisted across sessions (capped at 100 entries
per agent). It's how an agent "remembers what it was thinking" next time.

```text
ctx_agent action=diary category=discovery content="rate limiting is in middleware/rl.rs"
ctx_agent action=diary category=decision  content="chose token bucket over sliding window"
ctx_agent action=recall_diary           # read your own diary
ctx_agent action=diaries                 # list all agents' diaries
```

Diary entry types: `discovery` | `decision` | `blocker` | `progress` | `insight`.
Stored at `agents/diaries/`.

> The workspace rules already nudge agents to use this: after significant work,
> `ctx_agent(action=diary, category=…)`.

## 4. Shared knowledge — the team whiteboard

Distinct from diaries (private logs), shared knowledge is a broadcast of facts
every agent can pull.

```text
ctx_agent action=share_knowledge message="db=postgres;cache=redis;auth=jwt"
ctx_agent action=receive_knowledge       # pull facts other agents shared
```

- `message` is `key=value;key=value` pairs.
- Persisted to `agents/shared_knowledge.json` (capped at 500 facts, oldest
  dropped), and each fact records which agents have `received` it.

## 5. Handoffs — pass the baton (Context Ledger Protocol)

A handoff is a **deterministic bundle** of everything the next agent needs:
workflow state, a session snapshot, and curated knowledge facts. This is the
clean way to move work between agents (or between sessions) without re-deriving
context.

### Lightweight handoff (within the message bus)

```text
ctx_agent action=handoff to_agent=<id> message="finished impl; please run tests"
```

### Full bundle — `ctx_handoff`

```text
ctx_handoff action=create paths=["src/auth.rs","src/mw/rl.rs"]
ctx_handoff action=export write=true filename=auth-handoff.json
ctx_handoff action=list
ctx_handoff action=pull path=auth-handoff.json
ctx_handoff action=import path=auth-handoff.json
```

On `pull`/`import` you control what gets applied (all default `true`):

| Flag | Applies |
|------|---------|
| `apply_workflow` | the workflow state machine position |
| `apply_session` | the session snapshot (tasks/findings/decisions) |
| `apply_knowledge` | knowledge facts (contradictions are surfaced, not silently merged) |

- `privacy`: `redacted` (default) or `full` (admin only) for exports.
- Bundles are written to `handoffs/<ts>-<md5>.json`.

This is the production path for "agent A did the analysis, agent B implements" —
B imports A's bundle and starts with A's exact context.

## 6. Task orchestration — the shared board (A2A)

`ctx_task` is agent-to-agent task management: create tasks, assign them, track
state, and message about a specific task.

```text
ctx_task action=create description="add OAuth" to_agent=<id>
ctx_task action=list
ctx_task action=get task_id=<id>
ctx_task action=update task_id=<id> state=in_progress
ctx_task action=message task_id=<id> message="blocked on secret rotation"
ctx_task action=cancel task_id=<id>
```

Use this when work needs explicit ownership and state, rather than the looser
message bus.

## 7. Sharing loaded context — `ctx_share`

When agent A has already read and cached a set of files, A can push that context
to B so B doesn't pay to read them again.

```text
ctx_share action=push to_agent=<id> paths=["src/auth.rs","src/db.rs"]
ctx_share action=pull                 # receive contexts pushed to you
ctx_share action=list
ctx_share action=clear
```

This is a token optimization: it moves *already-compressed cached context*
between agents instead of each agent re-reading the same files.

## 8. Cost & accountability per agent

When multiple agents share a project, you'll want to know who spent what:

```bash
lean-ctx gain --agents          # savings/usage broken down per agent
```

```text
ctx_cost action=agent agent_id=<id>    # cost attribution for one agent
ctx_cost action=report                 # all agents
```

Each agent has a cryptographic identity (`keys/<agent-id>.key` / `.pub`), so
attribution and audit (`audit/trail.jsonl`) are tamper-evident.

## 9. The Token Guardian companion — `lean-ctx buddy`

A lightweight, opt-in companion (config `buddy_enabled`, default on) that
personifies the team's token health.

```bash
lean-ctx buddy show       # status / stats
lean-ctx buddy ascii      # the little guardian
```

Purely motivational/observability — it never adds tokens to agent context.

## 10. A full multi-agent walkthrough

A planner + coder + reviewer on one repo:

1. Each agent registers: `ctx_agent register agent_type=… role=plan|dev|review`.
2. Planner writes the plan to shared knowledge and creates tasks:
   `ctx_agent share_knowledge …`, `ctx_task create … to_agent=<coder>`.
3. Coder pulls context (`ctx_overview`, `ctx_compose`), implements, logs a diary
   entry, posts status, and hands off: `ctx_handoff create` → `export`.
4. Reviewer imports the bundle (`ctx_handoff import`), runs `ctx_review`, posts
   findings (`ctx_agent post category=finding`).
5. Anyone checks team state with `ctx_agent sync` and cost with `gain --agents`.

Everything in steps 2–5 persists, so a fresh session for any agent resumes
exactly where it left off.

## Storage layout (multi-agent)

| Path | Contents |
|------|----------|
| `agents/registry.json` (+ `.lock`) | the agent registry + scratchpad |
| `agents/diaries/` | per-agent persistent diaries |
| `agents/shared_knowledge.json` | broadcast facts (cap 500) |
| `handoffs/<ts>-<md5>.json` | handoff bundles |
| `keys/<agent-id>.key` / `.pub` | per-agent identity keys |
| `audit/trail.jsonl` | tamper-evident action log |
