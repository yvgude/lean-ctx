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
    dedicated_rules_with_shadow(mode, false)
}

/// Returns the full rules block, shadow-mode aware.
/// When `shadow` is true, native tools are transparently intercepted by the
/// plugin so the mapping table is noise — emit workflow principles instead.
pub fn dedicated_rules_with_shadow(mode: Mode, shadow: bool) -> String {
    if shadow {
        return format!(
            "lean-ctx — Principles:\n\
             - Surgical precision: read exactly what's needed with the right tool for each job\n\
             - Parallelism: fire independent calls in the same turn, never sequence what can be batched\n\
             - Token discipline: avoid duplication, reuse cached content, compress aggressively\n\
             - Understanding first: use ctx_compose before editing, verify before shipping\n\n\
             {}: full=verbatim signatures=API map=structure auto=smart diff=git-delta lines:N-M=window",
            ctx_read_modes_header(mode)
        );
    }

    let table = tool_table(mode);
    let shell_note = match mode {
        Mode::Hybrid => {
            "Shell commands: use `lean-ctx -c \"<cmd>\"` via your Shell tool. NEVER use `ctx_shell` in Hybrid mode."
        }
        Mode::Mcp => "Shell commands: use `ctx_shell(command)`. NEVER use raw Shell/bash.",
    };
    let intent = intent_playbook();
    let anti = anti_patterns();
    let parallel = parallel_tool_guidance();
    let read_modes = ctx_read_modes_block();

    format!(
        "# lean-ctx \u{2014} Context Engineering Layer\n<!-- lean-ctx-rules-v12 -->\n\nCRITICAL: ALWAYS use lean-ctx tools instead of native equivalents. This is NOT optional.\n\n{table}\n\n{shell_note}\n\n{intent}\n\n{anti}\n\n{parallel}\n\n{read_modes}\n\nNEVER use native Read/Grep/Shell/Glob when ctx_* equivalents are available.\n<!-- /lean-ctx -->"
    )
}

/// Returns a shorter rules block for SHARED config files (appended to
/// the user's existing AGENTS.md / GEMINI.md / instructions.md).
pub fn shared_rules(mode: Mode) -> String {
    shared_rules_with_shadow(mode, false)
}

/// Returns a shorter rules block for SHARED config files, shadow-mode aware.
/// When `shadow` is true, native tools are transparently intercepted by the
/// plugin so the mapping table is noise — emit workflow principles instead.
pub fn shared_rules_with_shadow(mode: Mode, shadow: bool) -> String {
    if shadow {
        return format!(
            "lean-ctx — Principles:\n\
             - Surgical precision: read exactly what's needed with the right tool for each job\n\
             - Parallelism: fire independent calls in the same turn, never sequence what can be batched\n\
             - Token discipline: avoid duplication, reuse cached content, compress aggressively\n\
             - Understanding first: use ctx_compose before editing, verify before shipping\n\n\
             {}: full=verbatim signatures=API map=structure auto=smart diff=git-delta lines:N-M=window",
            ctx_read_modes_header(mode)
        );
    }

    let bullets = tool_mapping_bullets(mode);
    let shell_note = match mode {
        Mode::Hybrid => {
            "Shell commands: use `lean-ctx -c \"<cmd>\"` via your Shell tool. NEVER use `ctx_shell` in Hybrid mode."
        }
        Mode::Mcp => "Shell commands: use `ctx_shell(command)`. NEVER use raw Shell/bash.",
    };
    let read_modes = ctx_read_modes_block();

    format!(
        r#"# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v12 -->

## Tool Mapping (MANDATORY — use instead of native equivalents)
{bullets}
{shell_note}

{read_modes}

NEVER use native Read/Grep/Shell/Glob when ctx_* equivalents are available.
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
| `lean-ctx ls [path]` (via Shell) | `ctx_tree` / `ls` / `find` | Compact directory maps |
| `ctx_glob(pattern)` | `Glob` / `find` | .gitignore-aware, multi-root |";

const MCP_TABLE: &str = "\
| MUST USE | NEVER USE | Why |
|----------|-----------|-----|
| `ctx_read(path, mode)` | `Read` / `cat` / `head` / `tail` | Cached, 10 read modes, re-reads ~13 tokens |
| `ctx_search(pattern, path)` | `Grep` / `rg` | Compact, token-efficient results |
| `ctx_glob(pattern)` | `Glob` / `find` | .gitignore-aware, multi-root |
| `ctx_shell(command)` | `Shell` / `bash` / terminal | Pattern compression for git/npm/cargo output |
| `ctx_tree(path, depth)` | `ls` / `find` | Compact directory maps |";

const MCP_INSTRUCTIONS_HYBRID: &str = "\
Workflow: ctx_compose FIRST (understand) -> ctx_read(mode) -> Edit. ctx_glob & ctx_search replace Glob/Grep. Fire ctx_* calls in parallel.";

const MCP_INSTRUCTIONS_MCP: &str = "\
Workflow: ctx_compose FIRST (understand) -> ctx_read(mode) -> Edit. ctx_glob & ctx_search replace Glob/Grep. Fire ctx_* calls in parallel.";

/// Tool-mapping in bullet format for MCP instructions blocks.
pub fn tool_mapping_bullets(mode: Mode) -> &'static str {
    match mode {
        Mode::Hybrid => HYBRID_BULLETS,
        Mode::Mcp => MCP_BULLETS,
    }
}

// Bullets are deliberately minimal (#579): the MANDATORY header carries the
// imperative once, and the LITM-END preference line repeats it at the end.
const MCP_BULLETS: &str = "\
lean-ctx MCP — MANDATORY tool mapping:\n\
• Read/cat/head/tail -> ctx_read(path, mode required)\n\
• Glob/find -> ctx_glob(pattern)\n\
• Shell/bash -> ctx_shell(command)\n\
• Grep/rg -> ctx_search(pattern, path)\n\
• ls/find -> ctx_tree(path, depth)";

const HYBRID_BULLETS: &str = "\
lean-ctx — MANDATORY tool mapping:\n\
• Read/cat/head/tail -> ctx_read(path, mode required)\n\
• Glob/find -> ctx_glob(pattern)\n\
• Shell commands -> lean-ctx -c \"<cmd>\" (via Shell)  [NEVER ctx_shell]\n\
• Grep/rg -> ctx_search(pattern, path)\n\
• ls/find -> lean-ctx ls [path] (via Shell)";

/// Intent-to-tool playbook. Maps common agent questions to the right tool.
/// Compact enough to fit in the 800-token MCP instructions budget.
pub fn intent_playbook() -> &'static str {
    "Tool selection by intent:\n\
     • Understand code / find answers / before editing -> ctx_compose (PRIMARY — call FIRST)\n\
     • Read a file -> ctx_read(path, mode=full|signatures|map|auto)\n\
     • Find a symbol by name (exact) -> ctx_symbol\n\
     • Search code by pattern (fuzzy) -> ctx_search\n\
     • Search by meaning (concepts, not keywords) -> ctx_semantic_search\n\
     • Find files by pattern (glob) -> ctx_glob\n\
     • Project structure -> ctx_tree\n\
     • Who calls this / call graph -> ctx_callgraph\n\
     • Session state / memory -> ctx_session / ctx_knowledge"
}

/// Anti-patterns that waste tokens and round-trips.
pub fn anti_patterns() -> &'static str {
    "Anti-patterns — do NOT:\n\
     • Chain ctx_search -> ctx_read -> ctx_symbol — one ctx_compose replaces all three\n\
     • Read a file after ctx_compose returned its source — it IS the source\n\
     • Grep for symbol definitions — ctx_symbol or ctx_compose are faster + more precise\n\
     • Use ctx_read(mode=full) for orientation — use mode=auto or mode=signatures\n\
     • Re-verify tool output with grep — trust the index"
}

/// Encourage parallel tool calls to reduce round-trips.
pub fn parallel_tool_guidance() -> &'static str {
    "PARALLEL tool calls: fire independent calls in the SAME turn — don't sequence them.\n\
     One turn with 3 parallel ctx_read calls completes faster than 3 sequential turns.\n\
     ctx_compose bundles multiple lookups into one call; for anything it doesn't\n\
     cover, batch independent reads/searches together."
}

/// Label for the ctx_read modes reference, mode-appropriate.
pub fn ctx_read_modes_header(mode: Mode) -> &'static str {
    match mode {
        Mode::Hybrid => "ctx_read modes",
        Mode::Mcp => "ctx_read modes",
    }
}

/// One line on purpose (#579): every word here rides in EVERY session's MCP
/// instructions. Mode details live on disk (LEAN-CTX.md) and in tool schemas.
pub fn ctx_read_modes_block() -> &'static str {
    "ctx_read modes (required): full=verbatim(edit-ready) signatures=API map=structure auto=smart diff=git-delta lines:N-M=window. fresh=true forces disk re-read."
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
            "TOOL PREFERENCE (END): ctx_compose>chain ctx_read>Read ctx_search>Grep ctx_glob>Glob lean-ctx_-c>Shell ctx_tree>ls | Edit/Write/Delete=native"
        }
        Mode::Mcp => {
            "TOOL PREFERENCE (END): ctx_compose>chain ctx_read>Read ctx_shell>Shell ctx_search>Grep ctx_glob>Glob ctx_tree>ls | Edit/Write/Delete=native"
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
    fn shadow_dedicated_has_principles_no_mapping() {
        for mode in [Mode::Hybrid, Mode::Mcp] {
            let rules = dedicated_rules_with_shadow(mode, true);
            assert!(rules.contains("Surgical precision"));
            assert!(!rules.contains("MUST USE"));
            assert!(!rules.contains("NEVER use native"));
            assert!(!rules.contains("<!--"));
        }
    }

    #[test]
    fn shadow_shared_has_principles_no_mapping() {
        for mode in [Mode::Hybrid, Mode::Mcp] {
            let rules = shared_rules_with_shadow(mode, true);
            assert!(rules.contains("Surgical precision"));
            assert!(!rules.contains("Tool Mapping"));
            assert!(!rules.contains("MUST USE"));
            assert!(!rules.contains("<!--"));
        }
    }

    #[test]
    fn non_shadow_unchanged() {
        // Non-shadow dedicated must still contain the mapping table.
        for mode in [Mode::Hybrid, Mode::Mcp] {
            let rules = dedicated_rules(mode);
            assert!(rules.contains("MUST USE"), "non-shadow must have mapping");
            assert!(rules.contains("NEVER use native"), "non-shadow must have native admonition");
        }
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
    fn bullets_no_ctx_edit_fallback() {
        // ctx_edit was moved to power-only; native Edit is preferred.
        // Bullets must NOT mention ctx_edit as fallback.
        for mode in [Mode::Hybrid, Mode::Mcp] {
            assert!(
                !tool_mapping_bullets(mode).contains("ctx_edit"),
                "ctx_edit must not appear in bullets for {mode:?} (native Edit preferred)"
            );
        }
    }

    #[test]
    fn intent_playbook_is_not_empty() {
        assert!(!intent_playbook().is_empty());
        assert!(intent_playbook().contains("ctx_compose"));
        assert!(intent_playbook().contains("PRIMARY"));
    }

    #[test]
    fn anti_patterns_is_not_empty() {
        assert!(!anti_patterns().is_empty());
        assert!(anti_patterns().contains("do NOT"));
    }

    #[test]
    fn parallel_tool_guidance_is_not_empty() {
        assert!(!parallel_tool_guidance().is_empty());
        assert!(parallel_tool_guidance().contains("PARALLEL"));
    }
}
