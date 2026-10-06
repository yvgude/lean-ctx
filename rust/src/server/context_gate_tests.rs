// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::triage::profile::TaskProfileLocal;

#[test]
fn eviction_target_display_emits_resolvable_targets() {
    // #715: root-relative when under the root, full path otherwise —
    // never a display-shortened form the resolver cannot find.
    assert_eq!(
        eviction_target_display("/w/proj/src/a.rs", Some("/w/proj")),
        "src/a.rs"
    );
    assert_eq!(
        eviction_target_display("/other/b.rs", Some("/w/proj")),
        "/other/b.rs"
    );
    assert_eq!(eviction_target_display("/x/c.rs", None), "/x/c.rs");
}

#[test]
fn pre_dispatch_passthrough_for_full() {
    let result = pre_dispatch_read("src/main.rs", "full", None, None, None);
    assert!(result.overridden_mode.is_none());
}

#[test]
fn pre_dispatch_passthrough_for_diff() {
    let result = pre_dispatch_read("src/main.rs", "diff", None, None, None);
    assert!(result.overridden_mode.is_none());
}

#[test]
fn pre_dispatch_passthrough_for_anchored_window() {
    // #843: a windowed anchored:N-M read must keep its hash anchors —
    // bounce-prevention, pressure-downgrade, etc. must not clobber it to
    // "full" and silently drop the window.
    {
        let mut bt = crate::core::bounce_tracker::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bt.set_seq(101);
        bt.record_read("anchored-bouncy.yml", "map", 30, 400);
        bt.set_seq(102);
        bt.record_read("anchored-bouncy.yml", "full", 400, 400);
        bt.set_seq(103);
        bt.record_read("a2.yml", "map", 30, 400);
        bt.set_seq(104);
        bt.record_read("a2.yml", "full", 400, 400);
        bt.set_seq(105);
        bt.record_read("a3.yml", "map", 30, 400);
        bt.set_seq(106);
        bt.record_read("a3.yml", "full", 400, 400);
    }
    let result = pre_dispatch_read("anchored-new.yml", "anchored:10-40", None, None, None);
    assert!(
        result.overridden_mode.is_none(),
        "anchored:N-M must not be overridden by bounce-prevention"
    );
    let bare = pre_dispatch_read("anchored-new.yml", "anchored", None, None, None);
    assert!(
        bare.overridden_mode.is_none(),
        "bare anchored mode must not be overridden by bounce-prevention"
    );
}

#[test]
fn pre_dispatch_passthrough_for_lines_multi_select() {
    // #971: `lines:A-B,C-D` is a precise pinned window exactly like
    // `lines:A-B`, but it parsed as Malformed, so `is_precise_pinned_mode`
    // reported "not pinned" and bounce-prevention rewrote an 8-line request
    // into a full-file read. Exercises the edit-forced branch, which is what
    // the field report actually hit (a ctx_patch anchored edit immediately
    // before the read).
    {
        let mut bt = crate::core::bounce_tracker::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bt.set_seq(201);
        bt.record_edit("multi-select-971.rs");
        bt.set_seq(202);
    }

    // Precondition: the tracker really is armed for this path, so the
    // assertions below cannot pass vacuously.
    let forced = pre_dispatch_read("multi-select-971.rs", "map", None, None, None);
    assert_eq!(
        forced.overridden_mode.as_deref(),
        Some("full"),
        "precondition: a recent edit must force a non-pinned mode to full"
    );

    let multi = pre_dispatch_read(
        "multi-select-971.rs",
        "lines:620-622,1214-1218",
        None,
        None,
        None,
    );
    assert!(
        multi.overridden_mode.is_none(),
        "lines:A-B,C-D must not be overridden by bounce-prevention (#971)"
    );

    // Control: the single-range form was already protected.
    let single = pre_dispatch_read("multi-select-971.rs", "lines:620-622", None, None, None);
    assert!(
        single.overridden_mode.is_none(),
        "lines:A-B must not be overridden by bounce-prevention"
    );
}

#[test]
fn fresh_escapes_bounce_prevention() {
    // #1588: after an edit the gate pins the file to `full`. Without an
    // escape hatch, signatures/map/reference stay unreachable for the rest
    // of the session — `fresh=true` is that hatch.
    {
        let mut bt = crate::core::bounce_tracker::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bt.set_seq(301);
        bt.record_edit("fresh-escape-1586.rs");
        bt.set_seq(302);
    }

    let pinned = pre_dispatch_read("fresh-escape-1586.rs", "signatures", None, None, None);
    assert_eq!(
        pinned.overridden_mode.as_deref(),
        Some("full"),
        "precondition: the tracker must actually be armed for this path"
    );

    let escaped = pre_dispatch_read_for_agent(
        "fresh-escape-1586.rs",
        "signatures",
        None,
        None,
        None,
        None,
        true,
    );
    assert!(
        escaped.overridden_mode.is_none(),
        "fresh=true must keep the requested mode reachable"
    );
}

#[test]
fn intent_target_matches_whole_components_only() {
    // #1588: substring matching pinned unrelated files to `full`.
    assert!(path_matches_target(
        "/repo/src/server/context_gate.rs",
        "context_gate"
    ));
    assert!(path_matches_target(
        "/repo/src/server/context_gate.rs",
        "context_gate.rs"
    ));
    assert!(path_matches_target(
        "/repo/src/server/context_gate.rs",
        "src/server/context_gate.rs"
    ));

    assert!(
        !path_matches_target("/repo/src/printer/link_gate.rs", "print"),
        "a target must not match the middle of a longer component"
    );
    assert!(
        !path_matches_target("/repo/src/server/context_gate.rs", "src"),
        "targets below the minimum length are noise, not names"
    );
    assert!(!path_matches_target(
        "/repo/src/server/context_gate.rs",
        "gate"
    ));
}

#[test]
fn fresh_escapes_intent_target() {
    let result = pre_dispatch_read_for_agent(
        "/repo/src/server/context_gate.rs",
        "reference",
        Some("fix context_gate overrides"),
        None,
        None,
        None,
        true,
    );
    assert!(
        result.overridden_mode.is_none(),
        "fresh=true must also escape the intent-target override"
    );
}

#[test]
fn pinned_modes_preserve_budget_warning() {
    let agent_id = format!(
        "context_gate_pinned_advisory_{:?}",
        std::thread::current().id()
    );
    let path = format!(
        "context-gate-pinned-advisory-{:?}.rs",
        std::thread::current().id()
    );
    crate::core::agent_budget::remove(&agent_id);
    crate::core::agent_budget::set_limit(&agent_id, 10_000);
    crate::core::agent_budget::record_consumption(&agent_id, 8_000);

    for mode in ["full", "map", "aggressive"] {
        let result = pre_dispatch_pinned_read_for_agent(&path, mode, Some(&agent_id));
        assert!(result.overridden_mode.is_none(), "mode={mode}");
        assert!(result.reason.is_none(), "mode={mode}");
        assert!(!result.pressure_downgraded, "mode={mode}");
        assert!(!result.budget_blocked, "mode={mode}");
        assert_eq!(result.triage_filter_level, 0, "mode={mode}");
        assert!(
            result
                .budget_warning
                .as_deref()
                .is_some_and(|warning| warning.contains("BUDGET WARNING")),
            "mode={mode} must retain the budget warning"
        );
    }

    crate::core::agent_budget::remove(&agent_id);
}

#[test]
fn pinned_read_still_blocks_exceeded_budget() {
    let agent_id = format!(
        "context_gate_pinned_exceeded_{:?}",
        std::thread::current().id()
    );
    crate::core::agent_budget::remove(&agent_id);
    crate::core::agent_budget::set_limit(&agent_id, 1_000);
    crate::core::agent_budget::record_consumption(&agent_id, 900);

    let result = pre_dispatch_pinned_read_for_agent(
        "context-gate-pinned-exceeded-missing.rs",
        "map",
        Some(&agent_id),
    );
    assert!(result.overridden_mode.is_none());
    assert_eq!(result.reason, Some("agent-budget-exceeded"));
    assert!(result.budget_blocked);
    assert!(
        result
            .budget_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("900/1000"))
    );
    assert!(!result.pressure_downgraded);
    assert_eq!(result.triage_filter_level, 0);

    crate::core::agent_budget::remove(&agent_id);
}

#[test]
fn pre_dispatch_no_override_without_signals() {
    let result = pre_dispatch_read("src/unknown.rs", "auto", None, None, None);
    assert!(result.overridden_mode.is_none());
}

#[test]
fn pre_dispatch_bounce_prevention_forces_full() {
    {
        let mut bt = crate::core::bounce_tracker::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bt.set_seq(1);
        bt.record_read("src/bouncy.yml", "map", 30, 400);
        bt.set_seq(2);
        bt.record_read("src/bouncy.yml", "full", 400, 400);
        bt.set_seq(3);
        bt.record_read("a2.yml", "map", 30, 400);
        bt.set_seq(4);
        bt.record_read("a2.yml", "full", 400, 400);
        bt.set_seq(5);
        bt.record_read("a3.yml", "map", 30, 400);
        bt.set_seq(6);
        bt.record_read("a3.yml", "full", 400, 400);
    }
    let result = pre_dispatch_read("new.yml", "auto", None, None, None);
    assert_eq!(result.overridden_mode, Some("full".to_string()));
    assert_eq!(result.reason, Some("bounce-prevention"));
}

#[test]
fn pressure_does_not_downgrade_explicit_full() {
    let result = pre_dispatch_read(
        "c.rs",
        "full",
        None,
        None,
        Some(&PressureAction::ForceCompression),
    );
    assert!(
        result.overridden_mode.is_none(),
        "explicit mode=full must never be downgraded by pressure"
    );
    assert!(!result.pressure_downgraded);
}

#[test]
fn pressure_does_not_downgrade_when_enforce_off() {
    // Default profile has degradation.enforce = false, so pressure
    // should NOT downgrade any mode.
    let result = pre_dispatch_read(
        "c.rs",
        "map",
        None,
        None,
        Some(&PressureAction::EvictLeastRelevant),
    );
    assert!(
        result.overridden_mode.is_none(),
        "pressure must not downgrade when degradation.enforce is off"
    );
    assert!(!result.pressure_downgraded);
}

#[test]
fn no_pressure_downgrade_when_low() {
    let result = pre_dispatch_read("c.rs", "full", None, None, Some(&PressureAction::NoAction));
    assert!(result.overridden_mode.is_none());
    assert!(!result.pressure_downgraded);
}

#[test]
fn suggest_compression_does_not_downgrade_when_enforce_off() {
    // Default profile has degradation.enforce = false
    let result = pre_dispatch_read(
        "c.rs",
        "auto",
        None,
        None,
        Some(&PressureAction::SuggestCompression),
    );
    assert!(
        result.overridden_mode.is_none(),
        "suggest_compression must not downgrade when enforce is off"
    );
    assert!(!result.pressure_downgraded);
}

#[test]
fn suggest_compression_does_not_touch_explicit_full() {
    let result = pre_dispatch_read(
        "c.rs",
        "full",
        None,
        None,
        Some(&PressureAction::SuggestCompression),
    );
    assert!(result.overridden_mode.is_none());
    assert!(!result.pressure_downgraded);
}

#[test]
fn post_dispatch_reinjection_downgrades_entries() {
    let mut ledger = ContextLedger::with_window_size(1000);
    ledger.record("a.rs", "full", 400, 400);
    ledger.record("b.rs", "full", 400, 400);
    let overlay = OverlayStore::new();
    let result = post_dispatch_record("c.rs", "full", 300, 300, &mut ledger, &overlay);
    assert!(result.resource_changed);
    let a_entry = ledger.entries.iter().find(|e| e.path == "a.rs").unwrap();
    assert_eq!(a_entry.mode, "map");
}

#[test]
fn ignited_item_resists_reinjection_downgrade() {
    // #6: a high-salience outlier ignites (pins) and keeps its full view,
    // while the rest are downgraded to map by pressure reinjection.
    let mut ledger = ContextLedger::with_window_size(1000);
    for i in 0..5 {
        ledger.record(&format!("bg{i}.rs"), "full", 250, 250);
    }
    ledger.record("hot.rs", "full", 250, 250);
    // Set the salience distribution explicitly (record recomputes Phi, so we
    // overwrite afterwards) to make ignition deterministic in the test.
    for e in &mut ledger.entries {
        e.phi = Some(if e.path == "hot.rs" { 0.97 } else { 0.1 });
    }
    let ignited = ledger.ignite_high_salience();
    assert_eq!(ignited, vec!["hot.rs".to_string()], "outlier should ignite");

    apply_reinjection_plan(&mut ledger, &PressureAction::ForceCompression);
    let hot = ledger.entries.iter().find(|e| e.path == "hot.rs").unwrap();
    assert_eq!(hot.mode, "full", "ignited item keeps its full view");
    let bg = ledger.entries.iter().find(|e| e.path == "bg0.rs").unwrap();
    assert_eq!(bg.mode, "map", "non-ignited items are downgraded");
}

#[test]
fn overlay_pin_forces_full_mode() {
    let dir = tempfile::tempdir().expect("tmp dir");
    let root = dir.path();
    let mut store = OverlayStore::new();
    let target = ContextItemId::from_file("src/important.rs");
    store.add(crate::core::context_overlay::ContextOverlay::new(
        target,
        OverlayOp::Pin { verbatim: false },
        crate::core::context_overlay::OverlayScope::Project,
        String::new(),
        crate::core::context_overlay::OverlayAuthor::User,
    ));
    store.save_project(root).unwrap();

    let result = pre_dispatch_read(
        "src/important.rs",
        "auto",
        None,
        Some(root.to_str().unwrap()),
        None,
    );
    assert_eq!(result.overridden_mode, Some("full".to_string()));
    assert_eq!(result.reason, Some("pinned"));
}

#[test]
fn overlay_exclude_forces_signatures_mode() {
    let dir = tempfile::tempdir().expect("tmp dir");
    let root = dir.path();
    let mut store = OverlayStore::new();
    let target = ContextItemId::from_file("src/noisy.rs");
    store.add(crate::core::context_overlay::ContextOverlay::new(
        target,
        OverlayOp::Exclude {
            reason: "noise".to_string(),
        },
        crate::core::context_overlay::OverlayScope::Project,
        String::new(),
        crate::core::context_overlay::OverlayAuthor::User,
    ));
    store.save_project(root).unwrap();

    let result = pre_dispatch_read(
        "src/noisy.rs",
        "auto",
        None,
        Some(root.to_str().unwrap()),
        None,
    );
    assert_eq!(result.overridden_mode, Some("signatures".to_string()));
    assert_eq!(result.reason, Some("excluded"));
}

// --- pressure_downgrade unit tests (pure function) ---

#[test]
fn pressure_downgrade_suggest_auto_to_map() {
    let result = pressure_downgrade("auto", &PressureAction::SuggestCompression);
    assert_eq!(result, Some("map".to_string()));
}

#[test]
fn pressure_downgrade_suggest_full_to_map() {
    let result = pressure_downgrade("full", &PressureAction::SuggestCompression);
    assert_eq!(result, Some("map".to_string()));
}

#[test]
fn pressure_downgrade_suggest_does_not_touch_signatures() {
    let result = pressure_downgrade("signatures", &PressureAction::SuggestCompression);
    assert!(result.is_none());
}

#[test]
fn pressure_downgrade_suggest_does_not_touch_diff() {
    let result = pressure_downgrade("diff", &PressureAction::SuggestCompression);
    assert!(result.is_none());
}

#[test]
fn pressure_downgrade_force_full_to_map() {
    let result = pressure_downgrade("full", &PressureAction::ForceCompression);
    assert_eq!(result, Some("map".to_string()));
}

#[test]
fn pressure_downgrade_force_auto_to_signatures() {
    let result = pressure_downgrade("auto", &PressureAction::ForceCompression);
    assert_eq!(result, Some("signatures".to_string()));
}

#[test]
fn pressure_downgrade_force_map_to_signatures() {
    let result = pressure_downgrade("map", &PressureAction::ForceCompression);
    assert_eq!(result, Some("signatures".to_string()));
}

#[test]
fn pressure_downgrade_force_does_not_touch_signatures() {
    let result = pressure_downgrade("signatures", &PressureAction::ForceCompression);
    assert!(result.is_none());
}

#[test]
fn pressure_downgrade_force_does_not_touch_lines() {
    let result = pressure_downgrade("lines:1-50", &PressureAction::ForceCompression);
    assert!(result.is_none());
}

#[test]
fn pressure_downgrade_evict_full_to_map() {
    let result = pressure_downgrade("full", &PressureAction::EvictLeastRelevant);
    assert_eq!(result, Some("map".to_string()));
}

#[test]
fn pressure_downgrade_evict_auto_to_signatures() {
    let result = pressure_downgrade("auto", &PressureAction::EvictLeastRelevant);
    assert_eq!(result, Some("signatures".to_string()));
}

#[test]
fn pressure_downgrade_evict_map_to_signatures() {
    let result = pressure_downgrade("map", &PressureAction::EvictLeastRelevant);
    assert_eq!(result, Some("signatures".to_string()));
}

#[test]
fn pressure_downgrade_noaction_returns_none() {
    let result = pressure_downgrade("full", &PressureAction::NoAction);
    assert!(result.is_none());
}

#[test]
fn pressure_downgrade_noaction_auto_returns_none() {
    let result = pressure_downgrade("auto", &PressureAction::NoAction);
    assert!(result.is_none());
}

// --- pre_dispatch_inner: no_degrade integration ---
// When LCTX_NO_DEGRADE is NOT set (test default), pressure downgrade is active.

#[test]
fn pre_dispatch_does_not_downgrade_full_under_force() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    // Explicit mode=full is protected: pressure cannot downgrade it
    let result = pre_dispatch_read(
        "nd_test.rs",
        "full",
        None,
        None,
        Some(&PressureAction::ForceCompression),
    );
    assert!(result.overridden_mode.is_none());
    assert!(!result.pressure_downgraded);
}

#[test]
fn pre_dispatch_does_not_downgrade_auto_when_enforce_off() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    // Default profile has degradation.enforce = false, so pressure
    // should not downgrade even non-full modes
    let result = pre_dispatch_read(
        "nd_test2.rs",
        "auto",
        None,
        None,
        Some(&PressureAction::EvictLeastRelevant),
    );
    assert!(result.overridden_mode.is_none());
    assert!(!result.pressure_downgraded);
}

// --- estimate_read_tokens unit tests ---

#[test]
fn estimate_tokens_diff_mode_is_small() {
    let tokens = estimate_read_tokens("nonexistent.rs", "diff");
    assert!(tokens < 500, "diff mode should estimate low: got {tokens}");
}

#[test]
fn estimate_tokens_signatures_smaller_than_full() {
    let sig = estimate_read_tokens("nonexistent.rs", "signatures");
    let full = estimate_read_tokens("nonexistent.rs", "full");
    assert!(sig < full, "signatures={sig} should be < full={full}");
}

#[test]
fn estimate_tokens_lines_range() {
    let tokens = estimate_read_tokens("nonexistent.rs", "lines:1-10");
    assert!(tokens <= 200, "lines:1-10 should be small: got {tokens}");
}

#[test]
fn overlay_set_view_forces_specified_mode() {
    let dir = tempfile::tempdir().expect("tmp dir");
    let root = dir.path();
    let mut store = OverlayStore::new();
    let target = ContextItemId::from_file("src/big.rs");
    store.add(crate::core::context_overlay::ContextOverlay::new(
        target,
        OverlayOp::SetView(crate::core::context_field::ViewKind::Map),
        crate::core::context_overlay::OverlayScope::Project,
        String::new(),
        crate::core::context_overlay::OverlayAuthor::User,
    ));
    store.save_project(root).unwrap();

    let result = pre_dispatch_read(
        "src/big.rs",
        "auto",
        None,
        Some(root.to_str().unwrap()),
        None,
    );
    assert_eq!(result.overridden_mode, Some("map".to_string()));
    assert_eq!(result.reason, Some("overlay-set-view"));
}
use crate::core::triage::profile::TaskScopeLocal;

fn test_profile(confidence: u16, context_need: u16) -> TaskProfileLocal {
    TaskProfileLocal {
        task_class: "bug_fix".into(),
        intent: "fix context gate filtering".into(),
        complexity: "low".into(),
        scope: TaskScopeLocal::SingleFile,
        context_need_milli: context_need,
        reasoning_need_milli: 0,
        risk_signal_milli: 0,
        confidence_milli: confidence,
    }
}

#[test]
fn triage_filter_level_selects_expected_level() {
    for (confidence, context_need, expected) in [
        (200, 200, 0),
        // context_need == 0 means "unknown", never "needs no context" —
        // the fallback profile must pass through untouched.
        (300, 0, 0),
        (500, 0, 0),
        (500, 200, 1),
        (500, 450, 1),
        (500, 700, 0),
        // A rules-derived profile never selects the lossy level 2, no
        // matter how confident the intent classification is.
        (700, 200, 1),
        (700, 450, 1),
        (700, 700, 0),
        (1000, 100, 1),
    ] {
        assert_eq!(
            triage_filter_level(&test_profile(confidence, context_need)),
            expected,
            "confidence={confidence} context_need={context_need}"
        );
    }
}

#[test]
fn no_model_installed_means_passthrough() {
    use crate::core::triage::{TaskAnalysisInput, TaskAnalyzer, rules::RuleTriageBackend};

    let profile = RuleTriageBackend
        .analyze(&TaskAnalysisInput::default())
        .unwrap()
        .profile;
    assert_eq!(triage_filter_level(&profile), 0);
}

#[test]
fn apply_triage_filter_level_zero_is_passthrough() {
    let profile = test_profile(500, 200);
    let output = "// boilerplate\n".repeat(40);
    assert_eq!(apply_triage_filter(&output, &profile, 0), (output, 0));
}

#[test]
fn apply_triage_filter_short_output_is_passthrough() {
    let profile = test_profile(500, 200);
    let output = "fn main() {\n    println!(\"hi\");\n}";
    assert_eq!(apply_triage_filter(output, &profile, 2), (output.into(), 0));
}

// #1570 P4: a <protect> span is a user contract — the whole output is
// exempt from lossy filtering at every level.
#[test]
fn protect_span_bypasses_lossy_filtering_entirely() {
    let profile = test_profile(700, 200);
    let output = "// boilerplate noise\n".repeat(30)
        + "<protect>operational checklist — must stay verbatim</protect>\n";
    let (filtered, removed) = apply_triage_filter(&output, &profile, 2);
    assert_eq!(removed, 0, "protected output must lose nothing");
    assert_eq!(filtered, output);
}

#[test]
fn apply_triage_filter_level_one_preserves_actionable_comments() {
    let profile = test_profile(500, 450);
    let output = "// boilerplate comment\n".repeat(30)
        + "// TODO: keep this\n// FIXME: keep this\n// SAFETY: keep this\nfn keep_me() {}\n";
    let (filtered, removed) = apply_triage_filter(&output, &profile, 1);
    assert!(!filtered.contains("// boilerplate comment"));
    assert!(filtered.contains("// TODO: keep this"));
    assert!(filtered.contains("// FIXME: keep this"));
    assert!(filtered.contains("// SAFETY: keep this"));
    assert!(
        filtered.contains("[lean-ctx: 30 lines filtered by triage (level 1) — rerun with raw=true")
    );
    assert_eq!(removed, 30);
}

/// Community regression (3.9.19 "abridged into something that still looks
/// right"): level 2 on NON-markdown content must never delete a
/// non-comment line — plain `let` bindings used to vanish from source
/// files while the survivors still parsed.
#[test]
fn apply_triage_filter_level_two_never_drops_code_lines() {
    let profile = test_profile(700, 200);
    let output = "// boilerplate noise\n".repeat(20)
        + "pub fn read_identity() -> Identity {\n"
        + "    let serial = read_serial();\n"
        + "    let model = read_model();\n"
        + "    Identity { serial, model }\n"
        + "}\n";
    let (filtered, removed) = apply_triage_filter(&output, &profile, 2);
    // Every code line survives — especially the plain `let` bindings that
    // carry neither braces, keywords, nor task keywords.
    assert!(filtered.contains("let serial = read_serial();"));
    assert!(filtered.contains("let model = read_model();"));
    assert!(filtered.contains("Identity { serial, model }"));
    // Only the boilerplate comments were stripped, and the footer names
    // the escape hatch.
    assert_eq!(removed, 20);
    assert!(filtered.contains("rerun with raw=true"));
}

#[test]
fn apply_triage_filter_level_two_keeps_keywords_and_structure() {
    let profile = test_profile(500, 200);
    let output = "// unrelated noise about widgets\n".repeat(20)
        + "fix the context gate\n"
        + "pub fn unrelated() {}\n"
        + "#[derive(Debug)]\n"
        + "mod inner {}\n";
    let (filtered, removed) = apply_triage_filter(&output, &profile, 2);
    assert!(filtered.contains("fix the context gate"));
    assert!(filtered.contains("pub fn unrelated() {}"));
    assert!(filtered.contains("#[derive(Debug)]"));
    assert!(filtered.contains("mod inner {}"));
    assert!(filtered.contains("[lean-ctx:"));
    assert_eq!(removed, 20);
}

#[test]
fn benchmark_apply_triage_filter() {
    use std::time::Instant;

    let profile = test_profile(500, 200);
    // Use a mix of keyword-matching and non-matching lines so triage
    // actually filters (100% deletion is now a passthrough per #1493).
    let output = "// unrelated noise about widgets\n".repeat(1_999) + "fix the context gate\n";

    let started = Instant::now();
    for _ in 0..1_000 {
        let (_, removed) = apply_triage_filter(&output, &profile, 2);
        assert_eq!(removed, 1_999);
    }
    let average = started.elapsed() / 1_000;
    println!("benchmark_apply_triage_filter: {average:?} per call");
}

#[test]
fn gh1493_total_deletion_returns_original_output() {
    let profile = test_profile(500, 200);
    // 30 lines of plain file names — no keyword match, would be 100% deleted
    let output = (1..=30)
        .map(|i| format!("plugin/device_{i:03}.go"))
        .collect::<Vec<_>>()
        .join("\n");
    let (filtered, removed) = apply_triage_filter(&output, &profile, 2);
    assert_eq!(
        removed, 0,
        "100% deletion must be prevented — output should pass through"
    );
    assert_eq!(filtered, output);
}

#[test]
fn extract_task_keywords_normalizes_and_deduplicates() {
    assert_eq!(
        extract_task_keywords("bug_fix", "Fix context-gate"),
        ["bug", "context", "fix", "gate"]
    );
}

#[test]
fn markdown_level_two_keeps_section_leads_not_just_headings() {
    let profile = test_profile(500, 200);
    let filler = "Later filler that restates the same loop in racing language. ".repeat(8);
    let extra = "Ignore this extra dashboard discussion. ".repeat(8);
    let output = [
        "# LeanCTX: The Context SDK for AI Agents",
        "",
        "## The context-performance loop",
        "Connect, measure, tune, prove, deploy, repeat.",
        "",
        filler.as_str(),
        "",
        "## The Receipt is the trust product",
        "A Receipt is evidence, not a decorative log.",
        "",
        extra.as_str(),
        "",
        "## Guardrails",
        "Local runtime, SDK, CLI, and Receipt stay useful offline.",
        "",
        extra.as_str(),
    ]
    .join("\n");
    let (filtered, removed) = apply_triage_filter(&output, &profile, 2);
    assert!(
        filtered.contains("Connect, measure, tune, prove"),
        "section lead must survive: {filtered}"
    );
    assert!(
        filtered.contains("A Receipt is evidence"),
        "receipt lead must survive: {filtered}"
    );
    assert!(
        !filtered.contains("racing language"),
        "later filler should drop: {filtered}"
    );
    assert!(removed > 0);
}
