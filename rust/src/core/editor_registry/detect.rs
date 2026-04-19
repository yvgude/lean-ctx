use std::path::{Path, PathBuf};

use super::paths::*;
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

    vec![
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
            config_path: home.join(".codex/config.toml"),
            detect_path: detect_codex_path(home),
            config_type: ConfigType::Codex,
        },
        EditorTarget {
            name: "Gemini CLI",
            agent_key: "gemini".to_string(),
            config_path: home.join(".gemini/settings/mcp.json"),
            detect_path: home.join(".gemini"),
            config_type: ConfigType::McpJson,
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
            agent_key: "".to_string(),
            config_path: zed_settings_path(home),
            detect_path: zed_config_dir(home),
            config_type: ConfigType::Zed,
        },
        EditorTarget {
            name: "VS Code / Copilot",
            agent_key: "copilot".to_string(),
            config_path: vscode_mcp_path(),
            detect_path: detect_vscode_path(),
            config_type: ConfigType::VsCodeMcp,
        },
        EditorTarget {
            name: "OpenCode",
            agent_key: "".to_string(),
            config_path: opencode_cfg,
            detect_path: opencode_detect,
            config_type: ConfigType::OpenCode,
        },
        EditorTarget {
            name: "Qwen Code",
            agent_key: "qwen".to_string(),
            config_path: home.join(".qwen/mcp.json"),
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
            config_path: home.join(".aws/amazonq/mcp.json"),
            detect_path: home.join(".aws/amazonq"),
            config_type: ConfigType::McpJson,
        },
        EditorTarget {
            name: "JetBrains IDEs",
            agent_key: "jetbrains".to_string(),
            config_path: home.join(".jb-mcp.json"),
            detect_path: detect_jetbrains_path(home),
            config_type: ConfigType::McpJson,
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
    ]
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
    let codex_dir = home.join(".codex");
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
