use super::{Config, solution::*};
use std::sync::Mutex;

static TRACKER_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn default_solution_config_is_enabled_and_balanced() {
    let cfg = SolutionConfig::default();
    assert!(cfg.enabled);
    assert_eq!(cfg.intensity.label(), "balanced");
    assert!(cfg.inject_in_instructions);
    assert!(cfg.inject_in_compose);
    assert!(cfg.inject_in_subagents);
    assert!(cfg.track_decisions);
    assert!(cfg.track_loc);
    assert!(cfg.platform_hints);
}

#[test]
fn ladder_text_matches_each_intensity() {
    let cfg = SolutionConfig {
        intensity: SolutionIntensity::Minimal,
        ..SolutionConfig::default()
    };
    assert!(!cfg.ladder_text().is_empty());

    let cfg = SolutionConfig {
        intensity: SolutionIntensity::Balanced,
        ..SolutionConfig::default()
    };
    let text = cfg.ladder_text();
    assert!(text.to_lowercase().contains("stdlib"));
    assert!(text.to_lowercase().contains("reuse"));

    let cfg = SolutionConfig {
        intensity: SolutionIntensity::Aggressive,
        ..SolutionConfig::default()
    };
    let text = cfg.ladder_text();
    assert!(text.to_lowercase().contains("delet"));

    let cfg = SolutionConfig {
        intensity: SolutionIntensity::Off,
        ..SolutionConfig::default()
    };
    assert!(cfg.ladder_text().is_empty());
}

#[test]
fn effective_intensity_is_off_when_disabled() {
    let cfg = SolutionConfig {
        enabled: false,
        ..SolutionConfig::default()
    };
    assert_eq!(cfg.effective_intensity().label(), "off");
}

#[test]
fn solution_config_deserializes_from_toml() {
    let toml_str = r#"
        enabled = true
        intensity = "aggressive"
        inject_in_instructions = false
    "#;
    let cfg: SolutionConfig = toml::from_str(toml_str).unwrap();
    assert!(cfg.enabled);
    assert_eq!(cfg.intensity.label(), "aggressive");
    assert!(!cfg.inject_in_instructions);
}

#[test]
fn config_parses_solution_section_with_all_sprint_one_fields() {
    let cfg: Config = toml::from_str(
        r#"
        [solution]
        enabled = false
        intensity = "aggressive"
        inject_in_instructions = false
        inject_in_compose = true
        inject_in_subagents = false
        track_decisions = true
        track_loc = false
        platform_hints = true
        "#,
    )
    .expect("[solution] must parse every Sprint 1 field");

    assert!(!cfg.solution.enabled);
    assert!(matches!(
        cfg.solution.intensity,
        SolutionIntensity::Aggressive
    ));
    assert!(!cfg.solution.inject_in_instructions);
    assert!(cfg.solution.inject_in_compose);
    assert!(!cfg.solution.inject_in_subagents);
    assert!(cfg.solution.track_decisions);
    assert!(!cfg.solution.track_loc);
    assert!(cfg.solution.platform_hints);
}

#[test]
fn empty_solution_toml_section_uses_defaults() {
    let cfg: SolutionConfig = toml::from_str("").unwrap();
    assert!(cfg.enabled);
    assert_eq!(cfg.intensity.label(), "balanced");
}

#[test]
fn solution_tracker_records_and_snapshots() {
    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let _guard = TRACKER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::core::solution_tracker::reset();

    crate::core::solution_tracker::record_decision("stdlib");
    crate::core::solution_tracker::record_decision("reuse");
    crate::core::solution_tracker::record_decision("other");
    crate::core::solution_tracker::record_loc_change(4, 10);
    crate::core::solution_tracker::record_output_tokens(1_000, 700);

    let snapshot = crate::core::solution_tracker::snapshot();

    assert!(snapshot.decisions_total >= 3);
    assert!(
        snapshot
            .decisions_by_kind
            .get("stdlib")
            .copied()
            .unwrap_or(0)
            >= 1
    );
    assert!(
        snapshot
            .decisions_by_kind
            .get("reuse")
            .copied()
            .unwrap_or(0)
            >= 1
    );
}

#[test]
fn solution_tracker_reset_zeroes_all_counters() {
    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let _guard = TRACKER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::core::solution_tracker::record_decision("stdlib");
    crate::core::solution_tracker::reset();

    let s = crate::core::solution_tracker::snapshot();
    assert_eq!(s.decisions_total, 0);
    assert_eq!(s.loc_added, 0);
    assert_eq!(s.output_tokens_baseline, 0);
}

#[test]
fn gain_summary_formats_correctly() {
    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let _guard = TRACKER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::core::solution_tracker::reset();
    crate::core::solution_tracker::record_decision("stdlib");
    crate::core::solution_tracker::record_output_tokens(500, 350);

    let summary = crate::core::solution_tracker::gain_summary();
    assert!(summary.contains("decisions") || summary.contains("reduction"));
}
