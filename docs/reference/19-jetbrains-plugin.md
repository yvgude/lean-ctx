# Journey 19 — JetBrains Plugin

Authoritative sources:

- Plugin: `packages/jetbrains-lean-ctx/src/main/kotlin/com/leanctx/plugin/{server,endpoint,psi,dto}/…`
- Rust backend: `rust/src/lsp/{backend,jetbrains_backend,router,edit_apply,port_discovery}.rs`
- MCP tool schema: `rust/src/tools/registered/ctx_refactor.rs`

---

## 0. Serena as Inspiration

The lean-ctx JetBrains plugin is conceptually inspired by **Serena** (Oraios'
IntelliJ-Platform MCP tool). Serena was the model because it was the only tool to
deliver the semantic core — `references`, `implementations`, `type_hierarchy` **and**
symbolic edits — directly from the IDE; the official JetBrains MCP
(`mcp__jetbrains__*`) never closed this gap.

**Clear delineation:** The plugin is an **independent reimplementation at the
architecture and class-name level — not a derivation, not decompiled
Serena code**. It is published under the lean-ctx project license and ships in the
repository (`packages/jetbrains-lean-ctx`). Goal: make Serena (and the official
JetBrains MCP) **dispensable** as a code-intelligence dependency, so that
lean-ctx becomes the sole interface for symbols, navigation, and refactoring.

### 0.1 Delineation Serena ↔ lean-ctx Plugin

| Aspect         | Serena                     | lean-ctx JetBrains plugin                                                |
|----------------|----------------------------|--------------------------------------------------------------------------|
| Hosting        | external Oraios component  | in the lean-ctx repo (`packages/jetbrains-lean-ctx`)                     |
| Interface      | several separate MCP tools | bundled under `ctx_refactor` (token compression)                         |
| Backend model  | running IDE only           | Backing B (IDE) **+** Backing A (rust-analyzer) **+** Headless           |
| Headless / CI  | no                         | yes — tree-sitter fallback for `symbols_overview` + edits                |
| Conflict guard | none                       | BLAKE3 `expected_hash` (edits) / `plan_hash` (refactoring), Rust-central |
| Security       | —                          | PathJail (project-root validation) + token auth per project              |
| License        | proprietary (Oraios)       | lean-ctx project license                                                 |

### 0.2 Mapping: Serena concept → `ctx_refactor` action → HTTP endpoint

| Serena concept             | `ctx_refactor` action            | HTTP endpoint                                       |
|----------------------------|----------------------------------|-----------------------------------------------------|
| `find_referencing_symbols` | `references`                     | `POST /references`                                  |
| `find_declaration`         | `declaration`                    | `POST /declaration`                                 |
| (goto definition)          | `definition`                     | `POST /definition`                                  |
| `find_implementations`     | `implementations`                | `POST /implementations`                             |
| `get_symbols_overview`     | `symbols_overview`               | `POST /symbols_overview`                            |
| `type_hierarchy`           | `type_hierarchy`                 | `POST /type_hierarchy`                              |
| `run_inspections` / list   | `inspections` (`mode=run\|list`) | `POST /inspections`, `POST /list_inspections`       |
| `replace_symbol_body`      | `replace_symbol_body`            | `POST /replaceSymbolBody`                           |
| `insert_before_symbol`     | `insert_before_symbol`           | `POST /insertBeforeSymbol`                          |
| `insert_after_symbol`      | `insert_after_symbol`            | `POST /insertAfterSymbol`                           |
| `rename`                   | `rename`                         | `POST /renamePreview` → `POST /renameApply`         |
| (reformat_file)            | `reformat`                       | `POST /reformat`                                    |
| `move`                     | `move`                           | `POST /movePreview` → `POST /moveApply`             |
| `safe_delete`              | `safe_delete`                    | `POST /safeDeletePreview` → `POST /safeDeleteApply` |
| `inline`                   | `inline`                         | `POST /inlinePreview` → `POST /inlineApply`         |

> `find_symbol` (pure symbol search) is not part of `ctx_refactor` but of
> `ctx_search action="symbol"` / `ctx_outline` (lean-ctx symbol index). See
> [MCP tool map](appendix-mcp-tools.md).

---

## 1. Architecture (Plugin ↔ Rust ↔ MCP tool)

```text
   Agent
     │  ctx_refactor action=… (MCP)
     ▼
  │ Rust: ctx_refactor  →  select_backend        │
        │ IDE reachable?         │ no
        ▼ yes                    ▼
  Backing B                 Headless / Backing A
  JetBrainsHttpBackend      • local_range_write (edits, atomic)
  HTTP → Plugin             • overview_from_index (tree-sitter)
        │                   • rust-analyzer (navigation)
        ▼
  │ JetBrains IDE plugin (Kotlin HTTP server)    │
  │ 127.0.0.1 · token-guarded · PSI/read-action  │
```

### 1.1 Backing choice & degradation (`backend.rs`)

`select_backend` (`rust/src/lsp/router.rs`) decides per call which path applies.
The `LspBackend` trait tiers the methods:

| Class                                       | Methods                                                                                                                      | Default without IDE                       |
|---------------------------------------------|------------------------------------------------------------------------------------------------------------------------------|-------------------------------------------|
| **Mandatory** (both backings)               | `open_file`, `references`, `definition`, `implementations`                                                                   | served by Backing A                       |
| **Default-degrading** (Backing B preferred) | `declaration`, `type_hierarchy`, `inspections`, `list_inspections`                                                           | `Err` — "requires the JetBrains backend"  |
| **Headless-default** (lossless)             | `symbols_overview` (tree-sitter), `replace_symbol_body`, `insert_before_symbol`, `insert_after_symbol` (`local_range_write`) | works without IDE                         |
| **`BACKEND_REQUIRED`**                      | refactoring engine (`rename`, `move`, `safe_delete`, `inline`)                                                               | `Err` — no headless usage search possible |

### 1.2 Port discovery & staleness

On project start the plugin writes a **port file** (atomic, idempotent) with the
JSON keys `port`, `token`, `pid`, `project_root`, `ide_version`, `started_at`
(snake_case on the wire; `PortFileWriter.kt`, `BackendHttpServer.kt` →
`LeanCtxPaths.portFile(dataDir, projectRoot)`). On `projectClosing` (Disposable)
it is deleted. The Rust reader (`PortFile`, `port_discovery.rs`) consumes
`port`/`token`/`pid`/`project_root`/`ide_version`.

Rust checks reachability in **three stages** (`rust/src/lsp/port_discovery.rs`):

1. Port file exists & is readable → `port`/`token`/`pid`,
2. process with `pid` is alive,
3. `GET /health` responds within the timeout.

Only when all three pass is Backing B considered reachable; otherwise Headless
or `BACKEND_REQUIRED` applies.

### 1.3 Worktrees & project windows

The HTTP server is a **project-level service** (`BackendHttpServer` as a
`Disposable`, booted by `LeanCtxStartupActivity` per `Project`, bound to
`127.0.0.1:0` = ephemeral port). The port file is keyed
**per project** via `projecthash = sha256(canonical(projectRoot))[..16]`. From this
follows for `git worktree`:

- **One dedicated port file per worktree** — but only if the worktree is opened as its
  own **project window**. Multiple terminals **within one** project window
  share **one** port file (terminals do not start a plugin).
- **One open project window serves exactly one worktree path.** A lean-ctx session
  running in a **different** worktree computes a diverging `projecthash`,
  finds **no** port file → clean **fallback to Backing A** (rust-analyzer);
  with `lsp.<lang>="jetbrains"` instead `BACKEND_REQUIRED`. **No** path collision.
- **Backing B for N worktrees in parallel:** one project window per worktree.
  A **single** IDE instance suffices — *File → Open → in new window* instantiates the
  project service again (own server, own port, own port file). **No**
  second IDE installation/process needed.
- **JetBrains VCS ↔ PSI orthogonal:** The Git tool-window confusion with worktrees
  (`.git` file → `gitdir:` indirection) concerns the **VCS layer**, not
  indexing. The Backing-B endpoints need an **indexed Cargo project**, not a
  recognized VCS root → **PSI works** even when the Git panel is acting up.
- **Per terminal** the lean-ctx session must be `cd`'d into the **matching** worktree;
  the `projecthash` match then runs automatically.

> Cost trade-off: N project windows = N× indexing/RAM (shared JVM, separate
> indexes). Worth it only with a genuine need for PSI symbolics in multiple worktrees
> **simultaneously** — otherwise leave the secondary worktree in the terminal and
> accept the rust-analyzer fallback (Backing A). The same **branch** cannot be checked
> out in two worktrees at once (git constraint).

---

## 2. Function Reference

Conventions for all endpoints:

- HTTP: `POST` to `127.0.0.1:<port>`, header `X-LeanCtx-Token: <token>`,
  body = JSON. `GET /health` is the only exception (no body).
- **Coordinates:** At the `ctx_refactor` level, `line` is **1-indexed**, `column`
  is **0-indexed**. At the **wire level** (HTTP DTO), `line`/`character` of the
  navigation/edit endpoints are **0-based** (LSP convention); the `line` fields in
  `type_hierarchy`, `symbols_overview`, and `inspections` responses are **1-based**.
- Domain negative cases arrive as an envelope `{"error":{"code","message"}}` with
  HTTP 200 (see §9).

### 2.1 Navigation (read-only)

**Actions:** `references`, `definition`, `implementations`, `declaration`
**Endpoints:** `POST /references` · `/definition` · `/implementations` · `/declaration`

**What it does:** Finds semantic occurrences of a symbol (usages,
declaration, implementations). `declaration` is only available via Backing B.

**Agent invocation:**

```text
ctx_refactor action=references path=src/Main.kt line=42 column=8 scope=project
```

**HTTP (curl):**

```bash
curl -s -X POST http://127.0.0.1:$PORT/references \
  -H "X-LeanCtx-Token: $TOKEN" -H "Content-Type: application/json" \
  -d '{"path":"src/Main.kt","line":41,"character":8,"scope":"project"}'
```

**Response (`LocationsResponse`):**

```text
{"locations":[{"path":"src/Main.kt","range":{"start":{"line":41,"character":8},
 "end":{"line":41,"character":14}}}],"truncated":false,"total":1}
```

**Parameters:** `path`, `line`/`character` (0-based, wire), `scope ∈ {project, all}`
(default `project`; `all` includes libraries/SDK).
**Backing:** Backing B preferred; Backing A (rust-analyzer) as fallback for
`references`/`definition`/`implementations`. `declaration` is Backing-B-only.

### 2.2 Structure

**Actions:** `type_hierarchy`, `symbols_overview`
**Endpoints:** `POST /type_hierarchy` · `POST /symbols_overview`

**What it does:** `type_hierarchy` returns the super-/subtype tree; `symbols_overview`
lists the top-level symbols of a file.

**Agent invocation:**

```text
ctx_refactor action=type_hierarchy path=src/Main.kt line=10 column=6 direction=subtypes
ctx_refactor action=symbols_overview path=src/Main.kt
```

**HTTP (curl):**

```bash
curl -s -X POST http://127.0.0.1:$PORT/symbols_overview \
  -H "X-LeanCtx-Token: $TOKEN" -d '{"path":"src/Main.kt"}'
```

**Response (`SymbolsOverviewResponse`, `line` 1-based):**

```text
{"symbols":[{"name":"Main","kind":"class","line":3},
            {"name":"run","kind":"method","line":7}],"truncated":false,"total":2}
```

**Parameters:** `type_hierarchy`: `path`, `line`/`character`, `direction ∈
{supertypes, subtypes}` (default `supertypes`), `scope`. `symbols_overview`: `path`.
**Backing:** `type_hierarchy` is Backing-B-only. `symbols_overview` has a
**lossless headless default** via the tree-sitter symbol index
(`overview_from_index`, the same source as `ctx_search action="symbol"` / `ctx_outline`).

**IDE-neutral loading & degradation.** The Core `plugin.xml` depends only on
`com.intellij.modules.platform`, so it loads in every IntelliJ IDE (RustRover, PyCharm,
GoLand, WebStorm, IDEA, …). The two JVM-PSI-bound structure ops live in an optional
module (`leanctx-jvm.xml`, loaded only when the Kotlin plugin is present) and are wired
through the `com.leanctx.plugin.structureProvider` extension point. In non-JVM IDEs the
EP is empty and the Core degrades cleanly:

| Feature              | RustRover (Rust)                          | PyCharm (Python)      | IDEA / Android Studio (JVM)        |
|----------------------|-------------------------------------------|-----------------------|------------------------------------|
| Navigation           | ✅ Plugin-PSI (Rust) / Backing-A fallback | ✅ Plugin-PSI         | ✅ Plugin-PSI                      |
| `symbols_overview`   | ✅ lean-ctx tree-sitter (`ctx_outline`)   | ✅ tree-sitter        | ✅ IDE-PSI (Kotlin) + tree-sitter  |
| `type_hierarchy`     | → `implementations` / `ctx_callgraph`     | → `implementations`   | ✅ IDE-PSI (Java + Kotlin)         |
| Edits / Refactor / `reformat` / `inspections` | ✅ Plugin (platform)     | ✅                    | ✅                                 |
| UI (Gain, Status-bar, Doctor, Editor-signal)  | ✅                       | ✅                    | ✅                                 |

For Rust/Python the IDE-PSI variant of `type_hierarchy` and Kotlin `symbols_overview`
is not registered; the Rust backend serves the equivalent via `ctx_outline`
(tree-sitter), `implementations` (rust-analyzer / Backing A) and `ctx_callgraph`.

> **Live-verified (2026-06-13, RustRover-2026.1 / IU-2026.1.3 sandbox).** All 12 cross-IDE
> gate checks passed: the Core loads with no `java-capable` error (`leanctx-jvm.xml` skipped
> via the K2 gate), every Rust feature in the matrix works, and `type_hierarchy` degrades
> with the exact `UNSUPPORTED_LANGUAGE: type_hierarchy requires a JVM-capable IDE` envelope.
> Runbook + result table: `docs/lean-md/runbooks/runrustrover-cross-ide-gate.md`.

### 2.3 Quality — Inspections

**Action:** `inspections` (`mode=run|list`)
**Endpoints:** `POST /inspections` · `POST /list_inspections`

**What it does:** `mode=run` runs the active inspections on a file and
returns diagnostics; `mode=list` lists the inspections enabled in the project profile.

**Agent invocation:**

```text
ctx_refactor action=inspections path=src/Main.kt mode=run
ctx_refactor action=inspections path=src/Main.kt mode=list
```

**Response `run` (`InspectionsResponse`, `line` 1-based):**

```text
{"diagnostics":[{"path":"src/Main.kt","line":12,"severity":"WARNING",
 "message":"Unused symbol"}],"truncated":false,"total":1}
```

**Response `list` (`ListInspectionsResponse`):**

```text
{"inspections":[{"id":"UnusedSymbol","name":"Unused declaration",
 "severity":"WARNING"}],"truncated":false,"total":1}
```

**Backing:** Backing-B-only (no headless equivalent).

### 2.4 Symbol-body edits (write)

**Actions:** `replace_symbol_body`, `insert_before_symbol`, `insert_after_symbol`
**Endpoints:** `POST /replaceSymbolBody` · `/insertBeforeSymbol` · `/insertAfterSymbol`

**What it does:** Replaces the complete declaration of a named symbol or
inserts a sibling element before/after it. The target is addressed via `name_path`
(`'Class/method'` qualified or bare `'name'`), resolved through the
symbol index. Alternatively as a fallback via `path`+`line`(+`end_line`).

**Agent invocation:**

```text
ctx_refactor action=replace_symbol_body name_path=Main/run \
  new_body="fun run() { println(\"new\") }" expected_hash=<blake3-hex>

ctx_refactor action=insert_after_symbol name_path=Main/run \
  text="fun helper() = 42"
```

**HTTP (curl) — wire body carries `path`/`range`/`text` (no hash, see §7.1):**

```bash
curl -s -X POST http://127.0.0.1:$PORT/replaceSymbolBody \
  -H "X-LeanCtx-Token: $TOKEN" -d '{
    "path":"src/Main.kt",
    "range":{"start":{"line":6,"character":0},"end":{"line":8,"character":1}},
    "text":"fun run() { println(\"new\") }"
  }'
```

**Response (`EditResponse`):**

```text
{"applied":true,
 "newRange":{"start":{"line":6,"character":0},"end":{"line":6,"character":28}},
 "editedText":"fun run() { println(\"new\") }"}
```

**Parameters (action):** `name_path` **or** `path`+`line`(+`end_line`);
`new_body` (replace) or `text` (insert); optional `expected_hash`.
**Behavior:** Backing B executes the edit as a `WriteCommandAction` (a
single undo entry, document save). Headless writes atomically via `local_range_write`
(temp file + `rename`). **Both paths apply the same tree-sitter range
→ byte-identical result.** No automatic reformatting.

---

## 3. Refactoring Engine

All refactorings (except `reformat`) run through the **shared two-phase engine**:
`*Preview` collects usages + conflicts and forms the `plan_hash`; `*Apply`
performs the multi-file change as **one** transaction (one undo entry).
Because the semantic usage search needs the finished IDE index, there is **no**
lossless headless path — without a running IDE you get `BACKEND_REQUIRED`.

### 3.1 Rename (two-phase)

**Action:** `rename` (`new_name`)
**Endpoints:** `POST /renamePreview` → `POST /renameApply`

**What it does:** Renames a symbol project-wide — declaration **and all usages**.
Phase 1 (`/renamePreview`) collects `usages` and `conflicts` and forms the
`plan_hash` from them; Phase 2 (`/renameApply`) performs the rename as **one**
multi-file transaction.

**Agent invocation:**

```text
ctx_refactor action=rename path=src/Main.kt line=7 column=4 new_name=execute
```

**HTTP (curl) — Phase 1:**

```bash
curl -s -X POST http://127.0.0.1:$PORT/renamePreview \
  -H "X-LeanCtx-Token: $TOKEN" -d '{
    "path":"src/Main.kt",
    "range":{"start":{"line":6,"character":4},"end":{"line":6,"character":7}},
    "new_name":"execute","search_comments":false,"search_text_occurrences":false
  }'
# → {"usages":[{"path":"src/Main.kt","range":{…},"context":"run()"}],"conflicts":[]}
```

**HTTP (curl) — Phase 2:**

```bash
curl -s -X POST http://127.0.0.1:$PORT/renameApply \
  -H "X-LeanCtx-Token: $TOKEN" -d '{
    "path":"src/Main.kt","range":{…},"new_name":"execute","force":false
  }'
# → {"applied":true,"changed_paths":["src/Main.kt","src/Caller.kt"]}
```

**Parameters:** `new_name` (required); optional `search_comments`,
`search_text_occurrences` (preview); `force` (apply — skips the conflict gate).
**Behavior:** `BACKEND_REQUIRED` without a running IDE. If conflicts exist and
`force=false`, the gate blocks with `CONFLICT`. Between preview and apply the
`plan_hash` (BLAKE3, Rust-central) protects against TOCTOU drift.

### 3.2 Reformat

**Action:** `reformat`
**Endpoint:** `POST /reformat`

**What it does:** Formats a file in place according to the IDE's active code-style
profile (`CodeStyleManager` — equivalent to `mcp__jetbrains__reformat_file`).
Single-phase (no preview): formatting is idempotent and scoped to one file.

**Agent invocation:**

```text
ctx_refactor action=reformat path=src/Main.kt
```

**HTTP (curl):**

```bash
curl -s -X POST http://127.0.0.1:$PORT/reformat \
  -H "X-LeanCtx-Token: $TOKEN" -d '{"path":"src/Main.kt"}'
# → {"reformatted":true,"path":"src/Main.kt"}
```

**Behavior:** Backing-B-only (`WriteCommandAction` → `CodeStyleManager.reformat` →
`saveDocument`). Deliberately **decoupled** from the edit ops: symbol-body edits
do not reformat automatically; `reformat` is applied afterward when needed.

### 3.3 Move

**Action:** `move`
**Endpoints:** `POST /movePreview` → `POST /moveApply`

**What it does:** Moves a symbol (class/file/member) into another
package/target and adjusts all references + imports. Same two-phase mechanic as
`rename`: preview reports affected files + conflicts (`plan_hash`), apply performs
the multi-file transaction. `BACKEND_REQUIRED` without IDE.

### 3.4 Safe Delete

**Action:** `safe_delete`
**Endpoints:** `POST /safeDeletePreview` → `POST /safeDeleteApply`

**What it does:** Deletes a symbol only if no blocking usages
exist. Preview reports the found usages as conflicts; apply deletes (or
blocks with `CONFLICT` unless `force`). Same engine as `rename`.

### 3.5 Inline

**Action:** `inline`
**Endpoints:** `POST /inlinePreview` → `POST /inlineApply`

**What it does:** Replaces a symbol with its body at all call sites and
removes the declaration. Preview reports the affected sites + conflicts; apply
performs the multi-file replacement. Same engine as `rename`.

---

## 4. Gain Tool Window

A dockable bottom tool window (`LeanCtxGain`) that renders the rich
`lean-ctx gain` report inside the IDE — a hero Gain Score, four sub-scores, a
task-category table and a top-files heatmap, plus a footer with the model name
and refresh age. It is a read-only consumer; the existing status-bar widget keeps
its cheap local `StatsReader` and merely acts as one of the triggers.

### 4.1 Data flow

`GainService.load()` spawns `lean-ctx gain --json` as a **subprocess** (via the
shared `BinaryResolver.runCommand`, off the EDT) with a **10-second timeout** —
shorter than the status bar's 30 s so a hung binary surfaces an error quickly.
The captured stdout is parsed by `GainCodec.parse` (Gson, `disableHtmlEscaping`),
which maps the snake_case JSON payload onto typed DTOs via `@SerializedName`. The
service classifies the outcome into a typed `GainLoadResult`, which the panel maps
1:1 onto one of four UI states:

| `GainLoadResult` | Trigger                                       | Panel state                                   |
|------------------|-----------------------------------------------|-----------------------------------------------|
| `Ok(data)`       | exit 0, parsed, has data                      | data view (hero, sub-scores, tables, footer)  |
| `Empty`          | exit 0 but `tokens_saved == 0` and 0 commands | "no data captured yet"                        |
| `BinaryNotFound` | stderr contains `binary not found`            | hint to run `lean-ctx setup` / check PATH     |
| `Failed(reason)` | exit ≠ 0, timeout (exit `-1`), or parse error | error message + stderr excerpt + retry button |

Choosing a subprocess over the existing HTTP backend is deliberate: that backend
is **plugin-as-server** (Rust queries the IDE for PSI), which is the wrong
direction here — the tool window is the consumer and Rust is the producer. The
subprocess keeps the `GainScore` logic as the single source of truth in Rust;
Kotlin only renders.

### 4.2 Schema contract (DTO keys)

Because `gain --json` is effectively the tool window's API, its top-level keys are
pinned against the Kotlin DTOs (`dto/GainData.kt`) by a Rust drift test
(`e82ddbec`) — a schema change breaks the test instead of silently breaking the
plugin. Only the rendered subset is parsed; extra payload keys (`model`,
`energy_wh`, `co2_grams`, `roi`, …) are ignored by Gson.

| JSON key                  | DTO                          | Notes                                                                        |
|---------------------------|------------------------------|------------------------------------------------------------------------------|
| `summary`                 | `GainSummaryDTO`             | hero + sub-scores root                                                       |
| `summary.model.model_key` | `ModelDTO.modelKey`          | footer model name                                                            |
| `summary.tokens_saved`    | `GainSummaryDTO.tokensSaved` | hero                                                                         |
| `summary.gain_rate_pct`   | `GainSummaryDTO.gainRatePct` | hero                                                                         |
| `summary.avoided_usd`     | `GainSummaryDTO.avoidedUsd`  | hero                                                                         |
| `summary.score`           | `ScoreDTO`                   | `total`, `compression`, `cost_efficiency`, `quality`, `consistency`, `trend` |
| `tasks[]`                 | `TaskRow`                    | `category`, `commands`, `tokens_saved`, `tool_calls`, `tool_spend_usd`       |
| `heatmap[]`               | `FileRow`                    | `path`, `access_count`, `tokens_saved`, `compression_pct`                    |

The `tasks` and `heatmap` arrays default to empty when absent (Gson bypasses the
Kotlin constructor defaults, so `GainCodec.parse` normalizes them post-parse).

### 4.3 Visibility-gated polling

`GainPollController` is **visibility-gated** via
`ToolWindowManagerListener.stateChanged` + `toolWindow.isVisible`: it loads
**immediately** when the window becomes visible (no initial delay), then polls on
a 30 s timer only while the window stays visible. Hiding, detaching or switching
tabs stops the timer at once — no subprocess is spawned while the window is not
shown. A manual refresh button in the toolbar forces an immediate reload, and the
timer is bound to a `Disposable` on the tool-window content for cleanup on close.

### 4.4 Triggers

Two entry points open the window, both referencing the `GAIN_TOOL_WINDOW_ID`
constant (`"LeanCtxGain"`) rather than a string literal:

- **Status-bar click** — `LeanCtxStatusBarWidget` activates the tool window via
  its click consumer.
- **Tools menu → "Gain Report"** — the existing `GainAction` was repurposed to
  activate the tool window instead of showing a text popup.

### 4.5 Output hygiene

Command output is stripped of ANSI escape sequences before display
(`util/AnsiText.stripAnsi`, fix `b933e510`) so colored CLI output never leaks raw
escape codes into the Swing panel or the command-result popups.

---

## 5. Editor-Focus Reporter

The plugin reports the path of the focused editor file to lean-ctx so the
context engine can rank it up. This is the **JetBrains producer side of #500
(editor focus)** — 1:1 parity with the VS Code producer
(`vscode-extension/src/editor-signal.ts`). Until this was added, JetBrains users
got none of the #500 ranking boost; the reporter
(`EditorFocusReporter`, wired in `LeanCtxStartupActivity`) closes that gap.

**Privacy — path only, never content.** The signal carries nothing but the
absolute file path. The file's contents are never read, hashed, or transmitted.
Only real, local files **inside the current project** are reported (no
scratch/decompiled/library buffers, no directories).

### 5.1 Mechanism (producer side)

The reporter mirrors the focused-file path into lean-ctx's existing #500 ingress;
it does **not** introduce a new signal format or daemon:

- **Trigger:** a focused-file change (`FileEditorManagerListener.selectionChanged`)
  and the initially open file at project start both call
  `EditorFocusReporter.onFileFocused(file)`.
- **Filter:** the file must be `isInLocalFileSystem`, not a directory, and sit
  under `project.basePath` (segment-boundary check, so `/foo/bar2` is not treated
  as under `/foo/bar`). Anything else is dropped.
- **Dedup + debounce:** the same path back-to-back is skipped (`lastSent`); a 2 s
  pooled-thread `Alarm` (`DEBOUNCE_MS = 2_000`, identical to VS Code) collapses
  rapid tab hops to a single emission.
- **Emit:** fire-and-forget on a pooled thread (never the EDT) — it shells out to
  the resolved binary as `lean-ctx editor-signal --file <absPath>` (via
  `BinaryResolver`). The Rust side (`core::editor_signal::record_focus`) is the
  **single source of truth** for the on-disk format
  (`~/.lean-ctx/editor_signal.json`, `recent_files` ring, path normalization,
  freshness) — the plugin only passes the path, so there is no Kotlin drift of
  the signal format. The consumer (`apply_boost` in `ctx_preload`) then lifts
  matching ranking candidates.

A missing or too-old binary (no `editor-signal` subcommand) and any spawn/IO error
are swallowed silently — a lost signal is harmless, the next focus change resends.
The debounce `Alarm` is bound to a project-scoped `Disposable`, so it is cancelled
on project close (no leak, no spawn after close).

> **Known limit (inherited from #500, not a JetBrains regression):**
> `editor_signal.json` is a single **global** file, so multiple IDE/editor windows
> are last-write-wins. This is identical to VS Code's behavior; per-window
> correctness would be an editor-agnostic #500 core change and is out of scope.

### 5.2 Opt-out (registry key)

The reporter is **on by default**. It can be disabled via the built-in IntelliJ
registry key `leanctx.editor.signal.enabled` (default **`true`**), evaluated
producer-side on every focus event:

| Registry key                    | Default | Effect when `false`                                    |
|---------------------------------|---------|--------------------------------------------------------|
| `leanctx.editor.signal.enabled` | `true`  | no signal is emitted on focus change (no binary spawn) |

Toggling the key takes effect on the next focus change — no IDE restart needed. A
registry key (rather than a visible settings page) was chosen deliberately: the
signal is a path-only ranking hint and the plugin has no other config layer, so a
power-user opt-out with minimal surface is sufficient.

---

## 6. IDE UI Integration

Beyond the headless HTTP surface (§2–§3), the plugin ships three user-facing IDE
touchpoints: a status-bar widget, a `lean-ctx` Tools menu, and explicit K2
(Kotlin-2 compiler mode) support. All three are registered in
`META-INF/plugin.xml`.

### 6.1 Status-bar widget

The widget shows real-time token savings and is registered as a
`statusBarWidgetFactory` with `id="com.leanctx.statusBar"` and
`order="after encodingWidget"` — so it sits immediately right of the encoding
indicator in the IDE status bar.

- **Factory** (`LeanCtxStatusBarFactory`): `isAvailable`/`canBeEnabledOn` both
  return `true`; `createWidget` produces a `LeanCtxStatusBarWidget`, disposed via
  `Disposer.dispose`.
- **Widget** (`LeanCtxStatusBarWidget`, a `StatusBarWidget.TextPresentation`):
  on `install` it renders once and then arms a daemon `Timer` that re-reads the
  stats **every 30 s** and calls `statusBar.updateWidget(ID())`.
- **Text:** `⚡ <N> saved` (e.g. `⚡ 12.4K saved`) when savings are positive,
  otherwise the idle label `⚡ lean-ctx`. The tooltip reads
  `lean-ctx: <N> tokens saved · <M> commands`, or `lean-ctx: No stats yet` when
  no stats file exists.
- **Click → Gain Tool Window:** the click consumer calls
  `ToolWindowManager.getInstance(project).getToolWindow(GAIN_TOOL_WINDOW_ID).activate(null)`
  — the same `GAIN_TOOL_WINDOW_ID` constant documented in §4, so a click on the
  widget opens the Gain Tool Window.

**Stats source** (`StatsReader` + `LeanCtxStats`): `StatsReader.read()` reads
`~/.lean-ctx/stats.json` and regex-extracts the long fields
`total_input_tokens`, `total_output_tokens`, `total_commands` (missing file or
parse error → `null`, never throws). `tokensSaved` mirrors the Rust source of
truth `input.saturating_sub(output)`:
`(totalInputTokens − totalOutputTokens).coerceAtLeast(0)`. `formattedSavings()`
renders `M`/`K`/raw with a `Locale.US` decimal point. The same reader feeds the
Gain panel (§4).

### 6.2 Tools menu (`lean-ctx`)

`plugin.xml` registers an action group `LeanCtx.Menu` (`text="lean-ctx"`,
`popup="true"`) added to the IDE `ToolsMenu` (anchor `last`). It contains four
actions:

| Action       | ID                | Runs                                                  |
| ------------ | ----------------- | ----------------------------------------------------- |
| Setup        | `LeanCtx.Setup`   | `lean-ctx setup` — output in a Messages popup         |
| Doctor       | `LeanCtx.Doctor`  | `lean-ctx doctor` — output in a Messages popup        |
| Gain Report  | `LeanCtx.Gain`    | opens the Gain Tool Window (`GAIN_TOOL_WINDOW_ID`)    |
| Dashboard    | `LeanCtx.Dashboard` | `lean-ctx dashboard` — fire-and-forget              |

- **Base class:** `SetupAction` and `DoctorAction` extend the abstract
  `LeanCtxCommandAction(vararg args)` (in `actions/LeanCtxActions.kt`). Its
  `actionPerformed` runs `BinaryResolver.runCommand(*args)`, takes the captured
  `stdout` (falling back to `stderr` when blank), pipes it through `stripAnsi`,
  and shows the result in a `Messages.showInfoMessage` popup titled `lean-ctx`.
  So `SetupAction = LeanCtxCommandAction("setup")` and
  `DoctorAction = LeanCtxCommandAction("doctor")` differ only by their argument.
- **`GainAction`** extends `AnAction` directly and only activates the Gain Tool
  Window via `GAIN_TOOL_WINDOW_ID` — it spawns no binary.
- **`DashboardAction`** extends `AnAction` directly and calls
  `BinaryResolver.runCommand("dashboard")` fire-and-forget (no popup; the CLI
  opens its own dashboard).

**ANSI strip** (`util/AnsiText.kt`, `stripAnsi`): the `lean-ctx` CLI emits ANSI
CSI escape sequences (colour/SGR) that a Swing `Messages` dialog cannot render.
`stripAnsi` removes them with the regex `\[[0-9;?]*[ -/]*[@-~]` before the
captured output is shown, so the Setup/Doctor popups display clean text rather
than raw escape codes.

### 6.3 K2 mode

The plugin declares K2 support via
`<supportsKotlinPluginMode supportsK2="true"/>` (under the
`org.jetbrains.kotlin` extension namespace). K2 is the Kotlin-2 compiler/analysis
mode of the Kotlin IDE plugin; this declaration tells the IDE the plugin is
compatible with the K2 frontend, so it remains enabled when the user runs the
IDE in K2 mode. The plugin's PSI/navigation/refactoring operations (§2–§3) work
under both the legacy and the K2 Kotlin plugin modes.

The `<supportsKotlinPluginMode supportsK2="true"/>` declaration lives in the optional
`leanctx-jvm.xml` module (not in the Core `plugin.xml`), because it references the
`org.jetbrains.kotlin` namespace. It is loaded only in JVM-capable IDEs where the Kotlin
plugin is present; non-JVM IDEs (RustRover, PyCharm) never parse this block, which is
what keeps the Core free of any hard `java-capable` / Kotlin dependency.

---

## 7. Behavioral Guarantees & Guards

### 7.1 BLAKE3 conflict guard (Rust-central)

The `expected_hash` (edits) or `plan_hash` (refactoring) is a **BLAKE3 hex**
(`crate::core::hasher::hash_hex`) and is checked **exclusively in Rust** — the
plugin does not hash and does not know the field in the wire protocol (`EditRequest`
carries only `path`/`range`/`text`).

- **Headless:** `local_range_write` reads the current bytes of the range, compares
  against `expected_hash`, and aborts on divergence with `CONFLICT: range hash
  mismatch` — the file stays unchanged.
- **IDE (Backing B):** Rust checks the same hash against the disk bytes **before** the
  HTTP POST. So the guard is identical on both paths (same disk bytes,
  same BLAKE3 check).

This prevents blindly overwriting externally modified locations.

### 7.2 Smart mode, language, PathJail

- **Smart mode:** If the IDE is in dumb mode (index being built),
  PSI operations return `INDEXING` instead of a partial result (no automatic waiting).
  For the refactoring engine this is mandatory: an incomplete usage set would be
  a broken refactoring.
- **Language:** If an LSP configuration is missing (Backing A) or a PSI processor
  (Backing B), `UNSUPPORTED_LANGUAGE` is returned (defensive, nullable EP resolution).
- **PathJail:** Every file operation is validated against the `project_root` before
  execution — both the name_path/position resolution and every
  `usage`/`changed_path` returned by the plugin.

### 7.3 Idempotency & atomicity

| Operation                                    | Transaction                                    | Idempotent                    |
|----------------------------------------------|------------------------------------------------|-------------------------------|
| Navigation, structure, inspections           | smart-mode read action                         | yes (index-stable)            |
| Symbol-body edits                            | `WriteCommandAction` (IDE) / atomic (headless) | protected via `expected_hash` |
| Refactoring (rename/move/safe_delete/inline) | multi-file `WriteCommandAction`                | protected via `plan_hash`     |
| Reformat                                     | `WriteCommandAction` (single file)             | yes (formatting-stable)       |

Headless writes are atomic (temp file `.<name>.lean-ctx.tmp.<pid>` + `rename`,
`local_range_write` in `rust/src/lsp/edit_apply.rs`).

### 7.4 Cache coherence

After every write, lean-ctx evicts the file from the cache; the next `ctx_read`
re-validates via mtime (~13 tokens). The `editedText` of the `EditResponse` allows an
immediate rewarm; for multi-file refactoring each `changed_path` is mtime-checked.

---

## 8. Authentication & Security

- **Token per project:** On start the plugin generates a random token
  (`SecureRandom`, hex), stored in the port file. It is checked on every HTTP request
  via the header **`X-LeanCtx-Token`**.
- **401 on missing/mismatch:** `headerToken != token` →
  `HttpResult(401, {"error":{"code":"UNAUTHORIZED",…}})` — no processing.
- **Loopback only:** The HTTP server listens on `127.0.0.1` (not exposed on the
  network) and runs in the IDE user context.
- **Rotation:** On IDE restart a new port file with a new token is created.

See also [Journey 13 — Security & Governance](13-security-and-governance.md).

---

## 9. Error Catalog

**HTTP status:** `200` = success **or** domain negative case (envelope); `401`
= token missing/wrong; `404` = no route for `METHOD /path`; `500` = a real,
unexpected exception. (An `IllegalArgumentException`, e.g. an empty body, is returned
as `200` + `INTERNAL`.)

**Envelope:** `{"error":{"code":"<CODE>","message":"<text>"}}`

| Code                    | Trigger                                                                        | Source                                     | Remedy                                             |
|-------------------------|--------------------------------------------------------------------------------|--------------------------------------------|----------------------------------------------------|
| `UNAUTHORIZED`          | token missing/wrong (401)                                                      | plugin (`RequestRouter`)                   | send a valid `X-LeanCtx-Token`                     |
| `NOT_FOUND`             | unknown route (404)                                                            | plugin                                     | check the endpoint path                            |
| `FILE_NOT_FOUND`        | file not readable                                                              | Rust (`edit_apply`) / plugin               | verify the path with `ctx_tree`                    |
| `POSITION_OUT_OF_RANGE` | line/column past EOF / `end < start`                                           | Rust / plugin                              | re-resolve the range (`ctx_read`)                  |
| `CONFLICT`              | `expected_hash`/`plan_hash` mismatch; or conflicts ∧ `!force`                  | Rust                                       | read fresh, refresh the hash; if needed `force`    |
| `AMBIGUOUS_SYMBOL`      | `name_path` matches >1 symbol                                                  | Rust (`ctx_refactor`)                      | qualify (`Class/method`) — note the candidate list |
| `NO_SYMBOL`             | `name_path` / target range matches 0 symbols                                   | Rust / plugin (refactor)                   | correct the name/path                              |
| `NO_SYMBOL_AT_POSITION` | no resolvable element/reference at the given `line:character`                  | plugin (nav/structure PSI)                 | re-resolve the position (`ctx_read`)               |
| `INVALID_TARGET`        | unknown move/reformat scope kind, or move destination missing/not a directory  | plugin (`SymbolMover`/`SymbolReformatter`) | fix the `target`/`scope` kind or destination path  |
| `INDEXING`              | IDE in dumb mode                                                               | plugin (`PsiLocator`)                      | wait until indexing is finished, retry             |
| `UNSUPPORTED`           | refactoring engine refused the operation (e.g. recursive/non-inlinable symbol) | plugin (`SymbolInliner`)                   | pick a different symbol/operation                  |
| `UNSUPPORTED_LANGUAGE`  | no LSP config / no PSI processor; or `type_hierarchy`/IDE-PSI `symbols_overview` in a non-JVM IDE (empty `structureProvider` EP) | Rust / plugin                              | language is not (yet) supported; for structure ops use `ctx_outline`/`implementations`/`ctx_callgraph` |
| `BACKEND_REQUIRED`      | refactoring without a running IDE                                              | Rust (trait default)                       | start the IDE with an open project                 |
| `INTERNAL`              | other error / parse                                                            | both                                       | check `message`; report a bug if needed            |

---

## 10. End-to-End Examples

**Example 1 — Replace a function body conflict-safely.**

```text
# 1. fetch the current range + hash (ctx_read delivers bytes; hash = BLAKE3 of the range)
ctx_refactor action=symbols_overview path=src/Main.kt        # find symbol + line
# 2. replace, secured against the expected hash
ctx_refactor action=replace_symbol_body name_path=Main/run \
  new_body="fun run() { println(\"v2\") }" expected_hash=<blake3-hex>
# → applied:true ; on a concurrent change → CONFLICT (file untouched)
```

**Example 2 — Project-wide rename (two-phase).**

```text
# Phase 1: preview — see usages + conflicts
ctx_refactor action=rename path=src/Main.kt line=7 column=4 new_name=execute
#   internal: POST /renamePreview → {usages:[…], conflicts:[]}
# Phase 2: with empty conflicts, apply automatically (one transaction, one undo)
#   internal: POST /renameApply → {applied:true, changed_paths:[…]}
```

**Example 3 — Reformat a file (after an edit).**

```text
ctx_refactor action=replace_symbol_body name_path=Main/run new_body="…"
ctx_refactor action=reformat path=src/Main.kt    # apply code style afterward
# → {"reformatted":true,"path":"src/Main.kt"}
```

---

## 11. Cross-references & Sources

- [Concise agent reference](appendix-jetbrains-plugin.md) — tables for quick lookup
- [Per-IDE quickstarts](appendix-ide-quickstarts.md) — setup for JetBrains IDEs
- [MCP tool map](appendix-mcp-tools.md) — all MCP tools incl. `ctx_refactor`, `ctx_search`
- [Journey 4 — Code Intelligence](04-code-intelligence.md)
- [Journey 13 — Security & Governance](13-security-and-governance.md) — PathJail, auth
- Source code: `rust/src/lsp/{backend,jetbrains_backend,router,edit_apply,port_discovery}.rs`,
  `rust/src/tools/registered/ctx_refactor.rs`,
  `packages/jetbrains-lean-ctx/src/main/kotlin/com/leanctx/plugin/{server,endpoint,psi,dto}/…`
