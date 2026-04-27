use super::super::{mcp_server_quiet_mode, resolve_binary_path, write_file};
use super::shared::install_standard_hook_scripts;

pub(crate) fn install_gemini_hook() {
    let Some(home) = dirs::home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_gemini_hook_scripts(&home);
    install_gemini_hook_config(&home);
}

pub(crate) fn install_gemini_hook_scripts(home: &std::path::Path) {
    let hooks_dir = home.join(".gemini").join("hooks");
    install_standard_hook_scripts(
        &hooks_dir,
        "lean-ctx-rewrite-gemini.sh",
        "lean-ctx-redirect-gemini.sh",
    );
}

pub(crate) fn install_gemini_hook_config(home: &std::path::Path) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let settings_path = home.join(".gemini").join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    let has_new_format = settings_content.contains("hook rewrite")
        && settings_content.contains("hook redirect")
        && settings_content.contains("\"type\"")
        && settings_content.contains("\"matcher\"");
    let has_old_hooks = settings_content.contains("lean-ctx-rewrite")
        || settings_content.contains("lean-ctx-redirect")
        || (settings_content.contains("hook rewrite") && !settings_content.contains("\"matcher\""));

    if has_new_format && !has_old_hooks {
        return;
    }

    let hook_config = serde_json::json!({
        "hooks": {
            "BeforeTool": [
                {
                    "matcher": "shell|execute_command|run_shell_command",
                    "hooks": [{
                        "type": "command",
                        "command": rewrite_cmd
                    }]
                },
                {
                    "matcher": "read_file|read_many_files|grep|search|list_dir",
                    "hooks": [{
                        "type": "command",
                        "command": redirect_cmd
                    }]
                }
            ]
        }
    });

    if settings_content.is_empty() {
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_config).unwrap_or_default(),
        );
    } else if let Ok(mut existing) = crate::core::jsonc::parse_jsonc(&settings_content) {
        if let Some(obj) = existing.as_object_mut() {
            obj.insert("hooks".to_string(), hook_config["hooks"].clone());
            write_file(
                &settings_path,
                &serde_json::to_string_pretty(&existing).unwrap_or_default(),
            );
        }
    }
    if !mcp_server_quiet_mode() {
        println!(
            "Installed Gemini CLI hooks at {}",
            settings_path.parent().unwrap_or(&settings_path).display()
        );
    }
}
