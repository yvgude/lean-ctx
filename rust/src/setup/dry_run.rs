// SPDX-License-Identifier: Apache-2.0
//! `setup --dry-run` / `onboard --dry-run` / `update --dry-run`: the files and
//! actions a setup run would touch, computed from the same agent scope and
//! config gates as the real run. Nothing here writes, starts or stops anything.

use std::path::Path;

use crate::core::editor_registry::EditorTarget;
use crate::core::setup_report::{SetupItem, SetupReport, SetupStepReport};

use super::agent_selection::AgentScope;
use super::helpers::shorten_path;
use super::options::SetupOptions;
use super::with_options::{deselected_items, setup_step};

pub(super) struct PlanInput<'a> {
    pub opts: &'a SetupOptions,
    pub home: &'a Path,
    pub scope: &'a AgentScope,
    pub targets: &'a [EditorTarget],
    pub deselected: &'a [EditorTarget],
    pub update_mcp: bool,
    pub inject_rules: bool,
    pub inject_skills: bool,
}

fn item(name: &str, status: &str, path: Option<String>, note: Option<String>) -> SetupItem {
    SetupItem {
        name: name.to_string(),
        status: status.to_string(),
        path,
        note,
    }
}

pub(super) fn plan_steps(input: &PlanInput<'_>) -> Vec<SetupStepReport> {
    let home_str = input.home.to_string_lossy().to_string();
    let short = |p: &Path| shorten_path(&p.to_string_lossy(), &home_str);
    let allows = |key: &str| input.scope.allows(key);
    let cfg = crate::core::config::Config::load();

    let mut selection = setup_step("agent_selection");
    let (status, note) = match input.scope {
        AgentScope::All => ("all_detected", "no selection recorded".to_string()),
        AgentScope::Only(keys) if keys.is_empty() => ("selected", "none".to_string()),
        AgentScope::Only(keys) => (
            "selected",
            keys.iter().cloned().collect::<Vec<_>>().join(","),
        ),
    };
    selection
        .items
        .push(item("scope", status, None, Some(note)));
    if let Some(keys) = &input.opts.agents {
        selection.items.push(item(
            "setup.agents",
            "would_save",
            None,
            Some(keys.iter().cloned().collect::<Vec<_>>().join(",")),
        ));
    }

    let mut shell = setup_step("shell_hook");
    if input.opts.non_interactive && !input.opts.yes {
        shell.items.push(item(
            "init --global",
            "skipped",
            None,
            Some("requires --yes in --non-interactive mode".to_string()),
        ));
    } else if cfg.shell_hook_disabled_effective() {
        shell.items.push(item(
            "shell hook",
            "skipped",
            None,
            Some("shell_hook_disabled=true".to_string()),
        ));
    } else {
        for rc in [".zshenv", ".bashenv"] {
            shell.items.push(item(
                "shell hook",
                "would_update",
                Some(format!("~/{rc}")),
                Some("only if that shell is installed".to_string()),
            ));
        }
        let (status, note) = if cfg.skip_agent_aliases {
            ("would_remove", "skip_agent_aliases=true")
        } else {
            ("would_update", "claude/codex/gemini aliases")
        };
        for rc in [".zshrc", ".bashrc"] {
            shell.items.push(item(
                "agent aliases",
                status,
                Some(format!("~/{rc}")),
                Some(note.to_string()),
            ));
        }
    }

    let mut daemon = setup_step("daemon");
    let status = if crate::daemon::is_daemon_running() {
        "would_restart"
    } else {
        "would_start"
    };
    daemon
        .items
        .push(item("serve --daemon", status, None, None));

    let mut editors = setup_step("editors");
    for target in input.targets {
        let path = short(&target.config_path);
        let (status, note) = if !target.detect_path.exists() {
            ("not_detected", None)
        } else if !input.update_mcp {
            ("skipped", Some("auto_update_mcp=false".to_string()))
        } else if !target.config_path.exists() {
            ("would_create", None)
        } else if std::fs::read_to_string(&target.config_path).is_ok_and(|c| c.contains("lean-ctx"))
        {
            (
                "would_refresh",
                Some("lean-ctx entry present; rewritten only if it differs".to_string()),
            )
        } else {
            ("would_add", Some("adds the lean-ctx MCP entry".to_string()))
        };
        editors
            .items
            .push(item(target.name, status, Some(path), note));
    }
    editors
        .items
        .extend(deselected_items(input.deselected, &home_str));

    let mut hooks = setup_step("agent_hooks");
    for target in input
        .targets
        .iter()
        .filter(|t| t.detect_path.exists() && !t.agent_key.is_empty())
    {
        hooks.items.push(item(
            &format!("{} hooks", target.name),
            "would_install",
            Some(short(&target.detect_path)),
            Some("merge-based; preserves other hooks/plugins".to_string()),
        ));
    }

    let mut rules = setup_step("agent_rules");
    if input.inject_rules {
        for (name, path) in crate::rules_inject::planned_rules_files(input.home, &allows) {
            rules.items.push(item(
                &name,
                "would_write",
                Some(short(&path)),
                Some("marker-delimited lean-ctx block".to_string()),
            ));
        }
    } else {
        rules.items.push(item(
            "agent_rules",
            "skipped",
            None,
            Some(if input.opts.skip_rules {
                "--skip-rules".to_string()
            } else {
                "auto_inject_rules not enabled".to_string()
            }),
        ));
    }

    let mut skills = setup_step("skill_files");
    if input.inject_skills {
        for (name, path) in crate::rules_inject::planned_skill_files(input.home, &allows) {
            skills
                .items
                .push(item(&name, "would_write", Some(short(&path)), None));
        }
    } else {
        skills.items.push(item(
            "skill_files",
            "skipped",
            None,
            Some("auto_inject_skills not enabled".to_string()),
        ));
    }

    let mut steps = vec![selection, shell, daemon, editors, hooks, rules, skills];

    if input
        .targets
        .iter()
        .any(|t| t.agent_key == "claude" && t.detect_path.exists())
    {
        let mut claude_mod = setup_step("claude_mod");
        claude_mod.items.push(item(
            crate::hooks::agents::claude_mod::PLUGIN_ID,
            "would_install_or_refresh",
            None,
            Some("skipped if you removed it (lean-ctx claude-mod uninstall)".to_string()),
        ));
        steps.push(claude_mod);
    }

    let mut proxy = setup_step("proxy");
    if !input.opts.skip_proxy && cfg.proxy_enabled == Some(true) {
        proxy.items.push(item(
            "proxy_autostart",
            "would_install",
            None,
            Some("LaunchAgent/systemd auto-start + base-URL env".to_string()),
        ));
    } else {
        proxy.items.push(item(
            "proxy",
            "skipped",
            None,
            Some("not enabled".to_string()),
        ));
    }
    steps.push(proxy);

    if input.opts.yes && !input.opts.fix && cfg.allow_ide_config_dirs.is_none() {
        let mut config = setup_step("config");
        config.items.push(item(
            "allow_ide_config_dirs",
            "would_set",
            None,
            Some("true".to_string()),
        ));
        steps.push(config);
    }

    steps
}

/// Human-readable dry-run plan. `--json` callers print the report instead.
pub fn print_plan(report: &SetupReport) {
    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let rst = "\x1b[0m";
    println!();
    println!("  {bold}Dry run — nothing was written.{rst} This run would:");
    for step in &report.steps {
        let items: Vec<&SetupItem> = step
            .items
            .iter()
            .filter(|i| i.status != "not_detected")
            .collect();
        if items.is_empty() {
            continue;
        }
        println!();
        println!("  {bold}{}{rst}", step.name);
        for i in items {
            let path = i
                .path
                .as_deref()
                .map(|p| format!("  {p}"))
                .unwrap_or_default();
            let note = i
                .note
                .as_deref()
                .map(|n| format!("  {dim}({n}){rst}"))
                .unwrap_or_default();
            println!("    {:<24} {:<26}{path}{note}", i.status, i.name);
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::super::options::SetupOptions;
    use super::super::with_options::run_setup_with_options;

    /// The plan's content. That a dry run writes nothing is proven out of
    /// process (`tests/suite/setup_dry_run.rs`): in here the data-dir env is
    /// process-wide, so other tests' background writers can land in it.
    #[test]
    fn dry_run_reports_the_selection_without_recording_it() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".cursor")).unwrap();
        std::fs::create_dir_all(home.path().join(".codeium/windsurf")).unwrap();
        let _home = crate::setup::EnvVarGuard::set("HOME", home.path().to_str().unwrap());

        let report = run_setup_with_options(&SetupOptions {
            non_interactive: true,
            yes: true,
            fix: true,
            json: true,
            dry_run: true,
            agents: Some(["cursor".to_string()].into_iter().collect()),
            ..Default::default()
        })
        .expect("dry run");

        let editors = report.steps.iter().find(|s| s.name == "editors").unwrap();
        let status = |name: &str| {
            editors
                .items
                .iter()
                .find(|i| i.name == name)
                .map(|i| i.status.clone())
        };
        assert_eq!(status("Cursor").as_deref(), Some("would_create"));
        assert_eq!(status("Windsurf").as_deref(), Some("deselected"));
        assert!(
            !home.path().join(".cursor/mcp.json").exists(),
            "dry run registered the MCP server"
        );

        let selection = report
            .steps
            .iter()
            .find(|s| s.name == "agent_selection")
            .unwrap();
        assert!(
            selection
                .items
                .iter()
                .any(|i| i.name == "setup.agents" && i.status == "would_save")
        );
        assert!(
            crate::core::config::Config::load_global()
                .setup
                .agents
                .is_none(),
            "dry run must not record the selection"
        );
    }
}
