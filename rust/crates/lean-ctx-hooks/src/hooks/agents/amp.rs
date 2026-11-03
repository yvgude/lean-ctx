// SPDX-License-Identifier: Apache-2.0
use super::super::{install_named_json_server, resolve_binary_path};

pub(crate) fn install_amp_hook() {
    let binary = resolve_binary_path();
    let home = lean_ctx::core::home::resolve_home_dir().unwrap_or_default();
    let config_path = home.join(".config/amp/settings.json");
    let display_path = "~/.config/amp/settings.json";

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let entry = serde_json::json!({
        "command": binary,
        "env": super::super::mcp_server_env_json()
    });
    install_named_json_server("Amp", display_path, &config_path, "amp.mcpServers", entry);
}
