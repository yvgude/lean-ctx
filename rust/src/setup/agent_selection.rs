// SPDX-License-Identifier: Apache-2.0
//! Which AI tools `setup`, `onboard` and the post-update refresh may touch.
//!
//! Precedence: an explicit `--agents` list, then the saved `setup.agents`,
//! then — for the post-update refresh only — the tools that already carry a
//! lean-ctx entry. A setup with neither keeps the historical default and
//! offers every detected tool. An update therefore never connects a tool that
//! was installed after the last setup.

use std::collections::BTreeSet;

pub use crate::core::editor_registry::known_agent_keys;
use crate::core::editor_registry::{EditorTarget, PI_AGENT_KEY as PI_KEY};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentScope {
    /// Every detected tool (no selection recorded, fresh setup).
    All,
    /// Only these agent keys.
    Only(BTreeSet<String>),
}

impl AgentScope {
    #[must_use]
    pub fn allows(&self, agent_key: &str) -> bool {
        match self {
            Self::All => true,
            Self::Only(keys) => keys.contains(agent_key),
        }
    }
}

/// One selectable tool. Several registry targets can share an agent key
/// (Augment CLI + VS Code, Cline + Cline CLI, Qoder paths); they are one choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedAgent {
    pub key: String,
    pub label: String,
    /// Already carries a lean-ctx entry from an earlier setup.
    pub connected: bool,
}

/// Parse a `--agents` value: comma-separated agent keys, or `none` to connect
/// no tool at all. Unknown names are an error, never silently dropped.
pub fn parse_agent_list(raw: &str, known: &BTreeSet<String>) -> Result<BTreeSet<String>, String> {
    let items: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        return Err("--agents needs a comma-separated list of agent keys (or `none`)".to_string());
    }
    if items == ["none"] {
        return Ok(BTreeSet::new());
    }
    let unknown: Vec<&str> = items
        .iter()
        .filter(|item| !known.contains(*item))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "unknown agent(s): {}\nKnown agents: {}",
            unknown.join(", "),
            known.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(items.into_iter().collect())
}

/// Pull `--agents <list>` / `--agents=<list>` out of CLI args. Returns the
/// remaining args and the raw list; `Err` when the flag has no value.
pub fn take_agents_flag(args: &[String]) -> Result<(Vec<String>, Option<String>), String> {
    let mut rest = Vec::with_capacity(args.len());
    let mut raw = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--agents=") {
            raw = Some(value.to_string());
        } else if arg == "--agents" {
            match iter.next() {
                Some(value) if !value.starts_with('-') => raw = Some(value.clone()),
                _ => {
                    return Err(
                        "--agents needs a value, e.g. --agents claude,cursor (or none)".to_string(),
                    );
                }
            }
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((rest, raw))
}

/// Resolve the scope for one run. `connected` is only evaluated for the
/// post-update refresh without a recorded choice.
pub fn resolve_scope(
    explicit: Option<&BTreeSet<String>>,
    saved: Option<&[String]>,
    refresh_only: bool,
    connected: impl FnOnce() -> BTreeSet<String>,
) -> AgentScope {
    if let Some(keys) = explicit {
        return AgentScope::Only(keys.clone());
    }
    if let Some(keys) = saved {
        return AgentScope::Only(keys.iter().map(|k| k.trim().to_ascii_lowercase()).collect());
    }
    if refresh_only {
        return AgentScope::Only(connected());
    }
    AgentScope::All
}

fn has_lean_ctx_entry(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|content| content.contains("lean-ctx"))
}

/// Installed tools, one entry per agent key, in registry order.
#[must_use]
pub fn detected_agents(home: &std::path::Path, targets: &[EditorTarget]) -> Vec<DetectedAgent> {
    let mut agents: Vec<DetectedAgent> = Vec::new();
    for target in targets {
        if target.agent_key.is_empty() || !target.detect_path.exists() {
            continue;
        }
        let connected = has_lean_ctx_entry(&target.config_path);
        if let Some(existing) = agents.iter_mut().find(|a| a.key == target.agent_key) {
            if !existing.label.split(" / ").any(|l| l == target.name) {
                existing.label = format!("{} / {}", existing.label, target.name);
            }
            existing.connected |= connected;
        } else {
            agents.push(DetectedAgent {
                key: target.agent_key.clone(),
                label: target.name.to_string(),
                connected,
            });
        }
    }
    let pi_dir = home.join(".pi");
    if pi_dir.exists() {
        agents.push(DetectedAgent {
            key: PI_KEY.to_string(),
            label: "Pi Coding Agent".to_string(),
            connected: has_lean_ctx_entry(&pi_dir.join("rules/lean-ctx.md")),
        });
    }
    agents
}

/// Agent keys an earlier setup connected (a lean-ctx entry is present).
#[must_use]
pub fn connected_agent_keys(home: &std::path::Path, targets: &[EditorTarget]) -> BTreeSet<String> {
    detected_agents(home, targets)
        .into_iter()
        .filter(|a| a.connected)
        .map(|a| a.key)
        .collect()
}

/// Scope for a setup run: `--agents`, then `setup.agents`, then (refresh only)
/// the already-connected tools, else every detected tool.
#[must_use]
pub fn scope_for_run(
    home: &std::path::Path,
    targets: &[EditorTarget],
    explicit: Option<&BTreeSet<String>>,
    refresh_only: bool,
) -> AgentScope {
    let saved = crate::core::config::Config::load_global().setup.agents;
    resolve_scope(explicit, saved.as_deref(), refresh_only, || {
        connected_agent_keys(home, targets)
    })
}

/// Record the choice so later updates refresh exactly these tools.
pub fn save_selection(keys: &BTreeSet<String>) -> Result<(), String> {
    crate::core::config::Config::update_global(|cfg| {
        cfg.setup.agents = Some(keys.iter().cloned().collect());
    })
    .map(|_| ())
    .map_err(|e| format!("could not save the agent selection: {e}"))
}

/// `init --agent <key>` connects a tool on purpose: add it to a recorded
/// selection so later updates keep it current. Without a recorded selection
/// nothing is written — updates already follow the connected tools.
pub fn remember_connected_agent(agent_key: &str) -> Result<(), String> {
    let key = agent_key.trim().to_ascii_lowercase();
    let Some(mut saved) = crate::core::config::Config::load_global().setup.agents else {
        return Ok(());
    };
    if saved.iter().any(|k| k.eq_ignore_ascii_case(&key)) {
        return Ok(());
    }
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return Ok(());
    };
    if !known_agent_keys(&crate::core::editor_registry::build_targets(&home)).contains(&key) {
        return Ok(());
    }
    saved.push(key);
    save_selection(&saved.into_iter().collect())
}

/// Interpret the answer to the selection prompt. Empty keeps `default`;
/// otherwise `all`, `none`, or numbers / agent keys separated by commas or
/// spaces select exactly those tools.
pub fn parse_selection_answer(
    answer: &str,
    agents: &[DetectedAgent],
    default: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let answer = answer.trim().to_ascii_lowercase();
    match answer.as_str() {
        "" => return Ok(default.clone()),
        "all" | "a" => return Ok(agents.iter().map(|a| a.key.clone()).collect()),
        "none" | "n" => return Ok(BTreeSet::new()),
        _ => {}
    }
    let mut selected = BTreeSet::new();
    for token in answer
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        let agent = match token.parse::<usize>() {
            Ok(n) => n.checked_sub(1).and_then(|i| agents.get(i)),
            Err(_) => agents.iter().find(|a| a.key == token),
        };
        match agent {
            Some(agent) => {
                selected.insert(agent.key.clone());
            }
            None => return Err(format!("'{token}' is not in the list")),
        }
    }
    Ok(selected)
}

/// Default for the prompt: the recorded choice, else the connected tools,
/// else (first setup) every detected tool.
#[must_use]
pub fn prompt_default(agents: &[DetectedAgent], saved: Option<&[String]>) -> BTreeSet<String> {
    let detected: BTreeSet<String> = agents.iter().map(|a| a.key.clone()).collect();
    if let Some(saved) = saved {
        return saved
            .iter()
            .map(|k| k.trim().to_ascii_lowercase())
            .filter(|k| detected.contains(k))
            .collect();
    }
    let connected: BTreeSet<String> = agents
        .iter()
        .filter(|a| a.connected)
        .map(|a| a.key.clone())
        .collect();
    if connected.is_empty() {
        detected
    } else {
        connected
    }
}

/// Ask which detected tools lean-ctx should connect. Reads stdin; callers
/// must only use it on an interactive terminal.
#[must_use]
pub fn prompt_agent_selection(
    agents: &[DetectedAgent],
    default: &BTreeSet<String>,
) -> BTreeSet<String> {
    use std::io::Write;

    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let rst = "\x1b[0m";
    println!("  Which AI tools should lean-ctx connect to?");
    for (i, agent) in agents.iter().enumerate() {
        let mark = if default.contains(&agent.key) {
            "x"
        } else {
            " "
        };
        let state = if agent.connected { " (connected)" } else { "" };
        println!(
            "    [{mark}] {:>2}. {:<28} {dim}{}{state}{rst}",
            i + 1,
            agent.label,
            agent.key
        );
    }
    println!(
        "  {dim}Enter = keep [x] · numbers or names (e.g. 1,3 or claude cursor) · all · none{rst}"
    );
    for _ in 0..3 {
        print!("  {bold}>{rst} ");
        std::io::stdout().flush().ok();
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).unwrap_or(0) == 0 {
            return default.clone();
        }
        match parse_selection_answer(&input, agents, default) {
            Ok(selected) => return selected,
            Err(e) => println!("  \x1b[33m{e}{rst} — try again"),
        }
    }
    println!("  {dim}Keeping the marked selection.{rst}");
    default.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(ToString::to_string).collect()
    }

    fn agents() -> Vec<DetectedAgent> {
        ["claude", "cursor", "codex"]
            .iter()
            .enumerate()
            .map(|(i, key)| DetectedAgent {
                key: (*key).to_string(),
                label: key.to_uppercase(),
                connected: i == 0,
            })
            .collect()
    }

    #[test]
    fn agents_flag_validates_names() {
        let known = keys(&["claude", "cursor", "codex", "pi"]);
        assert_eq!(
            parse_agent_list("Claude, cursor", &known).unwrap(),
            keys(&["claude", "cursor"])
        );
        assert_eq!(parse_agent_list("none", &known).unwrap(), keys(&[]));
        let err = parse_agent_list("claude,vscodee", &known).unwrap_err();
        assert!(err.contains("unknown agent(s): vscodee"), "{err}");
        assert!(
            err.contains("Known agents: claude, codex, cursor, pi"),
            "{err}"
        );
        assert!(parse_agent_list(" , ", &known).is_err());
    }

    #[test]
    fn agents_flag_is_extracted_in_both_spellings() {
        let args: Vec<String> = ["--yes", "--agents", "claude,cursor", "--json"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let (rest, raw) = take_agents_flag(&args).unwrap();
        assert_eq!(rest, ["--yes", "--json"]);
        assert_eq!(raw.as_deref(), Some("claude,cursor"));

        let (rest, raw) = take_agents_flag(&["--agents=codex".to_string()]).unwrap();
        assert!(rest.is_empty());
        assert_eq!(raw.as_deref(), Some("codex"));

        assert!(take_agents_flag(&["--agents".to_string()]).is_err());
        assert!(take_agents_flag(&["--agents".to_string(), "--yes".to_string()]).is_err());
    }

    #[test]
    fn explicit_beats_saved_beats_connected() {
        let explicit = keys(&["codex"]);
        let saved = vec!["Cursor".to_string()];
        let never = || -> BTreeSet<String> { panic!("connected must not be evaluated") };

        assert_eq!(
            resolve_scope(Some(&explicit), Some(&saved), true, never),
            AgentScope::Only(explicit.clone())
        );
        assert_eq!(
            resolve_scope(None, Some(&saved), true, never),
            AgentScope::Only(keys(&["cursor"]))
        );
    }

    #[test]
    fn update_refresh_never_adds_newly_detected_tools() {
        let scope = resolve_scope(None, None, true, || keys(&["claude"]));
        assert!(scope.allows("claude"));
        assert!(
            !scope.allows("cursor"),
            "a tool installed later stays untouched"
        );

        let nothing_connected = resolve_scope(None, None, true, BTreeSet::new);
        assert!(!nothing_connected.allows("claude"));
    }

    #[test]
    fn fresh_setup_without_choice_keeps_all_detected() {
        assert_eq!(
            resolve_scope(None, None, false, BTreeSet::new),
            AgentScope::All
        );
        assert!(AgentScope::All.allows("anything"));
    }

    #[test]
    fn selection_answer_forms() {
        let agents = agents();
        let default = keys(&["claude"]);
        assert_eq!(
            parse_selection_answer("\n", &agents, &default).unwrap(),
            default
        );
        assert_eq!(
            parse_selection_answer("all", &agents, &default).unwrap(),
            keys(&["claude", "cursor", "codex"])
        );
        assert_eq!(
            parse_selection_answer("none", &agents, &default).unwrap(),
            keys(&[])
        );
        assert_eq!(
            parse_selection_answer("2 3", &agents, &default).unwrap(),
            keys(&["cursor", "codex"])
        );
        assert_eq!(
            parse_selection_answer("codex,1", &agents, &default).unwrap(),
            keys(&["claude", "codex"])
        );
        assert!(parse_selection_answer("4", &agents, &default).is_err());
        assert!(parse_selection_answer("0", &agents, &default).is_err());
        assert!(parse_selection_answer("vim", &agents, &default).is_err());
    }

    #[test]
    fn prompt_default_prefers_saved_then_connected_then_all() {
        let agents = agents();
        let saved = vec!["codex".to_string(), "gone".to_string()];
        assert_eq!(prompt_default(&agents, Some(&saved)), keys(&["codex"]));
        assert_eq!(prompt_default(&agents, None), keys(&["claude"]));

        let fresh: Vec<DetectedAgent> = agents
            .into_iter()
            .map(|a| DetectedAgent {
                connected: false,
                ..a
            })
            .collect();
        assert_eq!(
            prompt_default(&fresh, None),
            keys(&["claude", "cursor", "codex"])
        );
    }

    #[test]
    fn detection_groups_targets_by_key_and_reads_connection_state() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mk = |name: &'static str, key: &str, dir: &str, content: Option<&str>| {
            let detect = home.join(dir);
            std::fs::create_dir_all(&detect).unwrap();
            let config = detect.join("mcp.json");
            if let Some(content) = content {
                std::fs::write(&config, content).unwrap();
            }
            EditorTarget {
                name,
                agent_key: key.to_string(),
                config_path: config,
                detect_path: detect,
                config_type: crate::core::editor_registry::ConfigType::McpJson,
            }
        };
        let targets = vec![
            mk("Cline", "cline", "cline-ext", None),
            mk(
                "Cline CLI",
                "cline",
                "cline-cli",
                Some(r#"{"mcpServers":{"lean-ctx":{}}}"#),
            ),
            mk("Cursor", "cursor", "cursor", Some("{}")),
            EditorTarget {
                name: "Zed",
                agent_key: "zed".to_string(),
                config_path: home.join("missing/settings.json"),
                detect_path: home.join("missing"),
                config_type: crate::core::editor_registry::ConfigType::Zed,
            },
        ];

        let detected = detected_agents(home, &targets);
        assert_eq!(
            detected,
            vec![
                DetectedAgent {
                    key: "cline".to_string(),
                    label: "Cline / Cline CLI".to_string(),
                    connected: true,
                },
                DetectedAgent {
                    key: "cursor".to_string(),
                    label: "Cursor".to_string(),
                    connected: false,
                },
            ]
        );
        assert_eq!(connected_agent_keys(home, &targets), keys(&["cline"]));
    }
}
