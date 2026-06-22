//! Tests for rules injection. `super::*` resolves to the `rules_inject` module.

use super::content::rules_content;
use super::skills::{SKILL_TEMPLATE, build_skill_targets};
use std::sync::OnceLock;

use super::*;
use crate::core::rules_canonical::{END_MARK, RULES_VERSION, RulesFile, START_MARK, Wrapper};

fn shared_content() -> String {
    crate::core::rules_canonical::render(false, Wrapper::Shared)
}

fn dedicated_content_cached() -> &'static str {
    static RULES: OnceLock<String> = OnceLock::new();
    RULES.get_or_init(|| crate::core::rules_canonical::render(false, Wrapper::Dedicated))
}

// ── Canonical rules content ──────────────────────────────────

#[test]
fn shared_rules_have_markers() {
    let s = shared_content();
    assert!(s.contains(START_MARK));
    assert!(s.contains(END_MARK));
    assert!(s.contains(&format!("<!-- version: {RULES_VERSION} -->")));
    assert!(s.contains("MANDATORY MAPPING"));
    assert!(s.contains("NEVER"));
}

#[test]
fn dedicated_rules_have_markers() {
    let d = dedicated_content_cached();
    assert!(d.contains(START_MARK));
    assert!(d.contains(END_MARK));
    assert!(d.contains(&format!("<!-- version: {RULES_VERSION} -->")));
    assert!(d.contains("CRITICAL"));
    assert!(d.contains("intent"));
}

// ── Shadow mode ──────────────────────────────────────────────

#[test]
fn shadow_dedicated_omits_mapping() {
    let rules = crate::core::rules_canonical::render(true, Wrapper::Dedicated);
    assert!(
        !rules.contains("MUST USE"),
        "shadow must not include tool mapping"
    );
    assert!(
        !rules.contains("NEVER use native"),
        "shadow must not include native tool admonition"
    );
    assert!(rules.contains(START_MARK), "shadow keeps markers");
    assert!(rules.contains(END_MARK), "shadow keeps markers");
}

#[test]
fn shadow_shared_omits_mapping() {
    let rules = crate::core::rules_canonical::render(true, Wrapper::Shared);
    assert!(
        !rules.contains("MANDATORY MAPPING"),
        "shadow shared must not include mapping header"
    );
    assert!(rules.contains(START_MARK), "shadow shared keeps markers");
    assert!(rules.contains(END_MARK), "shadow shared keeps markers");
}

// ── Agent target catalog ─────────────────────────────────────

#[test]
fn zed_rules_path_is_os_aware_and_matches_config_dir() {
    let home = std::path::Path::new("/home/tester");
    let zed = build_rules_targets(home, crate::core::config::RulesInjection::Shared)
        .into_iter()
        .find(|t| t.name == "Zed")
        .expect("Zed rules target must exist");
    let expected = crate::core::editor_registry::zed_config_dir(home).join("rules/lean-ctx.md");
    assert_eq!(zed.path, expected);
}

#[test]
fn target_count() {
    let home = std::path::PathBuf::from("/tmp/fake_home");
    let targets = build_rules_targets(&home, crate::core::config::RulesInjection::Shared);
    assert_eq!(targets.len(), 24);
    assert!(
        !targets.iter().any(|t| t.name == "Claude Code"),
        "Claude Code must not get a rules target"
    );
    assert!(
        !targets.iter().any(|t| t.name == "CodeBuddy"),
        "CodeBuddy must not get a rules target"
    );
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
    assert!(matches!(gemini.format, RulesFormat::DedicatedMarkdown));
    assert_eq!(gemini.path, gemini_dedicated_rules_path(home));
    assert!(!gemini.path.ends_with("GEMINI.md"));
    assert!(matches!(opencode.format, RulesFormat::DedicatedMarkdown));
    assert_eq!(opencode.path, opencode_dedicated_rules_path(home));
    assert!(!opencode.path.ends_with("AGENTS.md"));
}

#[test]
fn rules_catalog_includes_previously_missing_agents_for_detection() {
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

// ── Cursor MDC ───────────────────────────────────────────────

#[test]
fn cursor_mdc_has_frontmatter_and_markers() {
    let mdc = rules_content(&RulesFormat::CursorMdc);
    assert!(mdc.contains("alwaysApply: true"));
    assert!(mdc.contains(START_MARK));
    assert!(mdc.contains(END_MARK));
    assert!(mdc.contains(&format!("<!-- version: {RULES_VERSION} -->")));
}

// ── RulesFile operations ─────────────────────────────────────

#[test]
fn rules_file_merged_replaces_section_preserving_user_content() {
    let path = std::env::temp_dir().join("test_rules_merged.md");
    let old = format!(
        "user before\n{START_MARK}\n<!-- version: 0 -->\n\nold rules\n{END_MARK}\nuser after"
    );
    std::fs::write(&path, &old).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let file = RulesFile::parse(&content);
    assert!(file.has_content());
    assert_eq!(file.version(), 0);
    assert!(!file.is_current());

    let merged = file.merged(false, Wrapper::Shared);
    std::fs::write(&path, &merged).unwrap();

    let result = std::fs::read_to_string(&path).unwrap();
    assert!(result.contains("user before"), "prefix preserved");
    assert!(result.contains("user after"), "suffix preserved");
    assert!(!result.contains("old rules"), "old content replaced");
    assert!(
        result.contains(&format!("<!-- version: {RULES_VERSION} -->")),
        "version updated"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn rules_file_merged_appends_when_no_section() {
    let content = "user content only";
    let file = RulesFile::parse(content);
    assert!(!file.has_content());

    let merged = file.merged(false, Wrapper::Shared);
    assert!(merged.contains("user content only"));
    assert!(merged.contains(START_MARK));
}

#[test]
fn rules_file_without_section_strips_lean_ctx_block() {
    let content = format!("header\n{START_MARK}\n<!-- version: 1 -->\n\nbody\n{END_MARK}\nfooter");
    let file = RulesFile::parse(&content);
    let stripped = file.without_section();
    assert!(stripped.contains("header"));
    assert!(stripped.contains("footer"));
    assert!(!stripped.contains("body"));
    assert!(!stripped.contains(START_MARK));
}

// ── Injection ────────────────────────────────────────────────

#[test]
fn inject_rules_for_agent_opencode() {
    let home = std::env::temp_dir().join("test_inject_rules_agent");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::create_dir_all(&home);

    let opencode_dir = home.join(".config/opencode");
    let _ = std::fs::create_dir_all(&opencode_dir);

    let result = inject_rules_for_agent(&home, "opencode");
    assert!(
        !result.updated.is_empty() || !result.already.is_empty(),
        "should inject or find rules for OpenCode"
    );
    assert!(result.errors.is_empty(), "no errors expected");

    let agents_md = opencode_dir.join("AGENTS.md");
    if agents_md.exists() {
        let content = std::fs::read_to_string(&agents_md).unwrap();
        assert!(content.contains(START_MARK));
    }

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn inject_rules_for_agent_cursor() {
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
        assert!(content.contains(START_MARK));
    }

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn inject_rules_for_unknown_agent_is_empty() {
    let home = std::path::PathBuf::from("/tmp/fake_home_unknown");
    let result = inject_rules_for_agent(&home, "unknown_agent_xyz");
    assert!(result.updated.is_empty());
    assert!(result.already.is_empty());
    assert!(result.errors.is_empty());
}

#[test]
fn any_rules_marker_present_detects_opencode() {
    let home = std::env::temp_dir().join("lc_test_marker_opencode");
    let _ = std::fs::remove_dir_all(&home);
    let opencode_dir = home.join(".config/opencode");
    std::fs::create_dir_all(&opencode_dir).unwrap();
    std::fs::write(
        opencode_dir.join("AGENTS.md"),
        format!("# preamble\n\n{}\n", shared_content()),
    )
    .unwrap();
    assert!(
        any_rules_marker_present(&home),
        "OpenCode AGENTS.md with the lean-ctx marker must be detected"
    );
    let _ = std::fs::remove_dir_all(&home);
}

// ── Skills ───────────────────────────────────────────────────

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

// ── Agent name matching ──────────────────────────────────────

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

// ── InjectResult ─────────────────────────────────────────────

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
