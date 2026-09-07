//! Tests for rules injection. `super::*` resolves to the `rules_inject` module.

use super::content::rules_content;
use super::skills::{SKILL_TEMPLATE, build_skill_targets};
use std::sync::OnceLock;

use super::*;
use crate::core::config::CompressionLevel;
use crate::core::rules_canonical::{END_MARK, RULES_VERSION, RulesFile, START_MARK, Wrapper};

fn power() -> crate::core::tool_profiles::ToolProfile {
    crate::core::tool_profiles::ToolProfile::Power
}

fn shared_content() -> String {
    crate::core::rules_canonical::render(false, Wrapper::Shared, CompressionLevel::Off, &power())
}

fn dedicated_content_cached() -> &'static str {
    static RULES: OnceLock<String> = OnceLock::new();
    RULES.get_or_init(|| {
        crate::core::rules_canonical::render(
            false,
            Wrapper::Dedicated,
            CompressionLevel::Off,
            &power(),
        )
    })
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
    let rules = crate::core::rules_canonical::render(
        true,
        Wrapper::Dedicated,
        CompressionLevel::Off,
        &power(),
    );
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
    let rules = crate::core::rules_canonical::render(
        true,
        Wrapper::Shared,
        CompressionLevel::Off,
        &power(),
    );
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
    // Includes Grok and Oh My Pi (`~/.omp/agent/AGENTS.md`).
    assert_eq!(targets.len(), 27);
    assert!(
        targets.iter().any(|t| t.name == "Grok"),
        "Grok must have a rules target"
    );
    assert!(
        !targets.iter().any(|t| t.name == "Claude Code"),
        "Claude Code must not get a rules target"
    );
    assert!(
        !targets.iter().any(|t| t.name == "CodeBuddy"),
        "CodeBuddy must not get a rules target"
    );
    let dedicated = build_rules_targets(&home, crate::core::config::RulesInjection::Dedicated);
    assert_eq!(dedicated.len(), 27);
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
fn omp_rules_target_merges_into_the_native_agents_file() {
    let home = std::path::PathBuf::from("/tmp/fake_home");
    for injection in [
        crate::core::config::RulesInjection::Shared,
        crate::core::config::RulesInjection::Dedicated,
    ] {
        let targets = build_rules_targets(&home, injection);
        let omp = targets
            .iter()
            .find(|t| t.name == "Oh My Pi")
            .expect("Oh My Pi must have a rules target");
        assert_eq!(
            omp.path,
            crate::core::editor_registry::omp_agents_path(&home)
        );
        assert_eq!(omp.path.file_name().unwrap(), "AGENTS.md");
        // Shared user instruction file -> marker-delimited merge. A dedicated
        // (lean-ctx-owned) file would clobber the user's own guidance.
        assert!(matches!(omp.format, RulesFormat::SharedMarkdown));
    }
}

#[test]
fn omp_and_pi_cli_keys_do_not_cross_match() {
    assert!(match_agent_name("omp", "Oh My Pi"));
    assert!(!match_agent_name("omp", "Pi Coding Agent"));
    assert!(!match_agent_name("pi", "Oh My Pi"));
    assert!(match_agent_name("pi", "Pi Coding Agent"));
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
    for agent in [
        "OpenCode", "Zed", "Cline", "Roo Code", "Continue", "Crush", "Oh My Pi",
    ] {
        assert!(
            names.contains(agent),
            "{agent} must be in the rules catalog used for presence detection (#442)"
        );
    }
}

// ── Cursor MDC ───────────────────────────────────────────────

#[test]
fn cursor_mdc_has_frontmatter_and_markers() {
    let mdc = rules_content(
        &RulesFormat::CursorMdc,
        CompressionLevel::Off,
        Wrapper::Dedicated,
        &power(),
    );
    assert!(mdc.contains("alwaysApply: true"));
    assert!(mdc.contains(START_MARK));
    assert!(mdc.contains(END_MARK));
    assert!(mdc.contains(&format!("<!-- version: {RULES_VERSION} -->")));
}

#[test]
fn cursor_wrapper_follows_hook_coverage() {
    // GL #1153: with lean-ctx rewrite+redirect hooks installed next to the
    // mdc, the injector selects the honest HookCovered profile; without them
    // (or with foreign hooks) it stays on the full Dedicated mapping.
    let home = std::env::temp_dir().join("lc_test_cursor_wrapper_coverage");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
    let mdc_path = home.join(".cursor/rules/lean-ctx.mdc");

    assert!(matches!(
        super::content::cursor_wrapper_for_mdc(&mdc_path),
        Wrapper::Dedicated
    ));

    std::fs::write(
        home.join(".cursor/hooks.json"),
        r#"{"version":1,"hooks":{"preToolUse":[
            {"matcher":"Shell","command":"/usr/local/bin/lean-ctx hook rewrite"},
            {"matcher":"Read|Grep","command":"/usr/local/bin/lean-ctx hook redirect"}
        ]}}"#,
    )
    .unwrap();
    assert!(matches!(
        super::content::cursor_wrapper_for_mdc(&mdc_path),
        Wrapper::HookCovered
    ));

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn inject_cursor_switches_profile_when_hooks_appear() {
    // End-to-end through the real injector: with shadow_mode=true (default),
    // both Dedicated and HookCovered collapse to the same shadow-minimal
    // output, so the test verifies that both states produce valid rules.
    // The non-shadow transition (Dedicated→HookCovered) is covered by the
    // existing shadow_omits_loop_and_paradox test in rules_canonical.
    let _guard = crate::core::data_dir::test_env_lock();
    let home = std::env::temp_dir().join("lc_test_inject_cursor_hookcovered");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".cursor")).unwrap();

    let result = inject_rules_for_agent(&home, "cursor");
    assert!(result.errors.is_empty());
    let mdc_path = home.join(".cursor/rules/lean-ctx.mdc");
    let before = std::fs::read_to_string(&mdc_path).unwrap();
    assert!(
        before.contains("auto-route") || before.contains("MANDATORY MAPPING"),
        "initial rules must contain shadow nudge or full mapping"
    );

    std::fs::write(
        home.join(".cursor/hooks.json"),
        r#"{"version":1,"hooks":{"preToolUse":[
            {"matcher":"Shell","command":"/usr/local/bin/lean-ctx hook rewrite"},
            {"matcher":"Read|Grep","command":"/usr/local/bin/lean-ctx hook redirect"}
        ]}}"#,
    )
    .unwrap();
    let result = inject_rules_for_agent(&home, "cursor");
    assert!(result.errors.is_empty());
    let after = std::fs::read_to_string(&mdc_path).unwrap();
    assert!(
        after.contains("auto-route")
            || (after.contains("ALWAYS prefer lean-ctx")
                && after.contains("Hooks compress native")),
        "with hooks the mdc carries shadow block or HookCovered profile"
    );
    assert!(
        after.contains("alwaysApply: true"),
        "frontmatter survives the profile switch"
    );

    let _ = std::fs::remove_dir_all(&home);
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

    let merged = file.merged(false, Wrapper::Shared, CompressionLevel::Off, &power());
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

    let merged = file.merged(false, Wrapper::Shared, CompressionLevel::Off, &power());
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
fn inject_rewrites_on_compression_change_without_version_bump() {
    // #548: a version-only freshness check skips the rewrite when the
    // compression level changes but RULES_VERSION stays the same. Drive the real
    // inject path through LEAN_CTX_COMPRESSION to prove the block is regenerated.
    let _guard = crate::core::data_dir::test_env_lock();

    let dir = std::env::temp_dir().join("lc_test_inject_compression_drift");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let target = RulesTarget {
        name: "test",
        path: dir.join("lean-ctx.md"),
        format: RulesFormat::DedicatedMarkdown,
    };

    crate::test_env::set_var("LEAN_CTX_COMPRESSION", "off");
    assert!(matches!(
        super::write::inject_rules(&target).unwrap(),
        RulesResult::Updated
    ));
    let off = std::fs::read_to_string(&target.path).unwrap();

    // Same level → idempotent (block already matches a fresh render).
    assert!(matches!(
        super::write::inject_rules(&target).unwrap(),
        RulesResult::AlreadyPresent
    ));

    // Switch to max → must rewrite even though RULES_VERSION is unchanged.
    crate::test_env::set_var("LEAN_CTX_COMPRESSION", "max");
    assert!(matches!(
        super::write::inject_rules(&target).unwrap(),
        RulesResult::Updated
    ));
    let max = std::fs::read_to_string(&target.path).unwrap();

    crate::test_env::remove_var("LEAN_CTX_COMPRESSION");
    let _ = std::fs::remove_dir_all(&dir);

    assert_ne!(off, max, "compression-level change must alter the body");
    assert!(max.contains(&format!("<!-- version: {RULES_VERSION} -->")));
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
fn skill_template_lean_profile_matches_core_tool_registry() {
    let lean_row = SKILL_TEMPLATE
        .lines()
        .find(|line| line.starts_with("| Lean (default, unpinned) |"))
        .expect("SKILL.md must document the default lean tool surface");

    for tool in crate::tool_defs::core_tool_names() {
        assert!(
            lean_row.contains(&format!("`{tool}`")),
            "default lean row is missing {tool}"
        );
    }
    assert!(
        lean_row.contains("`ctx_patch`*"),
        "client-dependent ctx_patch visibility must be marked"
    );
    for hidden in ["ctx_knowledge", "ctx_overview", "ctx_graph"] {
        assert!(
            !lean_row.contains(hidden),
            "{hidden} is not part of the default lean surface"
        );
    }
}

#[test]
fn skill_targets_count() {
    // Claude, CodeBuddy, Cursor, Codex, Copilot, Grok, OpenClaw + OpenCode (GH #686).
    let home = std::path::PathBuf::from("/tmp/fake_home");
    let targets = build_skill_targets(&home);
    assert_eq!(targets.len(), 8);
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
    assert!(match_agent_name("qodercli", "Qoder"));
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
