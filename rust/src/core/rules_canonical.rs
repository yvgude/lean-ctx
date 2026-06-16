//! Canonical Hybrid Policy — the single source of truth for all lean-ctx rules.
//!
//! Every template, injected rule file, SKILL.md, and MCP instructions field
//! MUST derive its tool-mapping table from this module. No inline tool tables
//! elsewhere in the codebase.

/// Generates the canonical tool-mapping table for the given mode.
pub fn tool_table(mode: Mode) -> &'static str {
    match mode {
        Mode::Hybrid => HYBRID_TABLE,
        Mode::Mcp => MCP_TABLE,
    }
}

/// Returns the full rules block (LITM-optimized) for dedicated rule files.
pub fn dedicated_rules(mode: Mode) -> String {
    let table = tool_table(mode);
    let shell_note = match mode {
        Mode::Hybrid => {
            "Shell commands: use `lean-ctx -c \"<cmd>\"` via your Shell tool. NEVER use `ctx_shell` in Hybrid mode."
        }
        Mode::Mcp => "Shell commands: use `ctx_shell(command)`. NEVER use raw Shell/bash.",
    };

    format!(
        r#"# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v12 -->

CRITICAL: ALWAYS use lean-ctx tools instead of native equivalents. This is NOT optional.

{table}

{shell_note}

## Workflow: Orient → Locate → Read → Edit → Verify → Record
1. Orient: ctx_overview(task) or ctx_compose(task, path)
2. Locate: ctx_search(pattern, path) or ctx_semantic_search(query)
3. Read: ctx_read(path, mode) — omit mode for auto; full only right before edits
4. Edit: ctx_edit(path, old_string, new_string) or native Edit
5. Verify: ctx_read(path, "diff") + ctx_shell("test command")
6. Record: ctx_knowledge(action="remember", content="...")

File editing: use native Edit/StrReplace. Write, Delete, Glob → use normally.
NEVER loop on Edit failures — switch to ctx_edit immediately.

NEVER use native Read/Grep/Shell when ctx_* equivalents are available.
<!-- /lean-ctx -->"#
    )
}

/// Returns a short instructions string for MCP JSON `instructions` field.
pub fn mcp_instructions(mode: Mode) -> &'static str {
    match mode {
        Mode::Hybrid => MCP_INSTRUCTIONS_HYBRID,
        Mode::Mcp => MCP_INSTRUCTIONS_MCP,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Hybrid,
    Mcp,
}

impl Mode {
    pub fn from_hook_mode(hook: &crate::hooks::HookMode) -> Self {
        match hook {
            crate::hooks::HookMode::Hybrid => Mode::Hybrid,
            crate::hooks::HookMode::Mcp => Mode::Mcp,
        }
    }
}

const HYBRID_TABLE: &str = "\
| MUST USE | NEVER USE | Why |
|----------|-----------|-----|
| `ctx_read(path, mode)` | `Read` / `cat` / `head` / `tail` | Cached, 10 read modes, re-reads ~13 tokens |
| `ctx_search(pattern, path)` | `Grep` / `rg` | Compact, token-efficient results |
| `lean-ctx -c \"<cmd>\"` (via Shell) | `ctx_shell` / raw `Shell` | CLI compression, no MCP overhead |
| `lean-ctx ls [path]` (via Shell) | `ctx_tree` / `ls` / `find` | Compact directory maps |";

const MCP_TABLE: &str = "\
| MUST USE | NEVER USE | Why |
|----------|-----------|-----|
| `ctx_read(path, mode)` | `Read` / `cat` / `head` / `tail` | Cached, 10 read modes, re-reads ~13 tokens |
| `ctx_search(pattern, path)` | `Grep` / `rg` | Compact, token-efficient results |
| `ctx_shell(command)` | `Shell` / `bash` / terminal | Pattern compression for git/npm/cargo output |
| `ctx_tree(path, depth)` | `ls` / `find` | Compact directory maps |";

const MCP_INSTRUCTIONS_HYBRID: &str = "\
lean-ctx tools replace Read/Grep/Shell/ls. Workflow: Orient(ctx_overview) → Locate(ctx_search) → Read(ctx_read) → Edit(ctx_edit/native) → Verify(ctx_read diff + lean-ctx -c test) → Record(ctx_knowledge). Edit/Write/Glob: native.";

const MCP_INSTRUCTIONS_MCP: &str = "\
lean-ctx tools replace Read/Grep/Shell/ls. Workflow: Orient(ctx_overview) → Locate(ctx_search) → Read(ctx_read) → Edit(ctx_edit/native) → Verify(ctx_read diff + ctx_shell test) → Record(ctx_knowledge). Edit/Write/Glob: native.";

/// Tool-mapping in bullet format for MCP instructions blocks.
pub fn tool_mapping_bullets(mode: Mode) -> &'static str {
    match mode {
        Mode::Hybrid => HYBRID_BULLETS,
        Mode::Mcp => MCP_BULLETS,
    }
}

// Bullets are deliberately minimal (#579): the MANDATORY header carries the
// imperative once, and the LITM-END preference line repeats it at the end —
// per-bullet "[NEVER ...]" tails were redundant token weight in every session.
const MCP_BULLETS: &str = "\
lean-ctx MCP — MANDATORY tool mapping:\n\
• Read/cat/head/tail -> ctx_read(path, mode)\n\
• Shell/bash -> ctx_shell(command)\n\
• Grep/rg -> ctx_search(pattern, path)\n\
• ls/find -> ctx_tree(path, depth)\n\
• Edit/Write/Delete/Glob -> native (lean-ctx replaces READ only); if Edit fails, switch to ctx_edit(path, old, new) — never loop";

const HYBRID_BULLETS: &str = "\
lean-ctx — MANDATORY tool mapping:\n\
• Read/cat/head/tail -> ctx_read(path, mode)\n\
• Shell commands -> lean-ctx -c \"<cmd>\" (via Shell)  [NEVER ctx_shell]\n\
• Grep/rg -> ctx_search(pattern, path)\n\
• ls/find -> lean-ctx ls [path] (via Shell)\n\
• Edit/Write/Delete/Glob -> native (lean-ctx replaces READ only); if Edit fails, switch to ctx_edit(path, old, new) — never loop";

/// One line on purpose (#579): every word here rides in EVERY session's MCP
/// instructions. Mode details live on disk (LEAN-CTX.md) and in tool schemas.
pub fn ctx_read_modes_block() -> &'static str {
    "ctx_read modes: auto(default)|full|map|signatures|diff|task|reference|aggressive|entropy|lines:N-M. Re-reads ~13 tok; fresh=true forces disk re-read."
}

/// One line on purpose (#579) — background automation needs awareness, not a
/// manual. Long-form documentation lives in LEAN-CTX.md.
pub fn automation_block() -> &'static str {
    "Auto: preload/dedup/compress run in background. ctx_session=memory, ctx_knowledge=facts, ctx_semantic_search=meaning search, ctx_shell raw=true=uncompressed. Details: LEAN-CTX.md"
}

pub fn cep_block() -> &'static str {
    "CEP v1: 1.ACT FIRST 2.DELTA ONLY (Fn refs) 3.STRUCTURED (+/-/~) 4.ONE LINE PER ACTION 5.QUALITY ANCHOR"
}

pub fn litm_end_block(mode: Mode) -> &'static str {
    match mode {
        Mode::Hybrid => {
            "TOOL PREFERENCE (END): ctx_read>Read ctx_search>Grep lean-ctx_-c>Shell lean-ctx_ls>ls | Edit/Write/Glob=native"
        }
        Mode::Mcp => {
            "TOOL PREFERENCE (END): ctx_read>Read ctx_shell>Shell ctx_search>Grep ctx_tree>ls | Edit/Write/Glob=native"
        }
    }
}

pub fn unified_tool_mode_block() -> &'static str {
    "UNIFIED TOOL MODE (active):\n\
     Additional tools are accessed via ctx() meta-tool: ctx(tool=\"<name>\", ...params).\n\
     See the ctx() tool description for available sub-tools."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_table_contains_must() {
        assert!(HYBRID_TABLE.contains("MUST USE"));
        assert!(!HYBRID_TABLE.contains("PREFER"));
    }

    #[test]
    fn mcp_table_contains_must() {
        assert!(MCP_TABLE.contains("MUST USE"));
        assert!(!MCP_TABLE.contains("PREFER"));
    }

    #[test]
    fn hybrid_table_uses_cli() {
        assert!(HYBRID_TABLE.contains("lean-ctx -c"));
        for line in HYBRID_TABLE.lines() {
            assert!(
                !line.starts_with("| `ctx_shell"),
                "Hybrid table must not list ctx_shell in MUST USE column"
            );
        }
    }

    #[test]
    fn mcp_table_uses_ctx_shell() {
        assert!(MCP_TABLE.contains("ctx_shell"));
        assert!(!MCP_TABLE.contains("lean-ctx -c"));
    }

    #[test]
    fn dedicated_rules_have_markers() {
        let rules = dedicated_rules(Mode::Hybrid);
        assert!(rules.contains("lean-ctx-rules-v12"));
        assert!(rules.contains("<!-- /lean-ctx -->"));
    }

    #[test]
    fn dedicated_rules_litm_structure() {
        for mode in [Mode::Hybrid, Mode::Mcp] {
            let rules = dedicated_rules(mode);
            let lines: Vec<&str> = rules.lines().collect();
            let first_5 = lines[..5.min(lines.len())].join("\n");
            assert!(
                first_5.contains("CRITICAL") || first_5.contains("MUST"),
                "LITM: MUST instruction near start for {mode:?}"
            );
            let last_3 = lines[lines.len().saturating_sub(3)..].join("\n");
            assert!(
                last_3.contains("MUST") || last_3.contains("NEVER"),
                "LITM: reinforcement near end for {mode:?}"
            );
        }
    }

    #[test]
    fn no_prefer_in_any_output() {
        for mode in [Mode::Hybrid, Mode::Mcp] {
            let rules = dedicated_rules(mode);
            assert!(
                !rules.contains("PREFER"),
                "canonical rules must use MUST, not PREFER for {mode:?}"
            );
            let instructions = mcp_instructions(mode);
            assert!(
                !instructions.contains("PREFER"),
                "MCP instructions must use MUST, not PREFER for {mode:?}"
            );
        }
    }

    #[test]
    fn hybrid_bullets_use_cli() {
        let bullets = tool_mapping_bullets(Mode::Hybrid);
        for line in bullets.lines() {
            if line.starts_with('•') {
                assert!(
                    !line.starts_with("• Shell/bash -> ctx_shell"),
                    "Hybrid bullets must not map Shell to ctx_shell"
                );
            }
        }
        assert!(bullets.contains("lean-ctx -c"));
    }

    #[test]
    fn mcp_bullets_no_lean_ctx_c() {
        let bullets = tool_mapping_bullets(Mode::Mcp);
        assert!(
            !bullets.contains("lean-ctx -c"),
            "MCP bullets must not reference lean-ctx -c"
        );
        assert!(bullets.contains("ctx_shell"));
    }

    #[test]
    fn shared_sections_not_empty() {
        assert!(!ctx_read_modes_block().is_empty());
        assert!(!automation_block().is_empty());
        assert!(!cep_block().is_empty());
        assert!(!litm_end_block(Mode::Mcp).is_empty());
        assert!(!litm_end_block(Mode::Hybrid).is_empty());
        assert!(!unified_tool_mode_block().is_empty());
    }

    #[test]
    fn bullets_carry_edit_failure_path() {
        // The ctx_edit escape hatch is the one non-obvious compatibility rule;
        // it must survive in the mapping bullets (#579 folded the old
        // compatibility_block into them).
        for mode in [Mode::Hybrid, Mode::Mcp] {
            assert!(
                tool_mapping_bullets(mode).contains("ctx_edit"),
                "edit-failure path missing for {mode:?}"
            );
        }
    }
}
