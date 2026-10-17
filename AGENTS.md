# LeanCTX — Context Gateway for AI Systems

LeanCTX Engine selects context, applies supported access and content controls, and records observable context operations; compression is one capability within that path.

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

This data is delivered through the first tool call's `--- AUTO CONTEXT ---`
briefing (default `minimal_overhead = true`: initialize instructions stay
byte-stable for provider prompt caching, #498). With `minimal_overhead = false`
it is additionally injected at session start via the `ACTIVE SESSION` LITM block.

### Active Documentation (Agent Responsibility)

After completing a significant task (implementation, bugfix, refactoring):
1. Record the decision: `ctx_knowledge(action="remember", category="decision", content="...")`
2. Record progress: `ctx_session(action="task", value="<current task> [N%]")`
3. Record blockers: `ctx_knowledge(action="remember", category="blocker", content="...")`

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
and `ctx_read` shows cross-source hints (e.g. "Issue #42 references this file").

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
Provider-side prompt caching (Anthropic 90%, OpenAI 50% discount) rewards byte-stable text;
any timestamp, counter or random element in tool output bodies defeats it.

- No timestamps/counters in output bodies. Artifact paths are content-addressed
  (see `save_tee`: `{cmd_slug}_{blake3(cmd)[..8]}.log`).
- Dynamic additions (hints, checkpoints) only as state-triggered suffixes with stable headers.
- Regression guard: determinism tests in `ctx_read/tests.rs`, `ctx_search.rs`, `shell/redact.rs`.

<!-- lean-ctx -->
## lean-ctx

lean-ctx is active — the MCP tools replace native equivalents.
Full rules: LEAN-CTX.md (open on demand — do not auto-load).
<!-- /lean-ctx -->
<!-- lean-ctx-compression -->
OUTPUT STYLE: concise
- Bullet points over paragraphs
- Skip filler words and hedging ("I think", "probably", "it seems")
- 1-sentence explanations max, then code/action
- No repeating what the user said
<!-- /lean-ctx-compression -->
