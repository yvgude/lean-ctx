# Monorepo guide

This guide shows how to run lean-ctx in a monorepo without letting one package's cache, index, or agent activity spill into another.

## When you need this

Use this setup if your repo has multiple apps or packages, for example:

- `apps/web` + `apps/api` in a pnpm or Turborepo workspace
- `crates/*` in a Cargo workspace
- `services/*` managed by Bazel or Buck2

The goal is simple:

- keep config local to the package an agent is working on
- avoid indexing build artifacts and vendor trees
- keep concurrent agents from stepping on each other
- preserve fast search and cache behavior in large trees

## 1. Put `.lean-ctx.toml` at the workspace you want to tune

lean-ctx auto-merges a project-local `.lean-ctx.toml` with your global config.
That means each package can keep its own ignore rules and performance settings.

### Example: pnpm / Turborepo

```text
repo/
├─ .git/
├─ apps/
│  ├─ web/
│  │  └─ .lean-ctx.toml
│  └─ api/
│     └─ .lean-ctx.toml
├─ packages/
│  ├─ ui/
│  └─ config/
└─ pnpm-workspace.yaml
```

`apps/web/.lean-ctx.toml`

```toml
extra_ignore_patterns = [
  "dist/**",
  ".next/**",
  "coverage/**",
  "playwright-report/**"
]

memory_profile = "balanced"
memory_cleanup = "shared"
graph_index_max_files = 12000
```

`apps/api/.lean-ctx.toml`

```toml
extra_ignore_patterns = [
  "dist/**",
  "tmp/**",
  "coverage/**",
  "generated/**"
]

memory_profile = "balanced"
memory_cleanup = "shared"
```

### Example: Cargo workspace

```text
repo/
├─ Cargo.toml
├─ crates/
│  ├─ gateway/
│  │  └─ .lean-ctx.toml
│  └─ worker/
│     └─ .lean-ctx.toml
└─ target/
```

```toml
extra_ignore_patterns = ["target/**", "coverage/**"]
graph_index_max_files = 15000
```

### Example: Bazel / Buck2

```toml
extra_ignore_patterns = [
  "bazel-bin/**",
  "bazel-out/**",
  "bazel-testlogs/**",
  "buck-out/**"
]

memory_profile = "low"
```

## 2. Scope BM25 and graph indexing aggressively

Large monorepos get slow when indexes include generated output, vendored code, or build caches.
`extra_ignore_patterns` is the main lever.

Good candidates to ignore:

- JS: `node_modules/**`, `dist/**`, `.next/**`, `coverage/**`
- Rust: `target/**`
- Python: `.venv/**`, `__pycache__/**`
- Bazel/Buck2: `bazel-*/**`, `buck-out/**`
- Generic generated output: `generated/**`, `vendor/**`, `.cache/**`

A solid starting point for a mixed monorepo:

```toml
extra_ignore_patterns = [
  "node_modules/**",
  "dist/**",
  ".next/**",
  "coverage/**",
  "target/**",
  "vendor/**",
  "generated/**",
  "bazel-bin/**",
  "bazel-out/**",
  "buck-out/**"
]
```

If `lean-ctx doctor` warns about large BM25 indexes, tighten these patterns before increasing limits.

## 3. Let different agents work on different packages

The easiest pattern is one agent per package root.

Examples:

- Cursor on `apps/web`
- Claude Code on `apps/api`
- Codex on `crates/gateway`

That keeps read caches, session history, and search results naturally scoped to the folder each agent opened.

When you do need cross-package awareness, prefer explicit scoping over opening the whole repo and hoping for the best:

- run tools from the package root the agent owns
- use tool `path` parameters to narrow searches to one service or package
- keep package-specific `.lean-ctx.toml` files small and boring

If you intentionally want one workspace to see sibling projects, add a `.leanctx.json` file with `linkedProjects`:

```json
{
  "linkedProjects": ["../packages/ui", "../packages/config"]
}
```

Use this sparingly. It is useful for shared libraries, but it also widens the search surface.

## 4. Use `.lean-ctx-id` when paths are reused

This matters most in Docker, devcontainers, Codespaces, and remote sandboxes where multiple repos all mount at the same path such as `/workspace`.

Create a `.lean-ctx-id` file at the project root:

```text
my-monorepo-web
```

or:

```text
my-monorepo-api
```

lean-ctx uses this as an explicit project identity, which prevents cache and knowledge collisions between unrelated projects that happen to share the same mount path.

Use a different `.lean-ctx-id` for each logical project root that may appear at the same filesystem path.

## 5. Performance defaults for 1000+ files

For large repos, start with:

```toml
extra_ignore_patterns = [
  "node_modules/**",
  "dist/**",
  ".next/**",
  "coverage/**",
  "target/**",
  "vendor/**",
  "generated/**"
]

graph_index_max_files = 12000
memory_profile = "balanced"
memory_cleanup = "shared"
```

Then adjust from symptoms:

- **Index too large:** add more ignore patterns first
- **Several IDEs/agents share the repo:** keep `memory_cleanup = "shared"`
- **Machine is RAM-constrained:** switch `memory_profile = "low"`
- **Graph coverage feels too shallow:** raise `graph_index_max_files`

## Recommended layouts

### Turborepo / pnpm workspaces

- keep one `.lean-ctx.toml` per app or service
- ignore `.next`, `dist`, coverage, generated client code
- only link shared packages if the agent truly needs them

### Cargo workspaces

- ignore `target/**`
- keep service-specific config in `crates/<name>/.lean-ctx.toml`
- open the crate you are editing when possible instead of the full repo root

### Bazel / Buck2 polyglot repos

- ignore `bazel-*` / `buck-out`
- prefer one agent per subsystem
- use explicit `path` scoping for search-heavy workflows

## A practical default

If you are not sure where to start, do this:

1. add `.lean-ctx.toml` in each active package
2. fill `extra_ignore_patterns` first
3. set `memory_cleanup = "shared"` if multiple tools touch the repo
4. add `.lean-ctx-id` for containerized `/workspace` setups
5. only add `linkedProjects` when cross-package search is genuinely useful

That gets most monorepos into the fast and predictable zone without much tuning.