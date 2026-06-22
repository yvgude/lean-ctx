//! Injects compression prompts into agent rules files for all integration modes.
//!
//! Called from:
//! - `lean-ctx compression <level>` (CLI command)
//! - `lean-ctx setup` (initial setup)
//! - MCP server startup (ensures consistency after manual config edits)

use crate::core::config::CompressionLevel;

const COMPRESSION_BLOCK_START: &str = "<!-- lean-ctx-compression -->";
const COMPRESSION_BLOCK_END: &str = "<!-- /lean-ctx-compression -->";

/// Updates all detected agent rules files with the compression prompt for `level`.
/// Idempotent — safe to call repeatedly. Returns the number of files updated.
pub fn inject(level: &CompressionLevel) -> usize {
    let prompt = crate::core::rules_canonical::compression_text(level);
    let prompt_ascii = if prompt.is_empty() {
        String::new()
    } else {
        crate::core::output_sanitizer::ascii_safe_symbols(prompt)
    };
    let block = |p: &str| {
        if p.is_empty() {
            String::new()
        } else {
            format!("{COMPRESSION_BLOCK_START}\n{p}\n{COMPRESSION_BLOCK_END}")
        }
    };

    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut updated = 0;

    let global_cursor_mdc = home.join(".cursor/rules/lean-ctx.mdc");
    let cursorrules = cwd.join(".cursorrules");
    let project_agents_md = cwd.join("AGENTS.md");
    let codex_agents_global = crate::core::home::resolve_codex_dir()
        .unwrap_or_else(|| home.join(".codex"))
        .join("AGENTS.md");
    // Per-client dedicated carriers — each agent's own canonical rule file.
    let other_paths: Vec<std::path::PathBuf> = vec![
        cwd.join(".claude/rules/lean-ctx.md"),
        cwd.join(".kiro/steering/lean-ctx.md"),
        home.join(".config/crush/rules/lean-ctx.md"),
        home.join(".qoder/rules/lean-ctx.md"),
    ];

    // 1. Cursor's canonical global mdc — the carrier for Cursor.
    if global_cursor_mdc.exists()
        && let Ok(content) = std::fs::read_to_string(&global_cursor_mdc)
    {
        let new_content = upsert_block(&content, &block(&prompt_ascii));
        if new_content != content {
            let _ = std::fs::write(&global_cursor_mdc, &new_content);
            updated += 1;
        }
    }

    // 2. Codex's canonical global `~/.codex/AGENTS.md` — the carrier for Codex,
    //    so the *shared* project AGENTS.md no longer has to hold the block on
    //    Codex's behalf (#684). Only touched when Codex already maintains it.
    if codex_agents_global.exists()
        && let Ok(content) = std::fs::read_to_string(&codex_agents_global)
    {
        let new_content = upsert_block(&content, &block(&prompt));
        if new_content != content {
            let _ = std::fs::write(&codex_agents_global, &new_content);
            updated += 1;
        }
    }

    // 3. `.cursorrules` is Cursor-only and Cursor also auto-loads the global mdc.
    // When the mdc carries the block, a second copy here is pure duplication
    // (#578): remove an existing block instead of refreshing it, and never
    // append a new one. Without the mdc, `.cursorrules` stays the carrier.
    if cursorrules.exists()
        && let Ok(content) = std::fs::read_to_string(&cursorrules)
    {
        let desired = if global_cursor_mdc.exists() {
            remove_block(&content)
        } else {
            upsert_block(&content, &block(&prompt_ascii))
        };
        if desired != content {
            let _ = std::fs::write(&cursorrules, &desired);
            updated += 1;
        }
    }

    // 4. Shared project AGENTS.md (#684): Cursor, Codex and other agents all
    // auto-load it, so a compression block here duplicates each client's own
    // canonical carrier. Thin it to the `<!-- lean-ctx -->` pointer once EVERY
    // reader present on this machine is covered elsewhere; otherwise AGENTS.md
    // stays the carrier (conservative — no client may silently lose guidance).
    if project_agents_md.exists()
        && let Ok(content) = std::fs::read_to_string(&project_agents_md)
    {
        let desired = if crate::core::rules_channel::agents_md_can_thin(&home) {
            remove_block(&content)
        } else {
            upsert_block(&content, &block(&prompt))
        };
        if desired != content {
            let _ = std::fs::write(&project_agents_md, &desired);
            updated += 1;
        }
    }

    // 5. Remaining per-client dedicated carriers.
    for path in other_paths {
        if path.exists()
            && let Ok(content) = std::fs::read_to_string(&path)
        {
            let new_content = upsert_block(&content, &block(&prompt));
            if new_content != content {
                let _ = std::fs::write(&path, &new_content);
                updated += 1;
            }
        }
    }

    updated
}

fn upsert_block(content: &str, block: &str) -> String {
    if content.contains(COMPRESSION_BLOCK_START) {
        crate::marked_block::replace_marked_block(
            content,
            COMPRESSION_BLOCK_START,
            COMPRESSION_BLOCK_END,
            block,
        )
    } else if block.is_empty() {
        content.to_string()
    } else {
        let mut out = content.trim_end().to_string();
        out.push_str("\n\n");
        out.push_str(block);
        out.push('\n');
        out
    }
}

/// Strips an existing compression block (used when another auto-loaded file
/// already carries it for the same client, #578).
fn remove_block(content: &str) -> String {
    if !content.contains(COMPRESSION_BLOCK_START) {
        return content.to_string();
    }
    let stripped = crate::marked_block::remove_content(
        content,
        COMPRESSION_BLOCK_START,
        COMPRESSION_BLOCK_END,
    );
    let trimmed = stripped.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMP: &str =
        "<!-- lean-ctx-compression -->\nOUTPUT STYLE: dense\n<!-- /lean-ctx-compression -->";

    #[test]
    fn upsert_appends_block_to_plain_file() {
        let out = upsert_block("# Agent Instructions\n", COMP);
        assert!(out.contains("# Agent Instructions"));
        assert!(out.contains(COMPRESSION_BLOCK_START));
        assert!(out.contains("OUTPUT STYLE: dense"));
    }

    #[test]
    fn upsert_replaces_existing_block_in_place() {
        let existing = format!(
            "# Agent Instructions\n\n{COMPRESSION_BLOCK_START}\nOLD STYLE\n{COMPRESSION_BLOCK_END}\n"
        );
        let out = upsert_block(&existing, COMP);
        assert!(out.contains("OUTPUT STYLE: dense"));
        assert!(!out.contains("OLD STYLE"));
        // Exactly one block — no accidental duplication.
        assert_eq!(out.matches(COMPRESSION_BLOCK_START).count(), 1);
    }

    #[test]
    fn remove_block_thins_agents_md_to_pointer_only() {
        // A typical project AGENTS.md: heading + lean-ctx pointer + compression.
        let agents = format!(
            "# Agent Instructions\n\n\
             <!-- lean-ctx -->\n## lean-ctx\nFull rules: LEAN-CTX.md\n<!-- /lean-ctx -->\n\n{COMP}\n"
        );
        let thinned = remove_block(&agents);
        // Compression payload gone …
        assert!(!thinned.contains(COMPRESSION_BLOCK_START));
        assert!(!thinned.contains("OUTPUT STYLE"));
        // … but the pointer block and user heading survive.
        assert!(thinned.contains("<!-- lean-ctx -->"));
        assert!(thinned.contains("Full rules: LEAN-CTX.md"));
        assert!(thinned.contains("# Agent Instructions"));
        // Result is a pointer-only file per the cross-channel policy.
        assert!(crate::core::rules_channel::is_pointer_only(&thinned));
    }

    #[test]
    fn remove_block_is_noop_without_compression() {
        let pointer = "# Agent Instructions\n\n<!-- lean-ctx -->\npointer\n<!-- /lean-ctx -->\n";
        assert_eq!(remove_block(pointer), pointer);
    }
}
