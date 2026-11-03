// SPDX-License-Identifier: Apache-2.0
//! Claude Code's global `CLAUDE.md` pointer block and the managed-block
//! helpers every global instructions file (CLAUDE.md / CODEBUDDY.md) shares.
//!
//! `rules_inject` repairs the Claude pointer through
//! [`sync_claude_global_rules_block`] and `context_overhead` measures it, so
//! the block lives in core; the hook installers (`hooks::agents::claude`,
//! `codebuddy`, `claude_governor`) reuse it from here.

use crate::config_io::{ensure_dir_reported as ensure_state_dir, write_if_changed as write_file};

/// Setup output is silent while the MCP server runs or `--quiet` is set
/// (same rule as the hook installers' `mcp_server_quiet_mode`).
fn setup_quiet() -> bool {
    crate::core::runtime_flags::mcp_server_enabled() || crate::core::runtime_flags::quiet_enabled()
}

/// Shared with `doctor` so the instructions check recognises the same block
/// this installer writes (GH #396: doctor must not demand the retired rules file).
///
/// The CLAUDE.md block is a lightweight *pointer* block, so it is delimited by
/// `AGENTS_BLOCK_START`/`END` (`<!-- lean-ctx -->`), NOT the full-rules markers
/// `START_MARK`/`END_MARK` (`<!-- lean-ctx-rules -->`). Pointing these at the
/// rules markers made `contains()` never match the block lean-ctx actually
/// writes, so `doctor` reported it missing and every `setup`/`doctor --fix`
/// appended a duplicate (GH #549).
pub const CLAUDE_MD_BLOCK_START: &str = crate::core::rules_canonical::AGENTS_BLOCK_START;
pub const CLAUDE_MD_BLOCK_END: &str = crate::core::rules_canonical::AGENTS_BLOCK_END;
pub const CLAUDE_MD_BLOCK_VERSION: &str = "lean-ctx-claude-v13";

// v3 (GL #555): self-contained, no `@rules/lean-ctx.md` import. Claude Code
// expands `@` imports inline at launch ("imports do not reduce context usage"
// — code.claude.com/docs/en/memory), so the old pointer silently tripled the
// per-session footprint. Detail docs now live in the lean-ctx skill, which
// loads on demand only.
//
// v4 (GH #637 / GL #1138): MCP-aware guidance. The old block recommended
// `ctx_edit` unconditionally — in sessions where the lean-ctx MCP server is
// not connected the advertised fallback does not exist, and agents strand on
// shell heredocs. It also predated `read_redirect=auto`, which makes native
// Read → Edit a guard-safe editing path under Claude Code (the read-before-
// write gate is path-keyed, so the edited file must be *natively* read).
//
// v5 (#1008 / GL #1144): anchored editing routes to `ctx_patch` (now advertised
// in the lazy core for Claude Code) — line+hash anchors instead of old_string
// reproduction; `ctx_edit` demoted to a legacy power-profile mention. Native
// Read → Edit stays fully supported (v4's guard semantics unchanged).
//
// v7 (#1091): fix the Replace-mode block's write guidance. "Write and Delete —
// use native tools normally" was wrong under Replace mode: native `Edit` needs
// a prior native `Read` (denied), and native `Write` only succeeds for
// brand-new files, so `ctx_patch` is the actual edit path. The MCP block is
// unchanged; the shared version tag bumps so existing blocks get rewritten.
//
// v8 (#1228): stop denying native Read in Replace mode. Auto memory under
// `~/.claude/projects/<slug>/memory/` and the edit gate both need native Read;
// a bare permissions.deny Read cannot carve out exceptions (deny wins). Grep/
// Glob stay denied. Agents must not use MCP resources/read with file:// URIs.
//
// v9 (#1399): native Read must remain a narrow edit-gate carve-out, not imply
// that it is suitable for exploration. Existing blocks must be rewritten.
//
// v10 (#1909): drop the "~13 tokens" re-read claim. Under Claude Code the
// scope is process-derived and sub-agents share the MCP process, so re-read
// stubs are withheld (#1801) and the claim did not hold.
//
// v12 (A3): the search-hidden block dropped its "(Replace mode)" header — the
// default Hybrid install writes it too (it hides Grep/Glob as well), and the
// label told Hybrid users they were in Replace mode; "ALWAYS use ctx_read"
// became a plain routing line.
//
// v13 (read volume, pilot P7): the edit line sent agents to whole-file
// `mode="anchored"` reads, yet 72 of 78 measured ctx_patch ops were
// replace_unique, which needs no anchors — the anchored body was paid for and
// unused. Edits now read only the region (symbol= / lines:N-M) and name paths
// relative to the project root; anchored stays for line+hash ops, windowed.
pub const CLAUDE_MD_BLOCK_CONTENT_MCP: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-claude-v13 -->
## LeanCTX (lean-ctx) — Context Gateway for AI agents

When the `ctx_*` MCP tools are listed in this session, prefer them over native equivalents:
- `ctx_read` instead of `Read` / `cat` for task-relevant exploration (cached structural views)
- `ctx_shell` instead of `bash` / `Shell` (command-specific output shaping)
- `ctx_search` instead of `Grep` / `rg` (compact results)
- `ctx_tree` instead of `ls` / `find` (compact directory maps)
- Edits: `ctx_read` the region (`symbol=`/`lines:N-M`) → `ctx_patch` replace_unique; `anchored:N-M` for line+hash ops; `op=create` for new files; relative paths. `ctx_edit` (str_replace) is the legacy power-profile fallback.

Native `Read` → `Edit`/`StrReplace` stays fully supported — the edit gate requires a
prior native Read of the same file path. Auto memory under
`~/.claude/projects/<slug>/memory/` also uses native Read/Edit (not MCP resources).
Write, Delete, Glob — use normally.
If no `ctx_*` tools are listed in this session, use the native tools throughout.

Read modes: anchored (edit), full (verbatim), map (overview), signatures (API), diff (post-edit), lines:N-M (range), auto.
Details live in the `lean-ctx` skill (loads on demand — keep this file lean).
<!-- /lean-ctx -->";

pub const CLAUDE_MD_BLOCK_CONTENT_REPLACE: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-claude-v13 -->
## LeanCTX (lean-ctx) — Context Gateway for AI agents

Native Grep/Glob are denied by policy. Prefer `ctx_*` MCP tools for project work:
- `ctx_read` for task-relevant exploration (cached structural views)
- `ctx_shell` for shell commands (command-specific output shaping)
- `ctx_search` instead of Grep/rg (compact results)
- `ctx_tree` instead of ls/find (compact directory maps)
- `ctx_glob` instead of Glob (file pattern matching)
- Project edits: `ctx_read` the region (`symbol=`/`lines:N-M`) → `ctx_patch` replace_unique; `anchored:N-M` for line+hash ops; `op=create` for new files; relative paths.

Native `Read` is reserved for the edit gate (read-before-write); explore with `ctx_read`.
Claude auto memory (`~/.claude/projects/<slug>/memory/` — MEMORY.md and topic
files) uses native Read/Edit internally; do NOT call MCP `resources/read` with
file:// URIs (lean-ctx resources are `lean-ctx://context/*` only). Native Delete is fine.

Read modes: anchored (edit), full (verbatim), map (overview), signatures (API), diff (post-edit), lines:N-M (range), auto.
Details live in the `lean-ctx` skill (loads on demand — keep this file lean).
<!-- /lean-ctx -->";

pub fn install_claude_global_claude_md_for_mode(
    home: &std::path::Path,
    mode: crate::core::hook_mode::HookMode,
) {
    let claude_dir = crate::core::editor_registry::claude_state_dir(home);
    if !ensure_state_dir(&claude_dir) {
        return;
    }
    let claude_md_path = claude_dir.join("CLAUDE.md");

    // Neither dedicated nor off keep a lean-ctx block in the user's CLAUDE.md:
    //  - dedicated (#343): the SessionStart hook injects the compact summary;
    //  - off (#361): the user opted out of lean-ctx steering entirely.
    // Strip any block a previous shared install left so switching modes is clean.
    if matches!(
        crate::core::config::Config::load().rules_injection_effective(),
        crate::core::config::RulesInjection::Dedicated | crate::core::config::RulesInjection::Off
    ) {
        strip_claude_md_block(&claude_md_path);
        return;
    }

    let existing = std::fs::read_to_string(&claude_md_path).unwrap_or_default();
    // The block describes the tool list Claude actually has: the Grep/Glob
    // block whenever they are hidden, which the default mode keeps.
    let hides_search = crate::core::hook_mode::claude_hides_native_search(mode);
    let base_block = if hides_search {
        CLAUDE_MD_BLOCK_CONTENT_REPLACE
    } else {
        CLAUDE_MD_BLOCK_CONTENT_MCP
    };

    // A single up-to-date block needs no rewrite. Check both version tag AND
    // mode-specific content — the Replace and MCP blocks share the version tag
    // but have different instructions (GH #1250 follow-up).
    let solution_block = crate::core::rules_canonical::enabled_solution_rules_block();
    let block = if solution_block.is_empty() {
        base_block.to_owned()
    } else {
        format!("{base_block}\n\n{solution_block}")
    };
    let is_replace_block = existing.contains("denied by policy");
    let mode_matches = hides_search == is_replace_block;
    if managed_md_is_current(&existing, CLAUDE_MD_BLOCK_START, &block)
        && existing.contains(CLAUDE_MD_BLOCK_VERSION)
        && mode_matches
    {
        return;
    }
    let cleaned = remove_managed_md_blocks(&existing, CLAUDE_MD_BLOCK_START, CLAUDE_MD_BLOCK_END);
    let cleaned = cleaned.trim();
    let updated = if cleaned.is_empty() {
        format!("{block}\n")
    } else {
        format!("{cleaned}\n\n{block}\n")
    };
    write_file(&claude_md_path, &updated);
}

/// Repair Claude's compact global pointer without installing hooks or a full
/// rules target. Returns whether the file changed.
pub fn sync_claude_global_rules_block(
    home: &std::path::Path,
    mode: crate::core::hook_mode::HookMode,
) -> Result<bool, String> {
    // The governor's rules block shares the markers; never overwrite it.
    if crate::core::claude_host::is_governor_installed(home) {
        return Ok(false);
    }
    let path = crate::core::editor_registry::claude_state_dir(home).join("CLAUDE.md");
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    let removed_legacy = remove_claude_rules_file(home);
    install_claude_global_claude_md_for_mode(home, mode);
    let after = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {} after sync: {e}", path.display()))?;
    if !crate::marked_block::contains_marker_line(&after, CLAUDE_MD_BLOCK_START) {
        return Err(format!(
            "{} is missing the canonical lean-ctx pointer block after sync",
            path.display()
        ));
    }
    Ok(removed_legacy || before != after)
}

/// Remove the lean-ctx block (and its solution block) from `CLAUDE.md`
/// (dedicated/off mode). Deletes the file entirely if it becomes empty (i.e.
/// lean-ctx was its only content).
pub fn strip_claude_md_block(claude_md_path: &std::path::Path) {
    let Ok(existing) = std::fs::read_to_string(claude_md_path) else {
        return;
    };
    if !contains_managed_md_block(&existing, CLAUDE_MD_BLOCK_START) {
        return;
    }
    let cleaned = remove_managed_md_blocks(&existing, CLAUDE_MD_BLOCK_START, CLAUDE_MD_BLOCK_END);
    if cleaned.trim().is_empty() {
        let _ = std::fs::remove_file(claude_md_path);
    } else {
        write_file(claude_md_path, &format!("{}\n", cleaned.trim_end()));
    }
}

/// Remove the lean-ctx-owned `~/.claude/rules/lean-ctx.md` (GL #555).
///
/// Claude Code loads every rules file without `paths:` frontmatter
/// unconditionally at session start (code.claude.com/docs/en/memory), so this
/// file duplicated the CLAUDE.md block in *every* session — users reported
/// 12k+ tokens of memory files from stacked lean-ctx instructions. The
/// CLAUDE.md block is self-contained and detail docs live in the on-demand
/// skill; only files carrying our rules marker are touched.
///
/// A3: a user-extended copy keeps every line outside the lean-ctx markers
/// (`.bak` backup); only the lean-ctx region goes.
pub fn remove_claude_rules_file(home: &std::path::Path) -> bool {
    let rules_path = crate::core::editor_registry::claude_rules_dir(home).join("lean-ctx.md");
    let Some(line) = crate::core::rules_dedup::retire_rules_file(&rules_path) else {
        return false;
    };
    if !setup_quiet() {
        eprintln!(
            "{line} (always-loaded duplicate; the CLAUDE.md block + on-demand skill replace it)"
        );
    }
    true
}

/// Remove the first lean-ctx block delimited by `start`..`end` from `content`.
/// Shared by the Claude/CodeBuddy CLAUDE.md/CODEBUDDY.md installers and `doctor`.
/// Markers match as whole (trimmed) lines only (GL #1158) — a prose mention of
/// a marker must never trigger block surgery.
pub fn remove_block(content: &str, start: &str, end: &str) -> String {
    let s = crate::marked_block::marker_line_span(content, start);
    let e = s.and_then(|(si, _)| {
        crate::marked_block::marker_line_span(&content[si..], end)
            .map(|(es, ee)| (si + es, si + ee))
    });
    match (s, e) {
        (Some((si, _)), Some((_, end_after))) => {
            let before = content[..si].trim_end_matches('\n');
            let after = &content[end_after..];
            let mut out = before.to_string();
            out.push('\n');
            if !after.trim().is_empty() {
                out.push('\n');
                out.push_str(after.trim_start_matches('\n'));
            }
            out
        }
        _ => content.to_string(),
    }
}

/// Remove *every* lean-ctx block delimited by `start`..`end`. Heals files that
/// accumulated duplicate blocks from the pre-#549 marker mismatch (the detector
/// constant pointed at `<!-- lean-ctx-rules -->` while the written block used
/// `<!-- lean-ctx -->`, so every `setup`/`doctor --fix` appended a fresh copy).
/// Callers then write exactly one canonical block back.
pub fn remove_all_blocks(content: &str, start: &str, end: &str) -> String {
    let mut out = content.to_string();
    while crate::marked_block::contains_marker_line(&out, start) {
        let next = remove_block(&out, start, end);
        if next == out {
            break; // malformed (start without end) — avoid an infinite loop
        }
        out = next;
    }
    out
}

/// Whole-line occurrences of `marker` — the only form the writers emit, so a
/// prose mention of a marker is never counted as a block (GL #1158, #1901).
pub fn count_marker_lines(content: &str, marker: &str) -> usize {
    content.lines().filter(|l| l.trim() == marker).count()
}

/// Remove every lean-ctx block *and* every solution-rules block from a global
/// instructions file (CLAUDE.md / CODEBUDDY.md). The solution block is written
/// right after the lean-ctx block, outside its markers, so removing only the
/// lean-ctx block left it behind and each rewrite stacked another copy (#1901).
pub fn remove_managed_md_blocks(content: &str, start: &str, end: &str) -> String {
    let out = remove_all_blocks(content, start, end);
    remove_all_blocks(
        &out,
        crate::core::rules_canonical::SOLUTION_BLOCK_START,
        crate::core::rules_canonical::SOLUTION_BLOCK_END,
    )
}

/// True when a global instructions file carries a lean-ctx or solution-rules
/// block (as whole marker lines) that a strip must remove.
pub fn contains_managed_md_block(content: &str, start: &str) -> bool {
    crate::marked_block::contains_marker_line(content, start)
        || crate::marked_block::contains_marker_line(
            content,
            crate::core::rules_canonical::SOLUTION_BLOCK_START,
        )
}

/// True when a global instructions file already holds exactly the managed
/// content: one lean-ctx block, at most the one solution block `block`
/// carries, and `block` itself verbatim.
pub fn managed_md_is_current(existing: &str, start: &str, block: &str) -> bool {
    let solution_start = crate::core::rules_canonical::SOLUTION_BLOCK_START;
    count_marker_lines(existing, start) == 1
        && count_marker_lines(existing, solution_start) == count_marker_lines(block, solution_start)
        && existing.contains(block)
}
