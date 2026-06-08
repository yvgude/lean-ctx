//! Environment diagnostics for lean-ctx installation and integration.

mod checks;
mod common;
mod fix;
mod integrations;
mod workspace_scope;

#[allow(clippy::wildcard_imports)]
use checks::*;
#[allow(clippy::wildcard_imports)]
use common::*;

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

    // 5b) Shell allowlist (effective runtime view + silent-parse-error trap, #341)
    let allowlist_outcome = shell_allowlist_outcome();
    if allowlist_outcome.ok {
        passed += 1;
    }
    print_check(&allowlist_outcome);

    // 5c) Compact-format passthrough (preserve already-compact TOON output, #342)
    let passthrough_outcome = compact_format_passthrough_outcome();
    if passthrough_outcome.ok {
        passed += 1;
    }
    print_check(&passthrough_outcome);

    // 5d) IDE permission inheritance (mirror host IDE bash/rm rules onto ctx_*)
    let perm_inherit_outcome = permission_inheritance_outcome();
    if perm_inherit_outcome.ok {
        passed += 1;
    }
    print_check(&perm_inherit_outcome);

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

    // 15a) Semantic index runtime status (state/timing/persistence) for the
    // active project — surfaces a stuck "warming" index (issue #249).
    let semantic_index = semantic_index_outcome();
    if let Some(ref check) = semantic_index {
        if check.ok {
            passed += 1;
        }
        print_check(check);
    }

    // 15b) Archive FTS footprint
    let archive_footprint = archive_footprint_outcome();
    if archive_footprint.ok {
        passed += 1;
    }
    print_check(&archive_footprint);

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

    let mut effective_total = total + 10; // session_state + integrity + cache_safety + bm25_health + archive_footprint + daemon + mem_profile + mem_cleanup + ram_guardian + proxy_health
    effective_total += 1; // shell_allowlist (#341)
    effective_total += 1; // compact_format_passthrough (#342)
    effective_total += 1; // permission_inheritance
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
    if semantic_index.is_some() {
        effective_total += 1;
    }
    // Shadow mode status
    let cfg = crate::core::config::Config::load();
    let shadow_line = if cfg.shadow_mode {
        format!("{BOLD}Shadow mode{RST}  {GREEN}active{RST}  {DIM}(native tools intercepted → ctx_*){RST}")
    } else {
        format!("{BOLD}Shadow mode{RST}  {DIM}disabled{RST}  {DIM}(enable: lean-ctx config set shadow_mode true){RST}")
    };
    println!("  {shadow_line}");

    // Tool-schema footprint (informational, not scored). The active profile now
    // authoritatively determines the advertised set, so its description reflects
    // exactly what the MCP client sees (plus the always-on ctx_call gateway).
    let tool_profile = crate::core::tool_profiles::ToolProfile::from_config(&cfg);
    println!(
        "  {BOLD}Tool profile{RST}  {WHITE}{tool_profile}{RST}  {DIM}{} + ctx_call gateway{RST}",
        tool_profile.description()
    );

    // Session cache health (#361): answer "is the cache actually engaging?"
    // without external instrumentation. CEP sessions + the cross-call hit ratio
    // come from the persistent stats store; `verify-cache` proves it live.
    let cep = &crate::core::stats::load().cep;
    let hit_ratio = if cep.total_cache_reads > 0 {
        (cep.total_cache_hits as f64 / cep.total_cache_reads as f64) * 100.0
    } else {
        0.0
    };
    println!(
        "  {BOLD}Session cache{RST}  {WHITE}{} sessions{RST}  {DIM}{}/{} reads cached ({hit_ratio:.0}% hit) · prove: lean-ctx verify-cache{RST}",
        cep.sessions, cep.total_cache_hits, cep.total_cache_reads
    );

    let needs_attention = effective_total.saturating_sub(passed);
    println!();
    println!("  {BOLD}{WHITE}Summary:{RST}  {GREEN}{passed}{RST}{DIM}/{effective_total}{RST} checks passed");
    if needs_attention > 0 {
        println!(
            "  {YELLOW}{needs_attention} check(s) need attention.{RST}  Auto-repair what's fixable:  {BOLD}lean-ctx doctor --fix{RST}"
        );
    } else {
        println!("  {GREEN}Everything looks good.{RST}");
    }
    println!("  {DIM}LSP servers are optional enhancements (not counted in score){RST}");
    println!("  {DIM}{}{RST}", crate::core::integrity::origin_line());
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
    use super::is_active_shell_impl;

    // Mirrors the inline classification in `checks::capacity_warnings`: a store at
    // or below its cap is at most a WARN (healthy, eviction keeps it there); only
    // a store *over* cap is CRIT (eviction is not keeping up).
    fn make_capacity_check(name: &str, current: usize, limit: usize) -> Option<(bool, String)> {
        if limit == 0 {
            return None;
        }
        let pct = (current as f64 / limit as f64 * 100.0) as u32;
        if pct > 100 {
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
    fn capacity_at_95_is_warning_not_critical() {
        let result = make_capacity_check("facts", 190, 200);
        assert!(result.is_some());
        let (critical, msg) = result.unwrap();
        assert!(!critical, "95% is full-but-healthy, not over cap");
        assert!(msg.contains("190/200"));
        assert!(msg.contains("95%"));
    }

    #[test]
    fn capacity_at_100_is_warning_not_critical() {
        // A store exactly at its cap is healthy — eviction keeps it there.
        let result = make_capacity_check("facts", 200, 200);
        assert!(result.is_some());
        let (critical, _) = result.unwrap();
        assert!(!critical);
    }

    #[test]
    fn capacity_over_100_is_critical() {
        // Genuinely over cap => eviction is not keeping up (regression guard for
        // the 206/200 "CRIT" that fired before lifecycle eviction was fixed).
        let result = make_capacity_check("facts", 206, 200);
        assert!(result.is_some());
        let (critical, msg) = result.unwrap();
        assert!(critical);
        assert!(msg.contains("206/200"));
        assert!(msg.contains("103%"));
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
