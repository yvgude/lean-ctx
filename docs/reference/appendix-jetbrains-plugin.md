# Appendix — JetBrains Plugin (Agent Reference)

> Compact lookup tables for agents: every `ctx_refactor` action, its HTTP
> endpoint, key parameters, backing — plus the user-facing IDE surfaces (Gain
> Tool Window, editor-focus reporter, status-bar widget, Tools-menu actions).
> Full description (curl, responses, guards, architecture, E2E):
> **[Journey 19 — JetBrains Plugin](19-jetbrains-plugin.md)**.
>
> Language: English; tool/endpoint/parameter names and error codes stay verbatim.
> Serena delineation: an independent reimplementation (not a derivation,
> lean-ctx license) — it replaces Serena + the official JetBrains MCP as the
> code-intelligence interface.

## Coordinates & invocation

- Invocation: `ctx_refactor action=<action> …` (MCP) or
  `POST 127.0.0.1:<port><endpoint>` with header `X-LeanCtx-Token: <token>`,
  body = JSON.
- `ctx_refactor` layer: `line` **1-indexed**, `column` **0-indexed**.
  Wire layer: `line`/`character` **0-based**; `line` in `type_hierarchy` /
  `symbols_overview` / `inspections` responses is **1-based**.
- Business-level negative case: HTTP 200 + `{"error":{"code","message"}}`.

## Functions

| Action                 | HTTP endpoint                                  | Purpose                       | Key parameters                                         | Backing                     |
|------------------------|------------------------------------------------|-------------------------------|--------------------------------------------------------|-----------------------------|
| `references`           | `POST /references`                             | semantic usages               | `path`, `line`, `column`, `scope`                      | B (+A fallback)             |
| `definition`           | `POST /definition`                             | jump to definition            | `path`, `line`, `column`                               | B (+A fallback)             |
| `implementations`      | `POST /implementations`                        | implementations/overrides     | `path`, `line`, `column`, `scope`                      | B (+A fallback)             |
| `declaration`          | `POST /declaration`                            | declaration                   | `path`, `line`, `column`                               | B-only                      |
| `type_hierarchy`       | `POST /type_hierarchy`                         | super-/subtype tree           | `path`, `line`, `column`, `direction`                  | B-only                      |
| `symbols_overview`     | `POST /symbols_overview`                       | top-level symbols of the file | `path`                                                 | B (+headless tree-sitter)   |
| `inspections`          | `POST /inspections`, `POST /list_inspections`  | run/list inspections          | `path`, `mode=run\|list`                               | B-only                      |
| `replace_symbol_body`  | `POST /replaceSymbolBody`                      | replace symbol body           | `name_path`/`path`+`line`, `new_body`, `expected_hash` | B (+headless)               |
| `insert_before_symbol` | `POST /insertBeforeSymbol`                     | insert sibling before         | `name_path`, `text`, `expected_hash`                   | B (+headless)               |
| `insert_after_symbol`  | `POST /insertAfterSymbol`                      | insert sibling after          | `name_path`, `text`, `expected_hash`                   | B (+headless)               |
| `rename`               | `POST /renamePreview` → `/renameApply`         | rename symbol + all usages    | `new_name`, `force`, `search_comments`                 | B-only (`BACKEND_REQUIRED`) |
| `reformat`             | `POST /reformat`                               | reformat file in-place        | `path`                                                 | B-only                      |
| `move`                 | `POST /movePreview` → `/moveApply`             | move symbol + references      | target, `force`                                        | B-only (`BACKEND_REQUIRED`) |
| `safe_delete`          | `POST /safeDeletePreview` → `/safeDeleteApply` | delete when no blockers       | `force`                                                | B-only (`BACKEND_REQUIRED`) |
| `inline`               | `POST /inlinePreview` → `/inlineApply`         | inline symbol at call sites   | `force`                                                | B-only (`BACKEND_REQUIRED`) |

Backing: **B** = JetBrains IDE (plugin via HTTP); **A** = rust-analyzer
(headless); **headless** = tree-sitter / `local_range_write` without an IDE. The
refactoring engine (`rename`/`move`/`safe_delete`/`inline`) is two-phase
(`*Preview`→`*Apply`, `plan_hash`-protected) and has no headless path.

> `find_symbol` (Serena) → `ctx_search action="symbol"` / `ctx_outline`, not `ctx_refactor`.

## IDE UI surfaces (non-HTTP)

These are user-facing IDE touchpoints, not part of the `ctx_refactor` HTTP
surface. See the Gain Tool Window (§4), Editor-Focus Reporter (§5) and IDE UI
Integration (§6) sections of [Journey 19](19-jetbrains-plugin.md) for full
detail.

| Surface                 | Identifier / registry key                                    | What it does                                                                                                              | Backing                                                     |
|-------------------------|--------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------|
| Gain Tool Window        | `GAIN_TOOL_WINDOW_ID` (`"LeanCtxGain"`)                      | dockable bottom tool window rendering the Gain report; visibility-gated 30 s polling (`GainPollController`)               | `lean-ctx gain --json` subprocess (off EDT, 10 s timeout)   |
| Editor-focus reporter   | `leanctx.editor.signal.enabled` (default `true`)             | reports the focused editor file path (path only, in-project) to lift #500 ranking; 2 s debounce, opt-out via registry key | `lean-ctx editor-signal --file <absPath>` (fire-and-forget) |
| Status-bar widget       | `id="com.leanctx.statusBar"`, `order="after encodingWidget"` | shows `⚡ <N> saved` (30 s refresh); click activates the Gain Tool Window                                                  | `StatsReader` reads `~/.lean-ctx/stats.json`                |
| Tools menu (`lean-ctx`) | action group `LeanCtx.Menu` (`ToolsMenu`, anchor `last`)     | four actions: Setup / Doctor / Gain Report / Dashboard                                                                    | per-action (see below)                                      |

### Tools-menu actions

| Action      | ID                  | Runs                                                   | Output                                        |
|-------------|---------------------|--------------------------------------------------------|-----------------------------------------------|
| Setup       | `LeanCtx.Setup`     | `lean-ctx setup`                                       | `Messages` popup (ANSI-stripped)              |
| Doctor      | `LeanCtx.Doctor`    | `lean-ctx doctor`                                      | `Messages` popup (ANSI-stripped)              |
| Gain Report | `LeanCtx.Gain`      | activates the Gain Tool Window (`GAIN_TOOL_WINDOW_ID`) | tool window (no binary spawn)                 |
| Dashboard   | `LeanCtx.Dashboard` | `lean-ctx dashboard`                                   | fire-and-forget (CLI opens its own dashboard) |

Setup/Doctor extend `LeanCtxCommandAction(vararg args)`: run
`BinaryResolver.runCommand`, pipe captured `stdout` (or `stderr` when blank)
through `stripAnsi` (`util/AnsiText.kt`, regex `\[[0-9;?]*[ -/]*[@-~]`, fix
`b933e510`), then show a `Messages.showInfoMessage` popup titled `lean-ctx`.
`GainAction` and `DashboardAction` extend `AnAction` directly.

## Guards (short form)

- **BLAKE3 conflict guard** (`expected_hash` edits / `plan_hash` refactoring) —
  Rust-central; the plugin does not hash. Mismatch → `CONFLICT`.
- **PathJail** — every mutation and every reported path is checked against
  `project_root`.
- **Smart mode** — dumb mode → `INDEXING` (no partial result).
- **Auth** — `X-LeanCtx-Token` per project; missing → 401. `127.0.0.1` only.
- **Gain subprocess isolation** — the Gain Tool Window shells out to
  `lean-ctx gain --json` (off the EDT, 10 s timeout); `GainScore` stays the
  single source of truth in Rust, Kotlin only renders. Failures map to typed
  panel states (`BinaryNotFound` / `Failed(reason)`), never an exception.
- **Editor-focus privacy** — path only, never file content; only real, local
  files inside the current project are reported (no scratch/library/directory
  buffers). Missing/old binary or IO error is swallowed silently.
- **Output hygiene** — all CLI output shown in the IDE (Gain panel, Setup/Doctor
  popups) is ANSI-stripped via `stripAnsi` before display.

## Error codes

| Code                    | Trigger                               | Fix                                       |
|-------------------------|---------------------------------------|-------------------------------------------|
| `UNAUTHORIZED` (401)    | token missing/wrong                   | send a valid `X-LeanCtx-Token`            |
| `NOT_FOUND` (404)       | unknown route                         | check the endpoint path                   |
| `FILE_NOT_FOUND`        | file not readable                     | verify the path with `ctx_tree`           |
| `POSITION_OUT_OF_RANGE` | line/column past EOF                  | re-resolve the range                      |
| `CONFLICT`              | hash mismatch or conflicts ∧ `!force` | re-read fresh; use `force` if appropriate |
| `AMBIGUOUS_SYMBOL`      | `name_path` > 1 match                 | qualify (`Class/method`)                  |
| `NO_SYMBOL`             | `name_path` 0 matches                 | correct the name/path                     |
| `INDEXING`              | IDE in dumb mode                      | wait, retry                               |
| `UNSUPPORTED_LANGUAGE`  | no LSP/PSI processor                  | language not supported                    |
| `BACKEND_REQUIRED`      | refactoring without an IDE            | open the IDE with the project             |
| `INTERNAL`              | other error                           | check the `message`                       |

> The Gain Tool Window and editor-focus reporter are subprocess/CLI consumers,
> not HTTP endpoints — they do not surface the codes above. Gain failures map to
> the `BinaryNotFound` / `Failed(reason)` panel states (see §4.1); a lost
> editor-focus signal is silent and harmless.

## See also

- [Journey 19 — JetBrains Plugin](19-jetbrains-plugin.md) — full reference
  (Gain Tool Window §4, Editor-Focus Reporter §5, IDE UI Integration §6)
- [MCP tool map](appendix-mcp-tools.md) · [Per-IDE quickstarts](appendix-ide-quickstarts.md)
