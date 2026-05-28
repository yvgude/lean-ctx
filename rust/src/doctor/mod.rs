//! Environment diagnostics for lean-ctx installation and integration.

mod fix;
mod integrations;
mod workspace_scope;

use std::net::TcpListener;
use std::path::PathBuf;

pub(super) const GREEN: &str = "\x1b[32m";
pub(super) const RED: &str = "\x1b[31m";
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

fn is_active_shell_impl(rc_name: &str, shell: &str, is_windows: bool, is_powershell: bool) -> bool {
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
fn is_powershell_session() -> bool {
    std::env::var("PSModulePath").is_ok()
}

fn is_active_shell(rc_name: &str) -> bool {
    let shell = std::env::var("SHELL").unwrap_or_default();
    is_active_shell_impl(rc_name, &shell, cfg!(windows), is_powershell_session())
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

fn provider_outcome() -> Outcome {
    let registry = crate::core::providers::global_registry();
    let ids = registry.available_provider_ids();
    if ids.is_empty() {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Providers{RST}  {DIM}none configured (enable via [providers] in config.toml){RST}"
            ),
        };
    }
    let labels: Vec<String> = ids
        .iter()
        .map(|id| {
            if let Some(p) = registry.get(id) {
                if p.is_available() {
                    format!("{GREEN}{id}{RST}")
                } else {
                    format!("{YELLOW}{id}(no auth){RST}")
                }
            } else {
                format!("{RED}{id}(missing){RST}")
            }
        })
        .collect();
    Outcome {
        ok: true,
        line: format!("{BOLD}Providers{RST}  {}", labels.join(", ")),
    }
}

fn mcp_bridge_outcomes() -> Vec<Outcome> {
    let cfg = crate::core::config::Config::load();
    let bridges = &cfg.providers.mcp_bridges;
    if bridges.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();

    let auto_idx = if cfg.providers.auto_index {
        format!("{GREEN}auto_index=true{RST}")
    } else {
        format!("{YELLOW}auto_index=false (provider data won't be indexed into BM25/Graph/Knowledge){RST}")
    };
    results.push(Outcome {
        ok: cfg.providers.auto_index,
        line: format!("{BOLD}Provider indexing{RST}  {auto_idx}"),
    });

    for (name, entry) in bridges {
        let url = entry.url.as_deref().unwrap_or("");
        let cmd = entry.command.as_deref().unwrap_or("");
        let source = if !url.is_empty() {
            format!("url={url}")
        } else if !cmd.is_empty() {
            format!("cmd={cmd}")
        } else {
            "no url/command".to_string()
        };

        let ok = !url.is_empty() || !cmd.is_empty();
        let status = if ok {
            format!("{GREEN}configured{RST}")
        } else {
            format!("{RED}missing url/command{RST}")
        };

        results.push(Outcome {
            ok,
            line: format!("{BOLD}MCP Bridge{RST}  mcp:{name} ({source}) [{status}]"),
        });
    }

    results
}

fn plan_mode_outcomes() -> Vec<Outcome> {
    let status = crate::core::editor_registry::plan_mode::check_plan_mode_status();
    let mut results = Vec::new();

    if let Some(configured) = status.vscode_configured {
        if configured {
            results.push(Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Plan mode{RST}  VS Code  {GREEN}planAgent tools configured{RST}"
                ),
            });
        } else {
            results.push(Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Plan mode{RST}  VS Code  {YELLOW}not configured{RST}  {DIM}(run: lean-ctx setup){RST}"
                ),
            });
        }
    }

    if let Some(configured) = status.claude_configured {
        if configured {
            results.push(Outcome {
                ok: true,
                line: format!("{BOLD}Plan mode{RST}  Claude Code  {GREEN}permissions present{RST}"),
            });
        } else {
            results.push(Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Plan mode{RST}  Claude Code  {YELLOW}not configured{RST}  {DIM}(run: lean-ctx setup){RST}"
                ),
            });
        }
    }

    results
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
    let total = 10u32;

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

    let split_dirs = crate::core::data_dir::all_data_dirs_with_stats();
    if split_dirs.len() >= 2 {
        let dirs_str = split_dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        print_check(&Outcome {
            ok: false,
            line: format!(
                "{BOLD}data dir split{RST}  {RED}stats.json found in {count} locations{RST}: {dirs_str}  {DIM}(run: lean-ctx setup to auto-merge){RST}",
                count = split_dirs.len(),
            ),
        });
    }

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

    // 6) Proxy upstreams
    let proxy_outcome = proxy_upstream_outcome();
    if proxy_outcome.ok {
        passed += 1;
    }
    print_check(&proxy_outcome);

    // 7) Shell aliases
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

    // 8) Workspace-scope MCP (optional; only when a project-local config exists)
    let workspace_scope = workspace_scope::workspace_scope_outcome(mcp.ok);
    if let Some(ref ws) = workspace_scope {
        if ws.ok {
            passed += 1;
        }
        print_check(ws);
    }

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
    let daemon_outcome = {
        let autostart = crate::daemon_autostart::is_installed();
        let autostart_tag = if autostart {
            format!("  {DIM}[autostart: on]{RST}")
        } else {
            String::new()
        };
        if crate::daemon::is_daemon_running() {
            let pid_path = crate::daemon::daemon_pid_path();
            let pid_str = std::fs::read_to_string(&pid_path).unwrap_or_default();
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Daemon{RST}  {GREEN}running (PID {}){RST}{autostart_tag}",
                    pid_str.trim()
                ),
            }
        } else {
            let hint = if autostart {
                format!("{DIM}(autostart enabled, will restart){RST}")
            } else {
                format!("{DIM}(run: lean-ctx daemon start  or: lean-ctx daemon enable){RST}")
            };
            Outcome {
                ok: true,
                line: format!("{BOLD}Daemon{RST}  {YELLOW}not running{RST}  {hint}"),
            }
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

    // Daemon diagnostics: systemctl is-active, linger, crash-loop log
    #[cfg(target_os = "linux")]
    {
        if let Ok(o) = std::process::Command::new("systemctl")
            .args(["--user", "is-active", "lean-ctx-daemon.service"])
            .output()
        {
            let state = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if state != "active" {
                println!(
                    "  {DIM}  systemd unit state: {YELLOW}{state}{RST}{DIM} (expected: active){RST}"
                );
            }
        }
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "$(whoami)".to_string());
        if let Ok(o) = std::process::Command::new("loginctl")
            .args(["show-user", &username, "-p", "Linger", "--value"])
            .output()
        {
            let val = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if val != "yes" {
                println!(
                    "  {YELLOW}⚠{RST}  Linger not enabled — daemon won't start at boot without login"
                );
                println!("     {DIM}Fix: loginctl enable-linger {username}{RST}");
            }
        }
    }
    if let Some(log_path) = crate::core::startup_guard::crash_loop_log_path(
        crate::core::startup_guard::MCP_PROCESS_NAME,
    ) {
        if log_path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&log_path) {
                let lines: Vec<&str> = contents.lines().collect();
                if lines.len() >= 5 {
                    println!(
                        "  {YELLOW}⚠{RST}  Crash-loop log: {} recent restarts  {DIM}({}){RST}",
                        lines.len(),
                        log_path.display()
                    );
                }
            }
        }
    }

    // Providers
    let provider_outcome = provider_outcome();
    print_check(&provider_outcome);

    // MCP Bridges
    let bridge_outcomes = mcp_bridge_outcomes();
    for bridge_check in &bridge_outcomes {
        print_check(bridge_check);
    }

    // Plan mode
    let plan_outcomes = plan_mode_outcomes();
    for plan_check in &plan_outcomes {
        print_check(plan_check);
    }

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

    // 13) Cache safety
    let cache_safety = cache_safety_outcome();
    if cache_safety.ok {
        passed += 1;
    }
    print_check(&cache_safety);

    // 14) Claude Code instruction truncation guard
    let claude_truncation = claude_truncation_outcome();
    if let Some(ref ct) = claude_truncation {
        if ct.ok {
            passed += 1;
        }
        print_check(ct);
    }

    // 15) BM25 cache health
    let bm25_health = bm25_cache_health_outcome();
    if bm25_health.ok {
        passed += 1;
    }
    print_check(&bm25_health);

    // 16) Memory profile
    let mem_profile = memory_profile_outcome();
    passed += 1;
    print_check(&mem_profile);

    // 17) Memory cleanup
    let mem_cleanup = memory_cleanup_outcome();
    passed += 1;
    print_check(&mem_cleanup);

    // 18) RAM Guardian
    let ram_outcome = ram_guardian_outcome();
    if ram_outcome.ok {
        passed += 1;
    }
    print_check(&ram_outcome);

    // 19) Capacity warnings (memory stores near limits)
    let cap_warnings = capacity_warnings();
    for cw in &cap_warnings {
        if cw.ok {
            passed += 1;
        }
        print_check(cw);
    }

    // 20) Proxy health
    let proxy_health = proxy_health_outcome();
    if proxy_health.ok {
        passed += 1;
    }
    print_check(&proxy_health);

    // 20) Stale proxy env (ANTHROPIC_BASE_URL pointing to local proxy while proxy is not enabled)
    let stale_env = stale_proxy_env_outcome();
    if let Some(ref check) = stale_env {
        if check.ok {
            passed += 1;
        }
        print_check(check);
    }

    // LSP servers (optional, informational)
    println!("\n  {BOLD}{WHITE}LSP (optional — for ctx_refactor):{RST}");
    let lsp_outcomes = lsp_server_outcomes();
    for lsp_check in &lsp_outcomes {
        print_check(lsp_check);
    }

    let mut effective_total = total + 9; // session_state + integrity + cache_safety + bm25_health + daemon + mem_profile + mem_cleanup + ram_guardian + proxy_health
    effective_total += cap_warnings.len() as u32;
    effective_total += docker_outcomes.len() as u32;
    if pi.is_some() {
        effective_total += 1;
    }
    if claude_truncation.is_some() {
        effective_total += 1;
    }
    if stale_env.is_some() {
        effective_total += 1;
    }
    if workspace_scope.is_some() {
        effective_total += 1;
    }
    println!();
    println!("  {BOLD}{WHITE}Summary:{RST}  {GREEN}{passed}{RST}{DIM}/{effective_total}{RST} checks passed");
    println!("  {DIM}LSP servers are optional enhancements (not counted in score){RST}");
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
        (
            "Codex CLI",
            crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("skills/lean-ctx/SKILL.md"),
        ),
        (
            "GitHub Copilot",
            home.join(".copilot/skills/lean-ctx/SKILL.md"),
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

fn proxy_auth_probe(port: u16) -> bool {
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

fn proxy_health_outcome() -> Outcome {
    use crate::core::config::Config;

    let cfg = Config::load();
    let port = crate::proxy_setup::default_port();

    match cfg.proxy_enabled {
        Some(true) => {
            let installed = crate::proxy_autostart::is_installed();
            let reachable = {
                use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
                let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
                TcpStream::connect_timeout(&addr, crate::proxy_setup::proxy_timeout()).is_ok()
            };

            if installed && reachable {
                // Verify auth works: probe /health (no auth needed) to confirm HTTP layer
                let auth_ok = proxy_auth_probe(port);
                if auth_ok {
                    Outcome {
                        ok: true,
                        line: format!(
                            "{BOLD}Proxy{RST}  {GREEN}enabled, running on port {port}{RST}"
                        ),
                    }
                } else {
                    Outcome {
                        ok: false,
                        line: format!(
                            "{BOLD}Proxy{RST}  {YELLOW}running on port {port} but auth probe failed{RST}  {YELLOW}fix: lean-ctx proxy restart{RST}"
                        ),
                    }
                }
            } else if installed && !reachable {
                Outcome {
                    ok: false,
                    line: format!(
                        "{BOLD}Proxy{RST}  {RED}enabled but not reachable on port {port}{RST}  {YELLOW}fix: lean-ctx proxy start{RST}"
                    ),
                }
            } else {
                Outcome {
                    ok: false,
                    line: format!(
                        "{BOLD}Proxy{RST}  {RED}enabled but autostart not installed{RST}  {YELLOW}fix: lean-ctx proxy enable{RST}"
                    ),
                }
            }
        }
        Some(false) => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Proxy{RST}  {DIM}disabled (optional feature){RST}  {DIM}enable: lean-ctx proxy enable{RST}"
            ),
        },
        None => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Proxy{RST}  {DIM}not configured{RST}  {DIM}enable: lean-ctx proxy enable{RST}"
            ),
        },
    }
}

/// Detects stale `ANTHROPIC_BASE_URL` in Claude Code settings pointing to the local
/// lean-ctx proxy when the proxy is not enabled. Returns `None` when no mismatch exists
/// (no check needed), `Some(Outcome)` when a stale URL is found.
fn stale_proxy_env_outcome() -> Option<Outcome> {
    use crate::core::config::Config;

    let home = dirs::home_dir()?;
    let cfg = Config::load();
    let port = crate::proxy_setup::default_port();

    if cfg.proxy_enabled == Some(true) {
        return None;
    }

    let settings_dir = crate::core::editor_registry::claude_state_dir(&home);
    let settings_path = settings_dir.join("settings.json");
    let content = std::fs::read_to_string(&settings_path).ok()?;
    let doc: serde_json::Value = crate::core::jsonc::parse_jsonc(&content).ok()?;

    let base_url = doc
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if base_url.is_empty() {
        return None;
    }

    let local_proxy = format!("http://127.0.0.1:{port}");
    let is_local = base_url == local_proxy
        || base_url == format!("http://localhost:{port}")
        || base_url.starts_with("http://127.0.0.1:")
        || base_url.starts_with("http://localhost:");

    if !is_local {
        return None;
    }

    let state = if cfg.proxy_enabled == Some(false) {
        "disabled"
    } else {
        "not configured"
    };

    Some(Outcome {
        ok: false,
        line: format!(
            "{BOLD}Proxy env{RST}  {RED}ANTHROPIC_BASE_URL → {base_url} but proxy is {state}{RST}\n\
             {DIM}         Claude Code routes API traffic to lean-ctx, but lean-ctx proxy is {state}.{RST}\n\
             {DIM}         This causes 401 auth failures. Fix:{RST}\n\
             {YELLOW}           lean-ctx proxy cleanup    {DIM}(remove stale URL){RST}\n\
             {YELLOW}           lean-ctx proxy enable     {DIM}(enable the proxy){RST}"
        ),
    })
}

fn proxy_upstream_outcome() -> Outcome {
    use crate::core::config::{is_local_proxy_url, Config, ProxyProvider};

    let cfg = Config::load();
    let checks = [
        (
            "Anthropic",
            "proxy.anthropic_upstream",
            cfg.proxy.resolve_upstream(ProxyProvider::Anthropic),
        ),
        (
            "OpenAI",
            "proxy.openai_upstream",
            cfg.proxy.resolve_upstream(ProxyProvider::OpenAi),
        ),
        (
            "Gemini",
            "proxy.gemini_upstream",
            cfg.proxy.resolve_upstream(ProxyProvider::Gemini),
        ),
    ];

    let mut custom = Vec::new();
    for (label, key, resolved) in &checks {
        if is_local_proxy_url(resolved) {
            return Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Proxy upstream{RST}  {RED}{label} upstream points back to local proxy{RST}  {YELLOW}run: lean-ctx config set {key} <url>{RST}"
                ),
            };
        }
        if !resolved.starts_with("http://") && !resolved.starts_with("https://") {
            return Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Proxy upstream{RST}  {RED}invalid {label} upstream{RST}  {YELLOW}set {key} to an http(s) URL{RST}"
                ),
            };
        }
        let is_default = matches!(
            *label,
            "Anthropic" if resolved == "https://api.anthropic.com"
        ) || matches!(
            *label,
            "OpenAI" if resolved == "https://api.openai.com"
        ) || matches!(
            *label,
            "Gemini" if resolved == "https://generativelanguage.googleapis.com"
        );
        if !is_default {
            custom.push(format!("{label}={resolved}"));
        }
    }

    if custom.is_empty() {
        Outcome {
            ok: true,
            line: format!("{BOLD}Proxy upstream{RST}  {GREEN}provider defaults{RST}"),
        }
    } else {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Proxy upstream{RST}  {GREEN}custom: {}{RST}",
                custom.join(", ")
            ),
        }
    }
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

fn bm25_cache_health_outcome() -> Outcome {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return Outcome {
            ok: true,
            line: format!("{BOLD}BM25 cache{RST}  {DIM}skipped (no data dir){RST}"),
        };
    };

    let vectors_dir = data_dir.join("vectors");
    let Ok(entries) = std::fs::read_dir(&vectors_dir) else {
        return Outcome {
            ok: true,
            line: format!("{BOLD}BM25 cache{RST}  {GREEN}no vector dirs{RST}"),
        };
    };

    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    let effective_mb = if cfg.bm25_max_cache_mb == crate::core::config::default_bm25_max_cache_mb()
    {
        profile.bm25_max_cache_mb()
    } else {
        cfg.bm25_max_cache_mb
    };
    let max_bytes = effective_mb * 1024 * 1024;
    let warn_bytes = max_bytes * 80 / 100; // 80% of effective limit
    let mut total_dirs = 0u32;
    let mut total_bytes = 0u64;
    let mut oversized: Vec<(String, u64)> = Vec::new();
    let mut warnings: Vec<(String, u64)> = Vec::new();
    let mut quarantined_count = 0u32;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        total_dirs += 1;

        if dir.join("bm25_index.json.quarantined").exists()
            || dir.join("bm25_index.bin.quarantined").exists()
            || dir.join("bm25_index.bin.zst.quarantined").exists()
        {
            quarantined_count += 1;
        }

        let index_path = if dir.join("bm25_index.bin.zst").exists() {
            dir.join("bm25_index.bin.zst")
        } else if dir.join("bm25_index.bin").exists() {
            dir.join("bm25_index.bin")
        } else {
            dir.join("bm25_index.json")
        };
        if let Ok(meta) = std::fs::metadata(&index_path) {
            let size = meta.len();
            total_bytes += size;
            let display = index_path.display().to_string();
            if size > max_bytes {
                oversized.push((display, size));
            } else if size > warn_bytes {
                warnings.push((display, size));
            }
        }
    }

    if !oversized.is_empty() {
        let details: Vec<String> = oversized
            .iter()
            .map(|(p, s)| format!("{p} ({:.1} GB)", *s as f64 / 1_073_741_824.0))
            .collect();
        return Outcome {
            ok: false,
            line: format!(
                "{BOLD}BM25 cache{RST}  {RED}{} index(es) exceed limit ({:.0} MB){RST}: {}  {DIM}(run: lean-ctx cache prune){RST}",
                oversized.len(),
                max_bytes / (1024 * 1024),
                details.join(", ")
            ),
        };
    }

    if !warnings.is_empty() {
        let details: Vec<String> = warnings
            .iter()
            .map(|(p, s)| format!("{p} ({:.0} MB)", *s as f64 / 1_048_576.0))
            .collect();
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}BM25 cache{RST}  {YELLOW}{} index(es) >80% of {effective_mb} MB limit{RST}: {}  {DIM}(consider extra_ignore_patterns){RST}",
                warnings.len(),
                details.join(", ")
            ),
        };
    }

    let quarantine_note = if quarantined_count > 0 {
        format!("  {YELLOW}{quarantined_count} quarantined (run: lean-ctx cache prune){RST}")
    } else {
        String::new()
    };

    Outcome {
        ok: true,
        line: format!(
            "{BOLD}BM25 cache{RST}  {GREEN}{total_dirs} index(es), {:.1} MB total{RST}{quarantine_note}",
            total_bytes as f64 / 1_048_576.0
        ),
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

fn memory_profile_outcome() -> Outcome {
    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    let (label, detail) = match profile {
        crate::core::config::MemoryProfile::Low => {
            ("low", "embeddings+semantic cache disabled, BM25 64 MB")
        }
        crate::core::config::MemoryProfile::Balanced => {
            ("balanced", "default — BM25 128 MB, single embedding engine")
        }
        crate::core::config::MemoryProfile::Performance => {
            ("performance", "full caches, BM25 512 MB")
        }
    };
    let source = if crate::core::config::MemoryProfile::from_env().is_some() {
        "env"
    } else if cfg.memory_profile != crate::core::config::MemoryProfile::default() {
        "config"
    } else {
        "default"
    };
    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Memory profile{RST}  {GREEN}{label}{RST}  {DIM}({source}: {detail}){RST}"
        ),
    }
}

fn memory_cleanup_outcome() -> Outcome {
    let cfg = crate::core::config::Config::load();
    let cleanup = crate::core::config::MemoryCleanup::effective(&cfg);
    let (label, detail) = match cleanup {
        crate::core::config::MemoryCleanup::Aggressive => (
            "aggressive",
            "cache cleared after 5 min idle, single-IDE optimized",
        ),
        crate::core::config::MemoryCleanup::Shared => (
            "shared",
            "cache retained 30 min, multi-IDE/multi-model optimized",
        ),
    };
    let source = if crate::core::config::MemoryCleanup::from_env().is_some() {
        "env"
    } else if cfg.memory_cleanup != crate::core::config::MemoryCleanup::default() {
        "config"
    } else {
        "default"
    };
    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Memory cleanup{RST}  {GREEN}{label}{RST}  {DIM}({source}: {detail}){RST}"
        ),
    }
}

fn ram_guardian_outcome() -> Outcome {
    let Some(snap) = crate::core::memory_guard::MemorySnapshot::capture() else {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}RAM Guardian{RST}  {YELLOW}not available{RST}  {DIM}(platform unsupported){RST}"
            ),
        };
    };
    let allocator = if cfg!(all(feature = "jemalloc", not(windows))) {
        "jemalloc"
    } else {
        "system"
    };
    let ok = snap.pressure_level == crate::core::memory_guard::PressureLevel::Normal;
    let color = if ok { GREEN } else { RED };
    let pressure_hint = match snap.pressure_level {
        crate::core::memory_guard::PressureLevel::Normal => String::new(),
        level => {
            format!(
                "  {YELLOW}pressure={level:?} — consider: memory_profile=\"low\" or increase max_ram_percent{RST}"
            )
        }
    };
    Outcome {
        ok,
        line: format!(
            "{BOLD}RAM Guardian{RST}  {color}{:.0} MB{RST} / {:.1} GB system ({:.1}%)  {DIM}limit: {:.0} MB ({allocator}){RST}{pressure_hint}",
            snap.rss_bytes as f64 / 1_048_576.0,
            snap.system_ram_bytes as f64 / 1_073_741_824.0,
            snap.rss_percent,
            snap.rss_limit_bytes as f64 / 1_048_576.0,
        ),
    }
}

fn capacity_warnings() -> Vec<Outcome> {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return vec![];
    };

    let cfg = crate::core::config::Config::load();
    let policy = cfg.memory_policy_effective().unwrap_or_default();

    let knowledge_dir = data_dir.join("knowledge");
    let Ok(entries) = std::fs::read_dir(&knowledge_dir) else {
        return vec![Outcome {
            ok: true,
            line: format!("{BOLD}Capacity{RST} {GREEN}no memory stores{RST}"),
        }];
    };

    let mut results = Vec::new();

    for entry in entries.flatten() {
        let hash_dir = entry.path();
        if !hash_dir.is_dir() {
            continue;
        }
        let hash = hash_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let short_hash = &hash[..hash.len().min(8)];

        let mut checks: Vec<(String, usize, usize)> = Vec::new();

        if let Ok(content) = std::fs::read_to_string(hash_dir.join("knowledge.json")) {
            if let Ok(k) =
                serde_json::from_str::<crate::core::knowledge::ProjectKnowledge>(&content)
            {
                checks.push((
                    "facts".to_string(),
                    k.facts.len(),
                    policy.knowledge.max_facts,
                ));
                checks.push((
                    "patterns".to_string(),
                    k.patterns.len(),
                    policy.knowledge.max_patterns,
                ));
                checks.push((
                    "history".to_string(),
                    k.history.len(),
                    policy.knowledge.max_history,
                ));
            }
        }

        if let Ok(content) = std::fs::read_to_string(hash_dir.join("embeddings.json")) {
            if let Ok(idx) = serde_json::from_str::<
                crate::core::knowledge_embedding::KnowledgeEmbeddingIndex,
            >(&content)
            {
                checks.push((
                    "embeddings".to_string(),
                    idx.entries.len(),
                    policy.embeddings.max_facts,
                ));
            }
        }

        if let Ok(content) = std::fs::read_to_string(hash_dir.join("gotchas.json")) {
            if let Ok(g) =
                serde_json::from_str::<crate::core::gotcha_tracker::GotchaStore>(&content)
            {
                checks.push((
                    "gotchas".to_string(),
                    g.gotchas.len(),
                    policy.gotcha.max_gotchas_per_project,
                ));
            }
        }

        let episodes_path = data_dir
            .join("memory")
            .join("episodes")
            .join(format!("{hash}.json"));
        if let Ok(content) = std::fs::read_to_string(&episodes_path) {
            if let Ok(e) =
                serde_json::from_str::<crate::core::episodic_memory::EpisodicStore>(&content)
            {
                checks.push((
                    "episodes".to_string(),
                    e.episodes.len(),
                    policy.episodic.max_episodes,
                ));
            }
        }

        let procedures_path = data_dir
            .join("memory")
            .join("procedures")
            .join(format!("{hash}.json"));
        if let Ok(content) = std::fs::read_to_string(&procedures_path) {
            if let Ok(p) =
                serde_json::from_str::<crate::core::procedural_memory::ProceduralStore>(&content)
            {
                checks.push((
                    "procedures".to_string(),
                    p.procedures.len(),
                    policy.procedural.max_procedures,
                ));
            }
        }

        let mut warnings: Vec<String> = Vec::new();
        let mut critical = false;

        for (name, current, limit) in &checks {
            if *limit == 0 {
                continue;
            }
            let pct = (*current as f64 / *limit as f64 * 100.0) as u32;
            if pct >= 95 {
                critical = true;
                warnings.push(format!("{name}: {current}/{limit} ({pct}%)"));
            } else if pct >= 80 {
                warnings.push(format!("{name}: {current}/{limit} ({pct}%)"));
            }
        }

        if !warnings.is_empty() {
            let color = if critical { RED } else { YELLOW };
            let label = if critical { "CRIT" } else { "WARN" };
            results.push(Outcome {
                ok: !critical,
                line: format!(
                    "{BOLD}Capacity [{short_hash}]{RST} {color}{label}: {}{RST}",
                    warnings.join(", ")
                ),
            });
        }
    }

    // Global checks (not per project hash)

    // Archive disk usage vs limit
    let archive_limit_bytes = cfg.archive_max_disk_mb_effective() * 1_048_576;
    if archive_limit_bytes > 0 {
        let archive_used = crate::core::archive::disk_usage_bytes();
        let pct = (archive_used as f64 / archive_limit_bytes as f64 * 100.0) as u32;
        if pct >= 95 {
            results.push(Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Capacity [archive]{RST} {RED}CRIT: disk {}/{}MB ({pct}%){RST}",
                    archive_used / 1_048_576,
                    archive_limit_bytes / 1_048_576
                ),
            });
        } else if pct >= 80 {
            results.push(Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Capacity [archive]{RST} {YELLOW}WARN: disk {}/{}MB ({pct}%){RST}",
                    archive_used / 1_048_576,
                    archive_limit_bytes / 1_048_576
                ),
            });
        }
    }

    // Graph index file count vs limit
    let graph_max_files = cfg.graph_index_max_files;
    if graph_max_files > 0 {
        if let Some(session) = crate::core::session::SessionState::load_latest() {
            if let Some(ref project_root) = session.project_root {
                let disk_status = crate::core::index_orchestrator::disk_status(project_root);
                if let Some(graph_files) = disk_status.graph_index.file_count {
                    let pct = (graph_files as f64 / graph_max_files as f64 * 100.0) as u32;
                    if pct >= 95 {
                        results.push(Outcome {
                            ok: false,
                            line: format!(
                                "{BOLD}Capacity [graph]{RST} {RED}CRIT: files {graph_files}/{graph_max_files} ({pct}%){RST}"
                            ),
                        });
                    } else if pct >= 80 {
                        results.push(Outcome {
                            ok: true,
                            line: format!(
                                "{BOLD}Capacity [graph]{RST} {YELLOW}WARN: files {graph_files}/{graph_max_files} ({pct}%){RST}"
                            ),
                        });
                    }
                }
            }
        }
    }

    if results.is_empty() {
        results.push(Outcome {
            ok: true,
            line: format!("{BOLD}Capacity{RST} {GREEN}all stores within limits{RST}"),
        });
    }

    results
}

fn lsp_server_outcomes() -> Vec<Outcome> {
    use crate::lsp::config::{find_binary_in_path, KNOWN_SERVERS};

    KNOWN_SERVERS
        .iter()
        .map(|info| {
            let found = find_binary_in_path(info.binary);
            match found {
                Some(path) => Outcome {
                    ok: true,
                    line: format!(
                        "{BOLD}{}{RST}  {GREEN}✓ {}{RST}  {DIM}{}{RST}",
                        info.language,
                        info.binary,
                        path.display()
                    ),
                },
                None => Outcome {
                    ok: false,
                    line: format!(
                        "{BOLD}{}{RST}  {DIM}not installed{RST}  {YELLOW}{}{RST}",
                        info.language, info.install_hint
                    ),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::is_active_shell_impl;

    fn make_capacity_check(name: &str, current: usize, limit: usize) -> Option<(bool, String)> {
        if limit == 0 {
            return None;
        }
        let pct = (current as f64 / limit as f64 * 100.0) as u32;
        if pct >= 95 {
            Some((true, format!("{name}: {current}/{limit} ({pct}%)")))
        } else if pct >= 80 {
            Some((false, format!("{name}: {current}/{limit} ({pct}%)")))
        } else {
            None
        }
    }

    #[test]
    fn capacity_below_80_no_warning() {
        assert!(make_capacity_check("facts", 100, 200).is_none());
        assert!(make_capacity_check("facts", 159, 200).is_none());
    }

    #[test]
    fn capacity_at_80_yellow_warning() {
        let result = make_capacity_check("facts", 160, 200);
        assert!(result.is_some());
        let (critical, msg) = result.unwrap();
        assert!(!critical);
        assert!(msg.contains("160/200"));
        assert!(msg.contains("80%"));
    }

    #[test]
    fn capacity_at_92_yellow_warning() {
        let result = make_capacity_check("facts", 185, 200);
        assert!(result.is_some());
        let (critical, msg) = result.unwrap();
        assert!(!critical);
        assert!(msg.contains("185/200"));
        assert!(msg.contains("92%"));
    }

    #[test]
    fn capacity_at_95_critical() {
        let result = make_capacity_check("facts", 190, 200);
        assert!(result.is_some());
        let (critical, msg) = result.unwrap();
        assert!(critical);
        assert!(msg.contains("190/200"));
        assert!(msg.contains("95%"));
    }

    #[test]
    fn capacity_at_100_critical() {
        let result = make_capacity_check("facts", 200, 200);
        assert!(result.is_some());
        let (critical, _) = result.unwrap();
        assert!(critical);
    }

    #[test]
    fn capacity_zero_limit_skipped() {
        assert!(make_capacity_check("facts", 50, 0).is_none());
    }

    #[test]
    fn bashrc_active_on_non_windows_when_shell_empty() {
        assert!(is_active_shell_impl("~/.bashrc", "", false, false));
    }

    #[test]
    fn bashrc_not_active_on_windows_when_shell_empty() {
        assert!(!is_active_shell_impl("~/.bashrc", "", true, false));
    }

    #[test]
    fn bashrc_active_when_shell_contains_bash_on_linux() {
        assert!(is_active_shell_impl(
            "~/.bashrc",
            "/usr/bin/bash",
            false,
            false
        ));
    }

    #[test]
    fn bashrc_not_active_on_windows_even_with_bash_in_shell_env() {
        // Issue #214: On Windows, Git Bash sets $SHELL globally to bash.exe.
        // .bashrc should NOT be flagged on Windows unless actually inside bash.
        std::env::remove_var("BASH_VERSION");
        assert!(!is_active_shell_impl(
            "~/.bashrc",
            "C:\\\\Program Files\\\\Git\\\\bin\\\\bash.exe",
            true,
            false,
        ));
    }

    #[test]
    fn bashrc_not_active_on_windows_powershell_even_with_bash_in_shell() {
        assert!(!is_active_shell_impl(
            "~/.bashrc",
            "C:\\\\Program Files\\\\Git\\\\bin\\\\bash.exe",
            true,
            true,
        ));
    }

    #[test]
    fn bashrc_not_active_on_windows_powershell_with_empty_shell() {
        assert!(!is_active_shell_impl("~/.bashrc", "", true, true));
    }

    #[test]
    fn zshrc_unaffected_by_powershell_flag() {
        assert!(is_active_shell_impl("~/.zshrc", "/bin/zsh", false, false));
        assert!(is_active_shell_impl("~/.zshrc", "/bin/zsh", true, true));
    }

    #[test]
    fn bashrc_not_active_on_windows_without_powershell_detection() {
        // Windows + $SHELL=bash but NOT in actual bash session (no BASH_VERSION).
        // This is the exact scenario from issue #214: Git Bash sets $SHELL globally.
        std::env::remove_var("BASH_VERSION");
        assert!(!is_active_shell_impl(
            "~/.bashrc",
            "/usr/bin/bash",
            true,
            false,
        ));
    }

    #[test]
    fn bashrc_active_on_linux() {
        assert!(is_active_shell_impl("~/.bashrc", "/bin/bash", false, false));
        assert!(is_active_shell_impl("~/.bashrc", "", false, false));
    }
}
