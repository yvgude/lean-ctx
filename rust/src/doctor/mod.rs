//! Environment diagnostics for lean-ctx installation and integration.

mod fix;
mod integrations;

use std::net::TcpListener;
use std::path::PathBuf;

pub(super) const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
pub(super) const BOLD: &str = "\x1b[1m";
pub(super) const RST: &str = "\x1b[0m";
pub(super) const DIM: &str = "\x1b[2m";
pub(super) const WHITE: &str = "\x1b[97m";
pub(super) const YELLOW: &str = "\x1b[33m";

pub(super) struct Outcome {
    pub ok: bool,
    pub line: String,
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

fn lean_ctx_version_from_path() -> Outcome {
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

fn rc_references_shell_hook(content: &str) -> bool {
    content.contains("lean-ctx/shell-hook.") || content.contains("lean-ctx\\shell-hook.")
}

fn rc_has_pipe_guard(path: &PathBuf) -> bool {
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

fn hook_dirs() -> Vec<std::path::PathBuf> {
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

fn is_active_shell(rc_name: &str) -> bool {
    let shell = std::env::var("SHELL").unwrap_or_default();
    match rc_name {
        "~/.zshrc" => shell.contains("zsh"),
        "~/.bashrc" => shell.contains("bash") || shell.is_empty(),
        "~/.config/fish/config.fish" => shell.contains("fish"),
        _ => true,
    }
}

pub(super) fn shell_aliases_outcome() -> Outcome {
    let Some(home) = dirs::home_dir() else {
        return Outcome {
            ok: false,
            line: format!("{BOLD}Shell aliases{RST}  {RED}could not resolve home directory{RST}"),
        };
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
    let Some(home) = dirs::home_dir() else {
        return Outcome {
            ok: false,
            line: format!("{BOLD}MCP config{RST}  {RED}could not resolve home directory{RST}"),
        };
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
                .is_ok_and(|o| {
                    o.status.success() && String::from_utf8_lossy(&o.stdout).contains("pi-lean-ctx")
                });

            let has_mcp = dirs::home_dir()
                .map(|h| h.join(".pi/agent/mcp.json"))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .is_some_and(|c| c.contains("lean-ctx"));

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
    let env_sh = crate::core::data_dir::lean_ctx_data_dir().map_or_else(
        |_| "/root/.lean-ctx/env.sh".to_string(),
        |d| d.join("env.sh").to_string_lossy().to_string(),
    );

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
    let total = 11u32;

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
            let path_display = if let Some(p) = stats_path.as_ref() {
                p.display().to_string()
            } else {
                String::new()
            };
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}stats.json{RST}  {GREEN}exists{RST}  {WHITE}{size} bytes{RST}  {DIM}{path_display}{RST}",
                ),
            }
        }
        Some(_m) => {
            let path_display = if let Some(p) = stats_path.as_ref() {
                p.display().to_string()
            } else {
                String::new()
            };
            Outcome {
                ok: false,
                line: format!(
                    "{BOLD}stats.json{RST}  {RED}not a file{RST}  {DIM}{path_display}{RST}",
                ),
            }
        }
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

    // 6) API proxy upstream
    let proxy_upstream = proxy_upstream_outcome();
    if proxy_upstream.ok {
        passed += 1;
    }
    print_check(&proxy_upstream);

    // 7) Shell aliases
    let aliases = shell_aliases_outcome();
    if aliases.ok {
        passed += 1;
    }
    print_check(&aliases);

    // 8) MCP
    let mcp = mcp_config_outcome();
    if mcp.ok {
        passed += 1;
    }
    print_check(&mcp);

    // 9) SKILL.md
    let skill = skill_files_outcome();
    if skill.ok {
        passed += 1;
    }
    print_check(&skill);

    // 10) Port
    let port = port_3333_outcome();
    if port.ok {
        passed += 1;
    }
    print_check(&port);

    // Daemon status
    #[cfg(unix)]
    let daemon_outcome = if crate::daemon::is_daemon_running() {
        let pid_path = crate::daemon::daemon_pid_path();
        let pid_str = std::fs::read_to_string(&pid_path).unwrap_or_default();
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Daemon{RST}  {GREEN}running (PID {}){RST}",
                pid_str.trim()
            ),
        }
    } else {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Daemon{RST}  {YELLOW}not running{RST}  {DIM}(run: lean-ctx serve -d){RST}"
            ),
        }
    };
    #[cfg(not(unix))]
    let daemon_outcome = Outcome {
        ok: true,
        line: format!("{BOLD}Daemon{RST}  {DIM}not supported on this platform{RST}"),
    };
    if daemon_outcome.ok {
        passed += 1;
    }
    print_check(&daemon_outcome);

    // Session state (project_root + shell_cwd)
    let session_outcome = session_state_outcome();
    if session_outcome.ok {
        passed += 1;
    }
    print_check(&session_outcome);

    // Docker env vars (optional, only in containers)
    let docker_outcomes = docker_env_outcomes();
    for docker_check in &docker_outcomes {
        if docker_check.ok {
            passed += 1;
        }
        print_check(docker_check);
    }

    // Pi Coding Agent (optional)
    let pi = pi_outcome();
    if let Some(ref pi_check) = pi {
        if pi_check.ok {
            passed += 1;
        }
        print_check(pi_check);
    }

    // Build integrity (canary / origin check)
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

    // Cache safety
    let cache_safety = cache_safety_outcome();
    if cache_safety.ok {
        passed += 1;
    }
    print_check(&cache_safety);

    // Claude Code instruction truncation guard
    let claude_truncation = claude_truncation_outcome();
    if let Some(ref ct) = claude_truncation {
        if ct.ok {
            passed += 1;
        }
        print_check(ct);
    }

    let mut effective_total = total + 3; // session_state + integrity + cache_safety always shown
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

fn skill_files_outcome() -> Outcome {
    let Some(home) = dirs::home_dir() else {
        return Outcome {
            ok: false,
            line: format!("{BOLD}SKILL.md{RST}  {RED}could not resolve home directory{RST}"),
        };
    };

    let candidates = [
        ("Claude Code", home.join(".claude/skills/lean-ctx/SKILL.md")),
        ("Cursor", home.join(".cursor/skills/lean-ctx/SKILL.md")),
        ("Codex CLI", home.join(".codex/skills/lean-ctx/SKILL.md")),
        (
            "GitHub Copilot",
            home.join(".vscode/skills/lean-ctx/SKILL.md"),
        ),
    ];

    let mut found: Vec<&str> = Vec::new();
    for (name, path) in &candidates {
        if path.exists() {
            found.push(name);
        }
    }

    if found.is_empty() {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}SKILL.md{RST}  {YELLOW}not installed{RST}  {DIM}(run: lean-ctx setup){RST}"
            ),
        }
    } else {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}SKILL.md{RST}  {GREEN}installed for {}{RST}",
                found.join(", ")
            ),
        }
    }
}

fn proxy_upstream_outcome() -> Outcome {
    let cfg = crate::core::config::Config::load();
    let local_proxy_clients = local_proxy_clients_using_provider_defaults(&cfg);
    proxy_upstream_outcome_with_clients(&cfg, &local_proxy_clients)
}

#[derive(Clone, Copy)]
struct LocalProxyClient {
    client: &'static str,
    provider: &'static str,
}

#[cfg(test)]
fn proxy_upstream_outcome_for_config(cfg: &crate::core::config::Config) -> Outcome {
    proxy_upstream_outcome_with_clients(cfg, &[])
}

fn proxy_upstream_outcome_with_clients(
    cfg: &crate::core::config::Config,
    local_default_clients: &[LocalProxyClient],
) -> Outcome {
    let providers = [
        (
            "Anthropic",
            "proxy.anthropic_upstream",
            cfg.proxy.anthropic_upstream.as_deref(),
        ),
        (
            "OpenAI",
            "proxy.openai_upstream",
            cfg.proxy.openai_upstream.as_deref(),
        ),
        (
            "Gemini",
            "proxy.gemini_upstream",
            cfg.proxy.gemini_upstream.as_deref(),
        ),
    ];

    let mut configured = Vec::new();
    for (label, key, value) in providers {
        let Some(value) = value else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        let normalized = normalize_doctor_url(value);
        if !is_http_url(&normalized) {
            return Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Proxy upstream{RST}  {RED}invalid {label} upstream{RST}  {YELLOW}set {key} to an http(s) URL{RST}"
                ),
            };
        }
        if is_local_proxy_url(&normalized) {
            return Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Proxy upstream{RST}  {RED}{label} upstream points back to local proxy{RST}  {YELLOW}run: lean-ctx config set {key} <url>{RST}"
                ),
            };
        }
        configured.push(format!("{label}={normalized}"));
    }

    let default_clients = local_default_clients
        .iter()
        .map(|client| format!("{}/{}", client.client, client.provider))
        .collect::<Vec<_>>();

    if !configured.is_empty() {
        let mut details = vec![format!("configured: {}", configured.join(", "))];
        if !default_clients.is_empty() {
            details.push(format!("provider defaults: {}", default_clients.join(", ")));
        }

        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Proxy upstream{RST}  {GREEN}custom upstream configured{RST}  {DIM}{}{RST}",
                details.join("; ")
            ),
        };
    }

    if !default_clients.is_empty() {
        let clients = default_clients.join(", ");

        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Proxy upstream{RST}  {GREEN}provider defaults{RST}  {DIM}active local proxy clients: {clients}{RST}"
            ),
        };
    }

    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Proxy upstream{RST}  {GREEN}provider defaults{RST}  {DIM}(override keys: proxy.anthropic_upstream, proxy.openai_upstream, proxy.gemini_upstream){RST}"
        ),
    }
}

fn local_proxy_clients_using_provider_defaults(
    cfg: &crate::core::config::Config,
) -> Vec<LocalProxyClient> {
    let mut clients = Vec::new();

    if !proxy_upstream_override_present(
        "LEAN_CTX_ANTHROPIC_UPSTREAM",
        cfg.proxy.anthropic_upstream.as_deref(),
    ) && url_is_local_proxy(claude_code_anthropic_base().as_deref())
    {
        clients.push(LocalProxyClient {
            client: "Claude Code",
            provider: "Anthropic",
        });
    }

    if !proxy_upstream_override_present(
        "LEAN_CTX_OPENAI_UPSTREAM",
        cfg.proxy.openai_upstream.as_deref(),
    ) && url_is_local_proxy(codex_openai_base().as_deref())
    {
        clients.push(LocalProxyClient {
            client: "Codex",
            provider: "OpenAI",
        });
    }

    if !proxy_upstream_override_present(
        "LEAN_CTX_GEMINI_UPSTREAM",
        cfg.proxy.gemini_upstream.as_deref(),
    ) && url_is_local_proxy(gemini_api_base().as_deref())
    {
        clients.push(LocalProxyClient {
            client: "Gemini",
            provider: "Gemini",
        });
    }

    clients
}

fn proxy_upstream_override_present(env_name: &str, config_value: Option<&str>) -> bool {
    nonempty_env_var(env_name).is_some()
        || config_value.is_some_and(|value| !normalize_doctor_url(value).is_empty())
}

fn claude_code_anthropic_base() -> Option<String> {
    nonempty_env_var("ANTHROPIC_BASE_URL").or_else(claude_settings_anthropic_base)
}

fn codex_openai_base() -> Option<String> {
    nonempty_env_var("OPENAI_BASE_URL").or_else(codex_config_openai_base)
}

fn gemini_api_base() -> Option<String> {
    nonempty_env_var("GEMINI_API_BASE_URL").or_else(shell_gemini_api_base)
}

fn nonempty_env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| normalize_doctor_url(&value))
        .filter(|value| !value.is_empty())
}

fn claude_settings_anthropic_base() -> Option<String> {
    let home = dirs::home_dir()?;
    let settings_path = crate::core::editor_registry::claude_state_dir(&home).join("settings.json");
    let content = std::fs::read_to_string(settings_path).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&content).ok()?;
    doc.get("env")?
        .get("ANTHROPIC_BASE_URL")?
        .as_str()
        .map(str::to_string)
}

fn codex_config_openai_base() -> Option<String> {
    let home = dirs::home_dir()?;
    let content = std::fs::read_to_string(home.join(".codex").join("config.toml")).ok()?;
    let doc: toml::Value = toml::from_str(&content).ok()?;
    doc.get("env")?
        .get("OPENAI_BASE_URL")?
        .as_str()
        .map(str::to_string)
}

fn shell_gemini_api_base() -> Option<String> {
    let home = dirs::home_dir()?;
    for rc in [home.join(".zshrc"), home.join(".bashrc")] {
        let Ok(content) = std::fs::read_to_string(rc) else {
            continue;
        };
        if let Some(value) = extract_shell_assignment(&content, "GEMINI_API_BASE_URL") {
            return Some(value);
        }
    }
    None
}

fn extract_shell_assignment(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let mut trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("export ") {
            trimmed = rest.trim_start();
        }
        let Some(rest) = trimmed.strip_prefix(key) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        let value = strip_matching_shell_quotes(value.trim());
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn strip_matching_shell_quotes(value: &str) -> &str {
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|unquoted| unquoted.strip_suffix('"'))
    {
        return inner;
    }
    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|unquoted| unquoted.strip_suffix('\''))
    {
        return inner;
    }
    value
}

fn normalize_doctor_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn is_local_proxy_url(value: &str) -> bool {
    value.starts_with("http://127.0.0.1:")
        || value.starts_with("http://localhost:")
        || value.starts_with("http://[::1]:")
}

fn url_is_local_proxy(value: Option<&str>) -> bool {
    value
        .map(normalize_doctor_url)
        .is_some_and(|url| is_local_proxy_url(&url))
}

fn cache_safety_outcome() -> Outcome {
    use crate::core::neural::cache_alignment::CacheAlignedOutput;
    use crate::core::provider_cache::ProviderCacheState;

    let mut issues = Vec::new();

    let mut aligned = CacheAlignedOutput::new();
    aligned.add_stable_block("test", "stable content".into(), 1);
    aligned.add_variable_block("test_var", "variable content".into(), 1);
    let rendered = aligned.render();
    if rendered.find("stable content").unwrap_or(usize::MAX)
        > rendered.find("variable content").unwrap_or(0)
    {
        issues.push("cache_alignment: stable blocks not ordered first");
    }

    let mut state = ProviderCacheState::new();
    let section = crate::core::provider_cache::CacheableSection::new(
        "doctor_test",
        "test content".into(),
        crate::core::provider_cache::SectionPriority::System,
        true,
    );
    state.mark_sent(&section);
    if state.needs_update(&section) {
        issues.push("provider_cache: hash tracking broken");
    }

    if issues.is_empty() {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Cache safety{RST}  {GREEN}cache_alignment + provider_cache operational{RST}"
            ),
        }
    } else {
        Outcome {
            ok: false,
            line: format!("{BOLD}Cache safety{RST}  {RED}{}{RST}", issues.join("; ")),
        }
    }
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
    let (sub, rest) = match args.first().map(String::as_str) {
        Some("integrations") => ("integrations", &args[1..]),
        _ => ("", args),
    };

    let fix = rest.iter().any(|a| a == "--fix");
    let json = rest.iter().any(|a| a == "--json");
    let help = rest.iter().any(|a| a == "--help" || a == "-h");

    if help {
        println!("Usage:");
        println!("  lean-ctx doctor");
        println!("  lean-ctx doctor integrations [--json]");
        println!("  lean-ctx doctor --fix [--json]");
        return 0;
    }

    if sub == "integrations" {
        if fix {
            let _ = fix::run_fix(&fix::DoctorFixOptions { json: false });
        }
        return integrations::run_integrations(&integrations::IntegrationsOptions { json });
    }

    if !fix {
        run();
        return 0;
    }

    match fix::run_fix(&fix::DoctorFixOptions { json }) {
        Ok(code) => code,
        Err(e) => {
            tracing::error!("doctor --fix failed: {e}");
            2
        }
    }
}

pub fn compact_score() -> (u32, u32) {
    let mut passed = 0u32;
    let total = 6u32;

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
    if skill_files_outcome().ok {
        passed += 1;
    }

    (passed, total)
}

pub(super) fn print_compact_status(passed: u32, total: u32) {
    let status = if passed == total {
        format!("{GREEN}✓ All {total} checks passed{RST}")
    } else {
        format!("{YELLOW}{passed}/{total} passed{RST} — run {BOLD}lean-ctx doctor{RST} for details")
    };
    println!("  {status}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_upstream_outcome_accepts_custom_anthropic_upstream() {
        let mut cfg = crate::core::config::Config::default();
        cfg.proxy.anthropic_upstream = Some("https://gateway.example.test/api/code".to_string());

        let outcome = proxy_upstream_outcome_for_config(&cfg);
        assert!(outcome.ok);
        assert!(outcome.line.contains("custom upstream configured"));
        assert!(outcome
            .line
            .contains("Anthropic=https://gateway.example.test/api/code"));
    }

    #[test]
    fn proxy_upstream_outcome_rejects_local_proxy_loop() {
        let mut cfg = crate::core::config::Config::default();
        cfg.proxy.anthropic_upstream = Some("http://127.0.0.1:4444".to_string());

        let outcome = proxy_upstream_outcome_for_config(&cfg);
        assert!(!outcome.ok);
        assert!(outcome.line.contains("points back to local proxy"));
    }

    #[test]
    fn proxy_upstream_outcome_rejects_invalid_openai_upstream() {
        let mut cfg = crate::core::config::Config::default();
        cfg.proxy.openai_upstream = Some("not-a-url".to_string());

        let outcome = proxy_upstream_outcome_for_config(&cfg);
        assert!(!outcome.ok);
        assert!(outcome.line.contains("invalid OpenAI upstream"));
    }

    #[test]
    fn proxy_upstream_outcome_accepts_multiple_custom_upstreams() {
        let mut cfg = crate::core::config::Config::default();
        cfg.proxy.openai_upstream = Some("https://openai.example.test".to_string());
        cfg.proxy.gemini_upstream = Some("https://gemini.example.test".to_string());

        let outcome = proxy_upstream_outcome_for_config(&cfg);
        assert!(outcome.ok);
        assert!(outcome.line.contains("OpenAI=https://openai.example.test"));
        assert!(outcome.line.contains("Gemini=https://gemini.example.test"));
    }

    #[test]
    fn extract_shell_assignment_reads_exported_gemini_base() {
        let content = r#"
export OTHER=value
export GEMINI_API_BASE_URL="http://127.0.0.1:4444"
"#;

        let value = extract_shell_assignment(content, "GEMINI_API_BASE_URL");

        assert_eq!(value.as_deref(), Some("http://127.0.0.1:4444"));
    }
}
