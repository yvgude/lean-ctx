// SPDX-License-Identifier: Apache-2.0
use super::*;
use lean_ctx::core::config::{RulesInjection, RulesScope};
use std::path::Path;

fn write(home: &Path, rel: &str, content: &str) {
    let p = home.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn check(home: &Path, scope: RulesScope, injection: RulesInjection) -> Outcome {
    claude_instructions_check(home, scope, injection)
}

#[test]
fn recent_sessions_line_names_each_workspace_once_with_age() {
    use lean_ctx::core::session::SessionSummary;
    let now = chrono::Utc::now();
    let mk = |root: Option<&str>, mins_ago: i64| SessionSummary {
        id: format!("s{mins_ago}"),
        started_at: now - chrono::Duration::minutes(mins_ago + 5),
        updated_at: now - chrono::Duration::minutes(mins_ago),
        version: 1,
        task: None,
        tool_calls: 3,
        tokens_saved: 100,
        project_root: root.map(str::to_string),
    };

    // Two windows on two projects + a stale duplicate of the first root +
    // a rootless session that must be skipped.
    let line = environment::format_recent_sessions(
        vec![
            mk(Some("/Users/me/work/frontend"), 4),
            mk(Some("/Users/me/work/backend"), 90),
            mk(Some("/Users/me/work/frontend"), 200),
            mk(None, 1),
        ],
        now,
    )
    .expect("sessions exist");

    assert_eq!(line, "recent: frontend (4m ago), backend (1h ago)");
}

#[test]
fn recent_sessions_line_is_none_without_any_rooted_session() {
    let now = chrono::Utc::now();
    assert_eq!(environment::format_recent_sessions(vec![], now), None);
}

#[test]
fn humanized_ages_cover_all_magnitudes() {
    use chrono::Duration;
    assert_eq!(environment::humanize_age(Duration::seconds(20)), "just now");
    assert_eq!(environment::humanize_age(Duration::minutes(59)), "59m ago");
    assert_eq!(environment::humanize_age(Duration::hours(47)), "47h ago");
    assert_eq!(environment::humanize_age(Duration::days(3)), "3d ago");
}

#[test]
fn capacity_hint_is_actionable_for_both_states() {
    // WARN (at/near cap): reassure it is by-design, point at the cap lever.
    let warn = capacity_hint(false);
    assert!(warn.contains("healthy by design"));
    assert!(warn.contains("memory.*"));

    // CRIT (over cap): give an immediate compaction action.
    let crit = capacity_hint(true);
    assert!(crit.contains("lean-ctx knowledge consolidate --all"));
    assert!(crit.contains("memory.*"));

    assert_ne!(warn, crit);
}

#[test]
fn cwd_looks_like_agent_dir_matches_both_separators() {
    for sep in ['/', '\\'] {
        for dir in [".lmstudio", ".claude", ".codebuddy", ".codex"] {
            let cwd = format!("C:{sep}Users{sep}me{sep}{dir}{sep}mcp");
            assert!(
                cwd_looks_like_agent_dir(&cwd),
                "expected {cwd} to be flagged as an agent dir"
            );
        }
    }
}

#[test]
fn cwd_looks_like_agent_dir_ignores_real_projects() {
    for cwd in [
        "/home/me/work/myproj",
        "/Users/me/code/lean-ctx",
        "C:\\src\\app",
    ] {
        assert!(
            !cwd_looks_like_agent_dir(cwd),
            "{cwd} is a real project and must not be flagged"
        );
    }
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
            lean_ctx::core::rules_canonical::AGENTS_BLOCK_START,
            lean_ctx::core::rules_canonical::AGENTS_BLOCK_END,
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
        &format!("{}\nx", lean_ctx::core::rules_canonical::AGENTS_BLOCK_START),
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

// --- config parity (#594): pinned-data-dir extraction ---

#[test]
fn pinned_data_dir_parses_toml_json_yaml() {
    // The three editor config dialects all expose the value as the first quoted
    // token after the key; the extractor must read all of them.
    let toml = "[mcp_servers.lean-ctx.env]\nLEAN_CTX_DATA_DIR = \"/abs/data/lean-ctx\"\n";
    let json = "{ \"env\": { \"LEAN_CTX_DATA_DIR\": \"/abs/data/lean-ctx\" } }";
    let yaml = "    env:\n      LEAN_CTX_DATA_DIR: \"/abs/data/lean-ctx\"\n";
    for src in [toml, json, yaml] {
        assert_eq!(
            pinned_data_dir(src),
            Some(std::path::PathBuf::from("/abs/data/lean-ctx")),
            "failed to parse: {src:?}"
        );
    }
}

#[test]
fn pinned_data_dir_none_when_key_absent_or_empty() {
    assert_eq!(pinned_data_dir("no pin here"), None);
    assert_eq!(pinned_data_dir("LEAN_CTX_DATA_DIR = \"\""), None);
}

#[test]
fn pinned_data_dir_trims_trailing_separator() {
    assert_eq!(
        pinned_data_dir("LEAN_CTX_DATA_DIR = \"/abs/data/lean-ctx/\""),
        Some(std::path::PathBuf::from("/abs/data/lean-ctx")),
    );
}

#[test]
fn standard_data_pin_is_not_flagged_as_divergent() {
    // #594 regression: an editor that bakes the *standard* XDG data dir keeps
    // config parity — `data_pin_diverges_config` must report no divergence, so
    // doctor stays green instead of raising a false alarm.
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let data_home = tempfile::tempdir().unwrap();
    lean_ctx::test_env::set_var("XDG_DATA_HOME", data_home.path());

    let standard = data_home.path().join("lean-ctx");
    let custom = std::path::Path::new("/opt/custom/lean-ctx");
    let standard_diverges = lean_ctx::core::paths::data_pin_diverges_config(&standard);
    let custom_diverges = lean_ctx::core::paths::data_pin_diverges_config(custom);

    lean_ctx::test_env::remove_var("XDG_DATA_HOME");

    assert!(!standard_diverges, "standard XDG data pin must not diverge");
    assert!(custom_diverges, "a custom data pin diverges config");
}

// --- #1534: output-triage visibility ---
// The 3.9.19 filter dropped tool output while doctor reported all checks
// green; its state must be on the board in both directions.

#[test]
fn output_triage_off_names_the_key_and_reads_off() {
    let out = environment::output_triage_outcome_for(0);
    assert!(out.ok);
    assert!(out.line.contains("Output triage"), "{}", out.line);
    assert!(
        out.line.contains("decision_loop.max_filter_level"),
        "{}",
        out.line
    );
    assert!(out.line.contains("off"), "{}", out.line);
}

#[test]
fn output_triage_active_states_level_and_disable_command() {
    let out = environment::output_triage_outcome_for(2);
    assert!(out.ok, "active filtering is configuration, not a failure");
    assert!(out.line.contains("active (max level 2)"), "{}", out.line);
    assert!(
        out.line
            .contains("lean-ctx config set decision_loop.max_filter_level 0"),
        "the line must name the command that turns it off: {}",
        out.line
    );
}
