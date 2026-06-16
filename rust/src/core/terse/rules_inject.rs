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
    let prompt = super::agent_prompts::build_prompt_block(level);
    let prompt_ascii = super::agent_prompts::build_prompt_block_for_client(level, "cursor");
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
    let other_paths: Vec<std::path::PathBuf> = vec![
        cwd.join("AGENTS.md"),
        cwd.join(".claude/rules/lean-ctx.md"),
        cwd.join(".kiro/steering/lean-ctx.md"),
        home.join(".config/crush/rules/lean-ctx.md"),
        home.join(".qoder/rules/lean-ctx.md"),
    ];

    if global_cursor_mdc.exists()
        && let Ok(content) = std::fs::read_to_string(&global_cursor_mdc)
    {
        let new_content = upsert_block(&content, &block(&prompt_ascii));
        if new_content != content {
            let _ = std::fs::write(&global_cursor_mdc, &new_content);
            updated += 1;
        }
    }

    // `.cursorrules` is Cursor-only and Cursor also auto-loads the global mdc.
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
