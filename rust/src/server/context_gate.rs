use crate::core::context_field::{ContextItemId, ContextState};
use crate::core::context_ledger::{ContextLedger, PressureAction};
use crate::core::context_overlay::{OverlayOp, OverlayStore};

#[derive(Debug, Clone)]
pub struct PreDispatchResult {
    pub overridden_mode: Option<String>,
    pub reason: Option<&'static str>,
    pub pressure_downgraded: bool,
    pub budget_blocked: bool,
    pub budget_warning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PostDispatchResult {
    pub eviction_hint: Option<String>,
    pub elicitation_hint: Option<String>,
    pub resource_changed: bool,
    /// FEP prefetch suggestion (#9): files likely needed next, from the co-access
    /// graph. A warmup hint only — never an automatic read.
    pub prefetch_hint: Option<String>,
}

#[must_use]
pub fn pre_dispatch_read(
    path: &str,
    requested_mode: &str,
    task: Option<&str>,
    project_root: Option<&str>,
    pressure: Option<&PressureAction>,
) -> PreDispatchResult {
    pre_dispatch_read_for_agent(path, requested_mode, task, project_root, pressure, None)
}

#[must_use]
pub fn pre_dispatch_read_for_agent(
    path: &str,
    requested_mode: &str,
    task: Option<&str>,
    project_root: Option<&str>,
    pressure: Option<&PressureAction>,
    agent_id: Option<&str>,
) -> PreDispatchResult {
    let no_change = PreDispatchResult {
        overridden_mode: None,
        reason: None,
        pressure_downgraded: false,
        budget_blocked: false,
        budget_warning: None,
    };

    if let Some(aid) = agent_id {
        let estimated_tokens = estimate_read_tokens(path, requested_mode);
        match crate::core::agent_budget::check_budget(aid, estimated_tokens) {
            crate::core::agent_budget::BudgetCheckResult::Exceeded { limit, consumed } => {
                return PreDispatchResult {
                    overridden_mode: None,
                    reason: Some("agent-budget-exceeded"),
                    pressure_downgraded: false,
                    budget_blocked: true,
                    budget_warning: Some(format!(
                        "Agent budget exceeded: {consumed}/{limit} tokens consumed. Reset via ctx_session or set a higher limit."
                    )),
                };
            }
            crate::core::agent_budget::BudgetCheckResult::Warning {
                remaining,
                percent_used,
            } => {
                let warning = format!(
                    "[BUDGET WARNING] Agent '{aid}' at {:.0}% budget ({remaining} tokens remaining)",
                    percent_used * 100.0
                );
                let mut result = no_change.clone();
                result.budget_warning = Some(warning);
                if requested_mode == "diff" || requested_mode.starts_with("lines") {
                    return result;
                }
                let rest = pre_dispatch_inner(path, requested_mode, task, project_root, pressure);
                return PreDispatchResult {
                    budget_warning: result.budget_warning,
                    ..rest
                };
            }
            crate::core::agent_budget::BudgetCheckResult::Allowed { .. } => {}
        }
    }

    pre_dispatch_inner(path, requested_mode, task, project_root, pressure)
}

fn pre_dispatch_inner(
    path: &str,
    requested_mode: &str,
    task: Option<&str>,
    project_root: Option<&str>,
    pressure: Option<&PressureAction>,
) -> PreDispatchResult {
    let no_change = PreDispatchResult {
        overridden_mode: None,
        reason: None,
        pressure_downgraded: false,
        budget_blocked: false,
        budget_warning: None,
    };

    if requested_mode == "diff" || requested_mode.starts_with("lines") {
        return no_change;
    }

    if let Some(root) = project_root {
        let overlay = OverlayStore::load_project(&std::path::PathBuf::from(root));
        if let Some(result) = check_overlay_mode_override(path, requested_mode, &overlay) {
            return result;
        }
    }

    // Explicit mode=full must not be downgraded by pressure or other heuristics.
    // Only overlays (user-explicit) above can override it.
    if requested_mode == "full" {
        return no_change;
    }

    if let Some(action) = pressure {
        let no_degrade = crate::core::config::Config::load().no_degrade_effective();
        let profile = crate::core::profiles::active_profile();
        if !no_degrade
            && profile.degradation.enforce_effective()
            && let Some(downgraded) = pressure_downgrade(requested_mode, action)
        {
            return PreDispatchResult {
                overridden_mode: Some(downgraded),
                reason: Some("pressure-auto-downgrade"),
                pressure_downgraded: true,
                budget_blocked: false,
                budget_warning: None,
            };
        }
    }

    if let Ok(bt) = crate::core::bounce_tracker::global().lock()
        && bt.should_force_full(path)
    {
        return PreDispatchResult {
            overridden_mode: Some("full".to_string()),
            reason: Some("bounce-prevention"),
            pressure_downgraded: false,
            budget_blocked: false,
            budget_warning: None,
        };
    }

    if let Some(task_str) = task {
        let intent = crate::core::intent_engine::StructuredIntent::from_query(task_str);
        let norm = crate::core::pathutil::normalize_tool_path(path);
        let is_target = intent
            .targets
            .iter()
            .any(|t| norm.ends_with(t) || norm.contains(t));
        if is_target {
            return PreDispatchResult {
                overridden_mode: Some("full".to_string()),
                reason: Some("intent-target"),
                pressure_downgraded: false,
                budget_blocked: false,
                budget_warning: None,
            };
        }
    }

    if let Some(root) = project_root
        && let Some(open) = try_load_graph(root)
    {
        let gp = &open.provider;
        let related = gp.related(path, 1);
        if let Some(task_str) = task {
            let intent = crate::core::intent_engine::StructuredIntent::from_query(task_str);
            for target in &intent.targets {
                let target_related = gp.related(target, 1);
                let norm = crate::core::pathutil::normalize_tool_path(path);
                if target_related
                    .iter()
                    .any(|r| r.contains(&norm) || norm.contains(r))
                {
                    return PreDispatchResult {
                        overridden_mode: Some("map".to_string()),
                        reason: Some("graph-direct-import"),
                        pressure_downgraded: false,
                        budget_blocked: false,
                        budget_warning: None,
                    };
                }
            }
        }
        if !related.is_empty() && requested_mode == "auto" {
            let reverse_deps = gp.dependents(path);
            if reverse_deps.len() > 3 {
                return PreDispatchResult {
                    overridden_mode: Some("map".to_string()),
                    reason: Some("graph-hub-file"),
                    pressure_downgraded: false,
                    budget_blocked: false,
                    budget_warning: None,
                };
            }
        }
    }

    if let Some(root) = project_root
        && let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(root)
    {
        let norm = crate::core::pathutil::normalize_tool_path(path);
        let mentions = knowledge
            .facts
            .iter()
            .filter(|f| f.value.contains(&norm) || f.key.contains(&norm))
            .count();
        if mentions >= 3 {
            return PreDispatchResult {
                overridden_mode: Some("map".to_string()),
                reason: Some("knowledge-high-relevance"),
                pressure_downgraded: false,
                budget_blocked: false,
                budget_warning: None,
            };
        }
    }

    no_change
}

fn estimate_read_tokens(path: &str, mode: &str) -> usize {
    let file_size = std::fs::metadata(path).map_or(4000, |m| m.len() as usize);
    let char_estimate = file_size;
    let full_tokens = char_estimate / 4;
    match mode {
        "signatures" => full_tokens / 5,
        "map" => full_tokens / 3,
        "aggressive" | "entropy" => full_tokens / 4,
        "diff" => full_tokens / 10,
        _ if mode.starts_with("lines:") => {
            if let Some(range) = mode.strip_prefix("lines:") {
                let parts: Vec<&str> = range.split('-').collect();
                if parts.len() == 2 {
                    let start = parts[0].parse::<usize>().unwrap_or(1);
                    let end = parts[1].parse::<usize>().unwrap_or(start + 100);
                    (end.saturating_sub(start) + 1) * 10
                } else {
                    full_tokens / 10
                }
            } else {
                full_tokens / 10
            }
        }
        _ => full_tokens,
    }
}

fn pressure_downgrade(requested_mode: &str, action: &PressureAction) -> Option<String> {
    crate::core::auto_mode_resolver::pressure_downgrade(requested_mode, action)
}

fn check_overlay_mode_override(
    path: &str,
    requested_mode: &str,
    overlay: &OverlayStore,
) -> Option<PreDispatchResult> {
    let item_id = ContextItemId::from_file(path);
    let overlays = overlay.for_item(&item_id);

    for ov in overlays.iter().rev() {
        match &ov.operation {
            OverlayOp::SetView(view) => {
                let mode_str = view.as_str();
                if mode_str != requested_mode {
                    return Some(PreDispatchResult {
                        overridden_mode: Some(mode_str.to_string()),
                        reason: Some("overlay-set-view"),
                        pressure_downgraded: false,
                        budget_blocked: false,
                        budget_warning: None,
                    });
                }
            }
            OverlayOp::Pin { .. } if requested_mode != "full" => {
                return Some(PreDispatchResult {
                    overridden_mode: Some("full".to_string()),
                    reason: Some("pinned"),
                    pressure_downgraded: false,
                    budget_blocked: false,
                    budget_warning: None,
                });
            }
            OverlayOp::Exclude { .. } if requested_mode != "signatures" => {
                return Some(PreDispatchResult {
                    overridden_mode: Some("signatures".to_string()),
                    reason: Some("excluded"),
                    pressure_downgraded: false,
                    budget_blocked: false,
                    budget_warning: None,
                });
            }
            _ => {}
        }
    }
    None
}

pub fn post_dispatch_record(
    path: &str,
    mode: &str,
    original_tokens: usize,
    sent_tokens: usize,
    ledger: &mut ContextLedger,
    overlay: &OverlayStore,
) -> PostDispatchResult {
    post_dispatch_record_with_task(
        path,
        mode,
        original_tokens,
        sent_tokens,
        ledger,
        overlay,
        None,
        None,
    )
}

pub fn post_dispatch_record_with_task(
    path: &str,
    mode: &str,
    original_tokens: usize,
    sent_tokens: usize,
    ledger: &mut ContextLedger,
    overlay: &OverlayStore,
    task: Option<&str>,
    project_root: Option<&str>,
) -> PostDispatchResult {
    let prev_count = ledger.entries.len();
    let prev_pressure = ledger.pressure().recommendation;

    ledger.record_with_task(path, mode, original_tokens, sent_tokens, task);

    let item_id = ContextItemId::from_file(path);
    let state = overlay.apply_to_state(&item_id, ContextState::Included);

    if state == ContextState::Excluded {
        return PostDispatchResult {
            eviction_hint: Some(format!("File '{path}' is excluded by overlay.")),
            elicitation_hint: None,
            resource_changed: true,
            prefetch_hint: None,
        };
    }

    let elicitation =
        super::elicitation::check_elicitation_needed(ledger, Some(path), Some(sent_tokens))
            .map(|s| s.format_fallback_hint());

    let pressure = ledger.pressure();

    // #6 Global-Workspace ignition: salience outliers are broadcast (pinned) into
    // the working set BEFORE reinjection, so an ignited item keeps its view while
    // the rest are downgraded under pressure. Deterministic z-score threshold.
    let ignited = ledger.ignite_high_salience();

    apply_reinjection_plan(ledger, &pressure.recommendation);

    let new_entry = ledger.entries.len() != prev_count;
    let pressure_shifted = pressure.recommendation != prev_pressure;
    let resource_changed = new_entry || pressure_shifted || !ignited.is_empty();

    if pressure.utilization > 0.9 {
        let candidates = ledger.eviction_candidates_by_phi(3);
        if !candidates.is_empty() {
            let names: Vec<_> = candidates
                .iter()
                .take(3)
                .map(|p| crate::core::protocol::shorten_path(p))
                .collect();
            return PostDispatchResult {
                eviction_hint: Some(format!(
                    "Context pressure {:.0}%. Evict: ctx_ledger(action=\"evict\", targets=\"{}\")",
                    pressure.utilization * 100.0,
                    names.join(", ")
                )),
                elicitation_hint: elicitation,
                resource_changed,
                // Under pressure we evict rather than prefetch — no warmup hint.
                prefetch_hint: None,
            };
        }
    }

    // #9 FEP prefetch: with budget to spare, suggest the files most likely needed
    // next (co-access graph), so the agent can warm them before the surprise of a
    // miss. Deterministic; runs in the background post-dispatch, never in output.
    let prefetch_hint =
        project_root.and_then(|root| crate::core::fep_prefetch::prefetch_hint(root, path, ledger));

    PostDispatchResult {
        eviction_hint: None,
        elicitation_hint: elicitation,
        resource_changed,
        prefetch_hint,
    }
}

fn apply_reinjection_plan(ledger: &mut ContextLedger, action: &PressureAction) {
    if *action != PressureAction::ForceCompression && *action != PressureAction::EvictLeastRelevant
    {
        return;
    }
    for entry in &mut ledger.entries {
        // #6: ignited / user-pinned items stay broadcast — never downgraded.
        if entry.state == Some(ContextState::Pinned) {
            continue;
        }
        if entry.mode == "full" {
            entry.mode = "map".to_string();
        }
    }
}

fn try_load_graph(project_root: &str) -> Option<crate::core::graph_provider::OpenGraphProvider> {
    crate::core::graph_provider::open_best_effort(project_root)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn pre_dispatch_no_override_without_signals() {
        let result = pre_dispatch_read("src/unknown.rs", "auto", None, None, None);
        assert!(result.overridden_mode.is_none());
    }

    #[test]
    fn pre_dispatch_bounce_prevention_forces_full() {
        {
            let mut bt = crate::core::bounce_tracker::global().lock().unwrap();
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
}
