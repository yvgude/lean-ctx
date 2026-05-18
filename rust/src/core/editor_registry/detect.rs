use std::path::{Path, PathBuf};

use super::paths::{
    claude_mcp_json_path, cline_mcp_path, qoder_all_mcp_paths, qoderwork_mcp_path, roo_mcp_path,
    vscode_mcp_path, zed_config_dir, zed_settings_path,
};
use super::types::{ConfigType, EditorTarget};

pub fn build_targets(home: &Path) -> Vec<EditorTarget> {
    #[cfg(windows)]
    let opencode_cfg = if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata)
            .join("opencode")
            .join("opencode.json")
    } else {
        home.join(".config/opencode/opencode.json")
    };
    #[cfg(not(windows))]
    let opencode_cfg = home.join(".config/opencode/opencode.json");

    #[cfg(windows)]
    let opencode_detect = opencode_cfg
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| home.join(".config/opencode"));
    #[cfg(not(windows))]
    let opencode_detect = home.join(".config/opencode");

    let mut targets = vec![
        EditorTarget {
            name: "Cursor",
            agent_key: "cursor".to_string(),
            config_path: home.join(".cursor/mcp.json"),
            detect_path: home.join(".cursor"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Claude Code",
            agent_key: "claude".to_string(),
            config_path: claude_mcp_json_path(home),
            detect_path: detect_claude_path(),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Windsurf",
            agent_key: "windsurf".to_string(),
            config_path: home.join(".codeium/windsurf/mcp_config.json"),
            detect_path: home.join(".codeium/windsurf"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Codex CLI",
            agent_key: "codex".to_string(),
            config_path: crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("config.toml"),
            detect_path: detect_codex_path(home),
            config_type: ConfigType::Codex,
        },
        EditorTarget {
            name: "Gemini CLI",
            agent_key: "gemini".to_string(),
            config_path: home.join(".gemini/settings.json"),
            detect_path: home.join(".gemini"),
            config_type: ConfigType::GeminiSettings,
        },
        EditorTarget {
            name: "Antigravity",
            agent_key: "gemini".to_string(),
            config_path: home.join(".gemini/antigravity/mcp_config.json"),
            detect_path: home.join(".gemini/antigravity"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Zed",
            agent_key: "zed".to_string(),
            config_path: zed_settings_path(home),
            detect_path: zed_config_dir(home),
            config_type: ConfigType::Zed,
        },
        EditorTarget {
            name: "VS Code",
            agent_key: "vscode".to_string(),
            config_path: vscode_mcp_path(),
            detect_path: detect_vscode_path(),
            config_type: ConfigType::VsCodeMcp,
        },
        EditorTarget {
            name: "Copilot CLI",
            agent_key: "copilot".to_string(),
            config_path: home.join(".copilot/mcp-config.json"),
            detect_path: home.join(".copilot"),
            config_type: ConfigType::CopilotCli,
        },
        EditorTarget {
            name: "OpenCode",
            agent_key: "opencode".to_string(),
            config_path: opencode_cfg,
            detect_path: opencode_detect,
            config_type: ConfigType::OpenCode,
        },
        EditorTarget {
            name: "Qwen Code",
            agent_key: "qwen".to_string(),
            config_path: home.join(".qwen/settings.json"),
            detect_path: home.join(".qwen"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Trae",
            agent_key: "trae".to_string(),
            config_path: home.join(".trae/mcp.json"),
            detect_path: home.join(".trae"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Amazon Q Developer",
            agent_key: "amazonq".to_string(),
            config_path: home.join(".aws/amazonq/default.json"),
            detect_path: home.join(".aws/amazonq"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "JetBrains IDEs",
            agent_key: "jetbrains".to_string(),
            config_path: home.join(".jb-mcp.json"),
            detect_path: detect_jetbrains_path(home),
            config_type: ConfigType::JetBrains,
        },
        EditorTarget {
            name: "Cline",
            agent_key: "cline".to_string(),
            config_path: cline_mcp_path(),
            detect_path: detect_cline_path(),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Roo Code",
            agent_key: "roo".to_string(),
            config_path: roo_mcp_path(),
            detect_path: detect_roo_path(),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "AWS Kiro",
            agent_key: "kiro".to_string(),
            config_path: home.join(".kiro/settings/mcp.json"),
            detect_path: home.join(".kiro"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Verdent",
            agent_key: "verdent".to_string(),
            config_path: home.join(".verdent/mcp.json"),
            detect_path: home.join(".verdent"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Crush",
            agent_key: "crush".to_string(),
            config_path: home.join(".config/crush/crush.json"),
            detect_path: home.join(".config/crush"),
            config_type: ConfigType::Crush,
        },
        EditorTarget {
            name: "Pi Coding Agent",
            agent_key: "pi".to_string(),
            config_path: home.join(".pi/agent/mcp.json"),
            detect_path: home.join(".pi/agent"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Amp",
            agent_key: "amp".to_string(),
            config_path: home.join(".config/amp/settings.json"),
            detect_path: home.join(".config/amp"),
            config_type: ConfigType::Amp,
        },
        EditorTarget {
            name: "QoderWork",
            agent_key: "qoderwork".to_string(),
            config_path: qoderwork_mcp_path(home),
            detect_path: detect_qoderwork_path(home),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Hermes Agent",
            agent_key: "hermes".to_string(),
            config_path: home.join(".hermes/config.yaml"),
            detect_path: home.join(".hermes"),
            config_type: ConfigType::HermesYaml,
        },
        EditorTarget {
            name: "Aider",
            agent_key: "aider".to_string(),
            config_path: home.join(".aider/mcp.json"),
            detect_path: home.join(".aider"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Continue",
            agent_key: "continue".to_string(),
            config_path: home.join(".continue/mcp.json"),
            detect_path: home.join(".continue"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Neovim (mcphub.nvim)",
            agent_key: "neovim".to_string(),
            config_path: home.join(".config/mcphub/servers.json"),
            detect_path: home.join(".config/nvim"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Emacs (mcp.el)",
            agent_key: "emacs".to_string(),
            config_path: home.join(".emacs.d/mcp.json"),
            detect_path: home.join(".emacs.d"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "Sublime Text",
            agent_key: "sublime".to_string(),
            config_path: detect_sublime_mcp_path(home),
            detect_path: detect_sublime_path(home),
            config_type: ConfigType::McpJson,
        },
    ];

    targets.extend(
        qoder_all_mcp_paths(home)
            .into_iter()
            .map(|config_path| EditorTarget {
                name: "Qoder",
                agent_key: "qoder".to_string(),
                config_path,
                detect_path: detect_qoder_path(home),
                config_type: ConfigType::QoderSettings,
            }),
    );

    targets
}

fn detect_qoder_path(home: &Path) -> PathBuf {
    let qoder_dir = home.join(".qoder");
    if qoder_dir.exists() {
        return qoder_dir;
    }
    #[cfg(target_os = "macos")]
    {
        let app_dir = home.join("Library/Application Support/Qoder");
        if app_dir.exists() {
            return app_dir;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let app_dir = PathBuf::from(appdata).join("Qoder");
            if app_dir.exists() {
                return app_dir;
            }
        }
    }
    PathBuf::from("/nonexistent")
}

fn detect_qoderwork_path(home: &Path) -> PathBuf {
    let dir = home.join(".qoderwork");
    if dir.exists() {
        return dir;
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let app_dir = PathBuf::from(appdata).join("QoderWork");
            if app_dir.exists() {
                return app_dir;
            }
        }
    }
    PathBuf::from("/nonexistent")
}

fn detect_sublime_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let app_dir = home.join("Library/Application Support/Sublime Text");
        if app_dir.exists() {
            return app_dir;
        }
    }
    let xdg_dir = home.join(".config/sublime-text");
    if xdg_dir.exists() {
        return xdg_dir;
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let app_dir = PathBuf::from(appdata).join("Sublime Text");
            if app_dir.exists() {
                return app_dir;
            }
        }
    }
    PathBuf::from("/nonexistent")
}

fn detect_sublime_mcp_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let app_dir = home.join("Library/Application Support/Sublime Text/Packages/User/mcp.json");
        if app_dir.parent().is_some_and(std::path::Path::exists) {
            return app_dir;
        }
    }
    home.join(".config/sublime-text/mcp.json")
}

pub fn detect_claude_path() -> PathBuf {
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(which_cmd).arg("claude").output() {
        if output.status.success() {
            return PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        }
    }
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            let p = PathBuf::from(dir);
            if p.exists() {
                return p;
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let claude_json = claude_mcp_json_path(&home);
        if claude_json.exists() {
            return claude_json;
        }
    }
    PathBuf::from("/nonexistent")
}

pub fn detect_codex_path(home: &Path) -> PathBuf {
    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    if codex_dir.exists() {
        return codex_dir;
    }
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(which_cmd).arg("codex").output() {
        if output.status.success() {
            return codex_dir;
        }
    }
    PathBuf::from("/nonexistent")
}

pub fn detect_vscode_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            let vscode = home.join("Library/Application Support/Code/User/settings.json");
            if vscode.exists() {
                return vscode;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            let vscode = home.join(".config/Code/User/settings.json");
            if vscode.exists() {
                return vscode;
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let vscode = PathBuf::from(appdata).join("Code/User/settings.json");
            if vscode.exists() {
                return vscode;
            }
        }
    }
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(which_cmd).arg("code").output() {
        if output.status.success() {
            return PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        }
    }
    PathBuf::from("/nonexistent")
}

pub fn detect_jetbrains_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let lib = home.join("Library/Application Support/JetBrains");
        if lib.exists() {
            return lib;
        }
    }
    #[cfg(target_os = "linux")]
    {
        let cfg = home.join(".config/JetBrains");
        if cfg.exists() {
            return cfg;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let jb = std::path::PathBuf::from(appdata).join("JetBrains");
            if jb.exists() {
                return jb;
            }
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let jb = std::path::PathBuf::from(local).join("JetBrains");
            if jb.exists() {
                return jb;
            }
        }
    }
    if home.join(".jb-mcp.json").exists() {
        return home.join(".jb-mcp.json");
    }
    PathBuf::from("/nonexistent")
}

#[allow(unreachable_code)]
pub fn detect_cline_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let p = PathBuf::from(appdata).join("Code/User/globalStorage/saoudrizwan.claude-dev");
            if p.exists() {
                return p;
            }
        }
        return PathBuf::from("/nonexistent");
    }

    let Some(home) = dirs::home_dir() else {
        return PathBuf::from("/nonexistent");
    };
    #[cfg(target_os = "macos")]
    {
        let p =
            home.join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev");
        if p.exists() {
            return p;
        }
    }
    #[cfg(target_os = "linux")]
    {
        let p = home.join(".config/Code/User/globalStorage/saoudrizwan.claude-dev");
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("/nonexistent")
}

#[allow(unreachable_code)]
pub fn detect_roo_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let p =
                PathBuf::from(appdata).join("Code/User/globalStorage/rooveterinaryinc.roo-cline");
            if p.exists() {
                return p;
            }
        }
        return PathBuf::from("/nonexistent");
    }

    let Some(home) = dirs::home_dir() else {
        return PathBuf::from("/nonexistent");
    };
    #[cfg(target_os = "macos")]
    {
        let p = home
            .join("Library/Application Support/Code/User/globalStorage/rooveterinaryinc.roo-cline");
        if p.exists() {
            return p;
        }
    }
    #[cfg(target_os = "linux")]
    {
        let p = home.join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline");
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("/nonexistent")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn build_targets_includes_all_qoder_macos_mcp_locations() {
        let home = Path::new("/Users/tester");
        let qoder_paths: Vec<_> = build_targets(home)
            .into_iter()
            .filter(|target| target.agent_key == "qoder")
            .map(|target| target.config_path)
            .collect();

        assert_eq!(
            qoder_paths,
            vec![
                home.join(".qoder/mcp.json"),
                home.join("Library/Application Support/Qoder/User/mcp.json"),
                home.join("Library/Application Support/Qoder/SharedClientCache/mcp.json"),
            ]
        );
    }
}
