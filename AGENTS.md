# LeanCTX — Context Gateway for AI Systems

LeanCTX Engine selects context, applies supported access and content controls, and records observable context operations; compression is one capability within that path.

## Shared Context Gateway delivery skill

For LeanCTX implementation, integration, release, or repository migration,
load `skills/leanctx-gateway-delivery/SKILL.md`.
Codex and Claude discover the same canonical content through the
`.agents/skills/leanctx-gateway-delivery` and
`.claude/skills/leanctx-gateway-delivery` links.

## Mandatory Routing Decision (Direct or Swarm)

For every substantive task in this repository, load and follow
`docs/internal/skills/agent-orchestration.md` before taking task actions. The
**routing decision** is mandatory; a swarm is not. The lead uses 0 to 15
workers (0 = direct execution) and picks the path with the shortest
accepted-result time that keeps the full quality bar — the routing gate in the
skill defines how. Fifteen agents is a hard concurrent maximum; every worker
gets a distinct role. Security, redaction, public contracts/schemas,
migrations, releases, and data-loss risk normally get an independent reviewer.

This rule is owned by the orchestration lead. Agents with a concrete delegated
subtask are swarm workers: they coordinate through lean-ctx and complete their
assigned role, but do not recursively launch another swarm unless the lead
explicitly asks them to do so.

- **Runtime:** every Codex CLI agent MUST use `--model gpt-6-luna` and
  `-c 'model_reasoning_effort="max"'`.
- **LeanCTX coordination:** every agent registers on the lean-ctx agent bus,
  checks directives, and uses lean-ctx context tools; the lead records the
  integrated decision and progress through lean-ctx.
- **Topology:** assign independent, non-duplicative roles. When a reviewer is
  used on a non-parallel change, it validates the implementer's result after
  it is ready — including whether each new test earns its place (see
  Test Policy).
- **Safe ownership:** only one agent may edit a given file or shared worktree at
  a time. Parallel agents investigate, test, or review in isolation unless
  explicit file ownership or separate worktrees are assigned.
- **Completion:** the lead integrates the findings, runs the relevant quality
  gates, and reports the agents' evidence. A task is not complete merely
  because an agent returned successfully.

Direct conversational answers, status updates, and clarification questions are
not substantive tasks and do not require a swarm. The user may explicitly
request a different runtime or a single-agent exception.

## Integration Mode: Replace

Native Read/Grep/Glob/Shell are **denied by policy**. lean-ctx MCP tools are the
only path for reading files and running commands:

- **Reads/Search** → `ctx_read`, `ctx_search`, `ctx_compose` (cached, 10 modes, images via ContentBlock)
- **Shell commands** → `ctx_shell` (95+ compression patterns)
- **File editing** → native Edit/StrReplace (lean-ctx only handles READ operations)
- **File finding** → `ctx_glob`, `ctx_tree`

Native tools will return a deny error with instructions to use ctx_* equivalents.
Override: set `hook_mode = "hybrid"` in `~/.config/lean-ctx/config.toml`.

## Subagent Compatibility (Cursor / Claude Code)

Cursor blocks MCP tools in `readonly: true` subagents (Task tool).
lean-ctx tools are MCP tools → they WILL FAIL in readonly subagents.

**Rules for spawning subagents:**
- Always set `readonly: false` when the subagent needs ctx_* tools
- Use `subagent_type: "generalPurpose"` — NOT `"explore"` (always readonly)
- lean-ctx tools declare `readOnlyHint: true` in MCP annotations

**Fallback (MCP unavailable):** deny hook detects MCP-down and allows native tools.

## CLI commands (optimized shell, lower overhead)

```bash
git status                   # compressed by configured agent shell/wrapper
cargo test                   # compressed by configured agent shell/wrapper
lean-ctx -c "git status"     # only if explicitly requested / documented unwrapped
lean-ctx ls src/              # directory map
```

## Development Workflow

When working on lean-ctx itself:

1. **Iterate**: `cd rust && cargo check --locked`, then the affected tests.
2. **Build a runnable development binary**: `cargo fast` (incremental, no LTO).
3. **Verify the final diff once**: `cargo test --lib` + `cargo clippy --all-features -- -D warnings` + `cargo fmt --check`; repeat when changes or failures justify it.
4. **Install**: `lean-ctx dev-install` or `make dev` (build→atomic stop/install→restart).

### Build budget for parallel agents (owner 2026-10-30)

The engine crate `lean_ctx` is large; every compile with codegen costs
8–17 GB RAM and queues behind the machine's compile gate. With many agents
per machine, memory runs out. Therefore:

- **Workers** iterate with
  `cargo check --locked --all-targets --config build.incremental=true` only
  (type checks code and tests, no codegen). Incremental is off in the global
  Cargo config to save disk, but for this edit loop it pays: measured
  2026-10-31 after a one-line edit, `--all-targets` 95 s → 18 s and plain
  `check` 40 s → 11 s; the first incremental check of a worktree builds the
  cache (minutes) and keeps about 5.5 GB under its target dir. A worker runs
  affected tests at most once per task, filtered, and only when the task needs
  behaviour evidence.
- **Workers do not run** `cargo fast`, `cargo build`, `--release`,
  `dev-install`/`make dev`, or the full `cargo test --lib`.
- **The integrator** (lead) runs `cargo fast`, `dev-install` and the full gates
  once per integrated batch, on one machine at a time.
- A worker that needs a runnable binary asks the lead instead of building one.

### Split before you grow (owner 2026-11-01)

The engine crate `lean_ctx` is being split into workspace crates; Rust stays
the language. Every change and every new feature is checked for that split
before it is written, and the check is part of the handover:

- **New code goes into a crate, not into the monolith,** whenever it has a
  clear boundary: its own crate under `rust/crates/`, or the crate of the area
  it belongs to once that exists. New code in `rust/src/` needs a reason in the
  handover (what it needs from the monolith, and why it cannot depend on a
  crate instead).
- **The map is `rust/CRATE_MAP.md`** (machine form `rust/CRATE_MAP.toml`):
  extracted crates, modules scheduled for a crate, and the allowed direction.
  Crates never depend on `lean-ctx`. The engine depends on them through a thin
  facade (for example `core::patterns`), so engine paths stay stable. New code
  for an extracted area goes into its crate, never beside the facade.
- **Changes to existing code do not deepen the coupling:** no new
  `crate::` edges out of a module that is split out or scheduled for a split;
  shared types move down into a lower crate rather than up.
  `scripts/check-crate-boundaries.py` (preflight and the pounce gate) fails on
  such an edge. Its baseline `rust/crate_boundary_baseline.json` only grows with
  the crate-split lead's sign-off ("monolith: <reason>" plus `--update`), and it
  shrinks with every edge you cut.
- **A new heavy dependency** (a parser, ML runtime, HTTP stack, database
  client) lands behind a feature in its own crate, never directly in
  `lean_ctx`.
- **The handover names the decision:** "crate: <name>" or "monolith: <reason>".
  The integrator rejects a batch without it.
- Another language only where the component does not need Rust and an ADR
  shows the benefit (the plan's §10 gates); signed, crypto and policy logic is
  never implemented twice.

Published artifacts and release rehearsals keep the production `--release`
profile. Do not use `cargo clean` in the normal edit/build loop. Use one shared
target directory and the configured job budget across worktrees.

Run direct Unix test commands in a subprocess with `umask 077` so temporary
fixtures stay private; Make, preflight and CI configure this without changing
the login shell's umask.

Do not run `lean-ctx stop` before builds or tests. It unloads the user's global
proxy and daemon even though Cargo writes only to the worktree's target directory.
`lean-ctx dev-install` performs the required short stop immediately before replacing
the installed binary and restores autostart afterward.

## Session Continuity

lean-ctx automatically persists session context across restarts:
- **Findings**: Recent tool results (reads, searches, test outcomes)
- **Decisions**: Architecture choices made during the session
- **Files**: Touched files with summaries and modification status
- **Progress**: Task completion state and next steps

This data is delivered as a `--- AUTO CONTEXT ---` briefing on the first
`ctx_session` call with `action="load"`, `"status"` or `"show"` — never
prepended to other tool results, which stay deterministic for provider prompt
caching (#498). With `minimal_overhead = false` it is additionally injected at
session start via the `ACTIVE SESSION` LITM block.

Continuity is per agent: the briefing (last task, recent decisions), session
findings in `ctx_knowledge search` and every other session-continuity read come
only from the calling agent's own session, or from what an explicit handoff
brought into it — never from the project's latest saved session, which may
belong to another agent. A new agent starts with project knowledge only; after
a restart, resume your own session with `ctx_session` with `{"action":"load","session_id":"<your session id>"}`.
Project knowledge (`ctx_knowledge`) stays shared by all agents of the project.

### Active Documentation (Agent Responsibility)

After completing a significant task (implementation, bugfix, refactoring):
1. Record the decision: `ctx_knowledge(action="remember", category="decision", content="...")`
2. Record progress: `ctx_session(action="task", value="<current task> [N%]")`
3. Record blockers: `ctx_knowledge(action="remember", category="blocker", content="...")`

**When the checkout is linked to an emspace space** (a `.emspace` file at or
above the root), the space is the one place for decisions, blockers and tasks:
write them there (a note on the owning page, or a task) and **skip steps 1 and
3** — do not write the same decision twice. lean-ctx reaches the space
through the Emspace provider (below).
Step 2 (`ctx_session` progress) stays, since it is per-agent session state.

Working the space economically:
- read `emspace schema` at most once per session;
- read the part of a page you need: `ctx_provider(action="query",
  provider="emspace", resource="page", id="<slug>", mode="text",
  query="outline")`, then the one section (`query="<section>"`); the whole
  body is `emspace page show --body <slug>…` (several slugs in one call);
- before documenting, check the checkout is the base the space documents, and
  rescan only what `emspace coverage` and `emspace status` (stale) name;
- find a page for a file through the `ctx_read` hint rather than a separate
  `emspace search` → `page show` chain;
- check open sessions through `ctx_agent read` instead of a separate
  `emspace orient` when only the in-flight work matters.

Keeping knowledge current without rewriting it:
- `ctx_knowledge(action="changes", query="<lsn>")` lists what changed since
  an LSN (every save is journaled); `query="session"` lists what this session
  wrote, and a `ctx_session` handoff names that changeset;
- a fact marked *to verify* because its anchored code changed but the fact
  still holds: `ctx_knowledge(action="repin", category=…, key=…)` re-anchors
  it to the code as it is, without retyping its text;
- `ctx_knowledge(action="coverage")` lists the most edited files (90 days)
  no fact is anchored to: what to record next.

After 30+ tool calls without documentation:
- lean-ctx will prompt with `[CHECKPOINT: please document current progress]`
- Respond by calling `ctx_session(action="task")` with current status

## Provider Pipeline (Context Engine)

External data sources (GitHub, GitLab, Jira, Postgres, MCP bridges, custom REST) are first-class citizens.
All provider data flows through the same consolidation pipeline:

1. `ContextProvider::execute()` → raw `ProviderResult`
2. `consolidation::consolidate()` → `ConsolidationArtifacts` (BM25 chunks, graph edges, knowledge facts, cache entries)
3. `apply_artifacts_to_stores()` → persists to BM25 index, Graph index, ProjectKnowledge, Session cache (background thread)

This means `ctx_semantic_search` finds issues/PRs/tickets, `ctx_knowledge` recalls provider facts,
and `ctx_read` shows cross-source hints (e.g. "Issue #42 references this file"); a profile can
turn them off with `output_hints.cross_source_hint = false`.

**Emspace:** a linked checkout needs no setup. `ctx_read` indexes the space in
the background (a page change shows within about a minute) and, with
`output_hints.cross_source_hint` on (the default), names the page that
documents the file: `emspace://pages/<slug> [documents]`, or
`[documents, stale]` when its sources moved (knowledge to repair, not to
trust). No manual indexing is needed. Emspace pages are not copied into
`ctx_knowledge`; the space stays their one source.
`ctx_agent read`/`list` show the space's open sessions beside the bus,
so the directive check also shows what other agents declared there.
`resource="search"` (with `query`), `"tasks"` (with `state`) and `"sessions"`
answer without leaving lean-ctx. The provider only reads; writes stay with the
`emspace` CLI, whose page bodies, `--json`, `schema` and listings `ctx_shell`
passes unchanged; only a single `page show` header is shortened. `emspace` is
on the default shell allowlist (3.12.1).

## Agent Bus Registration (mandatory, first action)

Every ephemeral coding worker MUST register through the local MCP bus as its
first MCP operation: call `ctx_agent` with `action="register"`, its host type
(`codex`, `cursor`, or `claude`), and its assigned role. Retain the returned
local ID, then call `ctx_agent` with `action="read"` to check directives.

`lean-ctx agent register --id "<type>-$$" ...` is intentionally **not** for
ephemeral workers: it creates a durable, attested identity and using a shell
PID there turns normal short-lived worker history into misleading "active
agents". Reserve that CLI command for a deliberately provisioned, long-lived
identity with an owner. Use `ctx_agent action="list"` for project-scoped
workers, or `lean-ctx agent presence` for the local process view.

## Branch Hygiene (mandatory)

GitHub remote must stay clean: only `main` + `cla-signatures` + max 1 active PR branch per agent.

- **After merge → delete source branch**: `SKIP_PREFLIGHT=1 git push github --delete <branch>`
- **Branch naming**: `<type>/<issue-or-slug>` (e.g. `fix/1037-heal`, `feat/p4-usage-sink`)
- **Max 1 open branch per agent** on GitHub. No parallel feature branches.
- **Before push**: `git ls-remote --heads github | wc -l` — if >3, clean up first.
- **No branches without PRs** on GitHub. Either open a PR or keep it local.

## Compression Safety

lean-ctx shadow-mode compresses file reads. Edit tools (StrReplace, Write) can
write compression omission markers back to disk as literal text. The pre-commit
hook blocks such commits.

**If a commit is blocked or a file looks corrupted:**
```bash
LEAN_CTX_DISABLED=1 git restore --source=HEAD -- <file>   # restore raw content
LEAN_CTX_DISABLED=1 sed -i '' 's/old/new/g' <file>        # edit without hooks
```

**Never** use `git show commit:file > file` without `LEAN_CTX_DISABLED=1`.

`LEAN_CTX_DISABLED=1` bypasses all lean-ctx compression for a single command.

## Quality Bar

- Zero clippy warnings, all tests pass
- Security: PathJail, Shell Allowlist, bounded_lock, no hardcoded secrets
- No mock data, no placeholders, no stubs
- No feature creep: a new tool, module, default or background job needs its
  Select/Control/Prove role, demonstrated demand and a deciding measurement;
  learned or adaptive methods need an A/B result against a simple heuristic.
  Models follow the Embedded Intelligence Principle. Details:
  `skills/leanctx-gateway-delivery/SKILL.md` → "Add capability only with evidence".

## Test Policy (single source of truth)

~12 900 tests already exist; every new one costs CI minutes on three OSes
forever. A test earns its place only if it **catches a plausible future bug
that nothing else catches** (type system, clippy, existing tests, generators
with `--check`). Skills and rules reference this section instead of
prescribing their own test menus.

- **Bugfix:** exactly one regression test, at the lowest layer that
  reproduces the bug. It must fail without the fix.
- **Feature / contract change:** test the public behaviour and the contract
  (inputs → outputs, error paths, security boundaries), not internals.
- **Extend before adding:** put new cases into the nearest existing test
  module or table-driven test; do not create a new file or a new
  `rust/tests/*.rs` binary (each one links the full lib) unless it must mutate
  process-global state.
- **Do not write:** serde round-trips of derived types, default-value checks,
  trivial getters/constructors, tests that restate the implementation,
  hardcoded counts (tool counts, list lengths) that only change-detect.
- **Source-text tests** (`include_str!("….rs")` + string matching): no new
  ones unless no behavioural test is feasible (e.g. a security invariant
  across files). Existing ones are migrated to behaviour tests when their
  code is touched.
- **No test is also a valid answer** for pure refactors covered by existing
  tests, docs, config or wording changes. Say so in the PR instead of adding
  a token test.
- **Never** weaken, skip or delete a failing test to make a change pass.
  Deleting a test is fine when it is redundant or only change-detects —
  state why in the commit.
- **Reviewers** judge test value, not only coverage: "remove this test" is a
  legitimate review result.

## Quality Gate

Before every commit, all checks must pass (run from `rust/`):

```bash
cargo test --lib > /tmp/lib-tests.log 2>&1; echo "exit=$?"   # judge by exit code, grep the log for FAILED
cargo clippy --all-features -- -D warnings
cargo clippy --tests -- -D warnings -A clippy::too_many_lines -A clippy::unwrap_used   # --lib never compiles tests/*.rs
cargo fmt --check
```

Never judge a run by `| tail`: a single red test scrolls past unnoticed.
Before pushing, `PREFLIGHT_BASE=github/main bash scripts/preflight.sh` runs the
change-aware fast gate (the pre-push hook runs it too).

## Output Determinism (#498)

Tool outputs MUST be deterministic functions of (file content, mode, CRP mode, task).
Provider-side prompt caching (a cache hit bills at 0.1× input on Anthropic and OpenAI GPT-5.6+,
a miss rewrites the prefix at 1.25× or more) rewards byte-stable text;
any timestamp, counter or random element in tool output bodies defeats it.

- No timestamps/counters in output bodies. Artifact paths are content-addressed
  (see `save_tee`: `{cmd_slug}_{blake3(cmd)[..8]}.log`).
- Dynamic additions (hints, checkpoints) only as state-triggered suffixes with stable headers.
- Regression guard: determinism tests in `ctx_read/tests.rs`, `ctx_search.rs`, `shell/redact.rs`.

## LeanCTX Context Gateway product-scope, source-security and IP guard — REQUIRED FOR ALL AGENTS

Before **any** LeanCTX code, docs, connectors, SDK, EE, CI, packaging, refactor or release work, read `.agents/skills/leanctx-ip-governance/SKILL.md` and its **PRODUCT_SCOPE**, **CONNECTOR_AND_DECISION_SECURITY**, IP/licensing and release references. The product is the **Context Gateway for AI Systems** (Select → Control → Prove), not a general model router, agent orchestration framework or Thinkery OS; team coordination context (presence, leases, workspace bus, shared work graph) is in scope, agent execution and scheduling are not. New features must serve authorized source→ContextDecision→ContextBundle/Receipt, with current ACL/freshness/revocation checks; out-of-scope proposals require founder approval. Classify every affected file by product scope AND source rights. Preserve existing Apache functionality unless a separately approved reversible deprecation plan exists. New proprietary EE remains private; no public push, bulk deletion, history rewriting, relicense, CLA change or IP transfer claim without documented owner/legal/release authority. Respect stronger repository rules, preserve other agents' worktrees, record evidence and test security/compatibility. No public release credentials in agent workspaces.
