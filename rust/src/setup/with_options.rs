use chrono::Utc;

use crate::core::editor_registry::{EditorTarget, WriteAction, WriteOptions};
use crate::core::portable_binary::resolve_portable_binary;
use crate::core::setup_report::{PlatformInfo, SetupItem, SetupReport, SetupStepReport};
use crate::hooks::{HookMode, recommend_hook_mode};

use super::helpers::shorten_path;
use super::index_build::{may_autoindex_cwd, spawn_index_build_background};
use super::mcp::configure_agent_mcp;
use super::options::SetupOptions;

pub fn run_setup_with_options(opts: SetupOptions) -> Result<SetupReport, String> {
    let _quiet_guard = opts.json.then(crate::core::runtime_flags::scoped_quiet);
    let started_at = Utc::now();
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let binary = resolve_portable_binary();
    let home_str = home.to_string_lossy().to_string();

    // Commit to the XDG layout (and drain any residual ~/.lean-ctx) so a stray
    // marker can never re-collapse config/data/state/cache later (GL #623).
    crate::core::layout_pin::heal();

    let targets = crate::core::editor_registry::build_targets(&home);
    let setup_cfg = crate::core::config::Config::load().setup;
    let update_mcp = setup_cfg.should_update_mcp();
    let should_inject = should_inject_rules(opts, setup_cfg.should_inject_rules());
    let should_install_skills = should_inject_skills(opts, setup_cfg.should_inject_skills());

    let mut steps = vec![
        build_shell_hook_step(opts, &binary),
        build_daemon_step(),
        build_editor_step(opts, &home_str, &binary, &targets, update_mcp),
        build_rules_step(opts, &home, should_inject),
    ];

    if let Some(step) = build_rules_dedup_step(&home, should_inject) {
        steps.push(step);
    }
    if let Some(step) = build_skill_step(&home, should_install_skills) {
        steps.push(step);
    }
    if let Some(step) = build_agent_hooks_step(&targets, update_mcp) {
        steps.push(step);
    }

    steps.extend([
        build_tool_profile_step(),
        build_proxy_step(opts, &home),
        build_doctor_compact_step(),
    ]);

    maybe_add_project_root_warning(&mut steps);
    maybe_spawn_background_index();
    maybe_enable_ide_config_access(opts);

    let report = build_setup_report(started_at, steps);
    persist_setup_report(&report)?;

    Ok(report)
}

fn setup_step(name: &str) -> SetupStepReport {
    SetupStepReport {
        name: name.to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    }
}

fn build_shell_hook_step(opts: SetupOptions, _binary: &str) -> SetupStepReport {
    let mut shell_step = setup_step("shell_hook");
    if !opts.non_interactive || opts.yes {
        if opts.json {
            crate::cli::cmd_init_quiet(&["--global".to_string()]);
        } else {
            crate::cli::cmd_init(&["--global".to_string()]);
        }
        crate::shell_hook::install_all(opts.json);
        #[cfg(not(windows))]
        {
            let binary = _binary;
            let hook_content = crate::cli::generate_hook_posix(binary);
            if crate::shell::is_container() {
                crate::cli::write_env_sh_for_containers(&hook_content);
                shell_step.items.push(SetupItem {
                    name: "env_sh".to_string(),
                    status: "created".to_string(),
                    path: Some(crate::core::paths::config_dir().map_or_else(
                        |_| "~/.config/lean-ctx/env.sh".to_string(),
                        |d| d.join("env.sh").to_string_lossy().to_string(),
                    )),
                    note: Some("Docker/CI helper (BASH_ENV / CLAUDE_ENV_FILE)".to_string()),
                });
            } else {
                shell_step.items.push(SetupItem {
                    name: "env_sh".to_string(),
                    status: "skipped".to_string(),
                    path: None,
                    note: Some("not a container environment".to_string()),
                });
            }
        }
        shell_step.items.push(SetupItem {
            name: "init --global".to_string(),
            status: "ran".to_string(),
            path: None,
            note: None,
        });
        shell_step.items.push(SetupItem {
            name: "universal_shell_hook".to_string(),
            status: "installed".to_string(),
            path: None,
            note: Some("~/.zshenv, ~/.bashenv, agent aliases".to_string()),
        });
    } else {
        shell_step
            .warnings
            .push("non_interactive_without_yes: shell hook not installed (use --yes)".to_string());
        shell_step.ok = false;
        shell_step.items.push(SetupItem {
            name: "init --global".to_string(),
            status: "skipped".to_string(),
            path: None,
            note: Some("requires --yes in --non-interactive mode".to_string()),
        });
    }
    shell_step
}

fn build_daemon_step() -> SetupStepReport {
    let mut daemon_step = setup_step("daemon");
    let was_running = crate::daemon::is_daemon_running();
    if was_running {
        let _ = crate::daemon::stop_daemon();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    match crate::daemon::start_daemon(&[]) {
        Ok(()) => {
            let action = if was_running { "restarted" } else { "started" };
            daemon_step.items.push(SetupItem {
                name: "serve --daemon".to_string(),
                status: action.to_string(),
                path: Some(crate::daemon::daemon_addr().display()),
                note: Some("CLI commands can route via IPC when running".to_string()),
            });
        }
        Err(e) => {
            daemon_step
                .warnings
                .push(format!("daemon start failed (non-fatal): {e}"));
            daemon_step.items.push(SetupItem {
                name: "serve --daemon".to_string(),
                status: "skipped".to_string(),
                path: None,
                note: Some(format!("optional — {e}")),
            });
        }
    }
    daemon_step
}

fn build_editor_step(
    opts: SetupOptions,
    home_str: &str,
    binary: &str,
    targets: &[EditorTarget],
    update_mcp: bool,
) -> SetupStepReport {
    let mut editor_step = setup_step("editors");
    for target in targets {
        let short_path = shorten_path(&target.config_path.to_string_lossy(), home_str);
        if !target.detect_path.exists() {
            editor_step.items.push(SetupItem {
                name: target.name.to_string(),
                status: "not_detected".to_string(),
                path: Some(short_path),
                note: None,
            });
            continue;
        }

        let mode = if target.agent_key.is_empty() {
            HookMode::Mcp
        } else {
            recommend_hook_mode(&target.agent_key)
        };

        if !update_mcp {
            editor_step.items.push(SetupItem {
                name: target.name.to_string(),
                status: "skipped".to_string(),
                path: Some(short_path),
                note: Some(format!(
                    "mode={mode}; MCP registration skipped (auto_update_mcp=false)"
                )),
            });
            continue;
        }

        let res = crate::core::editor_registry::write_config_with_options(
            target,
            binary,
            WriteOptions {
                overwrite_invalid: opts.fix,
            },
        );
        match res {
            Ok(w) => {
                let note_parts: Vec<String> = [Some(format!("mode={mode}")), w.note]
                    .into_iter()
                    .flatten()
                    .collect();
                editor_step.items.push(SetupItem {
                    name: target.name.to_string(),
                    status: match w.action {
                        WriteAction::Created => "created".to_string(),
                        WriteAction::Updated => "updated".to_string(),
                        WriteAction::Already => "already".to_string(),
                    },
                    path: Some(short_path),
                    note: Some(note_parts.join("; ")),
                });
            }
            Err(e) => {
                editor_step.ok = false;
                editor_step.items.push(SetupItem {
                    name: target.name.to_string(),
                    status: "error".to_string(),
                    path: Some(short_path),
                    note: Some(e),
                });
            }
        }
    }
    editor_step
}

fn should_inject_rules(opts: SetupOptions, config_value: bool) -> bool {
    if opts.skip_rules {
        false
    } else if opts.force_inject_rules {
        true
    } else if opts.yes && opts.non_interactive {
        config_value
    } else {
        true
    }
}

fn should_inject_skills(opts: SetupOptions, config_value: bool) -> bool {
    should_inject_rules(opts, config_value)
}

fn build_rules_step(
    opts: SetupOptions,
    home: &std::path::Path,
    should_inject: bool,
) -> SetupStepReport {
    let mut rules_step = setup_step("agent_rules");
    if should_inject {
        let rules_result = crate::rules_inject::inject_all_rules(home);
        for n in rules_result.injected {
            rules_step.items.push(SetupItem {
                name: n,
                status: "injected".to_string(),
                path: None,
                note: None,
            });
        }
        for n in rules_result.updated {
            rules_step.items.push(SetupItem {
                name: n,
                status: "updated".to_string(),
                path: None,
                note: None,
            });
        }
        for n in rules_result.already {
            rules_step.items.push(SetupItem {
                name: n,
                status: "already".to_string(),
                path: None,
                note: None,
            });
        }
        if !rules_result.backed_up.is_empty() {
            for bak in &rules_result.backed_up {
                rules_step.items.push(SetupItem {
                    name: "backup".to_string(),
                    status: "created".to_string(),
                    path: Some(bak.clone()),
                    note: Some("previous version backed up".to_string()),
                });
            }
        }
        for e in rules_result.errors {
            rules_step.ok = false;
            rules_step.errors.push(e);
        }
    } else {
        let reason = if opts.skip_rules {
            "--skip-rules flag set"
        } else {
            "auto_inject_rules not enabled (run `lean-ctx setup` or set auto_inject_rules = true)"
        };
        rules_step.items.push(SetupItem {
            name: "agent_rules".to_string(),
            status: "skipped".to_string(),
            path: None,
            note: Some(reason.to_string()),
        });
    }
    rules_step
}

fn build_rules_dedup_step(home: &std::path::Path, should_inject: bool) -> Option<SetupStepReport> {
    if !should_inject {
        return None;
    }
    let mut dedup_step = setup_step("rules_dedup");
    let project = std::env::current_dir().unwrap_or_else(|_| home.to_path_buf());
    for line in crate::cli::rules_dedup::auto_apply(home, &project) {
        let failed = line.starts_with("FAILED");
        if failed {
            dedup_step.warnings.push(line.clone());
        }
        dedup_step.items.push(SetupItem {
            name: "dedup".to_string(),
            status: if failed { "failed" } else { "applied" }.to_string(),
            path: None,
            note: Some(line),
        });
    }
    Some(dedup_step)
}

fn build_skill_step(
    home: &std::path::Path,
    should_install_skills: bool,
) -> Option<SetupStepReport> {
    let mut skill_step = setup_step("skill_files");
    if should_install_skills {
        let skill_results = crate::rules_inject::install_all_skills(home);
        for (name, is_new) in &skill_results {
            skill_step.items.push(SetupItem {
                name: name.clone(),
                status: if *is_new { "installed" } else { "already" }.to_string(),
                path: None,
                note: Some("SKILL.md".to_string()),
            });
        }
    } else {
        skill_step.items.push(SetupItem {
            name: "skill_files".to_string(),
            status: "skipped".to_string(),
            path: None,
            note: Some("auto_inject_skills not enabled".to_string()),
        });
    }
    (!skill_step.items.is_empty()).then_some(skill_step)
}

fn build_agent_hooks_step(targets: &[EditorTarget], update_mcp: bool) -> Option<SetupStepReport> {
    let mut hooks_step = setup_step("agent_hooks");
    for target in targets {
        if !target.detect_path.exists() || target.agent_key.is_empty() {
            continue;
        }
        let mode = recommend_hook_mode(&target.agent_key);
        crate::hooks::install_agent_hook_with_mode(&target.agent_key, true, mode);
        let mcp_note = if update_mcp {
            match configure_agent_mcp(&target.agent_key) {
                Ok(()) => "; MCP config updated".to_string(),
                Err(e) => format!("; MCP config skipped: {e}"),
            }
        } else {
            "; MCP registration skipped (auto_update_mcp=false)".to_string()
        };
        hooks_step.items.push(SetupItem {
            name: format!("{} hooks", target.name),
            status: "installed".to_string(),
            path: Some(target.detect_path.to_string_lossy().to_string()),
            note: Some(format!(
                "mode={mode}; merge-based install/repair (preserves other hooks/plugins){mcp_note}"
            )),
        });
    }
    (!hooks_step.items.is_empty()).then_some(hooks_step)
}

fn build_tool_profile_step() -> SetupStepReport {
    let mut tool_profile_step = setup_step("tool_profile");
    let cfg = crate::core::config::Config::load();
    if cfg.tool_profile.is_none() && std::env::var("LEAN_CTX_TOOL_PROFILE").is_err() {
        let lazy_count = crate::tool_defs::core_tool_names().len();
        tool_profile_step.items.push(SetupItem {
            name: "tool_profile".to_string(),
            status: "lean default".to_string(),
            path: None,
            note: Some(format!(
                "{lazy_count} tools advertised, all reachable via ctx_call \
                 (pin more with: lean-ctx tools standard|power)"
            )),
        });
    } else {
        let profile = cfg.tool_profile_effective();
        let overhead_hint = match profile {
            crate::core::tool_profiles::ToolProfile::Power => {
                "; advertises ALL tool schemas — `lean-ctx tools lean` cuts this to the lazy core"
            }
            _ => "",
        };
        tool_profile_step.items.push(SetupItem {
            name: "tool_profile".to_string(),
            status: "already".to_string(),
            path: None,
            note: Some(format!("profile={}{overhead_hint}", profile.as_str())),
        });
    }
    tool_profile_step
}

fn build_proxy_step(opts: SetupOptions, home: &std::path::Path) -> SetupStepReport {
    let mut proxy_step = setup_step("proxy");
    if opts.skip_proxy {
        proxy_step.items.push(SetupItem {
            name: "proxy".to_string(),
            status: "skipped".to_string(),
            path: None,
            note: Some("Proxy not enabled (run `lean-ctx proxy enable`)".to_string()),
        });
    } else {
        let proxy_cfg = crate::core::config::Config::load();
        if proxy_cfg.proxy_enabled == Some(true) {
            let proxy_port = crate::proxy_setup::default_port();
            crate::proxy_autostart::install(proxy_port, true);
            std::thread::sleep(std::time::Duration::from_millis(500));
            crate::proxy_setup::install_proxy_env(home, proxy_port, opts.json);
            proxy_step.items.push(SetupItem {
                name: "proxy_autostart".to_string(),
                status: "installed".to_string(),
                path: None,
                note: Some("LaunchAgent/systemd auto-start on login".to_string()),
            });
            proxy_step.items.push(SetupItem {
                name: "proxy_env".to_string(),
                status: "configured".to_string(),
                path: None,
                note: Some("ANTHROPIC_BASE_URL, OPENAI_BASE_URL, GEMINI_API_BASE_URL".to_string()),
            });
        } else {
            proxy_step.items.push(SetupItem {
                name: "proxy".to_string(),
                status: "skipped".to_string(),
                path: None,
                note: Some(
                    "Proxy not opted-in (run `lean-ctx proxy enable` to activate)".to_string(),
                ),
            });
        }
    }
    proxy_step
}

fn build_doctor_compact_step() -> SetupStepReport {
    let mut env_step = setup_step("doctor_compact");
    let (passed, total) = crate::doctor::compact_score();
    env_step.items.push(SetupItem {
        name: "doctor".to_string(),
        status: format!("{passed}/{total}"),
        path: None,
        note: None,
    });
    if passed != total {
        env_step.warnings.push(format!(
            "doctor compact not fully passing: {passed}/{total}"
        ));
    }
    env_step
}

fn maybe_add_project_root_warning(steps: &mut Vec<SetupStepReport>) {
    let has_env_root = std::env::var("LEAN_CTX_PROJECT_ROOT").is_ok_and(|v| !v.is_empty());
    let cfg = crate::core::config::Config::load();
    let has_cfg_root = cfg.project_root.as_ref().is_some_and(|v| !v.is_empty());
    if !has_env_root
        && !has_cfg_root
        && let Ok(cwd) = std::env::current_dir()
    {
        let is_home = dirs::home_dir().is_some_and(|h| cwd == h);
        if is_home {
            let mut root_step = SetupStepReport {
                name: "project_root".to_string(),
                ok: true,
                items: Vec::new(),
                warnings: vec![
                    "No project_root configured. Running from $HOME can cause excessive scanning. \
                     Set via: lean-ctx config set project_root /path/to/project"
                        .to_string(),
                ],
                errors: Vec::new(),
            };
            root_step.items.push(SetupItem {
                name: "project_root".to_string(),
                status: "unconfigured".to_string(),
                path: None,
                note: Some(
                    "Set LEAN_CTX_PROJECT_ROOT or add project_root to config.toml".to_string(),
                ),
            });
            steps.push(root_step);
        }
    }
}

fn maybe_spawn_background_index() {
    if let Ok(cwd) = std::env::current_dir()
        && may_autoindex_cwd(&cwd)
        && crate::core::pathutil::has_project_marker(&cwd)
    {
        spawn_index_build_background(&cwd);
    }
}

fn maybe_enable_ide_config_access(opts: SetupOptions) {
    if opts.yes
        && !opts.fix
        && crate::core::config::Config::load()
            .allow_ide_config_dirs
            .is_none()
    {
        match crate::core::config::Config::update_global(|c| {
            c.allow_ide_config_dirs = Some(true);
        }) {
            Ok(_) => {
                if !opts.json {
                    println!(
                        "  Enabled IDE config access (allow_ide_config_dirs) — \
                         disable: lean-ctx config set allow_ide_config_dirs false"
                    );
                }
            }
            Err(e) => tracing::warn!("could not enable IDE config access: {e}"),
        }
    }
}

fn build_setup_report(
    started_at: chrono::DateTime<Utc>,
    steps: Vec<SetupStepReport>,
) -> SetupReport {
    let finished_at = Utc::now();
    let success = steps.iter().all(|s| s.ok);
    SetupReport {
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
    }
}

fn persist_setup_report(report: &SetupReport) -> Result<(), String> {
    let path = SetupReport::default_path()?;
    let mut content =
        serde_json::to_string_pretty(report).map_err(|e| format!("serialize report: {e}"))?;
    content.push('\n');
    crate::config_io::write_atomic(&path, &content)
}
