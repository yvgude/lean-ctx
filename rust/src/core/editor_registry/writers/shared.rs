// Auto-split from the former monolithic writers.rs. Grouped by operation
// (install/uninstall) + shared helpers; behavior is unchanged.

use serde_json::Value;

use super::{WriteAction, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

pub(super) fn toml_quote(value: &str) -> String {
    if value.contains('\\') {
        format!("'{value}'")
    } else {
        format!("\"{value}\"")
    }
}

pub fn auto_approve_tools() -> Vec<&'static str> {
    vec![
        "ctx_read",
        "ctx_shell",
        "shell",
        "ctx_search",
        "ctx_glob",
        "ctx_tree",
        "ctx_compose",
        "ctx_call",
        "ctx_patch",
        "ctx_optimize",
        "ctx_overview",
        "ctx_preload",
        "ctx_compress",
        "ctx_compare",
        "ctx_metrics",
        "ctx_session",
        "ctx_knowledge",
        "ctx_analyze",
        "ctx_benchmark",
        "ctx_cache",
        "ctx_discover",
        "ctx_delta",
        "ctx_edit",
        "ctx_dedup",
        "ctx_fill",
        "ctx_intent",
        "ctx_response",
        "ctx_context",
        "ctx_graph",
        "ctx_outline",
        "ctx_callgraph",
        "ctx_refactor",
        "ctx_routes",
        "ctx_cost",
        "ctx_heatmap",
        "ctx_gain",
        "ctx_expand",
        "ctx_impact",
        "ctx_architecture",
        "ctx_review",
        "ctx_pack",
        "ctx_index",
        "ctx_artifacts",
        "ctx_smells",
        "ctx_quality",
        "ctx_proof",
        "ctx_verify",
        "ctx_execute",
        "ctx_feedback",
        "ctx_control",
        "ctx_plan",
        "ctx_compile",
        "ctx_discover_tools",
        "ctx_provider",
        "ctx_radar",
        "ctx_retrieve",
        "ctx_compress_memory",
        "ctx_load_tools",
        "ctx",
    ]
}

pub(super) fn lean_ctx_server_entry(binary: &str, include_auto_approve: bool) -> Value {
    // No `env` block: lean-ctx auto-detects its per-category dirs (config/data/
    // state/cache) at runtime. Pinning `LEAN_CTX_DATA_DIR` here would set that var
    // in the server's environment, forcing single-dir mode and collapsing
    // config/state/cache onto the data dir — defeating the XDG split (GH #408).
    let mut entry = serde_json::json!({
        "command": binary
    });
    if include_auto_approve {
        entry["autoApprove"] = serde_json::json!(auto_approve_tools());
    }
    entry
}

pub(super) fn lean_ctx_server_entry_with_instructions(
    binary: &str,
    include_auto_approve: bool,
    agent_key: &str,
) -> Value {
    let mut entry = lean_ctx_server_entry(binary, include_auto_approve);
    let shadow = crate::core::config::Config::load().shadow_mode;
    let level =
        crate::core::config::CompressionLevel::effective(&crate::core::config::Config::load());
    // Hook-covered hosts (GL #1153): the static instructions snapshot must
    // agree with the injected rule file — repeating "NEVER use native tools"
    // to a Cursor whose hooks compress them re-creates the dissonance the
    // HookCovered profile removes.
    let hook_covered = crate::core::home::resolve_home_dir()
        .is_some_and(|home| crate::core::rules_channel::client_hook_covered(agent_key, &home));
    let cfg = crate::core::config::Config::load();
    let profile = crate::core::tool_profiles::ToolProfile::from_config(&cfg);
    let instructions = if hook_covered {
        crate::core::rules_canonical::render_hook_covered_bare(shadow, level, &profile)
    } else {
        crate::core::rules_canonical::render(
            shadow,
            crate::core::rules_canonical::Wrapper::Bare,
            level,
            &profile,
        )
    };

    let constraints = crate::core::client_constraints::by_client_id(agent_key);
    if let Some(max_chars) = constraints.and_then(|c| c.mcp_instructions_max_chars) {
        entry["instructions"] = serde_json::json!(truncate_instructions(&instructions, max_chars));
    } else {
        entry["instructions"] = serde_json::json!(&instructions);
    }
    entry
}

/// Cut `instructions` to a client's documented `max_chars`, never inside a
/// UTF-8 sequence. The rendered rules contain multi-byte punctuation
/// (em-dashes, arrows), and a raw `&s[..max]` byte slice panics when the limit
/// lands mid-char — live crash during `setup` level 3 (GH #680).
fn truncate_instructions(instructions: &str, max_chars: usize) -> &str {
    if instructions.len() <= max_chars {
        return instructions;
    }
    &instructions[..instructions.floor_char_boundary(max_chars)]
}

pub(super) fn supports_auto_approve(target: &EditorTarget) -> bool {
    crate::core::client_constraints::by_editor_name(target.name)
        .is_some_and(|c| c.supports_auto_approve)
}

/// Fixed UUIDv4-shaped id reserved for lean-ctx in Augment's VS Code MCP list.
/// The first segment hex-encodes "lean" (6c 65 61 6e) and the last segment
/// hex-encodes "leanct" (6c 65 61 6e 63 74) — a 6-byte ASCII tag that fits
/// exactly in the 12-hex-char node field. The middle bytes preserve the
/// version-4 / variant-RFC-4122 nibbles so the value parses as a valid UUID.
/// Only stability matters — the writer uses this id to locate and update its
/// own entry idempotently without colliding with user-added servers.
pub(super) const LEAN_CTX_AUGMENT_VSCODE_ID: &str = "6c65616e-c747-4000-8000-6c65616e6374";

pub(super) fn backup_invalid_file(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if !path.exists() {
        return Ok(path.to_path_buf());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "invalid path (no parent directory)".to_string())?;
    let filename = path
        .file_name()
        .ok_or_else(|| "invalid path (no filename)".to_string())?
        .to_string_lossy();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let bak = parent.join(format!("{filename}.lean-ctx.invalid.{pid}.{nanos}.bak"));
    std::fs::copy(path, &bak).map_err(|e| e.to_string())?;
    Ok(bak)
}

/// Safe handler for invalid JSON config files. NEVER silently overwrites.
/// Strategy:
/// 1. If lean-ctx is already present in text → skip (no-op)
/// 2. Try text-based injection into the container key
/// 3. If injection fails → warn user with clear instructions, do NOT modify file
pub(super) fn handle_invalid_json_write(
    path: &std::path::Path,
    content: &str,
    container_key: &str,
    entry_key: &str,
    value: &serde_json::Value,
    allow_inject: bool,
) -> Result<WriteResult, String> {
    if content.contains(&format!("\"{entry_key}\"")) {
        eprintln!(
            "\x1b[33m⚠\x1b[0m  {} has JSON syntax errors but already contains \"{entry_key}\".",
            path.display()
        );
        eprintln!("   Skipping — your config is untouched.");
        return Ok(WriteResult {
            action: WriteAction::Already,
            note: Some(format!("invalid JSON, {entry_key} already present")),
        });
    }

    if !allow_inject {
        return Err(format!(
            "{} contains invalid JSON. Fix the syntax and re-run lean-ctx setup.\n  Path: {}",
            path.display(),
            path.display()
        ));
    }

    // Try text-based injection
    if let Some(patched) = try_text_inject_mcp_entry(content, container_key, entry_key, value) {
        let bak = backup_invalid_file(path)?;
        crate::config_io::write_atomic_with_backup(path, &patched)?;
        eprintln!(
            "\x1b[32m✓\x1b[0m  Added {entry_key} to {} (text-based; file has syntax errors).",
            path.display()
        );
        eprintln!("   \x1b[33mNote:\x1b[0m Your config has JSON syntax errors — please fix them.");
        eprintln!("   Backup: {}", bak.display());
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: Some(format!(
                "text-injected into invalid JSON (backup: {})",
                bak.display()
            )),
        });
    }

    // Cannot safely modify — inform user
    eprintln!(
        "\x1b[33m⚠\x1b[0m  {} contains invalid JSON that lean-ctx cannot safely modify.",
        path.display()
    );
    eprintln!("   \x1b[1mYour config was NOT changed.\x1b[0m");
    eprintln!("   To fix:");
    eprintln!(
        "     1. Open {} and correct the JSON syntax errors",
        path.display()
    );
    eprintln!("     2. Re-run: lean-ctx setup");
    eprintln!("   (Common issue: trailing commas, missing quotes, unmatched braces)");
    Ok(WriteResult {
        action: WriteAction::Already,
        note: Some(format!(
            "invalid JSON — user must fix manually: {}",
            path.display()
        )),
    })
}

/// Attempt to inject an MCP entry into a JSON file using text manipulation.
/// Preserves the original file content even if it has syntax errors.
/// Returns None if text structure doesn't allow safe injection.
pub(super) fn try_text_inject_mcp_entry(
    content: &str,
    container_key: &str,
    entry_key: &str,
    value: &serde_json::Value,
) -> Option<String> {
    let entry = serde_json::to_string_pretty(value).ok()?;
    let indented_entry = entry
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                format!("    \"{entry_key}\": {line}")
            } else {
                format!("    {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Strategy 1: find the target container key and inject after its opening brace.
    // Prioritize the exact container_key, then fall back to common alternatives.
    let quoted_container = format!("\"{container_key}\"");
    let search_keys: Vec<&str> = std::iter::once(quoted_container.as_str())
        .chain(
            [
                "\"mcp\"",
                "\"mcpServers\"",
                "\"servers\"",
                "\"context_servers\"",
            ]
            .iter()
            .filter(|k| **k != quoted_container.as_str())
            .copied(),
        )
        .collect();

    for container in &search_keys {
        if let Some(pos) = content.find(container) {
            let after = &content[pos..];
            if let Some(brace_offset) = after.find('{') {
                let insert_pos = pos + brace_offset + 1;
                let before = &content[..insert_pos];
                let rest = &content[insert_pos..];
                let needs_comma = !rest.trim_start().starts_with('}');
                let injection = if needs_comma {
                    format!("\n{indented_entry},")
                } else {
                    format!("\n{indented_entry}\n  ")
                };
                return Some(format!("{before}{injection}{rest}"));
            }
        }
    }

    // Strategy 2: inject a new container block before the closing root brace
    if let Some(last_brace) = content.rfind('}') {
        let before = &content[..last_brace];
        let after = &content[last_brace..];
        let needs_comma = before.trim_end().ends_with('}')
            || before.trim_end().ends_with('"')
            || before.trim_end().ends_with(']');
        let comma = if needs_comma { "," } else { "" };
        let block = format!("{comma}\n  \"{container_key}\": {{\n{indented_entry}\n  }}\n");
        return Some(format!("{before}{block}{after}"));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::truncate_instructions;

    // GH #680 live repro: `setup` level 3 panicked with "end byte index 2048 is
    // not a char boundary; it is inside '—'" — the Claude Code 2048-char cap
    // landed mid em-dash. The cut must back up to the previous char boundary.
    #[test]
    fn truncation_never_splits_a_multibyte_char() {
        // 2047 ASCII bytes, then '—' (3 bytes, occupying 2047..2050): the exact
        // shape from the crash log.
        let mut s = "a".repeat(2047);
        s.push('—');
        s.push_str(" trailing rules text");

        let cut = truncate_instructions(&s, 2048);
        assert_eq!(cut.len(), 2047, "must back up to the boundary before '—'");
        assert!(cut.chars().all(|c| c == 'a'));
    }

    #[test]
    fn truncation_is_noop_when_under_limit() {
        assert_eq!(truncate_instructions("short — text", 2048), "short — text");
    }

    #[test]
    fn truncation_on_boundary_keeps_full_prefix() {
        let s = format!("{}—rest", "a".repeat(100));
        // Limit right at the end of the em-dash (100 + 3 bytes).
        assert_eq!(
            truncate_instructions(&s, 103),
            format!("{}—", "a".repeat(100))
        );
    }
}
