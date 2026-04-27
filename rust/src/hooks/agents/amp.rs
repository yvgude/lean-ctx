use super::super::{install_named_json_server, resolve_binary_path};

pub(crate) fn install_amp_hook() {
    let binary = resolve_binary_path();
    let home = dirs::home_dir().unwrap_or_default();
    let config_path = home.join(".config/amp/settings.json");
    let display_path = "~/.config/amp/settings.json";

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let entry = serde_json::json!({
        "command": binary,
        "env": { "LEAN_CTX_DATA_DIR": data_dir }
    });
    install_named_json_server("Amp", display_path, &config_path, "amp.mcpServers", entry);
}
