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
            crate::core::rules_canonical::AGENTS_BLOCK_START,
            crate::core::rules_canonical::AGENTS_BLOCK_END,
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
        &format!("{}\nx", crate::core::rules_canonical::AGENTS_BLOCK_START),
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
