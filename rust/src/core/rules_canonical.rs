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
        Mode::Hybrid => "Shell commands: use `lean-ctx -c \"<cmd>\"` via your Shell tool. NEVER use `ctx_shell` in Hybrid mode.",
        Mode::Mcp => "Shell commands: use `ctx_shell(command)`. NEVER use raw Shell/bash.",
    };

    format!(
        r"# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v9 -->

CRITICAL: ALWAYS use lean-ctx tools instead of native equivalents. This is NOT optional.

{table}

{shell_note}

File editing: use native Edit/StrReplace. Write, Delete, Glob → use normally.
NEVER loop on Edit failures — switch to ctx_edit immediately.

Fallback only if a lean-ctx tool is unavailable: use native equivalents.
REMINDER: You MUST use lean-ctx tools. NEVER use native Read, Grep, or Shell directly.
<!-- /lean-ctx -->"
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
lean-ctx tools replace Read/Grep/Shell/ls. See tool descriptions for details. Edit/Write/Glob: native.";

const MCP_INSTRUCTIONS_MCP: &str = "\
lean-ctx tools replace Read/Grep/Shell/ls. See tool descriptions for details. Edit/Write/Glob: native.";

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
        assert!(rules.contains("lean-ctx-rules-v9"));
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
}
