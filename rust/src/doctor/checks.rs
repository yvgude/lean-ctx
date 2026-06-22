// Auto-split from the former monolithic doctor/mod.rs.

#[allow(clippy::wildcard_imports)]
use super::common::*;
use super::{BOLD, DIM, GREEN, Outcome, RED, RST, YELLOW};
use std::net::TcpListener;

/// Reports the shell allowlist exactly as the MCP tools enforce it — and, crucially,
/// flags when `config.toml` fails to parse (the silent-default trap behind #341,
/// where an allowlist edit appears to "do nothing" because the file never loaded).
pub(super) fn shell_allowlist_outcome() -> Outcome {
    if let Some(err) = crate::core::config::last_config_parse_error() {
        let short = err.lines().next().unwrap_or("parse error");
        return Outcome {
            ok: false,
            line: format!(
                "{BOLD}Shell allowlist{RST}  {RED}config.toml fails to parse → running on DEFAULTS{RST}  {DIM}({short}){RST}"
            ),
        };
    }

    // GL #788: the security mode overrides the allowlist view, so surface a
    // relaxed posture loudly — an unexpected `off`/`warn` must never hide behind
    // a populated allowlist.
    match crate::core::shell_allowlist::ShellSecurity::resolve() {
        crate::core::shell_allowlist::ShellSecurity::Off => {
            return Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Shell allowlist{RST}  {YELLOW}off{RST}  {DIM}(shell_security=off — gating skipped, all commands allowed){RST}"
                ),
            };
        }
        crate::core::shell_allowlist::ShellSecurity::Warn => {
            return Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Shell allowlist{RST}  {YELLOW}warn-only{RST}  {DIM}(shell_security=warn — violations logged, never blocked){RST}"
                ),
            };
        }
        crate::core::shell_allowlist::ShellSecurity::Enforce => {}
    }

    let effective = crate::core::shell_allowlist::effective_allowlist_pub();
    if effective.is_empty() {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Shell allowlist{RST}  {YELLOW}disabled{RST}  {DIM}(all commands allowed){RST}"
            ),
        };
    }

    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Shell allowlist{RST}  {GREEN}{} command(s) enforced{RST}  {DIM}(add one: lean-ctx allow <cmd>){RST}",
            effective.len()
        ),
    }
}

/// Reports the effective PathJail state (GH #392): which knob (if any)
/// disabled it, and whether configured `allow_paths`/`extra_roots` entries
/// actually resolve — the silent failure mode behind "allow_paths has no
/// effect" reports (unexpanded `$VAR`, typos, paths that don't exist).
/// Cognition v2 activation: how many science-backed subsystems have actually
/// fired on this install. Proves the stack is wired (not dead code) without
/// needing external instrumentation — `lean-ctx introspect cognition` drills in.
pub(super) fn cognition_activity_outcome() -> Outcome {
    let snap = crate::core::introspect::snapshot();
    let total = snap.len();
    let active = snap.iter().filter(|(_, a)| a.count > 0).count();
    // Before any tool calls have run nothing has fired yet — neutral, not a
    // failure. Always pass; the value is the visibility, not a gate.
    let line = if active == 0 {
        format!(
            "{BOLD}Cognition{RST}  {DIM}no activity recorded yet{RST}  {DIM}(inspect: lean-ctx introspect cognition){RST}"
        )
    } else {
        format!(
            "{BOLD}Cognition{RST}  {GREEN}{active}/{total} subsystems active{RST}  {DIM}(details: lean-ctx introspect cognition){RST}"
        )
    };
    Outcome { ok: true, line }
}

pub(super) fn path_jail_outcome() -> Outcome {
    if cfg!(feature = "no-jail") {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Path jail{RST}  {YELLOW}disabled at compile time{RST}  {DIM}(built with the no-jail feature){RST}"
            ),
        };
    }

    let cfg = crate::core::config::Config::load();
    if cfg.path_jail == Some(false) {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Path jail{RST}  {YELLOW}disabled{RST}  {DIM}(path_jail = false in config.toml — all tool paths allowed){RST}"
            ),
        };
    }

    let entries: Vec<&String> = cfg
        .allow_paths
        .iter()
        .chain(cfg.extra_roots.iter())
        .collect();
    let mut grants_everything = false;
    let mut dead: Vec<String> = Vec::new();
    for raw in &entries {
        let expanded = crate::core::pathjail::expand_user_path(raw);
        if expanded == std::path::Path::new("/") {
            grants_everything = true;
        }
        if !expanded.exists() {
            dead.push((*raw).clone());
        }
    }

    if grants_everything {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Path jail{RST}  {YELLOW}active, but allow_paths contains \"/\"{RST}  {DIM}(grants everything — prefer the explicit `path_jail = false`){RST}"
            ),
        };
    }
    if !dead.is_empty() {
        return Outcome {
            ok: false,
            line: format!(
                "{BOLD}Path jail{RST}  {RED}{} allow_paths entr{} never match{RST}  {DIM}({} — unset $VAR or missing path){RST}",
                dead.len(),
                if dead.len() == 1 {
                    "y will"
                } else {
                    "ies will"
                },
                dead.join(", ")
            ),
        };
    }
    let detail = if entries.is_empty() {
        let cfg = crate::core::config::Config::path()
            .map_or_else(|| "config.toml".to_string(), |p| p.display().to_string());
        format!("project root only; extend via allow_paths in {cfg}")
    } else {
        format!("project root + {} configured allow path(s)", entries.len())
    };

    // Env-channel relaxations the config view above can't see (inherited from the
    // IDE/launchd process env, e.g. LEAN_CTX_ALLOW_PATH / EXTRA_ROOTS /
    // ALLOW_IDE_DIRS). Surface them as a standing security note (GH security
    // audit, finding 3); no-jail / path_jail=false are handled by the early
    // returns above, so only the env/IDE-dir relaxations reach here.
    let relaxed: Vec<&str> = crate::core::pathjail::active_relaxations()
        .iter()
        .map(|r| r.source)
        .collect();
    if relaxed.is_empty() {
        Outcome {
            ok: true,
            line: format!("{BOLD}Path jail{RST}  {GREEN}active{RST}  {DIM}({detail}){RST}"),
        }
    } else {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Path jail{RST}  {GREEN}active{RST} {YELLOW}but relaxed via {}{RST}  {DIM}({detail}; relaxations widen access beyond the project root){RST}",
                relaxed.join(", ")
            ),
        }
    }
}

/// Reports project-local config trust (security audit #4): whether the active
/// workspace's `.lean-ctx.toml` carries security-sensitive overrides and, if so,
/// whether they are honoured (workspace trusted) or withheld (untrusted). The
/// withheld state is the SECURE default, so it stays a yellow note — not a
/// failure — mirroring the path-jail-relaxed line above.
pub(super) fn workspace_trust_outcome() -> Outcome {
    let Some(root) = crate::core::config::Config::find_project_root() else {
        return Outcome {
            ok: true,
            line: format!("{BOLD}Workspace trust{RST}  {DIM}n/a (no project root){RST}"),
        };
    };
    let sensitive = std::fs::read_to_string(crate::core::config::Config::local_path(&root))
        .ok()
        .map(|c| crate::core::config::local_sensitive_overrides(&c))
        .unwrap_or_default();

    if sensitive.is_empty() {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Workspace trust{RST}  {GREEN}no project-local security overrides{RST}"
            ),
        };
    }

    if crate::core::workspace_trust::is_trusted(std::path::Path::new(&root)) {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Workspace trust{RST}  {GREEN}trusted{RST}  {DIM}({} sensitive override(s) honoured: {}){RST}",
                sensitive.len(),
                sensitive.join(", ")
            ),
        }
    } else {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Workspace trust{RST}  {YELLOW}untrusted — {} sensitive override(s) withheld{RST}  {DIM}(run `lean-ctx trust`: {}){RST}",
                sensitive.len(),
                sensitive.join(", ")
            ),
        }
    }
}

/// Reports the format-aware passthrough (#342): output already in a compact,
/// token-oriented format (TOON by default) is preserved verbatim instead of
/// recompressed, so an agent's proof-of-output-shape survives intact.
pub(super) fn compact_format_passthrough_outcome() -> Outcome {
    let cfg = crate::core::config::Config::load();
    if cfg.preserve_compact_formats.is_empty() {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Compact-format passthrough{RST}  {YELLOW}off{RST}  {DIM}(set preserve_compact_formats to keep e.g. TOON verbatim){RST}"
            ),
        };
    }
    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Compact-format passthrough{RST}  {GREEN}{}{RST}  {DIM}(preserved verbatim, not recompressed){RST}",
            cfg.preserve_compact_formats.join(", ")
        ),
    }
}

/// Reports IDE permission inheritance: when on, lean-ctx mirrors the host IDE's
/// bash/read/edit/grep permission rules onto its own tools, so `ctx_shell` honors
/// a `rm *: ask`/`deny` rule instead of forming a parallel, ungoverned path.
pub(super) fn permission_inheritance_outcome() -> Outcome {
    use crate::core::config::{Config, PermissionInheritance};
    let cfg = Config::load();
    if cfg.permission_inheritance_effective() != PermissionInheritance::On {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Permission inheritance{RST}  {YELLOW}off{RST}  {DIM}(enable: lean-ctx config set permission_inheritance on → ctx_shell honors your IDE's bash/rm rules){RST}"
            ),
        };
    }
    let policy = dirs::home_dir()
        .map(|home| crate::core::ide_permissions::load_opencode(&home, None))
        .unwrap_or_default();
    let detail = if policy.is_empty() {
        "on, but no OpenCode permission rules found yet".to_string()
    } else {
        format!(
            "mirroring {} OpenCode permission rule(s)",
            policy.rule_count()
        )
    };
    Outcome {
        ok: true,
        line: format!("{BOLD}Permission inheritance{RST}  {GREEN}on{RST}  {DIM}({detail}){RST}"),
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

pub(super) fn mcp_config_outcome() -> Outcome {
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
            format!(
                "{DIM}(Claude Code may overwrite ~/.claude.json on startup — lean-ctx entry missing from mcpServers){RST}"
            )
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

pub(super) fn port_3333_outcome() -> Outcome {
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

pub(super) fn pi_outcome() -> Option<Outcome> {
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

pub(super) fn provider_outcome() -> Outcome {
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
        .map(|id| match registry.get(id) {
            Some(p) => {
                if p.is_available() {
                    format!("{GREEN}{id}{RST}")
                } else {
                    format!("{YELLOW}{id}(no auth){RST}")
                }
            }
            _ => {
                format!("{RED}{id}(missing){RST}")
            }
        })
        .collect();
    Outcome {
        ok: true,
        line: format!("{BOLD}Providers{RST}  {}", labels.join(", ")),
    }
}

pub(super) fn mcp_bridge_outcomes() -> Vec<Outcome> {
    let cfg = crate::core::config::Config::load();
    let bridges = &cfg.providers.mcp_bridges;
    if bridges.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();

    let auto_idx = if cfg.providers.auto_index {
        format!("{GREEN}auto_index=true{RST}")
    } else {
        format!(
            "{YELLOW}auto_index=false (provider data won't be indexed into BM25/Graph/Knowledge){RST}"
        )
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

pub(super) fn plan_mode_outcomes() -> Vec<Outcome> {
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

pub(super) fn session_state_outcome() -> Outcome {
    use crate::core::session::SessionState;

    match SessionState::load_latest() {
        Some(session) => {
            let root = session.project_root.as_deref().unwrap_or("(not set)");
            let cwd = session.shell_cwd.as_deref().unwrap_or("(not tracked)");
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

pub(super) fn docker_env_outcomes() -> Vec<Outcome> {
    if !crate::shell::is_container() {
        return vec![];
    }
    let env_sh = crate::core::paths::config_dir().map_or_else(
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

pub(super) fn skill_files_outcome() -> Outcome {
    let Some(home) = dirs::home_dir() else {
        return Outcome {
            ok: false,
            line: format!("{BOLD}SKILL.md{RST}  {RED}could not resolve home directory{RST}"),
        };
    };

    let candidates = [
        ("Claude Code", home.join(".claude/skills/lean-ctx/SKILL.md")),
        (
            "CodeBuddy",
            home.join(".codebuddy/skills/lean-ctx/SKILL.md"),
        ),
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

pub(super) fn proxy_health_outcome() -> Outcome {
    use crate::core::config::Config;

    let cfg = Config::load();
    let port = crate::proxy_setup::default_port();

    match cfg.proxy_enabled {
        Some(true) => {
            let reachable = {
                use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
                let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
                TcpStream::connect_timeout(&addr, crate::proxy_setup::proxy_timeout()).is_ok()
            };
            // Autostart has no backend on Windows/other platforms, so a missing
            // autostart must never be a hard failure there (#416).
            let supported = crate::proxy_autostart::is_supported();

            if reachable {
                // Up now — verify the HTTP/auth layer regardless of autostart state.
                if !proxy_auth_probe(port) {
                    return Outcome {
                        ok: false,
                        line: format!(
                            "{BOLD}Proxy{RST}  {YELLOW}running on port {port} but auth probe failed{RST}  {YELLOW}fix: lean-ctx proxy restart{RST}"
                        ),
                    };
                }
                if supported && !crate::proxy_autostart::is_installed() {
                    // Running, but it won't survive a reboot without autostart.
                    Outcome {
                        ok: true,
                        line: format!(
                            "{BOLD}Proxy{RST}  {GREEN}running on port {port}{RST}  {YELLOW}autostart not installed — persist: lean-ctx proxy enable{RST}"
                        ),
                    }
                } else {
                    Outcome {
                        ok: true,
                        line: format!(
                            "{BOLD}Proxy{RST}  {GREEN}enabled, running on port {port}{RST}"
                        ),
                    }
                }
            } else if supported {
                Outcome {
                    ok: false,
                    line: format!(
                        "{BOLD}Proxy{RST}  {RED}enabled but not reachable on port {port}{RST}  {YELLOW}fix: lean-ctx proxy start{RST}"
                    ),
                }
            } else {
                // Windows/other: no autostart backend, so a stopped proxy is a
                // setup note (start it manually), not a doctor failure (#416).
                Outcome {
                    ok: true,
                    line: format!(
                        "{BOLD}Proxy{RST}  {YELLOW}enabled but not running{RST}  {DIM}autostart unavailable on this platform — start: lean-ctx proxy start{RST}"
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
pub(super) fn stale_proxy_env_outcome() -> Option<Outcome> {
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

/// Detects the Claude Pro/Max subscription + proxy conflict: the proxy is enabled and
/// Claude Code's `ANTHROPIC_BASE_URL` points at the local proxy, but no Anthropic API
/// key is available. A subscription OAuth token only authenticates against
/// `api.anthropic.com`, so routing it through the proxy causes a login loop / 401.
/// Returns `None` when not applicable, `Some(Outcome)` when the conflict is present.
pub(super) fn proxy_subscription_conflict_outcome() -> Option<Outcome> {
    use crate::core::config::Config;

    let home = dirs::home_dir()?;
    let cfg = Config::load();

    // Only relevant when the proxy is actively enabled.
    if cfg.proxy_enabled != Some(true) {
        return None;
    }

    let settings_path = crate::core::editor_registry::claude_state_dir(&home).join("settings.json");
    let content = std::fs::read_to_string(&settings_path).ok()?;
    let doc: serde_json::Value = crate::core::jsonc::parse_jsonc(&content).ok()?;

    let base_url = doc
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // No local redirect → nothing to warn about.
    if !crate::proxy_setup::is_local_lean_ctx_url(base_url) {
        return None;
    }

    // API key present → the proxy can forward it, redirect is fine.
    if crate::proxy_setup::anthropic_api_key_available(&home) {
        return None;
    }

    Some(Outcome {
        ok: false,
        line: format!(
            "{BOLD}Claude auth{RST}  {RED}ANTHROPIC_BASE_URL → proxy but no ANTHROPIC_API_KEY (Pro/Max subscription){RST}\n\
             {DIM}         A subscription token only authenticates against api.anthropic.com; routing it{RST}\n\
             {DIM}         through the proxy causes a login loop / 401. Fix one of:{RST}\n\
             {YELLOW}           lean-ctx proxy disable     {DIM}(keep your subscription; use ctx_* MCP tools for savings){RST}\n\
             {YELLOW}           export ANTHROPIC_API_KEY=…  {DIM}then: lean-ctx proxy enable  (pay-as-you-go via proxy){RST}"
        ),
    })
}

pub(super) fn proxy_upstream_outcome() -> Outcome {
    use crate::core::config::{Config, ProxyProvider, is_local_proxy_url};

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
    let mut plaintext = Vec::new();
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
            // Past the loopback guard above, any `http://` is a non-loopback
            // plaintext upstream that only resolved because the user opted in
            // (allow_insecure_http_upstream, #440). Valid config, but worth a
            // standing security reminder.
            if resolved.starts_with("http://") {
                plaintext.push(*label);
            }
        }
    }

    if custom.is_empty() {
        Outcome {
            ok: true,
            line: format!("{BOLD}Proxy upstream{RST}  {GREEN}provider defaults{RST}"),
        }
    } else {
        let mut line = format!(
            "{BOLD}Proxy upstream{RST}  {GREEN}custom: {}{RST}",
            custom.join(", ")
        );
        if !plaintext.is_empty() {
            line.push_str(&format!(
                "  {YELLOW}⚠ plaintext HTTP ({}) — trusted local network only{RST}",
                plaintext.join(", ")
            ));
        }
        Outcome { ok: true, line }
    }
}

/// #449 drift check: warns when the running proxy forwards to a different
/// upstream than the operator expects. Covers both traps — a shell-exported
/// `LEAN_CTX_*_UPSTREAM` that never reached the MCP/service-spawned proxy, and a
/// proxy started with an env override that now masks a later config.toml edit.
/// Returns `None` when the proxy is down or in sync, so the board stays quiet
/// unless there is something actionable.
pub(super) fn proxy_upstream_drift_outcome() -> Option<Outcome> {
    use crate::core::config::{
        Config, ProxyProvider, UpstreamDrift, diagnose_drift, env_upstream_override,
    };

    let cfg = Config::load();
    if cfg.proxy_enabled != Some(true) {
        return None;
    }
    let port = crate::proxy_setup::default_port();
    let (live_anthropic, live_openai, live_gemini) = proxy_live_upstreams(port)?;
    let disk = cfg.proxy.resolve_all_disk();

    let mut env_not_applied = Vec::new();
    let mut config_not_applied = Vec::new();
    for (label, key, provider, disk_val, live) in [
        (
            "Anthropic",
            "anthropic",
            ProxyProvider::Anthropic,
            &disk.anthropic,
            &live_anthropic,
        ),
        (
            "OpenAI",
            "openai",
            ProxyProvider::OpenAi,
            &disk.openai,
            &live_openai,
        ),
        (
            "Gemini",
            "gemini",
            ProxyProvider::Gemini,
            &disk.gemini,
            &live_gemini,
        ),
    ] {
        let env = env_upstream_override(provider);
        match diagnose_drift(env.as_deref(), disk_val, live) {
            Some(UpstreamDrift::EnvNotApplied) => {
                env_not_applied.push(format!(
                    "{label} → `lean-ctx config set proxy.{key}_upstream`"
                ));
            }
            Some(UpstreamDrift::ConfigNotApplied) => {
                config_not_applied.push(format!("{label} live {live} ≠ config {disk_val}"));
            }
            None => {}
        }
    }

    if env_not_applied.is_empty() && config_not_applied.is_empty() {
        return None;
    }
    let mut line = format!("{BOLD}Proxy upstream drift{RST}");
    if !env_not_applied.is_empty() {
        line.push_str(&format!(
            "  {YELLOW}LEAN_CTX_*_UPSTREAM set in this shell but not reaching the proxy — env never reaches an MCP/service-spawned proxy (#449); persist it (applies live): {}{RST}",
            env_not_applied.join(", ")
        ));
    }
    if !config_not_applied.is_empty() {
        line.push_str(&format!(
            "  {YELLOW}{} — apply: lean-ctx proxy restart{RST}",
            config_not_applied.join("; ")
        ));
    }
    Some(Outcome { ok: false, line })
}

pub(super) fn cache_safety_outcome() -> Outcome {
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

pub(super) fn claude_truncation_outcome() -> Option<Outcome> {
    let home = dirs::home_dir()?;
    let claude_detected = crate::core::editor_registry::claude_mcp_json_path(&home).exists()
        || crate::core::editor_registry::claude_state_dir(&home).exists()
        || claude_binary_exists();

    if !claude_detected {
        return None;
    }

    let cfg = crate::core::config::Config::load();
    Some(claude_instructions_check(
        &home,
        cfg.rules_scope_effective(),
        cfg.rules_injection_effective(),
    ))
}

pub(super) fn codebuddy_truncation_outcome() -> Option<Outcome> {
    let home = dirs::home_dir()?;
    let codebuddy_detected = crate::core::editor_registry::codebuddy_mcp_json_path(&home).exists()
        || crate::core::editor_registry::codebuddy_state_dir(&home).exists()
        || codebuddy_binary_exists();

    if !codebuddy_detected {
        return None;
    }

    let cfg = crate::core::config::Config::load();
    Some(codebuddy_instructions_check(
        &home,
        cfg.rules_scope_effective(),
        cfg.rules_injection_effective(),
    ))
}

/// Verify Claude Code receives the full lean-ctx instructions despite the
/// 2048-char MCP instructions cap.
///
/// The v3 layout (GL #555) replaced the always-loaded `~/.claude/rules/lean-ctx.md`
/// with a CLAUDE.md block + on-demand skill — `setup` actively *removes* the rules
/// file. The check therefore accepts every layout `setup` can produce (GH #396:
/// the old check demanded the retired rules file right after setup deleted it,
/// and its suggested fix could not recreate one). Layout detection lives in
/// `common::claude_instructions_state`, shared with `doctor integrations`.
fn claude_instructions_check(
    home: &std::path::Path,
    scope: crate::core::config::RulesScope,
    injection: crate::core::config::RulesInjection,
) -> Outcome {
    use super::common::ClaudeInstructionsState as S;

    let state = super::common::claude_instructions_state(home, scope, injection);
    let line = match state {
        S::ProjectScope => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}project scope{RST}  {DIM}(global instructions intentionally absent; project files carry them){RST}"
        ),
        S::InjectionOff => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}rules injection off{RST}  {DIM}(instructions intentionally not installed — config rules_injection=off){RST}"
        ),
        S::DedicatedWithSkill => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}dedicated injection + skill installed{RST}  {DIM}(SessionStart hook injects instructions){RST}"
        ),
        S::DedicatedMissingSkill => format!(
            "{BOLD}Claude Code instructions{RST}  {YELLOW}lean-ctx skill missing{RST}  {DIM}(run: lean-ctx setup){RST}"
        ),
        S::BlockAndSkill => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}CLAUDE.md block + skill installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via CLAUDE.md){RST}"
        ),
        S::BlockOnly => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}CLAUDE.md block installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via CLAUDE.md){RST}"
        ),
        S::LegacyRules => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}legacy rules file installed{RST}  {DIM}(next `lean-ctx setup` migrates it to the CLAUDE.md block + skill){RST}"
        ),
        S::Missing => format!(
            "{BOLD}Claude Code instructions{RST}  {YELLOW}no CLAUDE.md block or rules file found — MCP instructions truncated at 2048 chars{RST}  {DIM}(run: lean-ctx setup){RST}"
        ),
    };
    Outcome {
        ok: state.ok(),
        line,
    }
}

/// CodeBuddy instructions check — mirrors `claude_instructions_check` since
/// CodeBuddy uses the same CODEBUDDY.md block + skill pattern as Claude Code.
fn codebuddy_instructions_check(
    home: &std::path::Path,
    scope: crate::core::config::RulesScope,
    injection: crate::core::config::RulesInjection,
) -> Outcome {
    use super::common::ClaudeInstructionsState as S;

    let state = super::common::codebuddy_instructions_state(home, scope, injection);
    let line = match state {
        S::ProjectScope => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}project scope{RST}  {DIM}(global instructions intentionally absent; project files carry them){RST}"
        ),
        S::InjectionOff => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}rules injection off{RST}  {DIM}(instructions intentionally not installed — config rules_injection=off){RST}"
        ),
        S::DedicatedWithSkill => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}dedicated injection + skill installed{RST}  {DIM}(SessionStart hook injects instructions){RST}"
        ),
        S::DedicatedMissingSkill => format!(
            "{BOLD}CodeBuddy instructions{RST}  {YELLOW}lean-ctx skill missing{RST}  {DIM}(run: lean-ctx setup){RST}"
        ),
        S::BlockAndSkill => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}CODEBUDDY.md block + skill installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via CODEBUDDY.md){RST}"
        ),
        S::BlockOnly => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}CODEBUDDY.md block installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via CODEBUDDY.md){RST}"
        ),
        S::LegacyRules => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}legacy rules file installed{RST}  {DIM}(next `lean-ctx setup` migrates it to the CODEBUDDY.md block + skill){RST}"
        ),
        S::Missing => format!(
            "{BOLD}CodeBuddy instructions{RST}  {YELLOW}no CODEBUDDY.md block or rules file found — MCP instructions truncated at 2048 chars{RST}  {DIM}(run: lean-ctx setup){RST}"
        ),
    };
    Outcome {
        ok: state.ok(),
        line,
    }
}

pub(super) fn bm25_cache_health_outcome() -> Outcome {
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

    // Single source of truth with `save`/`load` (decoupled from the RAM profile;
    // see bm25_index::persist_ceiling_bytes) so the warning threshold here always
    // matches what is actually enforced on disk.
    let max_bytes = crate::core::bm25_index::persist_ceiling_bytes();
    let effective_mb = max_bytes / (1024 * 1024);
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

/// Runtime status of the semantic (BM25) index for the active project: whether
/// it is idle/building/ready/failed, how long the last build took, and — crucially
/// — *why* it might be stuck (e.g. "indexed but NOT persisted: too large").
///
/// This answers issue #249: users had no way to tell whether the semantic index
/// was working, how fast it was, or why it kept "warming up" forever.
pub(super) fn semantic_index_outcome() -> Option<Outcome> {
    let session = crate::core::session::SessionState::load_latest()?;
    let project_root = session.project_root?;

    let summary = crate::core::index_orchestrator::bm25_summary(&project_root);
    let disk = crate::core::index_orchestrator::disk_status(&project_root);
    let persisted = if disk.bm25_index.exists {
        match disk.bm25_index.size_bytes {
            Some(b) => format!("persisted {:.1} MB", b as f64 / 1_048_576.0),
            None => "persisted".to_string(),
        }
    } else {
        "not persisted".to_string()
    };

    let timing = match summary.elapsed_ms {
        Some(ms) if summary.state == "building" => format!(", {:.1}s elapsed", ms as f64 / 1000.0),
        Some(ms) => format!(", built in {:.1}s", ms as f64 / 1000.0),
        None => String::new(),
    };

    let outcome = match summary.state {
        "failed" => Outcome {
            ok: false,
            line: format!(
                "{BOLD}Semantic index{RST}  {RED}FAILED{RST}: {}  {DIM}(run: lean-ctx reindex){RST}",
                summary
                    .last_error
                    .or(summary.note)
                    .unwrap_or_else(|| "unknown error".to_string())
            ),
        },
        "building" => Outcome {
            ok: true,
            line: format!("{BOLD}Semantic index{RST}  {YELLOW}building{timing}{RST}"),
        },
        _ if summary
            .note
            .as_deref()
            .is_some_and(|n| n.contains("NOT persisted")) =>
        {
            Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Semantic index{RST}  {YELLOW}rebuilds every cold start{RST}: {}",
                    summary.note.unwrap_or_default()
                ),
            }
        }
        "ready" => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Semantic index{RST}  {GREEN}ready{RST} {DIM}({persisted}{timing}){RST}"
            ),
        },
        // idle: never asked to build this session — report disk state only.
        _ if disk.bm25_index.exists => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Semantic index{RST}  {GREEN}ready{RST} {DIM}({persisted}, on disk){RST}"
            ),
        },
        _ => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Semantic index{RST}  {DIM}not built yet (builds on first semantic search/compose){RST}"
            ),
        },
    };
    Some(outcome)
}

pub(super) fn archive_footprint_outcome() -> Outcome {
    let bytes = crate::core::archive_fts::db_size_bytes();
    let cap_mb = std::env::var("LEAN_CTX_ARCHIVE_DB_MAX_MB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|m| *m > 0)
        .unwrap_or(500);
    let cap_bytes = cap_mb * 1024 * 1024;
    let mb = bytes as f64 / 1_048_576.0;
    if bytes > cap_bytes {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}Archive FTS{RST}  {RED}{mb:.0} MB exceeds {cap_mb} MB cap{RST}  {DIM}(run: lean-ctx cache prune; auto-enforced on next session){RST}"
            ),
        }
    } else if bytes > cap_bytes * 80 / 100 {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Archive FTS{RST}  {YELLOW}{mb:.0} MB (>80% of {cap_mb} MB cap){RST}"
            ),
        }
    } else {
        Outcome {
            ok: true,
            line: format!("{BOLD}Archive FTS{RST}  {GREEN}{mb:.1} MB / {cap_mb} MB cap{RST}"),
        }
    }
}

pub(super) fn memory_profile_outcome() -> Outcome {
    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    // The BM25 *disk* ceiling is decoupled from the RAM profile (#249); show the
    // real effective ceiling rather than a hardcoded per-profile figure.
    let bm25_mb = cfg.bm25_max_cache_mb_effective();
    let (label, detail) = match profile {
        crate::core::config::MemoryProfile::Low => (
            "low",
            format!("embeddings+semantic cache disabled, BM25 disk {bm25_mb} MB"),
        ),
        crate::core::config::MemoryProfile::Balanced => (
            "balanced",
            format!("default — single embedding engine, BM25 disk {bm25_mb} MB"),
        ),
        crate::core::config::MemoryProfile::Performance => (
            "performance",
            format!("full caches, BM25 disk {bm25_mb} MB"),
        ),
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

pub(super) fn memory_cleanup_outcome() -> Outcome {
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

pub(super) fn ram_guardian_outcome() -> Outcome {
    // Measure the daemon's RSS (not the CLI process) when the daemon is running.
    let daemon_pid = crate::daemon::read_daemon_pid();
    let snap = match daemon_pid {
        Some(pid) if crate::ipc::process::is_alive(pid) => {
            crate::core::memory_guard::MemorySnapshot::capture_for_pid(pid)
        }
        _ => crate::core::memory_guard::MemorySnapshot::capture(),
    };
    let Some(snap) = snap else {
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
    let source = if daemon_pid.is_some() {
        "daemon"
    } else {
        "self"
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
            "{BOLD}RAM Guardian{RST}  {color}{:.0} MB{RST} / {:.1} GB system ({:.1}%)  {DIM}limit: {:.0} MB ({allocator}, {source}){RST}{pressure_hint}",
            snap.rss_bytes as f64 / 1_048_576.0,
            snap.system_ram_bytes as f64 / 1_073_741_824.0,
            snap.rss_percent,
            snap.rss_limit_bytes as f64 / 1_048_576.0,
        ),
    }
}

/// Reports knowledge stores whose `project_root` was deleted (removed git
/// worktrees, thrown-away projects). Such a store can never be written again, so
/// its eviction cap can never self-heal — it is pure accumulated bloat. This is
/// informational (never a hard failure); `lean-ctx doctor --fix` reclaims it (#615).
pub(super) fn orphaned_knowledge_outcome() -> Outcome {
    let orphans = crate::core::knowledge::maintenance::find_orphaned_stores();
    if orphans.is_empty() {
        return Outcome {
            ok: true,
            line: format!("{BOLD}Knowledge stores{RST}  {GREEN}no orphaned stores{RST}"),
        };
    }
    let bytes: u64 = orphans.iter().map(|o| o.size_bytes).sum();
    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Knowledge stores{RST}  {YELLOW}{} orphaned ({} reclaimable){RST}  {DIM}(deleted projects — reclaim: lean-ctx cache prune){RST}",
            orphans.len(),
            human_bytes(bytes)
        ),
    }
}

pub(super) fn capacity_warnings() -> Vec<Outcome> {
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

        if let Ok(content) = std::fs::read_to_string(hash_dir.join("knowledge.json"))
            && let Ok(k) =
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

        if let Ok(content) = std::fs::read_to_string(hash_dir.join("embeddings.json"))
            && let Ok(idx) = serde_json::from_str::<
                crate::core::knowledge_embedding::KnowledgeEmbeddingIndex,
            >(&content)
        {
            checks.push((
                "embeddings".to_string(),
                idx.entries.len(),
                policy.embeddings.max_facts,
            ));
        }

        if let Ok(content) = std::fs::read_to_string(hash_dir.join("gotchas.json"))
            && let Ok(g) =
                serde_json::from_str::<crate::core::gotcha_tracker::GotchaStore>(&content)
        {
            checks.push((
                "gotchas".to_string(),
                g.gotchas.len(),
                policy.gotcha.max_gotchas_per_project,
            ));
        }

        let episodes_path = data_dir
            .join("memory")
            .join("episodes")
            .join(format!("{hash}.json"));
        if let Ok(content) = std::fs::read_to_string(&episodes_path)
            && let Ok(e) =
                serde_json::from_str::<crate::core::episodic_memory::EpisodicStore>(&content)
        {
            checks.push((
                "episodes".to_string(),
                e.episodes.len(),
                policy.episodic.max_episodes,
            ));
        }

        let procedures_path = data_dir
            .join("memory")
            .join("procedures")
            .join(format!("{hash}.json"));
        if let Ok(content) = std::fs::read_to_string(&procedures_path)
            && let Ok(p) =
                serde_json::from_str::<crate::core::procedural_memory::ProceduralStore>(&content)
        {
            checks.push((
                "procedures".to_string(),
                p.procedures.len(),
                policy.procedural.max_procedures,
            ));
        }

        let mut warnings: Vec<String> = Vec::new();
        let mut critical = false;

        for (name, current, limit) in &checks {
            if *limit == 0 {
                continue;
            }
            let pct = (*current as f64 / *limit as f64 * 100.0) as u32;
            // A store sitting *at* its cap is healthy: eviction (run_lifecycle)
            // keeps it there by design. Only flag CRIT when it is genuinely
            // *over* cap, which means eviction is not keeping up.
            if pct > 100 {
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
    if graph_max_files > 0
        && let Some(session) = crate::core::session::SessionState::load_latest()
        && let Some(ref project_root) = session.project_root
    {
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

    if results.is_empty() {
        results.push(Outcome {
            ok: true,
            line: format!("{BOLD}Capacity{RST} {GREEN}all stores within limits{RST}"),
        });
    }

    results
}

pub(super) fn lsp_server_outcomes() -> Vec<Outcome> {
    use crate::lsp::config::{KNOWN_SERVERS, find_binary_in_path};

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
    use super::*;
    use crate::core::config::{RulesInjection, RulesScope};
    use std::path::Path;

    fn write(home: &Path, rel: &str, content: &str) {
        let p = home.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn check(home: &Path, scope: RulesScope, injection: RulesInjection) -> Outcome {
        claude_instructions_check(home, scope, injection)
    }

    // GH #396: the exact post-`setup` state — CLAUDE.md block + skill, rules
    // file removed by setup. Must pass, not demand the retired rules file.
    //
    // `serial(claude_config_dir)`: `claude_state_dir` honours the process-global
    // `CLAUDE_CONFIG_DIR`, which the contextops sync tests set for their own
    // sandbox. Without serialization a concurrent setter makes this check read
    // the wrong `.claude` dir and flake under load (seen on release CI, #401).
    #[test]
    #[serial_test::serial(claude_config_dir)]
    fn v3_layout_block_and_skill_passes() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".claude/CLAUDE.md",
            &format!(
                "{}\ncontent\n{}",
                crate::core::rules_canonical::START_MARK,
                crate::core::rules_canonical::END_MARK,
            ),
        );
        write(tmp.path(), ".claude/skills/lean-ctx/SKILL.md", "skill");
        let out = check(tmp.path(), RulesScope::Global, RulesInjection::Shared);
        assert!(out.ok, "post-setup layout must pass: {}", out.line);
        assert!(out.line.contains("CLAUDE.md block + skill"));
    }

    #[test]
    #[serial_test::serial(claude_config_dir)]
    fn block_without_skill_still_passes() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".claude/CLAUDE.md",
            &format!("{}\nx", crate::core::rules_canonical::START_MARK),
        );
        let out = check(tmp.path(), RulesScope::Global, RulesInjection::Shared);
        assert!(out.ok, "{}", out.line);
    }

    #[test]
    #[serial_test::serial(claude_config_dir)]
    fn legacy_rules_file_passes() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), ".claude/rules/lean-ctx.md", "rules");
        let out = check(tmp.path(), RulesScope::Global, RulesInjection::Shared);
        assert!(out.ok, "{}", out.line);
        assert!(out.line.contains("legacy rules file"));
    }

    #[test]
    #[serial_test::serial(claude_config_dir)]
    fn nothing_installed_fails_and_suggests_setup() {
        let tmp = tempfile::tempdir().unwrap();
        let out = check(tmp.path(), RulesScope::Global, RulesInjection::Shared);
        assert!(!out.ok);
        assert!(
            out.line.contains("lean-ctx setup"),
            "must suggest a command that actually fixes it: {}",
            out.line
        );
        assert!(
            !out.line.contains("init --agent claude"),
            "init --agent claude no longer creates a Claude rules target"
        );
    }

    #[test]
    fn dedicated_injection_with_skill_passes_without_block() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), ".claude/skills/lean-ctx/SKILL.md", "skill");
        let out = check(tmp.path(), RulesScope::Global, RulesInjection::Dedicated);
        assert!(out.ok, "{}", out.line);
    }

    #[test]
    fn dedicated_injection_without_skill_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let out = check(tmp.path(), RulesScope::Global, RulesInjection::Dedicated);
        assert!(!out.ok);
    }

    #[test]
    fn project_scope_passes_without_global_files() {
        let tmp = tempfile::tempdir().unwrap();
        let out = check(tmp.path(), RulesScope::Project, RulesInjection::Shared);
        assert!(out.ok, "{}", out.line);
    }

    #[test]
    fn injection_off_passes_without_any_files() {
        let tmp = tempfile::tempdir().unwrap();
        let out = check(tmp.path(), RulesScope::Global, RulesInjection::Off);
        assert!(out.ok, "{}", out.line);
    }
}
