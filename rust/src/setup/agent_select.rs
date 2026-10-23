//! Which AI tools `setup` / `onboard` / `update` may wire up, and a
//! read-only preview (`--dry-run`) of what a run would touch.
//!
//! The selection lives in `setup.agents` (global config). `None` keeps the
//! unattended default: every detected tool, including ones installed later.

use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::core::config::{Config, agent_selection_allows};
use crate::core::editor_registry::EditorTarget;

/// Parse an `--agents` value. `all` clears the selection (every detected
/// tool), `none` selects nothing; otherwise each comma-separated entry must
/// name a supported tool by key or display name and is stored as its key.
pub fn parse_agents_arg(
    value: &str,
    targets: &[EditorTarget],
) -> Result<Option<Vec<String>>, String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    if value.eq_ignore_ascii_case("none") {
        return Ok(Some(Vec::new()));
    }
    let mut keys: Vec<String> = Vec::new();
    for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Some(target) = targets.iter().find(|t| {
            entry.eq_ignore_ascii_case(&t.agent_key) || entry.eq_ignore_ascii_case(t.name)
        }) else {
            return Err(format!(
                "unknown agent '{entry}'. Supported: {}",
                supported_keys(targets).join(", ")
            ));
        };
        if !keys.contains(&target.agent_key) {
            keys.push(target.agent_key.clone());
        }
    }
    if keys.is_empty() {
        return Err("--agents needs a value: a comma-separated list, `all` or `none`".to_string());
    }
    Ok(Some(keys))
}

fn supported_keys(targets: &[EditorTarget]) -> Vec<&str> {
    let mut keys: Vec<&str> = targets
        .iter()
        .map(|t| t.agent_key.as_str())
        .filter(|k| !k.is_empty())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// Value of `--agents <list>` / `--agents=<list>`, if present.
pub fn agents_flag_value(args: &[String]) -> Option<Result<String, String>> {
    let pos = args
        .iter()
        .position(|a| a == "--agents" || a.starts_with("--agents="))?;
    let arg = &args[pos];
    if let Some(v) = arg.strip_prefix("--agents=") {
        return Some(Ok(v.to_string()));
    }
    Some(
        args.get(pos + 1)
            .filter(|v| !v.starts_with('-'))
            .cloned()
            .ok_or_else(|| {
                "--agents needs a value: a comma-separated list, `all` or `none`".to_string()
            }),
    )
}

/// An explicit `--agents` request; the inner `None` means `all`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedAgents(pub Option<Vec<String>>);

/// Parsed `--agents` flag: `None` when absent, else the requested selection.
pub fn resolve_agents_flag(args: &[String]) -> Result<Option<RequestedAgents>, String> {
    let Some(value) = agents_flag_value(args) else {
        return Ok(None);
    };
    let home = crate::core::home::resolve_home_dir()
        .ok_or_else(|| "Cannot determine home directory".to_string())?;
    let targets = crate::core::editor_registry::build_targets(&home);
    parse_agents_arg(&value?, &targets).map(|s| Some(RequestedAgents(s)))
}

/// `args` without `--agents` and its value, for unknown-flag checks.
pub fn strip_agents_flag(args: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut skip_value = false;
    for a in args {
        if skip_value {
            skip_value = false;
            if !a.starts_with('-') {
                continue;
            }
        }
        if a == "--agents" {
            skip_value = true;
            continue;
        }
        if a.starts_with("--agents=") {
            continue;
        }
        out.push(a);
    }
    out
}

/// Print (or emit as JSON) what a setup run would do with `selection`.
pub fn run_dry_run(
    selection: Option<&[String]>,
    skip_rules: bool,
    json: bool,
) -> Result<(), String> {
    let home = crate::core::home::resolve_home_dir()
        .ok_or_else(|| "Cannot determine home directory".to_string())?;
    let plan = build_plan(&home, selection, skip_rules);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?
        );
    } else {
        print_plan(&plan);
    }
    Ok(())
}

pub fn persist_agent_selection(selection: Option<Vec<String>>) -> Result<(), String> {
    Config::update_global(|c| c.setup.agents = selection)
        .map(|_| ())
        .map_err(|e| format!("could not save setup.agents: {e}"))
}

pub fn describe_selection(selection: Option<&[String]>) -> String {
    match selection {
        None => "all detected AI tools".to_string(),
        Some([]) => "no AI tools".to_string(),
        Some(list) => list.join(", "),
    }
}

/// Ask which detected tools to connect. Returns the new selection, or the
/// current one unchanged when nothing is detected or input ends.
pub fn prompt_agent_selection(
    targets: &[EditorTarget],
    current: Option<&[String]>,
) -> Option<Vec<String>> {
    let detected: Vec<&EditorTarget> = targets.iter().filter(|t| t.detect_path.exists()).collect();
    if detected.is_empty() {
        return current.map(<[String]>::to_vec);
    }

    println!("  Detected AI tools:");
    for (i, t) in detected.iter().enumerate() {
        let mark = if agent_selection_allows(current, &t.agent_key, t.name) {
            "\x1b[32m[x]\x1b[0m"
        } else {
            "[ ]"
        };
        println!(
            "    {mark} {:>2}) {:<22} \x1b[2m{}\x1b[0m",
            i + 1,
            t.name,
            t.agent_key
        );
    }
    println!();
    println!(
        "  \x1b[2mEnter = keep ({}) · a = all, incl. tools installed later · n = none\x1b[0m",
        describe_selection(current)
    );
    println!("  \x1b[2mOr list numbers/keys, e.g. 1,3 or claude,cursor\x1b[0m");

    for _ in 0..3 {
        print!("  Connect which tools? ");
        let _ = std::io::stdout().flush();
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).unwrap_or(0) == 0 {
            return current.map(<[String]>::to_vec);
        }
        match parse_choice(input.trim(), &detected, current) {
            Ok(selection) => return selection,
            Err(e) => println!("  \x1b[33m{e}\x1b[0m"),
        }
    }
    println!("  \x1b[2mKeeping the current selection.\x1b[0m");
    current.map(<[String]>::to_vec)
}

fn parse_choice(
    input: &str,
    detected: &[&EditorTarget],
    current: Option<&[String]>,
) -> Result<Option<Vec<String>>, String> {
    match input.to_ascii_lowercase().as_str() {
        "" => return Ok(current.map(<[String]>::to_vec)),
        "a" | "all" => return Ok(None),
        "n" | "none" => return Ok(Some(Vec::new())),
        _ => {}
    }
    let mut keys: Vec<String> = Vec::new();
    for token in input
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        let target = match token.parse::<usize>() {
            Ok(n) => detected
                .get(n.wrapping_sub(1))
                .ok_or_else(|| format!("No tool numbered {n}."))?,
            Err(_) => detected
                .iter()
                .find(|t| {
                    token.eq_ignore_ascii_case(&t.agent_key) || token.eq_ignore_ascii_case(t.name)
                })
                .ok_or_else(|| format!("'{token}' is not a detected tool."))?,
        };
        if !keys.contains(&target.agent_key) {
            keys.push(target.agent_key.clone());
        }
    }
    Ok(Some(keys))
}

#[derive(Debug, Serialize)]
pub struct PlannedAgent {
    pub name: String,
    pub key: String,
    pub selected: bool,
    pub config_path: String,
    pub hook_mode: String,
}

/// What a setup run would do, computed without writing anything.
#[derive(Debug, Serialize)]
pub struct SetupPlan {
    pub dry_run: bool,
    /// `None` = every detected tool.
    pub agent_selection: Option<Vec<String>>,
    pub agents: Vec<PlannedAgent>,
    pub not_detected: usize,
    pub mcp_registration: bool,
    pub shell_hook: bool,
    pub agent_aliases: Vec<String>,
    pub rules: bool,
    pub skills: bool,
    pub proxy: bool,
    pub daemon: &'static str,
}

pub fn build_plan(home: &Path, selection: Option<&[String]>, skip_rules: bool) -> SetupPlan {
    let cfg = Config::load();
    let targets = crate::core::editor_registry::build_targets(home);
    let home_str = home.to_string_lossy();
    let mut agents = Vec::new();
    let mut not_detected = 0;
    for t in &targets {
        if !t.detect_path.exists() {
            not_detected += 1;
            continue;
        }
        let mode = if t.agent_key.is_empty() {
            crate::hooks::HookMode::Mcp
        } else {
            crate::hooks::recommend_hook_mode(&t.agent_key)
        };
        agents.push(PlannedAgent {
            name: t.name.to_string(),
            key: t.agent_key.clone(),
            selected: agent_selection_allows(selection, &t.agent_key, t.name),
            config_path: super::helpers::shorten_path(&t.config_path.to_string_lossy(), &home_str),
            hook_mode: mode.to_string(),
        });
    }
    let steering_on =
        cfg.rules_injection_effective() != crate::core::config::RulesInjection::Off && !skip_rules;
    let agent_aliases = if cfg.skip_agent_aliases {
        Vec::new()
    } else {
        crate::shell_hook::selected_agent_aliases(selection)
            .into_iter()
            .map(|(alias, _)| alias.to_string())
            .collect()
    };
    SetupPlan {
        dry_run: true,
        agent_selection: selection.map(<[String]>::to_vec),
        agents,
        not_detected,
        mcp_registration: cfg.setup.should_update_mcp(),
        shell_hook: !cfg.shell_hook_disabled_effective(),
        agent_aliases,
        rules: steering_on && cfg.setup.should_inject_rules(),
        skills: steering_on && cfg.setup.should_inject_skills(),
        proxy: cfg.proxy_enabled == Some(true),
        daemon: if crate::daemon::is_daemon_running() {
            "restart"
        } else {
            "start"
        },
    }
}

pub fn print_plan(plan: &SetupPlan) {
    let on_off = |b: bool| if b { "yes" } else { "no" };
    println!();
    println!("  \x1b[1mDry run — nothing is written.\x1b[0m");
    println!(
        "  Agent selection: {}",
        describe_selection(plan.agent_selection.as_deref())
    );
    println!();
    if plan.agents.is_empty() {
        println!("  No AI tools detected.");
    }
    for a in &plan.agents {
        if a.selected {
            let mcp = if plan.mcp_registration {
                format!("MCP + hooks ({}) → {}", a.hook_mode, a.config_path)
            } else {
                format!(
                    "hooks ({}), MCP skipped (auto_update_mcp=false)",
                    a.hook_mode
                )
            };
            println!("  \x1b[32m+\x1b[0m {:<22} {mcp}", a.name);
        } else {
            println!("  \x1b[2m- {:<22} skipped (not selected)\x1b[0m", a.name);
        }
    }
    if plan.not_detected > 0 {
        println!(
            "  \x1b[2m○ {} supported tools not installed\x1b[0m",
            plan.not_detected
        );
    }
    println!();
    println!(
        "  Shell hook (~/.zshenv, ~/.bashenv): {}",
        if plan.shell_hook {
            "refresh"
        } else {
            "disabled"
        }
    );
    println!(
        "  Agent aliases: {}",
        if plan.agent_aliases.is_empty() {
            "none".to_string()
        } else {
            plan.agent_aliases.join(", ")
        }
    );
    println!("  Rules for selected tools: {}", on_off(plan.rules));
    println!("  Skill files for selected tools: {}", on_off(plan.skills));
    println!("  Daemon: {}", plan.daemon);
    println!(
        "  API proxy: {}",
        if plan.proxy { "refresh" } else { "off" }
    );
    println!();
    println!("  \x1b[2mChoose tools: lean-ctx setup --agents claude,cursor  (all / none)\x1b[0m");
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor_registry::ConfigType;

    fn target(name: &'static str, key: &str) -> EditorTarget {
        EditorTarget {
            name,
            agent_key: key.to_string(),
            config_path: "/nonexistent/config.json".into(),
            detect_path: "/nonexistent".into(),
            config_type: ConfigType::McpJson,
        }
    }

    fn targets() -> Vec<EditorTarget> {
        vec![
            target("Cursor", "cursor"),
            target("Claude Code", "claude"),
            target("VS Code", "vscode"),
            target("Cline", "cline"),
            target("Cline CLI", "cline"),
        ]
    }

    #[test]
    fn agents_arg_all_none_and_list() {
        let t = targets();
        assert_eq!(parse_agents_arg("all", &t).unwrap(), None);
        assert_eq!(parse_agents_arg("NONE", &t).unwrap(), Some(vec![]));
        assert_eq!(
            parse_agents_arg("Claude Code, cursor,claude", &t).unwrap(),
            Some(vec!["claude".to_string(), "cursor".to_string()])
        );
    }

    #[test]
    fn agents_arg_rejects_unknown_and_empty() {
        let t = targets();
        let err = parse_agents_arg("claude,emacsx", &t).unwrap_err();
        assert!(err.contains("emacsx") && err.contains("cursor"), "{err}");
        assert!(parse_agents_arg(" , ", &t).is_err());
    }

    #[test]
    fn agents_flag_value_forms() {
        let args = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            agents_flag_value(&args(&["--agents", "claude"])),
            Some(Ok("claude".to_string()))
        );
        assert_eq!(
            agents_flag_value(&args(&["--agents=cursor,claude"])),
            Some(Ok("cursor,claude".to_string()))
        );
        assert!(matches!(
            agents_flag_value(&args(&["--agents", "--dry-run"])),
            Some(Err(_))
        ));
        assert_eq!(agents_flag_value(&args(&["--yes"])), None);
    }

    #[test]
    fn interactive_choice_parsing() {
        let t = targets();
        let detected: Vec<&EditorTarget> = t.iter().collect();
        let current = vec!["cursor".to_string()];
        assert_eq!(
            parse_choice("", &detected, Some(&current)).unwrap(),
            Some(current.clone())
        );
        assert_eq!(parse_choice("a", &detected, Some(&current)).unwrap(), None);
        assert_eq!(parse_choice("n", &detected, None).unwrap(), Some(vec![]));
        assert_eq!(
            parse_choice("2, 4 5 vscode", &detected, None).unwrap(),
            Some(vec![
                "claude".to_string(),
                "cline".to_string(),
                "vscode".to_string()
            ])
        );
        assert!(parse_choice("9", &detected, None).is_err());
        assert!(parse_choice("0", &detected, None).is_err());
        assert!(parse_choice("zed", &detected, None).is_err());
    }

    #[test]
    fn selection_matches_key_or_name() {
        let sel = vec!["claude".to_string(), "VS Code".to_string()];
        assert!(agent_selection_allows(Some(&sel), "claude", "Claude Code"));
        assert!(agent_selection_allows(Some(&sel), "vscode", "VS Code"));
        assert!(!agent_selection_allows(Some(&sel), "cursor", "Cursor"));
        assert!(!agent_selection_allows(Some(&[]), "cursor", "Cursor"));
        assert!(agent_selection_allows(None, "cursor", "Cursor"));
    }
}
