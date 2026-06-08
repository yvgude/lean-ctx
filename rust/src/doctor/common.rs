// Auto-split from the former monolithic doctor/mod.rs.

use super::{Outcome, BOLD, DIM, GREEN, RED, RST, WHITE};
use std::path::PathBuf;

pub(super) fn print_check(outcome: &Outcome) {
    let mark = if outcome.ok {
        format!("{GREEN}✓{RST}")
    } else {
        format!("{RED}✗{RST}")
    };
    println!("  {mark}  {}", outcome.line);
}

pub(super) fn path_in_path_env() -> bool {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join("lean-ctx").is_file() {
                return true;
            }
            if cfg!(windows)
                && (dir.join("lean-ctx.exe").is_file() || dir.join("lean-ctx.cmd").is_file())
            {
                return true;
            }
        }
    }
    false
}

pub(super) fn resolve_lean_ctx_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            if cfg!(windows) {
                let exe = dir.join("lean-ctx.exe");
                if exe.is_file() {
                    return Some(exe);
                }
                let cmd = dir.join("lean-ctx.cmd");
                if cmd.is_file() {
                    return Some(cmd);
                }
            } else {
                let bin = dir.join("lean-ctx");
                if bin.is_file() {
                    return Some(bin);
                }
            }
        }
    }
    None
}

pub(super) fn lean_ctx_version_from_path() -> Outcome {
    let resolved = resolve_lean_ctx_binary();
    let bin = resolved
        .clone()
        .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| "lean-ctx".into()));

    let v = env!("CARGO_PKG_VERSION");
    let note = match std::env::current_exe() {
        Ok(exe) if exe == bin => format!("{DIM}(this binary){RST}"),
        Ok(_) | Err(_) => format!("{DIM}(resolved: {}){RST}", bin.display()),
    };
    Outcome {
        ok: true,
        line: format!("{BOLD}lean-ctx version{RST}  {WHITE}lean-ctx {v}{RST}  {note}"),
    }
}

pub(super) fn rc_contains_lean_ctx(path: &PathBuf) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => s.contains("lean-ctx"),
        Err(_) => false,
    }
}

pub(super) fn has_pipe_guard_in_content(content: &str) -> bool {
    content.contains("! -t 1")
        || content.contains("isatty stdout")
        || content.contains("IsOutputRedirected")
}

pub(super) fn rc_references_shell_hook(content: &str) -> bool {
    content.contains("lean-ctx/shell-hook.") || content.contains("lean-ctx\\shell-hook.")
}

pub(super) fn rc_has_pipe_guard(path: &PathBuf) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            if has_pipe_guard_in_content(&s) {
                return true;
            }
            if rc_references_shell_hook(&s) {
                let dirs_to_check = hook_dirs();
                for dir in &dirs_to_check {
                    for ext in &["zsh", "bash", "fish", "ps1"] {
                        let hook = dir.join(format!("shell-hook.{ext}"));
                        if let Ok(h) = std::fs::read_to_string(&hook) {
                            if has_pipe_guard_in_content(&h) {
                                return true;
                            }
                        }
                    }
                }
            }
            false
        }
        Err(_) => false,
    }
}

pub(super) fn hook_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(d) = crate::core::data_dir::lean_ctx_data_dir() {
        dirs.push(d);
    }
    if let Some(home) = dirs::home_dir() {
        let legacy = home.join(".lean-ctx");
        if !dirs.iter().any(|d| d == &legacy) {
            dirs.push(legacy);
        }
        let xdg = home.join(".config").join("lean-ctx");
        if !dirs.iter().any(|d| d == &xdg) {
            dirs.push(xdg);
        }
    }
    dirs
}

pub(super) fn is_active_shell_impl(
    rc_name: &str,
    shell: &str,
    is_windows: bool,
    is_powershell: bool,
) -> bool {
    match rc_name {
        "~/.zshrc" => shell.contains("zsh"),
        "~/.bashrc" => {
            // On Windows, .bashrc is only relevant when explicitly running
            // inside Git Bash (not PowerShell, cmd, or other Windows shells).
            // Git Bash sets $SHELL to bash.exe system-wide, which makes $SHELL
            // unreliable on Windows. We also check that the user is NOT in
            // PowerShell (PSModulePath) and NOT in plain cmd (PROMPT).
            if is_windows {
                if is_powershell {
                    return false;
                }
                // Even without PSModulePath, $SHELL containing "bash" on Windows
                // is unreliable (Git Bash sets it globally). Only flag if running
                // from an actual bash interactive session (BASH_VERSION is set).
                return std::env::var("BASH_VERSION").is_ok();
            }
            shell.contains("bash") || shell.is_empty()
        }
        "~/.config/fish/config.fish" => shell.contains("fish"),
        _ => true,
    }
}

/// Detect whether we are running inside a PowerShell session on Windows.
/// Git Bash may set `$SHELL` to bash.exe system-wide, so `$SHELL` alone
/// is not sufficient — we also need to rule out PowerShell as the actual
/// running host process.
pub(super) fn is_powershell_session() -> bool {
    std::env::var("PSModulePath").is_ok()
}

pub(super) fn is_active_shell(rc_name: &str) -> bool {
    let shell = std::env::var("SHELL").unwrap_or_default();
    is_active_shell_impl(rc_name, &shell, cfg!(windows), is_powershell_session())
}

pub(super) struct McpLocation {
    pub(super) name: &'static str,
    pub(super) display: String,
    pub(super) path: PathBuf,
}

pub(super) fn mcp_config_locations(home: &std::path::Path) -> Vec<McpLocation> {
    let mut locations = vec![
        McpLocation {
            name: "Cursor",
            display: "~/.cursor/mcp.json".into(),
            path: home.join(".cursor").join("mcp.json"),
        },
        McpLocation {
            name: "Claude Code",
            display: format!(
                "{}",
                crate::core::editor_registry::claude_mcp_json_path(home).display()
            ),
            path: crate::core::editor_registry::claude_mcp_json_path(home),
        },
        McpLocation {
            name: "Windsurf",
            display: "~/.codeium/windsurf/mcp_config.json".into(),
            path: home
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
        },
        McpLocation {
            name: "Codex",
            display: {
                let codex_dir =
                    crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
                format!("{}/config.toml", codex_dir.display())
            },
            path: crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("config.toml"),
        },
        McpLocation {
            name: "Gemini CLI",
            display: "~/.gemini/settings.json".into(),
            path: home.join(".gemini").join("settings.json"),
        },
        McpLocation {
            name: "Antigravity",
            display: "~/.gemini/antigravity/mcp_config.json".into(),
            path: home
                .join(".gemini")
                .join("antigravity")
                .join("mcp_config.json"),
        },
        McpLocation {
            name: "Antigravity CLI",
            display: "~/.gemini/antigravity-cli/mcp_config.json".into(),
            path: home
                .join(".gemini")
                .join("antigravity-cli")
                .join("mcp_config.json"),
        },
    ];

    #[cfg(unix)]
    {
        let zed_cfg = home.join(".config").join("zed").join("settings.json");
        locations.push(McpLocation {
            name: "Zed",
            display: "~/.config/zed/settings.json".into(),
            path: zed_cfg,
        });
    }

    locations.push(McpLocation {
        name: "Qwen Code",
        display: "~/.qwen/settings.json".into(),
        path: home.join(".qwen").join("settings.json"),
    });
    locations.push(McpLocation {
        name: "Trae",
        display: "~/.trae/mcp.json".into(),
        path: home.join(".trae").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Amazon Q",
        display: "~/.aws/amazonq/default.json".into(),
        path: home.join(".aws").join("amazonq").join("default.json"),
    });
    locations.push(McpLocation {
        name: "JetBrains",
        display: "~/.jb-mcp.json".into(),
        path: home.join(".jb-mcp.json"),
    });
    locations.push(McpLocation {
        name: "AWS Kiro",
        display: "~/.kiro/settings/mcp.json".into(),
        path: home.join(".kiro").join("settings").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Verdent",
        display: "~/.verdent/mcp.json".into(),
        path: home.join(".verdent").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Crush",
        display: "~/.config/crush/crush.json".into(),
        path: home.join(".config").join("crush").join("crush.json"),
    });
    locations.push(McpLocation {
        name: "Pi",
        display: "~/.pi/agent/mcp.json".into(),
        path: home.join(".pi").join("agent").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Amp",
        display: "~/.config/amp/settings.json".into(),
        path: home.join(".config").join("amp").join("settings.json"),
    });

    {
        #[cfg(unix)]
        let opencode_cfg = home.join(".config").join("opencode").join("opencode.json");
        #[cfg(unix)]
        let opencode_display = "~/.config/opencode/opencode.json";

        #[cfg(windows)]
        let opencode_cfg = if let Ok(appdata) = std::env::var("APPDATA") {
            std::path::PathBuf::from(appdata)
                .join("opencode")
                .join("opencode.json")
        } else {
            home.join(".config").join("opencode").join("opencode.json")
        };
        #[cfg(windows)]
        let opencode_display = "%APPDATA%/opencode/opencode.json";

        locations.push(McpLocation {
            name: "OpenCode",
            display: opencode_display.into(),
            path: opencode_cfg,
        });
    }

    #[cfg(target_os = "macos")]
    {
        let vscode_mcp = home.join("Library/Application Support/Code/User/mcp.json");
        locations.push(McpLocation {
            name: "VS Code",
            display: "~/Library/Application Support/Code/User/mcp.json".into(),
            path: vscode_mcp,
        });
    }
    #[cfg(target_os = "linux")]
    {
        let vscode_mcp = home.join(".config/Code/User/mcp.json");
        locations.push(McpLocation {
            name: "VS Code",
            display: "~/.config/Code/User/mcp.json".into(),
            path: vscode_mcp,
        });
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let vscode_mcp = std::path::PathBuf::from(appdata).join("Code/User/mcp.json");
            locations.push(McpLocation {
                name: "VS Code",
                display: "%APPDATA%/Code/User/mcp.json".into(),
                path: vscode_mcp,
            });
        }
    }

    locations.push(McpLocation {
        name: "Copilot CLI",
        display: "~/.copilot/mcp-config.json".into(),
        path: home.join(".copilot/mcp-config.json"),
    });

    locations.push(McpLocation {
        name: "Hermes Agent",
        display: "~/.hermes/config.yaml".into(),
        path: home.join(".hermes").join("config.yaml"),
    });

    {
        let cline_path = crate::core::editor_registry::cline_mcp_path();
        if cline_path.to_str().is_some_and(|s| s != "/nonexistent") {
            locations.push(McpLocation {
                name: "Cline",
                display: cline_path.display().to_string(),
                path: cline_path,
            });
        }
    }
    {
        let roo_path = crate::core::editor_registry::roo_mcp_path();
        if roo_path.to_str().is_some_and(|s| s != "/nonexistent") {
            locations.push(McpLocation {
                name: "Roo Code",
                display: roo_path.display().to_string(),
                path: roo_path,
            });
        }
    }

    locations
}

pub(super) fn has_lean_ctx_mcp_entry(content: &str) -> bool {
    // Parse as JSONC: editor config files (VS Code settings.json / mcp.json,
    // Cursor, Windsurf, …) commonly contain comments and trailing commas which
    // strict JSON rejects. See issue #311.
    if let Ok(json) = crate::core::jsonc::parse_jsonc(content) {
        // Known container keys across editors that hold a map of MCP servers:
        //   mcpServers       — most agents (Cursor, Claude, Windsurf, …)
        //   servers          — VS Code mcp.json
        //   context_servers  — Zed settings.json
        for key in ["mcpServers", "servers", "context_servers"] {
            if let Some(servers) = json.get(key).and_then(|v| v.as_object()) {
                if servers.contains_key("lean-ctx") {
                    return true;
                }
            }
        }
        // mcp.servers.lean-ctx (OpenCode et al.)
        if let Some(servers) = json
            .get("mcp")
            .and_then(|v| v.get("servers"))
            .and_then(|v| v.as_object())
        {
            if servers.contains_key("lean-ctx") {
                return true;
            }
        }
        // Parsed cleanly but no lean-ctx entry under any known key.
        return false;
    }
    // Unparseable even as JSONC: fall back to a substring heuristic.
    content.contains("lean-ctx")
}

pub(super) fn proxy_auth_probe(port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let token = crate::core::session_token::resolve_proxy_token("LEAN_CTX_PROXY_TOKEN");

    let Ok(mut stream) = TcpStream::connect_timeout(&addr, crate::proxy_setup::proxy_timeout())
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));

    let req = format!(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }

    let mut buf = [0u8; 128];
    let Ok(n) = stream.read(&mut buf) else {
        return false;
    };
    let response = String::from_utf8_lossy(&buf[..n]);
    response.contains("200") || response.contains("ok")
}

pub(super) fn claude_binary_exists() -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("which")
            .arg("claude")
            .output()
            .is_ok_and(|o| o.status.success())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("where")
            .arg("claude")
            .output()
            .is_ok_and(|o| o.status.success())
    }
}
