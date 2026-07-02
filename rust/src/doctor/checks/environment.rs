//! Install/environment wiring: shell hooks, MCP registration, editor
//! surfaces, session state, container env, CWD sanity, LSP servers.

#[allow(clippy::wildcard_imports)]
use crate::doctor::common::*;
use crate::doctor::{BOLD, DIM, GREEN, Outcome, RED, RST, YELLOW};
use std::net::TcpListener;

/// Cognition v2 activation: how many science-backed subsystems have actually
/// fired on this install. Proves the stack is wired (not dead code) without
/// needing external instrumentation — `lean-ctx introspect cognition` drills in.
pub(crate) fn cognition_activity_outcome() -> Outcome {
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
/// Reports the format-aware passthrough (#342): output already in a compact,
/// token-oriented format (TOON by default) is preserved verbatim instead of
/// recompressed, so an agent's proof-of-output-shape survives intact.
pub(crate) fn compact_format_passthrough_outcome() -> Outcome {
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
pub(crate) fn shell_aliases_outcome() -> Outcome {
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
pub(crate) fn mcp_config_outcome() -> Outcome {
    let Some(home) = dirs::home_dir() else {
        return Outcome {
            ok: false,
            line: format!("{BOLD}MCP config{RST}  {RED}could not resolve home directory{RST}"),
        };
    };

    let locations = mcp_config_locations(&home);
    let location_names = lean_ctx_mcp_location_names(&home);
    let mut found: Vec<String> = Vec::new();
    let mut exists_no_ref: Vec<String> = Vec::new();

    for loc in &locations {
        if std::fs::read_to_string(&loc.path).is_ok() {
            if location_names.contains(loc.name) {
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
/// WSL2 + VS Code (GH #669): VS Code's start-on-demand MCP lifecycle has a
/// known client-side race — the first tool call of a fresh conversation can
/// fire against cached tool metadata before the server's implementation is
/// bound, failing with `Cannot read properties of undefined (reading 'invoke')`
/// (microsoft/vscode#321150). Cold starts on WSL2 widen that window. Purely
/// informational (ok: true): the defect is upstream, a retry always succeeds.
pub(crate) fn wsl_vscode_mcp_outcome() -> Option<Outcome> {
    if !crate::core::io_health::is_wsl() {
        return None;
    }
    let home = dirs::home_dir()?;
    if !lean_ctx_mcp_location_names(&home).contains("VS Code") {
        return None;
    }
    Some(Outcome {
        ok: true,
        line: format!(
            "{BOLD}WSL2 + VS Code{RST}  {YELLOW}known first-call race in VS Code's MCP client{RST}  {DIM}(a fresh conversation's first ctx_* call may fail with \"reading 'invoke'\" — retry succeeds; upstream: microsoft/vscode#321150. Mitigation: run the MCP server once via \"MCP: List Servers\" → lean-ctx → Start){RST}"
        ),
    })
}

pub(crate) fn port_3333_outcome() -> Outcome {
    match TcpListener::bind("127.0.0.1:3333") {
        Ok(_listener) => Outcome {
            ok: true,
            line: format!("{BOLD}Dashboard port 3333{RST}  {GREEN}available on 127.0.0.1{RST}"),
        },
        // #644: a busy port is only a problem if it's *not* us. Reuse the dashboard's
        // own auth-aware /api/version probe (single source of truth) so our own
        // running dashboard reads as healthy rather than a false conflict.
        Err(_) if crate::dashboard::dashboard_responding("127.0.0.1", 3333) => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Dashboard port 3333{RST}  {GREEN}already serving lean-ctx dashboard{RST}  {DIM}(http://localhost:3333){RST}"
            ),
        },
        Err(e) => Outcome {
            ok: false,
            line: format!("{BOLD}Dashboard port 3333{RST}  {RED}not available: {e}{RST}"),
        },
    }
}
pub(crate) fn pi_outcome() -> Option<Outcome> {
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
pub(crate) fn plan_mode_outcomes() -> Vec<Outcome> {
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
pub(crate) fn session_state_outcome() -> Outcome {
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
pub(crate) fn docker_env_outcomes() -> Vec<Outcome> {
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
pub(crate) fn skill_files_outcome() -> Outcome {
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
/// GH #594 config parity: surface whether the CLI and the editor-spawned MCP
/// server resolve the *same* `config.toml`.
///
/// The current MCP writers never emit an `env` block, so any editor entry that
/// still pins `LEAN_CTX_DATA_DIR` is stale and would force that editor's MCP
/// server into single-dir mode — reading a different config than this CLI. This
/// complements the stray-config heal check (which catches the *symptom*, a
/// config.toml already stranded in the data dir) by flagging the *cause* before
/// a divergent file is ever written, and it always prints the resolved path so
/// users can compare it against `lean-ctx config path` run inside their editor.
/// Extract the editor-baked `LEAN_CTX_DATA_DIR` value from raw config text.
///
/// Works across the JSON / TOML / YAML editor configs because all three write
/// the value as the first double-quoted token after the key on its line
/// (`LEAN_CTX_DATA_DIR = "…"`, `"LEAN_CTX_DATA_DIR": "…"`, `LEAN_CTX_DATA_DIR: "…"`).
/// `~/`, `$HOME` and `$XDG_DATA_HOME` are expanded so the result can be compared
/// against the CLI's resolved standard data dir; a trailing separator is trimmed.
pub(crate) fn pinned_data_dir(content: &str) -> Option<std::path::PathBuf> {
    const KEY: &str = "LEAN_CTX_DATA_DIR";
    let line = content.lines().find(|l| l.contains(KEY))?;
    let after_key = &line[line.find(KEY)? + KEY.len()..];
    // Skip the assignment separator (`=` for TOML, `:` for JSON/YAML). In JSON
    // the key's own closing quote precedes the separator, so anchoring on the
    // separator avoids mistaking that quote for the value's opening quote.
    let after_sep = &after_key[after_key.find(['=', ':'])? + 1..];
    let open = after_sep.find('"')? + 1;
    let rest = &after_sep[open..];
    let raw = rest[..rest.find('"')?].trim();
    if raw.is_empty() {
        return None;
    }
    let mut s = raw.to_string();
    if let Ok(home) = std::env::var("HOME") {
        s = s.replace("${HOME}", &home).replace("$HOME", &home);
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        s = s
            .replace("${XDG_DATA_HOME}", &xdg)
            .replace("$XDG_DATA_HOME", &xdg);
    }
    let path = match s.strip_prefix("~/") {
        Some(tail) => dirs::home_dir()?.join(tail),
        None => std::path::PathBuf::from(s.trim_end_matches('/')),
    };
    Some(std::path::PathBuf::from(
        path.to_string_lossy().trim_end_matches('/'),
    ))
}

pub(crate) fn config_parity_outcome() -> Outcome {
    let config_path = crate::core::config::Config::path()
        .map_or_else(|| "<unresolved>".to_string(), |p| p.display().to_string());

    // Only a *non-standard* pin actually moves the MCP server's config.toml away
    // from the CLI's (#594). A pin equal to the standard `$XDG_DATA_HOME/lean-ctx`
    // is data-only and keeps parity, so it must NOT be flagged (no false alarm
    // for the value editors used to bake by default).
    let mut divergent: Vec<String> = match dirs::home_dir() {
        Some(home) => crate::core::editor_registry::detect::build_targets(&home)
            .into_iter()
            .filter(|t| {
                std::fs::read_to_string(&t.config_path)
                    .ok()
                    .filter(|c| c.contains("lean-ctx"))
                    .and_then(|c| pinned_data_dir(&c))
                    .is_some_and(|pin| crate::core::paths::data_pin_diverges_config(&pin))
            })
            .map(|t| t.name.to_string())
            .collect(),
        None => Vec::new(),
    };
    divergent.sort_unstable();
    divergent.dedup();

    if divergent.is_empty() {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}config parity{RST}  {GREEN}CLI + MCP agree{RST}  {DIM}{config_path}  (verify in your editor: lean-ctx config path){RST}"
            ),
        }
    } else {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}config parity{RST}  {YELLOW}{} pin a non-standard LEAN_CTX_DATA_DIR in their MCP config{RST}  {DIM}-> that editor's MCP server resolves a different config.toml than the CLI ({config_path}); run: lean-ctx setup (or doctor --fix) to unify{RST}",
                divergent.join(", ")
            ),
        }
    }
}
pub(crate) fn lsp_server_outcomes() -> Vec<Outcome> {
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
/// True when `cwd_str` points inside an IDE/agent config directory rather than a
/// real project (LM Studio, Claude, CodeBuddy, Codex). Matches both POSIX (`/`)
/// and Windows (`\`) separators so the warning fires on every platform.
pub(crate) fn cwd_looks_like_agent_dir(cwd_str: &str) -> bool {
    crate::core::pathutil::is_agent_config_dir(std::path::Path::new(cwd_str))
}

/// Warn if lean-ctx is running as an MCP server from a directory that lacks
/// a project marker and looks like an IDE/agent tool directory (e.g. .lmstudio,
/// .claude). This usually means the MCP client launched the process from the
/// wrong CWD, causing "path escapes project root" errors for every tool call.
pub(crate) fn mcp_server_cwd_outcome() -> Outcome {
    let is_mcp = std::env::var("LEAN_CTX_MCP_SERVER").is_ok_and(|v| v == "1");
    if !is_mcp {
        return Outcome {
            ok: true,
            line: format!("{BOLD}MCP server CWD{RST}  {DIM}(not running as MCP server){RST}"),
        };
    }

    let Ok(cwd) = std::env::current_dir() else {
        return Outcome {
            ok: true,
            line: format!("{BOLD}MCP server CWD{RST}  {YELLOW}could not resolve{RST}"),
        };
    };

    let has_marker = crate::core::pathutil::has_project_marker(&cwd);
    let cwd_str = cwd.to_string_lossy();
    let suspicious = cwd_looks_like_agent_dir(&cwd_str);

    if has_marker {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}MCP server CWD{RST}  {GREEN}under project root{RST}  {DIM}{}{RST}",
                cwd.display()
            ),
        }
    } else if suspicious {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}MCP server CWD{RST}  {YELLOW}launched from an IDE/agent config dir{RST}  {DIM}lean-ctx was launched from {}, which is not a project root. It auto-corrects to your real project on the first absolute path (#580); for relative paths to resolve immediately, set `cwd` in your MCP client config to your project directory.{RST}",
                cwd.display()
            ),
        }
    } else {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}MCP server CWD{RST}  {YELLOW}no project marker found in CWD{RST}  {DIM}cwd={} — \"path escapes project root\" errors may occur for files outside this directory. Add `cwd` to your MCP client config or add `extra_roots` / `allow_paths` in config.toml{RST}",
                cwd.display()
            ),
        }
    }
}
