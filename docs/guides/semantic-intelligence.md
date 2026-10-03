# Semantic code intelligence

lean-ctx builds its code graph from tree-sitter: fast, local, no setup. When a
language server or a JetBrains IDE is available, lean-ctx additionally asks it
where calls *actually* go and records how sure each graph edge is. This guide
covers how to turn that on, what you get, and how to read it. The design is in
[ADR-015](../adrs/ADR-015-semantic-code-intelligence.md).

## What you get

- **Correct call edges for ambiguous names.** Five `save()` methods in a
  project are told apart by the receiver's type, not guessed by name.
- **Vetoed false edges.** A call to `json.loads` is not linked to your own
  `loads()` just because the name matches.
- **`implements` edges** from implementations to the trait/interface they
  implement, so changing a trait shows its implementors as impacted.
- **Evidence on every call edge**, used by ranking (`ctx_compose`, related
  files), impact analysis and `ctx_callgraph`:

  | Grade | Meaning |
  |---|---|
  | `verified` | a language server / IDE resolved it |
  | `resolved` | bound by the caller's own scope (same file or a unique import) |
  | `heuristic` | name is unique in the project, but not in the caller's scope |

Without any language server lean-ctx works exactly as before — minus edges it
used to guess.

## Turning it on

`semantic_mode` in `~/.config/lean-ctx/config.toml` (or `LEAN_CTX_SEMANTIC_MODE`):

| Mode | Behaviour |
|---|---|
| `auto` *(default)* | Uses language servers that are already running (e.g. started by `ctx_refactor`) or a JetBrains IDE with the lean-ctx plugin. Never starts a server in the background. |
| `eager` | May start the project's language servers in the background. **Trusted workspaces only** (`lean-ctx trust`) — a language server runs project code (build scripts, proc macros); untrusted projects run as `auto`. |
| `off` | Structural graph only; no semantic backend is ever queried. |

A project's `.lean-ctx.toml` can lower the mode for that project, but only a
trusted workspace can raise it to `eager`.

## Installing language servers

lean-ctx never installs anything. Supported standalone servers:

| Language | Server | Install |
|---|---|---|
| Rust | `rust-analyzer` | `rustup component add rust-analyzer` |
| TypeScript / JavaScript | TypeScript ≥ 7: `tsc --lsp`; ≤ 6: `typescript-language-server` | `npm install -g typescript` (≤ 6: `npm install -g typescript-language-server typescript@6`) |
| Python | `pylsp` | `pip install python-lsp-server` |
| Go | `gopls` | `go install golang.org/x/tools/gopls@latest` |

For TypeScript the project's own `node_modules/typescript` decides which server
runs; without one, the machine-wide install does (for ≤ 6, lean-ctx points
`typescript-language-server` at the `typescript` installed next to it).

Java, Kotlin, C#, C/C++ and Ruby are served by a JetBrains IDE with the lean-ctx
plugin. `lean-ctx doctor` lists which servers can actually run — a
`rust-analyzer` rustup proxy without the installed component is reported as
missing.

## Reading the results

- `ctx_graph action=status` — mode, evidence counts, and per language the
  verified share plus whether its server can run:

  ```
  Semantic: mode=auto | calls 96 verified · 310 resolved · 24 heuristic | implements 12
    rust        96/120 verified (80%) · rust-analyzer ✓
    typescript  0/310 verified (0%) · typescript-language-server not installed (npm install -g typescript …)
  ```

- `ctx_graph action=enrich` — runs a pass now and reports what it resolved.
- `ctx_callgraph action=callees symbol=checkout` — each call shows its target:
  `→ save  (src/app.rs:L2)  ⇒ src/b.rs [verified]`.
- `ctx_impact` — files reachable *only* through name-match guesses are marked
  `(name match only)`; the JSON output lists them under `weak_files`.
- Dashboard → Graph → per-language legend, column **Semantic**.

## When does a pass run?

- After every graph build (zero cost in `auto` unless a backend is live).
- While a language server or IDE is in use by a lean-ctx process: at most one
  pass per project every 5 minutes, after the call that uses it. A pass that
  could not run (graph not ready yet) becomes due again on a use at least
  30 s later.
- On demand: `ctx_graph action=enrich`, and `ctx_callgraph` for the calls it lists.

All passes are bounded (background: 200 lookups / 20 s, 5 s per request;
`ctx_callgraph`: 20 lookups / 4 s) and never wait for a server busy with an
interactive `ctx_refactor` call. Answers are cached per call site and reused
until the code they depend on changes.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Everything `resolved`/`heuristic`, nothing `verified` | No live backend in `auto`: start one (`ctx_refactor` on a file of that language), open the project in a JetBrains IDE, or use `eager` in a trusted workspace. |
| A language shows "not installed" | Install its server (table above); `lean-ctx doctor` confirms. |
| First pass after opening a project verifies little | The server is still indexing; "no result" is never treated as evidence, the next pass picks it up. |
| `eager` has no effect | The workspace is not trusted: `lean-ctx trust`. |
