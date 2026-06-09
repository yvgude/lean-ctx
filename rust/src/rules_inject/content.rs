//! The rules/skill payloads lean-ctx injects: shared + dedicated markdown,
//! Cursor MDC, the dedicated-mode session summary, and per-agent rule paths.

use std::path::PathBuf;

use super::RulesFormat;

/// The canonical shared rules block lean-ctx injects into a host instruction file
/// (`CLAUDE.md` / `AGENTS.md`). Exposed for honest per-turn overhead accounting
/// (see `core::context_overhead`, GitHub #361).
#[must_use]
pub fn canonical_rules_block() -> &'static str {
    RULES_SHARED
}

pub fn rules_dedicated_markdown() -> &'static str {
    RULES_DEDICATED
}

pub fn rules_shared_content() -> &'static str {
    RULES_SHARED
}

/// Compact, agent-agnostic tool-mapping summary injected as `SessionStart`
/// `additionalContext` in `rules_injection = "dedicated"` mode.
///
/// It is the always-loaded counterpart of the (now-skipped) CLAUDE.md/AGENTS.md
/// marker block, so it carries the same essential per-session footprint. The
/// full ruleset stays lazy in the removable dedicated rules file. No HTML markers
/// and no `@import` pointer (Codex has no `@import`), so it renders cleanly as a
/// developer/system reminder on both Claude and Codex.
pub fn dedicated_session_summary() -> &'static str {
    DEDICATED_SESSION_SUMMARY
}

const DEDICATED_SESSION_SUMMARY: &str =
    "lean-ctx is active — prefer its tools over native equivalents to save context tokens:
- ctx_read  → instead of Read/cat/head/tail (cached, 10 modes, re-reads ~13 tokens)
- ctx_shell → instead of bash/Shell (95+ output-compression patterns)
- ctx_search → instead of Grep/rg/find (compact, token-efficient results)
- ctx_tree  → instead of ls/find (compact directory maps)
Native Edit/Write/Glob stay as-is. If Edit needs Read and Read is unavailable, use ctx_edit.
NEVER use native Read/Grep/Shell when a ctx_* equivalent exists.";

/// Dedicated-mode rules file for OpenCode, auto-loaded via `opencode.json`
/// `instructions[]` (absolute path) instead of polluting `~/.config/opencode/AGENTS.md`.
pub fn opencode_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".config/opencode/rules/lean-ctx.md")
}

/// Dedicated-mode rules file for Gemini CLI, auto-loaded via `settings.json`
/// `context.fileName` instead of polluting `~/.gemini/GEMINI.md`. The filename is
/// deliberately distinctive: Gemini discovers context files *by name* across the
/// project tree, so a generic `lean-ctx.md` would get picked up everywhere.
pub fn gemini_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".gemini").join(GEMINI_DEDICATED_CONTEXT_FILENAME)
}

/// The `context.fileName` entry registered for Gemini in dedicated mode.
pub const GEMINI_DEDICATED_CONTEXT_FILENAME: &str = "LEANCTX.md";

// ---------------------------------------------------------------------------
// Rules content for SHARED config files (appended to user's existing config).
// LITM-optimized: critical instruction at START and END of block.
// Used for: CLAUDE.md, instructions.md, GEMINI.md, copilot-instructions.md
// ---------------------------------------------------------------------------
pub(super) const RULES_SHARED: &str = r#"# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v11 -->

## Tool Mapping (MANDATORY — use instead of native equivalents)
| Instead of | Use | Example |
|------------|-----|---------|
| Read/cat/head/tail | `ctx_read(path, mode)` | `ctx_read("src/main.rs", "full")` |
| Grep/rg/find | `ctx_search(pattern, path)` | `ctx_search("fn handle", "src/")` |
| Shell/bash | `ctx_shell(command)` | `ctx_shell("cargo test")` |
| Edit (when Read unavailable) | `ctx_edit(path, old, new)` | `ctx_edit("f.rs", "old", "new")` |

## ctx_read Mode Selection
| Goal | Mode | When |
|------|------|------|
| Edit this file | `full` | Before any edit |
| Understand API | `signatures` | Context-only, won't edit |
| Re-read after edit | `diff` | Post-edit verification |
| Large file overview | `map` | >500 lines, won't edit |
| Specific region | `lines:N-M` | Know exact location |

## Workflow (follow this order)
1. **Orient:** `ctx_overview(task)` or `ctx_compose(task, path)` for unfamiliar tasks
2. **Locate:** `ctx_search(pattern, path)` for exact text; `ctx_semantic_search(query)` for concepts
3. **Read:** `ctx_read(path, mode)` with appropriate mode from table above
4. **Edit:** `ctx_edit(path, old_string, new_string)` or native Edit if available
5. **Verify:** `ctx_read(path, "diff")` + `ctx_shell("test command")`
6. **Record:** `ctx_knowledge(action="remember", content="...")` for non-obvious findings

## Session
- **Start:** `ctx_session(action="status")` + `ctx_knowledge(action="wakeup")`
- **End:** `ctx_session(action="decision", content="what was done + next steps")`
- **On [CHECKPOINT]:** `ctx_session(action="task", value="current status")`

NEVER use native Read/Grep/Shell when ctx_* equivalents are available.
<!-- /lean-ctx -->"#;

// ---------------------------------------------------------------------------
// Rules content for DEDICATED lean-ctx rule files (we control entire file).
// LITM-optimized with critical mapping at start and end.
// Used for: Windsurf, Zed, Cline, Roo Code, OpenCode, Continue, Aider
// ---------------------------------------------------------------------------
pub(super) const RULES_DEDICATED: &str = r#"# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v11 -->

## Tool Mapping (MANDATORY — use instead of native equivalents)
| Instead of | Use | Example |
|------------|-----|---------|
| Read/cat/head/tail | `ctx_read(path, mode)` | `ctx_read("src/main.rs", "full")` |
| Grep/rg/find | `ctx_search(pattern, path)` | `ctx_search("fn handle", "src/")` |
| Shell/bash | `ctx_shell(command)` | `ctx_shell("cargo test")` |
| Edit (when Read unavailable) | `ctx_edit(path, old, new)` | `ctx_edit("f.rs", "old", "new")` |

## ctx_read Mode Selection
| Goal | Mode | When |
|------|------|------|
| Edit this file | `full` | Before any edit |
| Understand API | `signatures` | Context-only, won't edit |
| Re-read after edit | `diff` | Post-edit verification |
| Large file overview | `map` | >500 lines, won't edit |
| Specific region | `lines:N-M` | Know exact location |
| Unsure | `auto` | System selects optimal mode |

## Workflow (follow this order)
1. **Orient:** `ctx_overview(task)` or `ctx_compose(task, path)` for unfamiliar tasks
2. **Locate:** `ctx_search(pattern, path)` for exact text; `ctx_semantic_search(query)` for concepts
3. **Read:** `ctx_read(path, mode)` with appropriate mode from table above
4. **Edit:** `ctx_edit(path, old_string, new_string)` or native Edit if available
5. **Verify:** `ctx_read(path, "diff")` + `ctx_shell("test command")`
6. **Record:** `ctx_knowledge(action="remember", content="...")` for non-obvious findings

## Proactive (use without being asked)
- `ctx_overview(task)` — at session start for orientation
- `ctx_compress` — when context grows large (at phase boundaries)
- `ctx_knowledge(action="wakeup")` — at session start to surface prior findings

## Compression Bypass (only when compressed output hides needed detail)
`ctx_read(path, "lines:N-M")` → `ctx_read(path, "full")` → `ctx_shell(cmd, raw=true)`
Return to compressed defaults after one expanded retrieval.

## Risk Gate (before high-impact edits)
Before editing exported symbols, auth, DB schemas, or 3+ files: run `ctx_impact(action="analyze")`
and `ctx_callgraph(action="callers")` to confirm blast radius.

## Session
- **Start:** `ctx_session(action="status")` + `ctx_knowledge(action="wakeup")`
- **End:** `ctx_session(action="decision", content="what was done + next steps")`
- **On [CHECKPOINT]:** `ctx_session(action="task", value="current status")`

NEVER use native Read/Grep/Shell when ctx_* equivalents are available.
<!-- /lean-ctx -->"#;

// ---------------------------------------------------------------------------
// Rules for Cursor MDC format (dedicated file with frontmatter).
// ---------------------------------------------------------------------------
pub(super) const RULES_CURSOR_MDC: &str = include_str!("../templates/lean-ctx.mdc");

pub(super) fn rules_content(format: &RulesFormat) -> &'static str {
    match format {
        RulesFormat::SharedMarkdown => RULES_SHARED,
        RulesFormat::DedicatedMarkdown => RULES_DEDICATED,
        RulesFormat::CursorMdc => RULES_CURSOR_MDC,
    }
}
