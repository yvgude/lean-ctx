// SPDX-License-Identifier: Apache-2.0
use super::super::resolve_binary_path;

pub(crate) fn install_jetbrains_hook() {
    // #281: the JetBrains integration is MCP-only (a copy/paste snippet), so an
    // MCP-disabled environment writes nothing here.
    if !super::super::should_register_mcp() {
        return;
    }
    install_jetbrains_solution_rules();
    let binary = resolve_binary_path();
    let home = lean_ctx::core::home::resolve_home_dir().unwrap_or_default();
    let config_path = home.join(".jb-mcp.json");
    let display_path = "~/.jb-mcp.json";

    // JetBrains AI Assistant expects a JSON snippet with "mcpServers".
    // We write it to a file for easy copy/paste into JetBrains settings.
    let entry = serde_json::json!({
        "command": binary,
        "args": [],
        "env": super::super::mcp_server_env_json()
    });

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            eprintln!("JetBrains MCP snippet already written to {display_path}");
            print_jetbrains_manual_step(display_path);
            return;
        }

        if let Ok(mut json) = lean_ctx::core::jsonc::parse_jsonc(&content)
            && let Some(obj) = json.as_object_mut()
        {
            let servers = obj
                .entry("mcpServers")
                .or_insert_with(|| serde_json::json!({}));
            if let Some(servers_obj) = servers.as_object_mut() {
                servers_obj.insert("lean-ctx".to_string(), entry.clone());
            }
            if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                let _ = std::fs::write(&config_path, formatted);
                eprintln!("  \x1b[32m✓\x1b[0m JetBrains MCP snippet written to {display_path}");
                print_jetbrains_manual_step(display_path);
                return;
            }
        }
    }

    let config = serde_json::json!({ "mcpServers": { "lean-ctx": entry } });
    if let Ok(json_str) = serde_json::to_string_pretty(&config) {
        let _ = std::fs::write(&config_path, json_str);
        eprintln!("  \x1b[32m✓\x1b[0m JetBrains MCP snippet written to {display_path}");
        print_jetbrains_manual_step(display_path);
    } else {
        tracing::error!("Failed to configure JetBrains");
    }
}

fn install_jetbrains_solution_rules() {
    let Some(home) = lean_ctx::core::home::resolve_home_dir() else {
        return;
    };
    let rules_path = home.join(".jb-rules/lean-ctx.md");
    let Some(parent) = rules_path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    let block = solution_aware_rules_block(lean_ctx::rules_inject::rules_dedicated_markdown());
    upsert_solution_rules(&rules_path, &block);
}

#[allow(clippy::needless_pass_by_value)]
fn solution_aware_rules_block(base_block: String) -> String {
    let solution_block = lean_ctx::core::rules_canonical::enabled_solution_rules_block();
    let base_block = lean_ctx::marked_block::remove_content(
        &base_block,
        lean_ctx::core::rules_canonical::SOLUTION_BLOCK_START,
        lean_ctx::core::rules_canonical::SOLUTION_BLOCK_END,
    );
    if solution_block.is_empty() {
        return base_block;
    }

    base_block.replacen(
        lean_ctx::core::rules_canonical::END_MARK,
        &format!(
            "{solution_block}\n{}",
            lean_ctx::core::rules_canonical::END_MARK
        ),
        1,
    )
}

fn upsert_solution_rules(path: &std::path::Path, block: &str) {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if existing.trim_end() == block.trim_end() {
        return;
    }

    lean_ctx::marked_block::upsert(
        path,
        lean_ctx::core::rules_canonical::START_MARK,
        lean_ctx::core::rules_canonical::END_MARK,
        block,
        true,
        "JetBrains rules",
    );
}

/// JetBrains AI Assistant does not auto-load `~/.jb-mcp.json`. The snippet must
/// be pasted into the IDE once, so we always state the manual step explicitly
/// to set the right expectation (no silent "configured" that never wires up).
fn print_jetbrains_manual_step(display_path: &str) {
    eprintln!(
        "    \x1b[33mManual step:\x1b[0m JetBrains has no auto-wiring — open \
Settings → Tools → AI Assistant → Model Context Protocol (MCP) and paste the \
`lean-ctx` server from {display_path}."
    );
}
