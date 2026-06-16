//! Tests for rules injection. `super::*` resolves to the `rules_inject` module.

use super::content::{RULES_CURSOR_MDC, RULES_DEDICATED, RULES_SHARED};
use super::skills::{SKILL_TEMPLATE, build_skill_targets};
use super::write::{append_to_shared, replace_markdown_section, write_dedicated};
use super::*;

#[test]
fn shared_rules_have_markers() {
    assert!(RULES_SHARED.contains(MARKER));
    assert!(RULES_SHARED.contains(END_MARKER));
    assert!(RULES_SHARED.contains(RULES_VERSION));
}

#[test]
fn zed_rules_path_is_os_aware_and_matches_config_dir() {
    // Zed's config dir is platform-specific (macOS uses Application Support).
    // Rules must live under the SAME dir as the MCP config, never a hardcoded
    // ~/.config/zed on every OS (regression: rules missed on macOS).
    let home = std::path::Path::new("/home/tester");
    let zed = build_rules_targets(home, crate::core::config::RulesInjection::Shared)
        .into_iter()
        .find(|t| t.name == "Zed")
        .expect("Zed rules target must exist");
    let expected = crate::core::editor_registry::zed_config_dir(home).join("rules/lean-ctx.md");
    assert_eq!(zed.path, expected);
}

#[test]
fn dedicated_rules_have_markers() {
    assert!(RULES_DEDICATED.contains(MARKER));
    assert!(RULES_DEDICATED.contains(END_MARKER));
    assert!(RULES_DEDICATED.contains(RULES_VERSION));
}

#[test]
fn cursor_mdc_has_markers_and_frontmatter() {
    assert!(RULES_CURSOR_MDC.contains("lean-ctx"));
    assert!(RULES_CURSOR_MDC.contains(END_MARKER));
    assert!(RULES_CURSOR_MDC.contains(RULES_VERSION));
    assert!(RULES_CURSOR_MDC.contains("alwaysApply: true"));
}

#[test]
fn shared_rules_contain_mode_selection() {
    assert!(RULES_SHARED.contains("Mode Selection"));
    assert!(RULES_SHARED.contains("full"));
    assert!(RULES_SHARED.contains("map"));
    assert!(RULES_SHARED.contains("signatures"));
    assert!(RULES_SHARED.contains("NEVER"));
}

#[test]
fn shared_rules_has_never_native() {
    assert!(RULES_SHARED.contains("NEVER use native"));
    assert!(RULES_SHARED.contains("ctx_read"));
}

#[test]
fn dedicated_rules_contain_modes() {
    assert!(RULES_DEDICATED.contains("auto"));
    assert!(RULES_DEDICATED.contains("full"));
    assert!(RULES_DEDICATED.contains("map"));
    assert!(RULES_DEDICATED.contains("signatures"));
    assert!(RULES_DEDICATED.contains("lines:N-M"));
    assert!(RULES_DEDICATED.contains("diff"));
}

#[test]
fn dedicated_rules_has_proactive_section() {
    assert!(RULES_DEDICATED.contains("Proactive"));
    assert!(RULES_DEDICATED.contains("ctx_overview"));
    assert!(RULES_DEDICATED.contains("ctx_compress"));
}

#[test]
fn cursor_mdc_contains_tool_mapping() {
    assert!(RULES_CURSOR_MDC.contains("Tool Mapping"));
    assert!(RULES_CURSOR_MDC.contains("ctx_read"));
    assert!(RULES_CURSOR_MDC.contains("ctx_search"));
    assert!(RULES_CURSOR_MDC.contains("Workflow"));
}

fn ensure_temp_dir() {
    let tmp = std::env::temp_dir();
    if !tmp.exists() {
        std::fs::create_dir_all(&tmp).ok();
    }
}

#[test]
fn replace_section_with_end_marker() {
    ensure_temp_dir();
    let old = "user stuff\n\n# lean-ctx — Context Engineering Layer\n<!-- lean-ctx-rules-v2 -->\nold rules\n<!-- /lean-ctx -->\nmore user stuff\n";
    let path = std::env::temp_dir().join("test_replace_with_end.md");
    std::fs::write(&path, old).unwrap();

    let result = replace_markdown_section(&path, old).unwrap();
    assert!(matches!(result, RulesResult::Updated));

    let new_content = std::fs::read_to_string(&path).unwrap();
    assert!(new_content.contains(RULES_VERSION));
    assert!(new_content.starts_with("user stuff"));
    assert!(new_content.contains("more user stuff"));
    assert!(!new_content.contains("lean-ctx-rules-v2"));

    std::fs::remove_file(&path).ok();
}

#[test]
fn replace_section_without_end_marker() {
    ensure_temp_dir();
    let old = "user stuff\n\n# lean-ctx — Context Engineering Layer\nold rules only\n";
    let path = std::env::temp_dir().join("test_replace_no_end.md");
    std::fs::write(&path, old).unwrap();

    let result = replace_markdown_section(&path, old).unwrap();
    assert!(matches!(result, RulesResult::Updated));

    let new_content = std::fs::read_to_string(&path).unwrap();
    assert!(new_content.contains(RULES_VERSION));
    assert!(new_content.starts_with("user stuff"));

    std::fs::remove_file(&path).ok();
}

#[test]
fn append_to_shared_preserves_existing() {
    ensure_temp_dir();
    let path = std::env::temp_dir().join("test_append_shared.md");
    std::fs::write(&path, "existing user rules\n").unwrap();

    let result = append_to_shared(&path).unwrap();
    assert!(matches!(result, RulesResult::Injected));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.starts_with("existing user rules"));
    assert!(content.contains(MARKER));
    assert!(content.contains(END_MARKER));

    std::fs::remove_file(&path).ok();
}

#[test]
fn write_dedicated_creates_file() {
    ensure_temp_dir();
    let path = std::env::temp_dir().join("test_write_dedicated.md");
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }

    let result = write_dedicated(&path, RULES_DEDICATED).unwrap();
    assert!(matches!(result, RulesResult::Injected));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains(MARKER));
    assert!(content.contains("Mode Selection"));

    std::fs::remove_file(&path).ok();
}

#[test]
fn write_dedicated_updates_existing() {
    ensure_temp_dir();
    let path = std::env::temp_dir().join("test_write_dedicated_update.md");
    std::fs::write(&path, "# lean-ctx — Context Engineering Layer\nold version").unwrap();

    let result = write_dedicated(&path, RULES_DEDICATED).unwrap();
    assert!(matches!(result, RulesResult::Updated));

    std::fs::remove_file(&path).ok();
}

#[test]
fn target_count() {
    // 24, not 25: Claude Code intentionally has no rules target — its rules
    // file loaded unconditionally every session and duplicated the CLAUDE.md
    // block (GL #555/#558). Guidance lives in CLAUDE.md + the on-demand skill.
    // CodeBuddy also has no rules target (same pattern as Claude Code).
    let home = std::path::PathBuf::from("/tmp/fake_home");
    let targets = build_rules_targets(&home, crate::core::config::RulesInjection::Shared);
    assert_eq!(targets.len(), 24);
    assert!(
        !targets.iter().any(|t| t.name == "Claude Code"),
        "Claude Code must not get a rules target (always-loaded duplicate)"
    );
    assert!(
        !targets.iter().any(|t| t.name == "CodeBuddy"),
        "CodeBuddy must not get a rules target (always-loaded duplicate, same as Claude Code)"
    );
    // Dedicated mode swaps paths/formats but never changes the target count.
    let dedicated = build_rules_targets(&home, crate::core::config::RulesInjection::Dedicated);
    assert_eq!(dedicated.len(), 24);
}

#[test]
fn dedicated_mode_swaps_shared_agents_to_dedicated_files() {
    use crate::core::config::RulesInjection;
    let home = std::path::Path::new("/home/tester");

    let shared = build_rules_targets(home, RulesInjection::Shared);
    let gemini_shared = shared.iter().find(|t| t.name == "Gemini CLI").unwrap();
    let opencode_shared = shared.iter().find(|t| t.name == "OpenCode").unwrap();
    assert!(matches!(gemini_shared.format, RulesFormat::SharedMarkdown));
    assert!(gemini_shared.path.ends_with("GEMINI.md"));
    assert!(matches!(
        opencode_shared.format,
        RulesFormat::SharedMarkdown
    ));
    assert!(opencode_shared.path.ends_with("AGENTS.md"));

    let dedicated = build_rules_targets(home, RulesInjection::Dedicated);
    let gemini = dedicated.iter().find(|t| t.name == "Gemini CLI").unwrap();
    let opencode = dedicated.iter().find(|t| t.name == "OpenCode").unwrap();
    // Never the user's shared instruction file in dedicated mode.
    assert!(matches!(gemini.format, RulesFormat::DedicatedMarkdown));
    assert_eq!(gemini.path, gemini_dedicated_rules_path(home));
    assert!(!gemini.path.ends_with("GEMINI.md"));
    assert!(matches!(opencode.format, RulesFormat::DedicatedMarkdown));
    assert_eq!(opencode.path, opencode_dedicated_rules_path(home));
    assert!(!opencode.path.ends_with("AGENTS.md"));
}

#[test]
fn dedicated_session_summary_is_clean_and_agent_agnostic() {
    let s = dedicated_session_summary();
    assert!(s.contains("ctx_read"));
    assert!(s.contains("ctx_shell"));
    assert!(s.contains("ctx_search"));
    // Must not carry HTML markers or an @import pointer (Codex has no @import).
    assert!(!s.contains("<!--"));
    assert!(!s.contains('@'));
}

#[test]
fn skill_template_not_empty() {
    assert!(!SKILL_TEMPLATE.is_empty());
    assert!(SKILL_TEMPLATE.contains("lean-ctx"));
}

#[test]
fn skill_targets_count() {
    let home = std::path::PathBuf::from("/tmp/fake_home");
    let targets = build_skill_targets(&home);
    assert_eq!(targets.len(), 6);
}

#[test]
fn install_skill_creates_file() {
    ensure_temp_dir();
    let home = std::env::temp_dir().join("test_skill_install");
    let _ = std::fs::create_dir_all(&home);

    let fake_cursor = home.join(".cursor");
    let _ = std::fs::create_dir_all(&fake_cursor);

    let result = install_skill_for_agent(&home, "cursor");
    assert!(result.is_ok());

    let path = result.unwrap();
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, SKILL_TEMPLATE);

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn install_skill_idempotent() {
    ensure_temp_dir();
    let home = std::env::temp_dir().join("test_skill_idempotent");
    let _ = std::fs::create_dir_all(&home);

    let fake_cursor = home.join(".cursor");
    let _ = std::fs::create_dir_all(&fake_cursor);

    let p1 = install_skill_for_agent(&home, "cursor").unwrap();
    let p2 = install_skill_for_agent(&home, "cursor").unwrap();
    assert_eq!(p1, p2);

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn install_skill_unknown_agent() {
    let home = std::path::PathBuf::from("/tmp/fake_home");
    let result = install_skill_for_agent(&home, "unknown_agent");
    assert!(result.is_err());
}

#[test]
fn match_agent_name_basic() {
    assert!(match_agent_name("cursor", "Cursor"));
    assert!(match_agent_name("opencode", "OpenCode"));
    assert!(match_agent_name("claude", "Claude Code"));
    assert!(match_agent_name("vscode", "VS Code"));
    assert!(match_agent_name("copilot", "Copilot CLI"));
    assert!(match_agent_name("kiro", "AWS Kiro"));
    assert!(match_agent_name("pi", "Pi Coding Agent"));
    assert!(match_agent_name("crush", "Crush"));
    assert!(match_agent_name("amp", "Amp"));
    assert!(match_agent_name("cline", "Cline"));
    assert!(match_agent_name("roo", "Roo Code"));
    assert!(match_agent_name("trae", "Trae"));
    assert!(match_agent_name("amazonq", "Amazon Q Developer"));
    assert!(match_agent_name("verdent", "Verdent"));
    assert!(match_agent_name("continue", "Continue"));
    assert!(match_agent_name("antigravity", "Antigravity"));
    assert!(match_agent_name("codebuddy", "CodeBuddy"));
    assert!(match_agent_name("gemini", "Gemini CLI"));
    assert!(match_agent_name("augment", "Augment"));
    assert!(match_agent_name("openclaw", "OpenClaw"));
}

#[test]
fn match_agent_name_no_false_positives() {
    assert!(!match_agent_name("cursor", "Claude Code"));
    assert!(!match_agent_name("opencode", "Cursor"));
    assert!(!match_agent_name("unknown_agent", "Cursor"));
}

#[test]
fn inject_rules_for_agent_opencode() {
    ensure_temp_dir();
    let home = std::env::temp_dir().join("test_inject_rules_agent");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::create_dir_all(&home);

    let opencode_dir = home.join(".config/opencode");
    let _ = std::fs::create_dir_all(&opencode_dir);

    let result = inject_rules_for_agent(&home, "opencode");
    assert!(
        !result.injected.is_empty() || !result.already.is_empty(),
        "should inject or find rules for OpenCode"
    );
    assert!(result.errors.is_empty(), "no errors expected");

    let agents_md = opencode_dir.join("AGENTS.md");
    if agents_md.exists() {
        let content = std::fs::read_to_string(&agents_md).unwrap();
        assert!(content.contains(RULES_VERSION));
    }

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn inject_rules_for_agent_cursor() {
    ensure_temp_dir();
    let home = std::env::temp_dir().join("test_inject_rules_cursor");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::create_dir_all(&home);

    let cursor_dir = home.join(".cursor");
    let _ = std::fs::create_dir_all(&cursor_dir);

    let result = inject_rules_for_agent(&home, "cursor");
    assert!(result.errors.is_empty(), "no errors expected");

    let mdc_path = home.join(".cursor/rules/lean-ctx.mdc");
    if mdc_path.exists() {
        let content = std::fs::read_to_string(&mdc_path).unwrap();
        assert!(content.contains(RULES_VERSION));
    }

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn inject_rules_for_unknown_agent_is_empty() {
    let home = std::path::PathBuf::from("/tmp/fake_home_unknown");
    let result = inject_rules_for_agent(&home, "unknown_agent_xyz");
    assert!(result.injected.is_empty());
    assert!(result.updated.is_empty());
    assert!(result.already.is_empty());
    assert!(result.errors.is_empty());
}

#[test]
fn rules_catalog_includes_previously_missing_agents_for_detection() {
    // #442: presence detection drifted behind the injector — OpenCode (and many
    // others) were written by build_rules_targets but absent from the hand-kept
    // presence list, so OpenCode-only users were reported as having no rules.
    // Guard that the catalog (now the single source of truth for detection)
    // covers them, so detection can never silently drop an agent again.
    let home = std::path::Path::new("/home/tester");
    let names: std::collections::HashSet<&str> = [
        crate::core::config::RulesInjection::Shared,
        crate::core::config::RulesInjection::Dedicated,
    ]
    .iter()
    .flat_map(|inj| build_rules_targets(home, *inj))
    .map(|t| t.name)
    .collect();
    for agent in ["OpenCode", "Zed", "Cline", "Roo Code", "Continue", "Crush"] {
        assert!(
            names.contains(agent),
            "{agent} must be in the rules catalog used for presence detection (#442)"
        );
    }
}

#[test]
fn any_rules_marker_present_detects_opencode() {
    // #442 regression guard: an OpenCode AGENTS.md carrying the lean-ctx marker
    // must count as "rules present" (the old hand-list never checked OpenCode).
    let home = std::env::temp_dir().join("lc_test_marker_opencode_442");
    let _ = std::fs::remove_dir_all(&home);
    let opencode_dir = home.join(".config/opencode");
    std::fs::create_dir_all(&opencode_dir).unwrap();
    std::fs::write(
        opencode_dir.join("AGENTS.md"),
        format!("# preamble\n\n{RULES_SHARED}\n"),
    )
    .unwrap();
    assert!(
        any_rules_marker_present(&home),
        "OpenCode AGENTS.md with the lean-ctx marker must be detected (#442)"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn write_dedicated_preserves_user_content_before_marker() {
    ensure_temp_dir();
    let path = std::env::temp_dir().join("test_dedicated_preserve_before.md");
    let old = format!(
        "# My custom rules\nDo not delete this!\n\n{MARKER}\n<!-- lean-ctx-rules-v2 -->\nold content\n{END_MARKER}"
    );
    std::fs::write(&path, &old).unwrap();

    let result = write_dedicated(&path, RULES_DEDICATED).unwrap();
    assert!(matches!(result, RulesResult::Updated));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.contains("My custom rules"),
        "user content before marker must be preserved"
    );
    assert!(
        content.contains("Do not delete this!"),
        "user content before marker must be preserved"
    );
    assert!(
        content.contains(RULES_VERSION),
        "new rules version must be present"
    );
    assert!(
        !content.contains("lean-ctx-rules-v2"),
        "old version must be replaced"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn write_dedicated_preserves_user_content_after_marker() {
    ensure_temp_dir();
    let path = std::env::temp_dir().join("test_dedicated_preserve_after.md");
    let old = format!(
        "{MARKER}\n<!-- lean-ctx-rules-v2 -->\nold content\n{END_MARKER}\n\n# User's extra notes\nKeep this too!\n"
    );
    std::fs::write(&path, &old).unwrap();

    let result = write_dedicated(&path, RULES_DEDICATED).unwrap();
    assert!(matches!(result, RulesResult::Updated));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.contains("User's extra notes"),
        "user content after marker must be preserved"
    );
    assert!(
        content.contains("Keep this too!"),
        "user content after marker must be preserved"
    );
    assert!(
        content.contains(RULES_VERSION),
        "new rules version must be present"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn write_dedicated_preserves_content_both_sides() {
    ensure_temp_dir();
    let path = std::env::temp_dir().join("test_dedicated_preserve_both.md");
    let old = format!(
        "BEFORE CONTENT\n\n{MARKER}\n<!-- lean-ctx-rules-v2 -->\nold\n{END_MARKER}\n\nAFTER CONTENT\n"
    );
    std::fs::write(&path, &old).unwrap();

    let result = write_dedicated(&path, RULES_DEDICATED).unwrap();
    assert!(matches!(result, RulesResult::Updated));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("BEFORE CONTENT"));
    assert!(content.contains("AFTER CONTENT"));
    assert!(content.contains(RULES_VERSION));

    std::fs::remove_file(&path).ok();
}

#[test]
fn write_dedicated_no_user_content_uses_template_directly() {
    ensure_temp_dir();
    let path = std::env::temp_dir().join("test_dedicated_no_user.md");
    let old = format!("{MARKER}\n<!-- lean-ctx-rules-v2 -->\nold content\n{END_MARKER}");
    std::fs::write(&path, &old).unwrap();

    let result = write_dedicated(&path, RULES_DEDICATED).unwrap();
    assert!(matches!(result, RulesResult::Updated));

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        content, RULES_DEDICATED,
        "without user content, template should be written as-is"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn write_dedicated_preserves_mdc_frontmatter() {
    ensure_temp_dir();
    let path = std::env::temp_dir().join("test_dedicated_mdc_frontmatter.mdc");
    let old = format!(
        "---\ndescription: custom\nglobs: **/*\nalwaysApply: true\n---\n\nUser preamble here\n\n{MARKER}\n<!-- lean-ctx-rules-v2 -->\nold\n{END_MARKER}\n"
    );
    std::fs::write(&path, &old).unwrap();

    let result = write_dedicated(&path, RULES_CURSOR_MDC).unwrap();
    assert!(matches!(result, RulesResult::Updated));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.contains("User preamble here"),
        "user preamble must be preserved"
    );
    assert!(
        content.contains("custom"),
        "user frontmatter description must be preserved"
    );
    assert!(content.contains(RULES_VERSION));

    std::fs::remove_file(&path).ok();
}

#[test]
fn inject_result_tracks_backed_up_files() {
    let result = InjectResult {
        backed_up: vec!["/tmp/test.md.bak".to_string()],
        ..Default::default()
    };
    assert_eq!(result.backed_up.len(), 1);
    assert!(
        std::path::Path::new(&result.backed_up[0])
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("bak"))
    );
}
