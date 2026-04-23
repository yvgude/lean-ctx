//! Environment diagnostics for lean-ctx installation and integration.

use std::net::TcpListener;
use std::path::PathBuf;

use chrono::Utc;

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const BOLD: &str = "\x1b[1m";
const RST: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const WHITE: &str = "\x1b[97m";
const YELLOW: &str = "\x1b[33m";

struct Outcome {
    ok: bool,
    line: String,
}

fn print_check(outcome: &Outcome) {
    let mark = if outcome.ok {
        format!("{GREEN}✓{RST}")
    } else {
        format!("{RED}✗{RST}")
    };
    println!("  {mark}  {}", outcome.line);
}

fn path_in_path_env() -> bool {
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

fn resolve_lean_ctx_binary() -> Option<PathBuf> {
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

fn lean_ctx_version_from_path() -> Outcome {
    let resolved = resolve_lean_ctx_binary();
    let bin = resolved
        .clone()
        .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| "lean-ctx".into()));

    let v = env!("CARGO_PKG_VERSION");
    let note = match std::env::current_exe() {
        Ok(exe) if exe == bin => format!("{DIM}(this binary){RST}"),
        Ok(_) => format!("{DIM}(resolved: {}){RST}", bin.display()),
        Err(_) => format!("{DIM}(resolved: {}){RST}", bin.display()),
    };
    Outcome {
        ok: true,
        line: format!("{BOLD}lean-ctx version{RST}  {WHITE}lean-ctx {v}{RST}  {note}"),
    }
}

fn rc_contains_lean_ctx(path: &PathBuf) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => s.contains("lean-ctx"),
        Err(_) => false,
    }
}

fn has_pipe_guard_in_content(content: &str) -> bool {
    content.contains("! -t 1")
        || content.contains("isatty stdout")
        || content.contains("IsOutputRedirected")
}

fn rc_has_pipe_guard(path: &PathBuf) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            if has_pipe_guard_in_content(&s) {
                return true;
            }
            if s.contains(".lean-ctx/shell-hook.") {
                if let Some(home) = dirs::home_dir() {
                    for ext in &["zsh", "bash", "fish", "ps1"] {
                        let hook = home.join(format!(".lean-ctx/shell-hook.{ext}"));
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

fn is_active_shell(rc_name: &str) -> bool {
    let shell = std::env::var("SHELL").unwrap_or_default();
    match rc_name {
        "~/.zshrc" => shell.contains("zsh"),
        "~/.bashrc" => shell.contains("bash") || shell.is_empty(),
        "~/.config/fish/config.fish" => shell.contains("fish"),
        _ => true,
    }
}

fn shell_aliases_outcome() -> Outcome {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            return Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Shell aliases{RST}  {RED}could not resolve home directory{RST}"
                ),
            };
        }
    };

    let mut parts = Vec::new();
    let mut needs_update = Vec::new();

    let zsh = home.join(".zshrc");
    if rc_contains_lean_ctx(&zsh) {
        parts.push(format!("{DIM}~/.zshrc{RST}"));
        if !rc_has_pipe_guard(&zsh) && is_active_shell("~/.zshrc") {
            needs_update.push("~/.zshrc");
        }
    }
    let bash = home.join(".bashrc");
    if rc_contains_lean_ctx(&bash) {
        parts.push(format!("{DIM}~/.bashrc{RST}"));
        if !rc_has_pipe_guard(&bash) && is_active_shell("~/.bashrc") {
            needs_update.push("~/.bashrc");
        }
    }

    let fish = home.join(".config").join("fish").join("config.fish");
    if rc_contains_lean_ctx(&fish) {
        parts.push(format!("{DIM}~/.config/fish/config.fish{RST}"));
        if !rc_has_pipe_guard(&fish) && is_active_shell("~/.config/fish/config.fish") {
            needs_update.push("~/.config/fish/config.fish");
        }
    }

    #[cfg(windows)]
    {
        let ps_profile = home
            .join("Documents")
            .join("PowerShell")
            .join("Microsoft.PowerShell_profile.ps1");
        let ps_profile_legacy = home
            .join("Documents")
            .join("WindowsPowerShell")
            .join("Microsoft.PowerShell_profile.ps1");
        if rc_contains_lean_ctx(&ps_profile) {
            parts.push(format!("{DIM}PowerShell profile{RST}"));
            if !rc_has_pipe_guard(&ps_profile) {
                needs_update.push("PowerShell profile");
            }
        } else if rc_contains_lean_ctx(&ps_profile_legacy) {
            parts.push(format!("{DIM}WindowsPowerShell profile{RST}"));
            if !rc_has_pipe_guard(&ps_profile_legacy) {
                needs_update.push("WindowsPowerShell profile");
            }
        }
    }

    if parts.is_empty() {
        let hint = if cfg!(windows) {
            "no \"lean-ctx\" in PowerShell profile, ~/.zshrc or ~/.bashrc"
        } else {
            "no \"lean-ctx\" in ~/.zshrc, ~/.bashrc, or ~/.config/fish/config.fish"
        };
        Outcome {
            ok: false,
            line: format!("{BOLD}Shell aliases{RST}  {RED}{hint}{RST}"),
        }
    } else if !needs_update.is_empty() {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}Shell aliases{RST}  {YELLOW}outdated hook in {} — run {BOLD}lean-ctx init --global{RST}{YELLOW} to fix (pipe guard missing){RST}",
                needs_update.join(", ")
            ),
        }
    } else {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Shell aliases{RST}  {GREEN}lean-ctx referenced in {}{RST}",
                parts.join(", ")
            ),
        }
    }
}

struct McpLocation {
    name: &'static str,
    display: String,
    path: PathBuf,
}

fn mcp_config_locations(home: &std::path::Path) -> Vec<McpLocation> {
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
            display: "~/.codex/config.toml".into(),
            path: home.join(".codex").join("config.toml"),
        },
        McpLocation {
            name: "Gemini CLI",
            display: "~/.gemini/settings/mcp.json".into(),
            path: home.join(".gemini").join("settings").join("mcp.json"),
        },
        McpLocation {
            name: "Antigravity",
            display: "~/.gemini/antigravity/mcp_config.json".into(),
            path: home
                .join(".gemini")
                .join("antigravity")
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
        display: "~/.qwen/mcp.json".into(),
        path: home.join(".qwen").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Trae",
        display: "~/.trae/mcp.json".into(),
        path: home.join(".trae").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Amazon Q",
        display: "~/.aws/amazonq/mcp.json".into(),
        path: home.join(".aws").join("amazonq").join("mcp.json"),
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
        name: "Aider",
        display: "~/.aider/mcp.json".into(),
        path: home.join(".aider").join("mcp.json"),
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
            name: "VS Code / Copilot",
            display: "~/Library/Application Support/Code/User/mcp.json".into(),
            path: vscode_mcp,
        });
    }
    #[cfg(target_os = "linux")]
    {
        let vscode_mcp = home.join(".config/Code/User/mcp.json");
        locations.push(McpLocation {
            name: "VS Code / Copilot",
            display: "~/.config/Code/User/mcp.json".into(),
            path: vscode_mcp,
        });
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let vscode_mcp = std::path::PathBuf::from(appdata).join("Code/User/mcp.json");
            locations.push(McpLocation {
                name: "VS Code / Copilot",
                display: "%APPDATA%/Code/User/mcp.json".into(),
                path: vscode_mcp,
            });
        }
    }

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

fn mcp_config_outcome() -> Outcome {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            return Outcome {
                ok: false,
                line: format!("{BOLD}MCP config{RST}  {RED}could not resolve home directory{RST}"),
            };
        }
    };

    let locations = mcp_config_locations(&home);
    let mut found: Vec<String> = Vec::new();
    let mut exists_no_ref: Vec<String> = Vec::new();

    for loc in &locations {
        if let Ok(content) = std::fs::read_to_string(&loc.path) {
            if has_lean_ctx_mcp_entry(&content) {
                found.push(format!("{} {DIM}({}){RST}", loc.name, loc.display));
            } else {
                exists_no_ref.push(loc.name.to_string());
            }
        }
    }

    found.sort();
    found.dedup();
    exists_no_ref.sort();
    exists_no_ref.dedup();

    if !found.is_empty() {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}MCP config{RST}  {GREEN}lean-ctx found in: {}{RST}",
                found.join(", ")
            ),
        }
    } else if !exists_no_ref.is_empty() {
        let has_claude = exists_no_ref.iter().any(|n| n.starts_with("Claude Code"));
        let cause = if has_claude {
            format!("{DIM}(Claude Code may overwrite ~/.claude.json on startup — lean-ctx entry missing from mcpServers){RST}")
        } else {
            String::new()
        };
        let hint = if has_claude {
            format!("{DIM}(run: lean-ctx doctor --fix OR lean-ctx init --agent claude){RST}")
        } else {
            format!("{DIM}(run: lean-ctx doctor --fix OR lean-ctx setup){RST}")
        };
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}MCP config{RST}  {YELLOW}config exists for {} but mcpServers does not contain lean-ctx{RST}  {cause} {hint}",
                exists_no_ref.join(", "),
            ),
        }
    } else {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}MCP config{RST}  {YELLOW}no MCP config found{RST}  {DIM}(run: lean-ctx setup){RST}"
            ),
        }
    }
}

fn has_lean_ctx_mcp_entry(content: &str) -> bool {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(servers) = json.get("mcpServers").and_then(|v| v.as_object()) {
            return servers.contains_key("lean-ctx");
        }
        if let Some(servers) = json
            .get("mcp")
            .and_then(|v| v.get("servers"))
            .and_then(|v| v.as_object())
        {
            return servers.contains_key("lean-ctx");
        }
    }
    content.contains("lean-ctx")
}

fn port_3333_outcome() -> Outcome {
    match TcpListener::bind("127.0.0.1:3333") {
        Ok(_listener) => Outcome {
            ok: true,
            line: format!("{BOLD}Dashboard port 3333{RST}  {GREEN}available on 127.0.0.1{RST}"),
        },
        Err(e) => Outcome {
            ok: false,
            line: format!("{BOLD}Dashboard port 3333{RST}  {RED}not available: {e}{RST}"),
        },
    }
}

fn pi_outcome() -> Option<Outcome> {
    let pi_result = std::process::Command::new("pi").arg("--version").output();

    match pi_result {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let has_plugin = std::process::Command::new("pi")
                .args(["list"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains("pi-lean-ctx"))
                .unwrap_or(false);

            let has_mcp = dirs::home_dir()
                .map(|h| h.join(".pi/agent/mcp.json"))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|c| c.contains("lean-ctx"))
                .unwrap_or(false);

            if has_plugin && has_mcp {
                Some(Outcome {
                    ok: true,
                    line: format!(
                        "{BOLD}Pi Coding Agent{RST}  {GREEN}{version}, pi-lean-ctx + MCP configured{RST}"
                    ),
                })
            } else if has_plugin {
                Some(Outcome {
                    ok: true,
                    line: format!(
                        "{BOLD}Pi Coding Agent{RST}  {GREEN}{version}, pi-lean-ctx installed{RST}  {DIM}(MCP not configured — embedded bridge active){RST}"
                    ),
                })
            } else {
                Some(Outcome {
                    ok: false,
                    line: format!(
                        "{BOLD}Pi Coding Agent{RST}  {YELLOW}{version}, but pi-lean-ctx not installed{RST}  {DIM}(run: pi install npm:pi-lean-ctx){RST}"
                    ),
                })
            }
        }
        _ => None,
    }
}

fn session_state_outcome() -> Outcome {
    use crate::core::session::SessionState;

    match SessionState::load_latest() {
        Some(session) => {
            let root = session
                .project_root
                .as_deref()
                .unwrap_or("(not set)");
            let cwd = session
                .shell_cwd
                .as_deref()
                .unwrap_or("(not tracked)");
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Session state{RST}  {GREEN}active{RST}  {DIM}root: {root}, cwd: {cwd}, v{}{RST}",
                    session.version
                ),
            }
        }
        None => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Session state{RST}  {YELLOW}no active session{RST}  {DIM}(will be created on first tool call){RST}"
            ),
        },
    }
}

fn docker_env_outcomes() -> Vec<Outcome> {
    if !crate::shell::is_container() {
        return vec![];
    }
    let env_sh = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.join("env.sh").to_string_lossy().to_string())
        .unwrap_or_else(|_| "/root/.lean-ctx/env.sh".to_string());

    let mut outcomes = vec![];

    let shell_name = std::env::var("SHELL").unwrap_or_default();
    let is_bash = shell_name.contains("bash") || shell_name.is_empty();

    if is_bash {
        let has_bash_env = std::env::var("BASH_ENV").is_ok();
        outcomes.push(if has_bash_env {
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}BASH_ENV{RST}  {GREEN}set{RST}  {DIM}({}){RST}",
                    std::env::var("BASH_ENV").unwrap_or_default()
                ),
            }
        } else {
            Outcome {
                ok: false,
                line: format!(
                    "{BOLD}BASH_ENV{RST}  {RED}not set{RST}  {YELLOW}(add to Dockerfile: ENV BASH_ENV=\"{env_sh}\"){RST}"
                ),
            }
        });
    }

    let has_claude_env = std::env::var("CLAUDE_ENV_FILE").is_ok();
    outcomes.push(if has_claude_env {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}CLAUDE_ENV_FILE{RST}  {GREEN}set{RST}  {DIM}({}){RST}",
                std::env::var("CLAUDE_ENV_FILE").unwrap_or_default()
            ),
        }
    } else {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}CLAUDE_ENV_FILE{RST}  {RED}not set{RST}  {YELLOW}(for Claude Code: ENV CLAUDE_ENV_FILE=\"{env_sh}\"){RST}"
            ),
        }
    });

    outcomes
}

/// Run diagnostic checks and print colored results to stdout.
pub fn run() {
    let mut passed = 0u32;
    let total = 8u32;

    println!("{BOLD}{WHITE}lean-ctx doctor{RST}  {DIM}diagnostics{RST}\n");

    // 1) Binary on PATH
    let path_bin = resolve_lean_ctx_binary();
    let also_in_path_dirs = path_in_path_env();
    let bin_ok = path_bin.is_some() || also_in_path_dirs;
    if bin_ok {
        passed += 1;
    }
    let bin_line = if let Some(p) = path_bin {
        format!("{BOLD}lean-ctx in PATH{RST}  {WHITE}{}{RST}", p.display())
    } else if also_in_path_dirs {
        format!(
            "{BOLD}lean-ctx in PATH{RST}  {YELLOW}found via PATH walk (not resolved by `command -v`){RST}"
        )
    } else {
        format!("{BOLD}lean-ctx in PATH{RST}  {RED}not found{RST}")
    };
    print_check(&Outcome {
        ok: bin_ok,
        line: bin_line,
    });

    // 2) Version from PATH binary
    let ver = if bin_ok {
        lean_ctx_version_from_path()
    } else {
        Outcome {
            ok: false,
            line: format!("{BOLD}lean-ctx version{RST}  {RED}skipped (binary not in PATH){RST}"),
        }
    };
    if ver.ok {
        passed += 1;
    }
    print_check(&ver);

    // 3) data directory (respects LEAN_CTX_DATA_DIR)
    let lean_dir = crate::core::data_dir::lean_ctx_data_dir().ok();
    let dir_outcome = match &lean_dir {
        Some(p) if p.is_dir() => {
            passed += 1;
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}data dir{RST}  {GREEN}exists{RST}  {DIM}{}{RST}",
                    p.display()
                ),
            }
        }
        Some(p) => Outcome {
            ok: false,
            line: format!(
                "{BOLD}data dir{RST}  {RED}missing or not a directory{RST}  {DIM}{}{RST}",
                p.display()
            ),
        },
        None => Outcome {
            ok: false,
            line: format!("{BOLD}data dir{RST}  {RED}could not resolve data directory{RST}"),
        },
    };
    print_check(&dir_outcome);

    // 4) stats.json + size
    let stats_path = lean_dir.as_ref().map(|d| d.join("stats.json"));
    let stats_outcome = match stats_path.as_ref().and_then(|p| std::fs::metadata(p).ok()) {
        Some(m) if m.is_file() => {
            passed += 1;
            let size = m.len();
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}stats.json{RST}  {GREEN}exists{RST}  {WHITE}{size} bytes{RST}  {DIM}{}{RST}",
                    stats_path.as_ref().unwrap().display()
                ),
            }
        }
        Some(_m) => Outcome {
            ok: false,
            line: format!(
                "{BOLD}stats.json{RST}  {RED}not a file{RST}  {DIM}{}{RST}",
                stats_path.as_ref().unwrap().display()
            ),
        },
        None => {
            passed += 1;
            Outcome {
                ok: true,
                line: match &stats_path {
                    Some(p) => format!(
                        "{BOLD}stats.json{RST}  {YELLOW}not yet created{RST}  {DIM}(will appear after first use) {}{RST}",
                        p.display()
                    ),
                    None => format!("{BOLD}stats.json{RST}  {RED}could not resolve path{RST}"),
                },
            }
        }
    };
    print_check(&stats_outcome);

    // 5) config.toml (missing is OK)
    let config_path = lean_dir.as_ref().map(|d| d.join("config.toml"));
    let config_outcome = match &config_path {
        Some(p) => match std::fs::metadata(p) {
            Ok(m) if m.is_file() => {
                passed += 1;
                Outcome {
                    ok: true,
                    line: format!(
                        "{BOLD}config.toml{RST}  {GREEN}exists{RST}  {DIM}{}{RST}",
                        p.display()
                    ),
                }
            }
            Ok(_) => Outcome {
                ok: false,
                line: format!(
                    "{BOLD}config.toml{RST}  {RED}exists but is not a regular file{RST}  {DIM}{}{RST}",
                    p.display()
                ),
            },
            Err(_) => {
                passed += 1;
                Outcome {
                    ok: true,
                    line: format!(
                        "{BOLD}config.toml{RST}  {YELLOW}not found, using defaults{RST}  {DIM}(expected at {}){RST}",
                        p.display()
                    ),
                }
            }
        },
        None => Outcome {
            ok: false,
            line: format!("{BOLD}config.toml{RST}  {RED}could not resolve path{RST}"),
        },
    };
    print_check(&config_outcome);

    // 6) Shell aliases
    let aliases = shell_aliases_outcome();
    if aliases.ok {
        passed += 1;
    }
    print_check(&aliases);

    // 7) MCP
    let mcp = mcp_config_outcome();
    if mcp.ok {
        passed += 1;
    }
    print_check(&mcp);

    // 9) Port
    let port = port_3333_outcome();
    if port.ok {
        passed += 1;
    }
    print_check(&port);

    // 9) Session state (project_root + shell_cwd)
    let session_outcome = session_state_outcome();
    if session_outcome.ok {
        passed += 1;
    }
    print_check(&session_outcome);

    // 10) Docker env vars (optional, only in containers)
    let docker_outcomes = docker_env_outcomes();
    for docker_check in &docker_outcomes {
        if docker_check.ok {
            passed += 1;
        }
        print_check(docker_check);
    }

    // 11) Pi Coding Agent (optional)
    let pi = pi_outcome();
    if let Some(ref pi_check) = pi {
        if pi_check.ok {
            passed += 1;
        }
        print_check(pi_check);
    }

    // 12) Build integrity (canary / origin check)
    let integrity = crate::core::integrity::check();
    let integrity_ok = integrity.seed_ok && integrity.origin_ok;
    if integrity_ok {
        passed += 1;
    }
    let integrity_line = if integrity_ok {
        format!(
            "{BOLD}Build origin{RST}  {GREEN}official{RST}  {DIM}{}{RST}",
            integrity.repo
        )
    } else {
        format!(
            "{BOLD}Build origin{RST}  {RED}MODIFIED REDISTRIBUTION{RST}  {YELLOW}pkg={}, repo={}{RST}",
            integrity.pkg_name, integrity.repo
        )
    };
    print_check(&Outcome {
        ok: integrity_ok,
        line: integrity_line,
    });

    // 13) Claude Code instruction truncation guard
    let claude_truncation = claude_truncation_outcome();
    if let Some(ref ct) = claude_truncation {
        if ct.ok {
            passed += 1;
        }
        print_check(ct);
    }

    let mut effective_total = total + 2; // session_state + integrity always shown
    effective_total += docker_outcomes.len() as u32;
    if pi.is_some() {
        effective_total += 1;
    }
    if claude_truncation.is_some() {
        effective_total += 1;
    }
    println!();
    println!("  {BOLD}{WHITE}Summary:{RST}  {GREEN}{passed}{RST}{DIM}/{effective_total}{RST} checks passed");
    println!("  {DIM}{}{RST}", crate::core::integrity::origin_line());
}

fn claude_binary_exists() -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("which")
            .arg("claude")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("where")
            .arg("claude")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

fn claude_truncation_outcome() -> Option<Outcome> {
    let home = dirs::home_dir()?;
    let claude_detected = crate::core::editor_registry::claude_mcp_json_path(&home).exists()
        || crate::core::editor_registry::claude_state_dir(&home).exists()
        || claude_binary_exists();

    if !claude_detected {
        return None;
    }

    let rules_path = crate::core::editor_registry::claude_rules_dir(&home).join("lean-ctx.md");
    let skill_path = home.join(".claude/skills/lean-ctx/SKILL.md");

    let has_rules = rules_path.exists();
    let has_skill = skill_path.exists();

    if has_rules && has_skill {
        Some(Outcome {
            ok: true,
            line: format!(
                "{BOLD}Claude Code instructions{RST}  {GREEN}rules + skill installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via rules file){RST}"
            ),
        })
    } else if has_rules {
        Some(Outcome {
            ok: true,
            line: format!(
                "{BOLD}Claude Code instructions{RST}  {GREEN}rules file installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via rules file){RST}"
            ),
        })
    } else {
        Some(Outcome {
            ok: false,
            line: format!(
                "{BOLD}Claude Code instructions{RST}  {YELLOW}MCP instructions truncated at 2048 chars, no rules file found{RST}  {DIM}(run: lean-ctx init --agent claude){RST}"
            ),
        })
    }
}

pub fn run_compact() {
    let (passed, total) = compact_score();
    print_compact_status(passed, total);
}

pub fn run_cli(args: &[String]) -> i32 {
    let fix = args.iter().any(|a| a == "--fix");
    let json = args.iter().any(|a| a == "--json");
    let help = args.iter().any(|a| a == "--help" || a == "-h");

    if help {
        println!("Usage:");
        println!("  lean-ctx doctor");
        println!("  lean-ctx doctor --fix [--json]");
        return 0;
    }

    if !fix {
        run();
        return 0;
    }

    match run_fix(DoctorFixOptions { json }) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{RED}doctor --fix failed:{RST} {e}");
            2
        }
    }
}

struct DoctorFixOptions {
    json: bool,
}

fn run_fix(opts: DoctorFixOptions) -> Result<i32, String> {
    use crate::core::setup_report::{
        doctor_report_path, PlatformInfo, SetupItem, SetupReport, SetupStepReport,
    };

    let _quiet_guard = opts
        .json
        .then(|| crate::setup::EnvVarGuard::set("LEAN_CTX_QUIET", "1"));
    let started_at = Utc::now();
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;

    let mut steps: Vec<SetupStepReport> = Vec::new();

    // Step: shell hook repair
    let mut shell_step = SetupStepReport {
        name: "shell_hook".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let before = shell_aliases_outcome();
    if before.ok {
        shell_step.items.push(SetupItem {
            name: "init --global".to_string(),
            status: "already".to_string(),
            path: None,
            note: None,
        });
    } else {
        if opts.json {
            crate::cli::cmd_init_quiet(&["--global".to_string()]);
        } else {
            crate::cli::cmd_init(&["--global".to_string()]);
        }
        let after = shell_aliases_outcome();
        shell_step.ok = after.ok;
        shell_step.items.push(SetupItem {
            name: "init --global".to_string(),
            status: if after.ok {
                "fixed".to_string()
            } else {
                "failed".to_string()
            },
            path: None,
            note: if after.ok {
                None
            } else {
                Some("shell hook still not detected by doctor checks".to_string())
            },
        });
        if !after.ok {
            shell_step
                .warnings
                .push("shell hook not detected after init --global".to_string());
        }
    }
    steps.push(shell_step);

    // Step: MCP config repair (detected tools)
    let mut mcp_step = SetupStepReport {
        name: "mcp_config".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let binary = crate::core::portable_binary::resolve_portable_binary();
    let targets = crate::core::editor_registry::build_targets(&home);
    for t in &targets {
        if !t.detect_path.exists() {
            continue;
        }
        let short = t.config_path.to_string_lossy().to_string();
        let res = crate::core::editor_registry::write_config_with_options(
            t,
            &binary,
            crate::core::editor_registry::WriteOptions {
                overwrite_invalid: true,
            },
        );
        match res {
            Ok(r) => {
                let status = match r.action {
                    crate::core::editor_registry::WriteAction::Created => "created",
                    crate::core::editor_registry::WriteAction::Updated => "updated",
                    crate::core::editor_registry::WriteAction::Already => "already",
                };
                mcp_step.items.push(SetupItem {
                    name: t.name.to_string(),
                    status: status.to_string(),
                    path: Some(short),
                    note: r.note,
                });
            }
            Err(e) => {
                mcp_step.ok = false;
                mcp_step.items.push(SetupItem {
                    name: t.name.to_string(),
                    status: "error".to_string(),
                    path: Some(short),
                    note: Some(e.clone()),
                });
                mcp_step.errors.push(format!("{}: {e}", t.name));
            }
        }
    }
    if mcp_step.items.is_empty() {
        mcp_step
            .warnings
            .push("no supported AI tools detected; skipped MCP config repair".to_string());
    }
    steps.push(mcp_step);

    // Step: agent rules injection
    let mut rules_step = SetupStepReport {
        name: "agent_rules".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let inj = crate::rules_inject::inject_all_rules(&home);
    if !inj.injected.is_empty() {
        rules_step.items.push(SetupItem {
            name: "injected".to_string(),
            status: inj.injected.len().to_string(),
            path: None,
            note: Some(inj.injected.join(", ")),
        });
    }
    if !inj.updated.is_empty() {
        rules_step.items.push(SetupItem {
            name: "updated".to_string(),
            status: inj.updated.len().to_string(),
            path: None,
            note: Some(inj.updated.join(", ")),
        });
    }
    if !inj.already.is_empty() {
        rules_step.items.push(SetupItem {
            name: "already".to_string(),
            status: inj.already.len().to_string(),
            path: None,
            note: Some(inj.already.join(", ")),
        });
    }
    if !inj.errors.is_empty() {
        rules_step.ok = false;
        rules_step.errors.extend(inj.errors.clone());
    }
    steps.push(rules_step);

    // Step: verify (compact)
    let mut verify_step = SetupStepReport {
        name: "verify".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let (passed, total) = compact_score();
    verify_step.items.push(SetupItem {
        name: "doctor_compact".to_string(),
        status: format!("{passed}/{total}"),
        path: None,
        note: None,
    });
    if passed != total {
        verify_step.warnings.push(format!(
            "doctor compact not fully passing: {passed}/{total}"
        ));
    }
    steps.push(verify_step);

    let finished_at = Utc::now();
    let success = steps.iter().all(|s| s.ok);

    let report = SetupReport {
        schema_version: 1,
        started_at,
        finished_at,
        success,
        platform: PlatformInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        steps,
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let path = doctor_report_path()?;
    let json_text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&path, &json_text)?;

    if opts.json {
        println!("{json_text}");
    } else {
        let (passed, total) = compact_score();
        print_compact_status(passed, total);
        println!("  {DIM}report saved:{RST} {}", path.display());
    }

    Ok(if report.success { 0 } else { 1 })
}

pub fn compact_score() -> (u32, u32) {
    let mut passed = 0u32;
    let total = 5u32;

    if resolve_lean_ctx_binary().is_some() || path_in_path_env() {
        passed += 1;
    }
    let lean_dir = crate::core::data_dir::lean_ctx_data_dir().ok();
    if lean_dir.as_ref().is_some_and(|p| p.is_dir()) {
        passed += 1;
    }
    if lean_dir
        .as_ref()
        .map(|d| d.join("stats.json"))
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|m| m.is_file())
    {
        passed += 1;
    }
    if shell_aliases_outcome().ok {
        passed += 1;
    }
    if mcp_config_outcome().ok {
        passed += 1;
    }

    (passed, total)
}

fn print_compact_status(passed: u32, total: u32) {
    let status = if passed == total {
        format!("{GREEN}✓ All {total} checks passed{RST}")
    } else {
        format!("{YELLOW}{passed}/{total} passed{RST} — run {BOLD}lean-ctx doctor{RST} for details")
    };
    println!("  {status}");
}
