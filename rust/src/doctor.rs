//! Environment diagnostics for lean-ctx installation and integration.

use std::net::TcpListener;
use std::path::PathBuf;

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const BOLD: &str = "\x1b[1m";
const RST: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const WHITE: &str = "\x1b[97m";
const YELLOW: &str = "\x1b[33m";

const VERSION: &str = env!("CARGO_PKG_VERSION");

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
    #[cfg(unix)]
    {
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("command -v lean-ctx")
            .env("LEAN_CTX_ACTIVE", "1")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }

    #[cfg(windows)]
    {
        let output = std::process::Command::new("where.exe")
            .arg("lean-ctx")
            .env("LEAN_CTX_ACTIVE", "1")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        let exe_line = lines.iter().find(|l| l.ends_with(".exe"));
        let best = exe_line.or(lines.first()).map(|s| s.to_string());
        best.map(PathBuf::from)
    }
}

fn lean_ctx_version_from_path() -> Outcome {
    let resolved = resolve_lean_ctx_binary();
    let bin = resolved
        .clone()
        .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| "lean-ctx".into()));

    let try_run = |cmd: &std::path::Path| -> Result<String, String> {
        let output = std::process::Command::new(cmd)
            .args(["--version"])
            .env("LEAN_CTX_ACTIVE", "1")
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "exited with {}",
                output.status.code().unwrap_or(-1)
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err("empty output".to_string());
        }
        Ok(text)
    };

    match try_run(&bin) {
        Ok(text) => Outcome {
            ok: true,
            line: format!("{BOLD}lean-ctx version{RST}  {WHITE}{text}{RST}"),
        },
        Err(_first_err) => {
            #[cfg(windows)]
            {
                let candidates = [
                    bin.with_extension("exe"),
                    bin.parent()
                        .unwrap_or(std::path::Path::new("."))
                        .join("node_modules")
                        .join("lean-ctx-bin")
                        .join("bin")
                        .join("lean-ctx.exe"),
                ];
                for candidate in &candidates {
                    if candidate.is_file() {
                        if let Ok(text) = try_run(candidate) {
                            return Outcome {
                                ok: true,
                                line: format!(
                                    "{BOLD}lean-ctx version{RST}  {WHITE}{text}{RST}  {DIM}(via {}){RST}",
                                    candidate.display()
                                ),
                            };
                        }
                    }
                }
            }

            let current_exe_result = std::env::current_exe();
            if let Ok(ref exe) = current_exe_result {
                if exe != &bin {
                    if let Ok(text) = try_run(exe) {
                        return Outcome {
                            ok: true,
                            line: format!("{BOLD}lean-ctx version{RST}  {WHITE}{text}{RST}  {DIM}(this binary){RST}"),
                        };
                    }
                }
            }

            Outcome {
                ok: false,
                line: format!(
                    "{BOLD}lean-ctx version{RST}  {RED}failed to run `lean-ctx --version`: {_first_err}{RST}  {DIM}(resolved: {}){RST}",
                    bin.display()
                ),
            }
        }
    }
}

fn rc_contains_lean_ctx(path: &PathBuf) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => s.contains("lean-ctx"),
        Err(_) => false,
    }
}

fn rc_has_pipe_guard(path: &PathBuf) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            s.contains("! -t 1") || s.contains("isatty stdout") || s.contains("IsOutputRedirected")
        }
        Err(_) => false,
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
        if !rc_has_pipe_guard(&zsh) {
            needs_update.push("~/.zshrc");
        }
    }
    let bash = home.join(".bashrc");
    if rc_contains_lean_ctx(&bash) {
        parts.push(format!("{DIM}~/.bashrc{RST}"));
        if !rc_has_pipe_guard(&bash) {
            needs_update.push("~/.bashrc");
        }
    }

    let fish = home.join(".config").join("fish").join("config.fish");
    if rc_contains_lean_ctx(&fish) {
        parts.push(format!("{DIM}~/.config/fish/config.fish{RST}"));
        if !rc_has_pipe_guard(&fish) {
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
    display: &'static str,
    path: PathBuf,
}

fn mcp_config_locations(home: &std::path::Path) -> Vec<McpLocation> {
    let mut locations = vec![
        McpLocation {
            name: "Cursor",
            display: "~/.cursor/mcp.json",
            path: home.join(".cursor").join("mcp.json"),
        },
        McpLocation {
            name: "Claude Code",
            display: "~/.claude.json",
            path: home.join(".claude.json"),
        },
        McpLocation {
            name: "Windsurf",
            display: "~/.codeium/windsurf/mcp_config.json",
            path: home
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
        },
        McpLocation {
            name: "Codex",
            display: "~/.codex/config.toml",
            path: home.join(".codex").join("config.toml"),
        },
        McpLocation {
            name: "Gemini CLI",
            display: "~/.gemini/settings/mcp.json",
            path: home.join(".gemini").join("settings").join("mcp.json"),
        },
        McpLocation {
            name: "Antigravity",
            display: "~/.gemini/antigravity/mcp_config.json",
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
            display: "~/.config/zed/settings.json",
            path: zed_cfg,
        });
    }

    locations.push(McpLocation {
        name: "Qwen Code",
        display: "~/.qwen/mcp.json",
        path: home.join(".qwen").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Trae",
        display: "~/.trae/mcp.json",
        path: home.join(".trae").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Amazon Q",
        display: "~/.aws/amazonq/mcp.json",
        path: home.join(".aws").join("amazonq").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "JetBrains",
        display: "~/.jb-mcp.json",
        path: home.join(".jb-mcp.json"),
    });
    locations.push(McpLocation {
        name: "AWS Kiro",
        display: "~/.kiro/settings/mcp.json",
        path: home.join(".kiro").join("settings").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Verdent",
        display: "~/.verdent/mcp.json",
        path: home.join(".verdent").join("mcp.json"),
    });
    locations.push(McpLocation {
        name: "Crush",
        display: "~/.config/crush/crush.json",
        path: home.join(".config").join("crush").join("crush.json"),
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
            display: opencode_display,
            path: opencode_cfg,
        });
    }

    #[cfg(target_os = "macos")]
    {
        let vscode_mcp = home.join("Library/Application Support/Code/User/mcp.json");
        locations.push(McpLocation {
            name: "VS Code / Copilot",
            display: "~/Library/Application Support/Code/User/mcp.json",
            path: vscode_mcp,
        });
    }
    #[cfg(target_os = "linux")]
    {
        let vscode_mcp = home.join(".config/Code/User/mcp.json");
        locations.push(McpLocation {
            name: "VS Code / Copilot",
            display: "~/.config/Code/User/mcp.json",
            path: vscode_mcp,
        });
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let vscode_mcp = std::path::PathBuf::from(appdata).join("Code/User/mcp.json");
            locations.push(McpLocation {
                name: "VS Code / Copilot",
                display: "%APPDATA%/Code/User/mcp.json",
                path: vscode_mcp,
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
        match std::fs::read_to_string(&loc.path) {
            Ok(content) if content.contains("lean-ctx") => {
                found.push(format!("{} {DIM}({}){RST}", loc.name, loc.display));
            }
            Ok(_) => {
                exists_no_ref.push(loc.name.to_string());
            }
            Err(_) => {}
        }
    }

    if !found.is_empty() {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}MCP config{RST}  {GREEN}lean-ctx found in: {}{RST}",
                found.join(", ")
            ),
        }
    } else if !exists_no_ref.is_empty() {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}MCP config{RST}  {YELLOW}config exists for {} but does not reference lean-ctx{RST}  {DIM}(run: lean-ctx init --agent <editor>){RST}",
                exists_no_ref.join(", ")
            ),
        }
    } else {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}MCP config{RST}  {YELLOW}no MCP config found{RST}  {DIM}(checked: Cursor, Claude, Windsurf, Codex, Gemini, Antigravity, Crush, Zed){RST}"
            ),
        }
    }
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

fn tool_adoption_outcome() -> Outcome {
    let store = crate::core::stats::load();
    let cep = &store.cep;

    let total_mode_calls: u64 = cep.modes.values().sum();
    let shell_commands = store.total_commands;

    if total_mode_calls == 0 && shell_commands == 0 {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Tool adoption{RST}  {YELLOW}no usage data yet{RST}  {DIM}(use lean-ctx tools to see adoption stats){RST}"
            ),
        };
    }

    let data_tools_used = total_mode_calls > 0
        && cep.modes.keys().any(|k| k != "overview" && k != "metrics");

    if shell_commands > 20 && !data_tools_used {
        return Outcome {
            ok: false,
            line: format!(
                "{BOLD}Tool adoption{RST}  {RED}shell hook active but MCP data tools not used{RST}\n\
                 {DIM}     Your AI client may be using native Read/Shell instead of ctx_read/ctx_shell.{RST}\n\
                 {DIM}     Fix: run {BOLD}lean-ctx init{RST}{DIM} to update rules, or check for conflicting{RST}\n\
                 {DIM}     instructions in CLAUDE.md / .cursorrules that say \"use Read/Edit tools only\".{RST}"
            ),
        };
    }

    if data_tools_used {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Tool adoption{RST}  {GREEN}MCP tools active{RST}  {DIM}{total_mode_calls} reads, {shell_commands} shell commands{RST}"
            ),
        }
    } else {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Tool adoption{RST}  {YELLOW}shell hook only{RST}  {DIM}{shell_commands} commands (enable MCP for full savings){RST}"
            ),
        }
    }
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

    // 3) ~/.lean-ctx directory
    let lean_dir = dirs::home_dir().map(|h| h.join(".lean-ctx"));
    let dir_outcome = match &lean_dir {
        Some(p) if p.is_dir() => {
            passed += 1;
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}~/.lean-ctx/{RST}  {GREEN}exists{RST}  {DIM}{}{RST}",
                    p.display()
                ),
            }
        }
        Some(p) => Outcome {
            ok: false,
            line: format!(
                "{BOLD}~/.lean-ctx/{RST}  {RED}missing or not a directory{RST}  {DIM}{}{RST}",
                p.display()
            ),
        },
        None => Outcome {
            ok: false,
            line: format!("{BOLD}~/.lean-ctx/{RST}  {RED}could not resolve home directory{RST}"),
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

    // 8) Port
    let port = port_3333_outcome();
    if port.ok {
        passed += 1;
    }
    print_check(&port);

    // 9) Session state (project_root + shell_cwd)
    let session_outcome = session_state_outcome();
    print_check(&session_outcome);

    // 10) Pi Coding Agent (optional)
    let pi = pi_outcome();
    if let Some(ref pi_check) = pi {
        if pi_check.ok {
            passed += 1;
        }
        print_check(pi_check);
    }

    // 11) Tool adoption check
    let adoption = tool_adoption_outcome();
    print_check(&adoption);

    let effective_total = if pi.is_some() { total + 3 } else { total + 2 };
    if adoption.ok {
        passed += 1;
    }
    println!();
    println!("  {BOLD}{WHITE}Summary:{RST}  {GREEN}{passed}{RST}{DIM}/{effective_total}{RST} checks passed");
    println!("  {DIM}This binary: lean-ctx {VERSION} (Cargo package version){RST}");
}

pub fn run_compact() {
    let mut passed = 0u32;
    let total = 5u32;

    if resolve_lean_ctx_binary().is_some() || path_in_path_env() {
        passed += 1;
    }
    let lean_dir = dirs::home_dir().map(|h| h.join(".lean-ctx"));
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

    let status = if passed == total {
        format!("{GREEN}✓ All {total} checks passed{RST}")
    } else {
        format!("{YELLOW}{passed}/{total} passed{RST} — run {BOLD}lean-ctx doctor{RST} for details")
    };
    println!("  {status}");
}
