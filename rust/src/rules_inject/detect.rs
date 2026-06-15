//! Tool detection: is the agent actually installed on this machine?

use std::path::PathBuf;

use super::RulesTarget;

pub(super) fn is_tool_detected(target: &RulesTarget, home: &std::path::Path) -> bool {
    match target.name {
        "Claude Code" => {
            if command_exists("claude") {
                return true;
            }
            let state_dir = crate::core::editor_registry::claude_state_dir(home);
            crate::core::editor_registry::claude_mcp_json_path(home).exists() || state_dir.exists()
        }
        "CodeBuddy" => {
            if command_exists("codebuddy") {
                return true;
            }
            let state_dir = crate::core::editor_registry::codebuddy_state_dir(home);
            crate::core::editor_registry::codebuddy_mcp_json_path(home).exists() || state_dir.exists()
        }
        "Codex CLI" => {
            let codex_dir =
                crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
            codex_dir.exists() || command_exists("codex")
        }
        "Cursor" => home.join(".cursor").exists(),
        "Windsurf" => home.join(".codeium/windsurf").exists(),
        "Gemini CLI" => home.join(".gemini").exists(),
        "VS Code" => detect_vscode_installed(home),
        "Copilot CLI" => home.join(".copilot").exists() || command_exists("copilot"),
        "Zed" => crate::core::editor_registry::zed_config_dir(home).exists(),
        "Cline" => detect_extension_installed(home, "saoudrizwan.claude-dev"),
        "Roo Code" => detect_extension_installed(home, "rooveterinaryinc.roo-cline"),
        "OpenCode" => home.join(".config/opencode").exists(),
        "Continue" => detect_extension_installed(home, "continue.continue"),
        "Amp" => command_exists("amp") || home.join(".ampcoder").exists(),
        "Qwen Code" => home.join(".qwen").exists(),
        "Trae" => home.join(".trae").exists(),
        "Amazon Q Developer" => home.join(".aws/amazonq").exists(),
        "JetBrains IDEs" => detect_jetbrains_installed(home),
        "Antigravity" => home.join(".gemini/antigravity").exists(),
        "Pi Coding Agent" => home.join(".pi").exists() || command_exists("pi"),
        "AWS Kiro" => home.join(".kiro").exists(),
        "Crush" => home.join(".config/crush").exists() || command_exists("crush"),
        "Verdent" => home.join(".verdent").exists(),
        // Augment ships as either the `auggie` CLI (writes to ~/.augment/) or
        // the VS Code extension (`augment.vscode-augment` globalStorage).
        "Augment" => {
            command_exists("auggie")
                || home.join(".augment").exists()
                || detect_extension_installed(home, "augment.vscode-augment")
        }
        "OpenClaw" => home.join(".openclaw").exists() || command_exists("openclaw"),
        "Hermes Agent" => home.join(".hermes").exists() || command_exists("hermes"),
        _ => false,
    }
}

pub(super) fn command_exists(name: &str) -> bool {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("where")
        .arg(name)
        .output()
        .is_ok_and(|o| o.status.success());

    #[cfg(not(target_os = "windows"))]
    let result = std::process::Command::new("which")
        .arg(name)
        .output()
        .is_ok_and(|o| o.status.success());

    result
}

fn detect_vscode_installed(_home: &std::path::Path) -> bool {
    let check_dir = |dir: PathBuf| -> bool {
        dir.join("settings.json").exists() || dir.join("mcp.json").exists()
    };

    #[cfg(target_os = "macos")]
    if check_dir(_home.join("Library/Application Support/Code/User")) {
        return true;
    }
    #[cfg(target_os = "linux")]
    if check_dir(_home.join(".config/Code/User")) {
        return true;
    }
    if check_dir(_home.join(".config/Code - Insiders/User")) {
        return true;
    }
    if check_dir(_home.join(".vscode-server/data/User")) {
        return true;
    }
    #[cfg(target_os = "windows")]
    if let Ok(appdata) = std::env::var("APPDATA") {
        if check_dir(PathBuf::from(&appdata).join("Code/User")) {
            return true;
        }
    }
    false
}

fn detect_jetbrains_installed(home: &std::path::Path) -> bool {
    #[cfg(target_os = "macos")]
    if home.join("Library/Application Support/JetBrains").exists() {
        return true;
    }
    #[cfg(target_os = "linux")]
    if home.join(".config/JetBrains").exists() {
        return true;
    }
    home.join(".jb-mcp.json").exists()
}

fn detect_extension_installed(_home: &std::path::Path, extension_id: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        if _home
            .join(format!(
                "Library/Application Support/Code/User/globalStorage/{extension_id}"
            ))
            .exists()
        {
            return true;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if _home
            .join(format!(".config/Code/User/globalStorage/{extension_id}"))
            .exists()
        {
            return true;
        }
        if _home
            .join(format!(
                ".config/Code - Insiders/User/globalStorage/{extension_id}"
            ))
            .exists()
        {
            return true;
        }
        if _home
            .join(format!(
                ".vscode-server/data/User/globalStorage/{extension_id}"
            ))
            .exists()
        {
            return true;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if std::path::PathBuf::from(&appdata)
                .join(format!("Code/User/globalStorage/{extension_id}"))
                .exists()
            {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Target definitions
// ---------------------------------------------------------------------------
