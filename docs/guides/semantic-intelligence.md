# Semantic code intelligence

lean-ctx builds its code graph from tree-sitter: fast, local, no setup. When a
language server, your editor (VS Code, Cursor, Windsurf) or a JetBrains IDE is
available, lean-ctx additionally asks it where code relationships *actually*
go and records how sure each graph edge is. This guide covers how to turn that
on, what you get, and how to read it. The design is in
[ADR-015](../adrs/ADR-015-semantic-code-intelligence.md).

## What you get

- **Correct call edges for ambiguous names.** Five `save()` methods in a
  project are told apart by the receiver's type, not guessed by name.
- **Vetoed false edges.** A call to `json.loads` is not linked to your own
  `loads()` just because the name matches.
- **`implements` and `extends` edges** from implementations to the
  trait/interface they implement and from subclasses to their base classes,
  so changing a trait or a base class shows what depends on it.
- **Verified users on demand.** `ctx_pack action=pr` asks the backend who
  really references the symbols of each changed file, and records the answer
  as `references` edges.
- **Evidence on every call edge**, used by ranking (`ctx_compose`, related
  files), impact analysis and `ctx_callgraph`:

  | Grade | Meaning |
  |---|---|
  | `verified` | a language server, editor or IDE resolved it |
  | `resolved` | bound by the caller's own scope (same file or a unique import) |
  | `heuristic` | name is unique in the project, but not in the caller's scope |

Without any semantic backend lean-ctx works exactly as before — minus edges it
used to guess.

## Where answers come from

In order of preference, for each language:

1. a **language server lean-ctx already runs** for the project (started by
   `eager` mode),
2. a **JetBrains IDE** with the lean-ctx plugin,
3. your **editor**, through the lean-ctx extension's semantic bridge — it
   answers with whatever language extensions you have installed (TypeScript is
   built into VS Code and Cursor; Rust, Python, Go, Java, C# … come with their
   extensions),
4. in `eager` mode only: a **language server lean-ctx starts**.

### Editor bridge (VS Code, Cursor, Windsurf)

Install the lean-ctx extension (`packages/vscode-lean-ctx`; it runs in VS Code
and its forks Cursor and Windsurf) and keep a window open on the project. The
extension
serves the editor's go-to-definition, references, implementations and type
hierarchy on `127.0.0.1` behind a random per-window token and announces itself
in `lean-ctx editor-bridge dir`. Nothing leaves the machine, it is read-only,
and lean-ctx never starts or edits anything through it. Turn it off with the
setting `leanctx.semanticBridge.enabled = false`; the extension's **lean-ctx**
output channel shows which folders it serves.

## Turning it on

`semantic_mode` in `~/.config/lean-ctx/config.toml` (or `LEAN_CTX_SEMANTIC_MODE`):

| Mode | Behaviour |
|---|---|
| `auto` *(default)* | Uses what is already running: language servers lean-ctx started, a JetBrains IDE, your editor's bridge. Never starts a server. |
| `eager` | May also start the project's language servers. **Trusted workspaces only** (`lean-ctx trust`) — a language server runs project code (build scripts, proc macros); untrusted projects run as `auto`. |
| `off` | Structural graph only; no semantic backend is ever queried. |

A project's `.lean-ctx.toml` can lower the mode for that project, but only a
trusted workspace can raise it to `eager`.

## Installing language servers

Needed only for `eager` mode — with the editor bridge, your
editor's language extensions answer instead. lean-ctx never installs
anything. Supported standalone servers:

| Language | Server | Install |
|---|---|---|
| Rust | `rust-analyzer` | `rustup component add rust-analyzer` |
| TypeScript / JavaScript | TypeScript ≥ 7: `tsc --lsp`; ≤ 6: `typescript-language-server` | `npm install -g typescript` (≤ 6: `npm install -g typescript-language-server typescript@6`) |
| Python | `pylsp` | `pip install python-lsp-server` |
| Go | `gopls` | `go install golang.org/x/tools/gopls@latest` |

For TypeScript the project's own `node_modules/typescript` decides which server
runs; without one, the machine-wide install does (for ≤ 6, lean-ctx points
`typescript-language-server` at the `typescript` installed next to it).

`lean-ctx doctor` lists which servers can actually run — a `rust-analyzer`
rustup proxy without the installed component is reported as missing.

## Reading the results

- `ctx_graph action=status` — mode, evidence counts, and per language the
  verified share, whether its server can run, and what the last server started
  for it negotiated (also in `lean-ctx doctor` and the dashboard legend):

  ```
  Semantic: mode=auto | calls 96 verified · 310 resolved · 24 heuristic | implements 12 · extends 3 · references 40
    rust        96/120 verified (80%) · rust-analyzer ✓ · offers definition, references, implementations; no type hierarchy
    typescript  0/310 verified (0%) · typescript-language-server not installed (npm install -g typescript …)
  ```

- `lean-ctx repomap` ranks files over the same call edges as the graph — verified
  where a backend answered, never linked by a bare name match.

- `ctx_graph action=enrich` — runs a pass now and reports what it resolved.
- `ctx_callgraph action=callees symbol=checkout` — each call shows its target:
  `→ save  (src/app.rs:L2)  ⇒ src/b.rs [verified]`.
- `ctx_graph action=impact path=…` — the files that depend on a file, over
  the same graph.
- Dashboard → Graph → per-language legend, column **Semantic**.

## When does a pass run?

- After every graph build (zero cost in `auto` unless a backend is live).
- While a language server or IDE is in use by a lean-ctx process: at most one
  pass per project every 5 minutes, started in a pause — no tool call for
  5 s — so it never slows down the agent. A pass that could not run (graph
  not ready yet, no pause within 2 minutes) becomes due again on a use at
  least 30 s later.
- Large projects: right after a language server starts it is still
  indexing, and answers are slow or missing for the first minutes (on a
  2.4k-file Rust crate: ~170 ms per lookup cold vs ~65 ms warm; workspace-wide
  `references` time out until indexing finishes). "No answer" is never
  evidence; later passes and the cache catch up.
- On demand: `ctx_graph action=enrich`, `ctx_callgraph` for the calls it lists,
  and `ctx_pack action=pr` for the changed files (`references`).

All passes are bounded — background calls: 1000 lookups / 60 s, 5 s per
request, the most decisive sites first; `implements`/`extends`: 100 lookups /
10 s each; `ctx_callgraph`: 20 lookups / 4 s; `references`: 40
lookups / 8 s per file — and never wait for a backend busy with an
interactive call. Answers are cached and reused until the code they depend on
changes, so coverage of a large repository grows pass by pass.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Everything `resolved`/`heuristic`, nothing `verified` | No backend in `auto`: open the project in your editor with the lean-ctx extension, open it in a JetBrains IDE, or use `eager` in a trusted workspace. |
| The editor is open but nothing is verified | Check the extension's **lean-ctx** output channel; the bridge needs `lean-ctx` on `PATH` (or `leanctx.binaryPath`) to find where to announce itself, and a language extension for the language. |
| `extends` stays at 0 | The backend has no type hierarchy for that language (e.g. rust-analyzer, pylsp); JetBrains IDEs, gopls and many editor language extensions have one. |
| A language shows "not installed" | Install its server (table above); `lean-ctx doctor` confirms. |
| First pass after opening a project verifies little | The server is still indexing; "no result" is never treated as evidence, the next pass picks it up. |
| `eager` has no effect | The workspace is not trusted: `lean-ctx trust`. |
