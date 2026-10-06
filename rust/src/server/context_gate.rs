use crate::core::context_field::{ContextItemId, ContextState};
use crate::core::context_ledger::{ContextLedger, PressureAction};
use crate::core::context_overlay::{OverlayOp, OverlayStore};

/// #1570 P4: protected path arguments bypass every lossy output filter.
pub(super) fn protected_path_requested(
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    let Some(args) = args else {
        return false;
    };
    let config = crate::core::config::Config::load();
    if config.protection.file_patterns.is_empty() {
        return false;
    }
    ["path", "file_path", "filePath"].iter().any(|key| {
        args.get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| config.protection.path_is_protected(path))
    })
}

/// #843: precise, pinned reads (`diff`, `lines:N-M`, `anchored`/`anchored:N-M`)
/// must pass through every mode-override path untouched — bounce-prevention,
/// pressure-downgrade, and the graph/knowledge heuristics below must never
/// silently reinterpret one of these to e.g. `full`, which would discard the
/// exact window/anchors/delta the caller asked for. Classifies off the typed
/// `ReadMode` (single source of truth, see `tools::ctx_read::mode`) rather than
/// a hand-maintained string-prefix list, so a future precise mode has to be
/// added to `ReadMode::is_precise_pinned_read` explicitly instead of silently
/// falling through an allowlist. An unparseable mode is conservatively treated
/// as *not* pinned, matching prior behaviour for anything outside this set.
fn is_precise_pinned_mode(requested_mode: &str) -> bool {
    requested_mode
        .parse::<crate::tools::ctx_read::ReadMode>()
        .is_ok_and(|m| m.is_precise_pinned_read())
}

#[derive(Debug, Clone)]
pub struct PreDispatchResult {
    pub overridden_mode: Option<String>,
    pub reason: Option<&'static str>,
    pub pressure_downgraded: bool,
    pub budget_blocked: bool,
    pub budget_warning: Option<String>,
    /// Triage-based output filter aggressiveness: 0=passthrough, 1=moderate, 2=aggressive.
    pub triage_filter_level: u8,
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

/// Best-effort KnowledgeRouter advice for the completed Context Gate flow.
pub fn knowledge_advice(
    query: &str,
) -> crate::core::knowledge_router::gate_integration::KnowledgeAdvice {
    let current = crate::core::context_kernel::ContextState::new(
        Vec::new(),
        crate::core::knowledge::KnowledgeQuery::default(),
    );
    crate::core::knowledge_router::gate_integration::KnowledgeGateAdvisor::advise(query, &current)
}

pub fn pre_dispatch_read(
    path: &str,
    requested_mode: &str,
    task: Option<&str>,
    project_root: Option<&str>,
    pressure: Option<&PressureAction>,
) -> PreDispatchResult {
    pre_dispatch_read_for_agent(
        path,
        requested_mode,
        task,
        project_root,
        pressure,
        None,
        false,
    )
}

enum AgentBudgetDecision {
    Allowed,
    Warning(String),
    Exceeded(PreDispatchResult),
}

fn no_change_result() -> PreDispatchResult {
    PreDispatchResult {
        overridden_mode: None,
        reason: None,
        pressure_downgraded: false,
        budget_blocked: false,
        budget_warning: None,
        triage_filter_level: 0,
    }
}

fn check_agent_budget(
    path: &str,
    requested_mode: &str,
    agent_id: Option<&str>,
) -> AgentBudgetDecision {
    let Some(aid) = agent_id else {
        return AgentBudgetDecision::Allowed;
    };

    let estimated_tokens = estimate_read_tokens(path, requested_mode);
    match crate::core::agent_budget::check_budget(aid, estimated_tokens) {
        crate::core::agent_budget::BudgetCheckResult::Exceeded { limit, consumed } => {
            let mut result = no_change_result();
            result.reason = Some("agent-budget-exceeded");
            result.budget_blocked = true;
            result.budget_warning = Some(format!(
                "Agent budget exceeded: {consumed}/{limit} tokens consumed. Reset via ctx_session or set a higher limit."
            ));
            AgentBudgetDecision::Exceeded(result)
        }
        crate::core::agent_budget::BudgetCheckResult::Warning {
            remaining,
            percent_used,
        } => AgentBudgetDecision::Warning(format!(
            "[BUDGET WARNING] Agent '{aid}' at {:.0}% budget ({remaining} tokens remaining)",
            percent_used * 100.0
        )),
        crate::core::agent_budget::BudgetCheckResult::Allowed { .. } => {
            AgentBudgetDecision::Allowed
        }
    }
}

/// Pre-dispatch gate for an explicit caller-pinned mode.
///
/// Only the hard per-agent budget gate applies; advisory mode rewriting is
/// intentionally bypassed so the requested concrete mode remains intact.
pub fn pre_dispatch_pinned_read_for_agent(
    path: &str,
    requested_mode: &str,
    agent_id: Option<&str>,
) -> PreDispatchResult {
    let no_change = no_change_result();
    match check_agent_budget(path, requested_mode, agent_id) {
        AgentBudgetDecision::Exceeded(result) => result,
        AgentBudgetDecision::Warning(warning) => PreDispatchResult {
            budget_warning: Some(warning),
            ..no_change
        },
        AgentBudgetDecision::Allowed => no_change,
    }
}

/// `fresh` is the caller's explicit escape hatch (#1588). Bounce-prevention and
/// intent-target exist to stop an agent from *drifting* into a compressed view
/// it will immediately re-read in full; neither should be able to pin a file to
/// `full` forever. A caller who passes `fresh=true` has said, in the request
/// itself, that they want this view recomputed — honour it, or those modes are
/// simply unreachable for the rest of the session.
pub fn pre_dispatch_read_for_agent(
    path: &str,
    requested_mode: &str,
    task: Option<&str>,
    project_root: Option<&str>,
    pressure: Option<&PressureAction>,
    agent_id: Option<&str>,
    fresh: bool,
) -> PreDispatchResult {
    let no_change = no_change_result();

    match check_agent_budget(path, requested_mode, agent_id) {
        AgentBudgetDecision::Exceeded(result) => return result,
        AgentBudgetDecision::Warning(warning) => {
            let mut result = no_change.clone();
            result.budget_warning = Some(warning);
            if is_precise_pinned_mode(requested_mode) {
                return result;
            }
            let rest =
                pre_dispatch_inner(path, requested_mode, task, project_root, pressure, fresh);
            return PreDispatchResult {
                budget_warning: result.budget_warning,
                ..rest
            };
        }
        AgentBudgetDecision::Allowed => {}
    }

    pre_dispatch_inner(path, requested_mode, task, project_root, pressure, fresh)
}

/// Does `norm` (a normalized path) actually name the intent target `target`?
///
/// The old rule was `norm.ends_with(t) || norm.contains(t)`, which matched on
/// any substring: a task mentioning `src` or `read` pinned every path
/// containing those letters to `full` (#1588), including matches that land in
/// the middle of a longer component (`print` inside `printer.rs`). A target now
/// has to line up with a whole path component or the file stem, and be long
/// enough to be a name rather than noise.
fn path_matches_target(norm: &str, target: &str) -> bool {
    const MIN_TARGET_LEN: usize = 4;
    let target = target.trim().trim_matches('/');
    if target.len() < MIN_TARGET_LEN {
        return false;
    }
    // A target that is itself a path suffix (`src/server/context_gate.rs`).
    if norm == target || norm.ends_with(&format!("/{target}")) {
        return true;
    }
    // Otherwise: a whole path component, with or without its extension.
    norm.split('/').any(|component| {
        component == target
            || component
                .rsplit_once('.')
                .is_some_and(|(stem, _)| stem == target)
    })
}

fn pre_dispatch_inner(
    path: &str,
    requested_mode: &str,
    task: Option<&str>,
    project_root: Option<&str>,
    pressure: Option<&PressureAction>,
    fresh: bool,
) -> PreDispatchResult {
    let no_change = PreDispatchResult {
        overridden_mode: None,
        reason: None,
        pressure_downgraded: false,
        budget_blocked: false,
        budget_warning: None,
        triage_filter_level: 0,
    };

    if is_precise_pinned_mode(requested_mode) {
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
                triage_filter_level: 0,
            };
        }
    }

    // `fresh` is the documented way out (#1588): the caller re-requested this
    // view deliberately, so the "you keep bouncing back to full" heuristic has
    // nothing left to prevent.
    if !fresh
        && let Ok(bt) = crate::core::bounce_tracker::global().lock()
        && bt.should_force_full(path)
    {
        return PreDispatchResult {
            overridden_mode: Some("full".to_string()),
            reason: Some("bounce-prevention"),
            pressure_downgraded: false,
            budget_blocked: false,
            budget_warning: None,
            triage_filter_level: 0,
        };
    }

    if let Some(task_str) = task
        && !fresh
    {
        let intent = crate::core::intent_engine::StructuredIntent::from_query(task_str);
        let norm = crate::core::pathutil::normalize_tool_path(path);
        let is_target = intent.targets.iter().any(|t| path_matches_target(&norm, t));
        if is_target {
            return PreDispatchResult {
                overridden_mode: Some("full".to_string()),
                reason: Some("intent-target"),
                pressure_downgraded: false,
                budget_blocked: false,
                budget_warning: None,
                triage_filter_level: 0,
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
                        triage_filter_level: 0,
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
                    triage_filter_level: 0,
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
                triage_filter_level: 0,
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
                // #971: comma is multi-select. Splitting the whole payload on
                // '-' yields 3 parts for `620-622,1214-1218` and fell through to
                // the whole-file tenth; sum the selected spans instead.
                let mut selected = 0usize;
                for part in range.split(',') {
                    let part = part.trim();
                    selected += if let Some((s, e)) = part.split_once('-') {
                        let start = s.trim().parse::<usize>().unwrap_or(1);
                        let end = e.trim().parse::<usize>().unwrap_or(start + 100);
                        end.saturating_sub(start) + 1
                    } else {
                        1
                    };
                }
                if selected == 0 {
                    full_tokens / 10
                } else {
                    selected * 10
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
                        triage_filter_level: 0,
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
                    triage_filter_level: 0,
                });
            }
            OverlayOp::Exclude { .. } if requested_mode != "signatures" => {
                return Some(PreDispatchResult {
                    overridden_mode: Some("signatures".to_string()),
                    reason: Some("excluded"),
                    pressure_downgraded: false,
                    budget_blocked: false,
                    budget_warning: None,
                    triage_filter_level: 0,
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
            // #715: emit targets the evict resolver can actually find —
            // root-relative paths (or the full path), never display-shortened
            // forms that used to produce "Evicted 0/N".
            let names: Vec<_> = candidates
                .iter()
                .take(3)
                .map(|p| eviction_target_display(p, project_root))
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

/// #715: a resolvable evict target for hint output — project-root-relative
/// when the candidate lives under the root, otherwise the full canonical
/// path. Both forms round-trip through `ContextLedger::resolve_entry`.
fn eviction_target_display(path: &str, project_root: Option<&str>) -> String {
    if let Some(root) = project_root.filter(|r| !r.is_empty()) {
        let root_prefix = format!("{}/", root.trim_end_matches(['/', '\\']).replace('\\', "/"));
        if let Some(rel) = path.strip_prefix(&root_prefix)
            && !rel.is_empty()
        {
            return rel.to_string();
        }
    }
    path.to_string()
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

/// Determines the output-filtering aggressiveness from a task profile.
///
/// Fail-safe by construction (community reports on 3.9.19, "triage always
/// level 2"): the original ladder inverted fail-safety — the empty-query
/// fallback profile landed on the MOST aggressive level, and per-tool-call
/// profiles almost always report a low context need (a single tool call
/// classifies as SingleFile, base 250). A rules-derived profile can now
/// select at most level 1; uncertainty degrades toward passthrough, never
/// toward harder filtering.
pub fn triage_filter_level(profile: &crate::core::triage::profile::TaskProfileLocal) -> u8 {
    use crate::core::triage::confidence::ACTIONABLE_FLOOR_MILLI;
    // An unset context need means "unknown", never "needs no context".
    if profile.confidence_milli < ACTIONABLE_FLOOR_MILLI || profile.context_need_milli == 0 {
        return 0;
    }
    // Level 2 removes declarations and leaves output that parses but no longer
    // means what it says (#1484). Nothing in a rules-derived profile is precise
    // enough to justify that: `confidence_milli` measures how sure the intent
    // classification is, not how much information loss the output tolerates.
    // The level stays reachable through `apply_triage_filter` for callers that
    // ask for it — where it is additionally markdown-only, so source code and
    // logs never lose non-comment lines — and should return here again only
    // once a model backend can supply that prediction.
    u8::from(profile.context_need_milli < 600)
}

/// Filters output according to the task profile and selected triage level.
pub fn apply_triage_filter(
    output: &str,
    profile: &crate::core::triage::profile::TaskProfileLocal,
    level: u8,
) -> (String, usize) {
    if level == 0 || output.chars().count() < 500 {
        return (output.to_string(), 0);
    }

    // Never filter structured JSON output — the line-based heuristics destroy
    // key-value pairs while keeping only braces, producing invalid JSON.
    let trimmed = output.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return (output.to_string(), 0);
    }

    let lines: Vec<&str> = output.lines().collect();

    // Also bypass when JSON is embedded after a short header (e.g. "ctx_handoff show\n path: …\n{…}")
    if let Some(json_start) = lines.iter().position(|l| {
        let t = l.trim_start();
        t.starts_with('{') || t.starts_with('[')
    }) {
        let json_portion = lines[json_start..].join("\n");
        if serde_json::from_str::<serde_json::Value>(&json_portion).is_ok() {
            return (output.to_string(), 0);
        }
    }
    // #1570 P4: an explicit <protect> span is a user contract — the whole
    // output bypasses lossy line filtering (gated on [protection].tags).
    // Lossless compression (archive digest + ctx_expand) is unaffected:
    // protection means "never lossy", not "never compressed".
    if output.contains("<protect>") && crate::core::config::Config::load().protection.tags {
        return (output.to_string(), 0);
    }
    // Build a set of kept lines for O(1) lookup.
    let keep: std::collections::HashSet<usize> = match level {
        1 => lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !is_boilerplate_line(line.trim_start()))
            .map(|(i, _)| i)
            .collect(),
        2 => {
            let keywords = extract_task_keywords(&profile.task_class, &profile.intent);
            if crate::core::triage::markdown::looks_like_markdown(&lines) {
                crate::core::triage::markdown::keep_indices(&lines, &keywords)
            } else {
                // Community report (3.9.19, "abridged into something that
                // still looks right"): the structural+keyword keep-set is
                // provably unsafe for source code — plain `let` bindings
                // vanished from a Rust fn while the survivors still parsed
                // (21 of 36 lines dropped, output looked complete, bindings
                // became undeclared). Line-level lossy triage is therefore
                // markdown-only; every other content shape gets at most the
                // level-1 boilerplate strip.
                lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| !is_boilerplate_line(line.trim_start()))
                    .map(|(i, _)| i)
                    .collect()
            }
        }
        _ => return (output.to_string(), 0),
    };

    // #1484: assemble output with inline elision markers so omissions are visible.
    let removed = lines.len().saturating_sub(keep.len());
    if removed == 0 {
        return (output.to_string(), 0);
    }
    // #1493: total deletion (100% of lines removed) is never a useful
    // compression outcome — the marker plus retry costs more than passing
    // through the original content. Return unfiltered when nothing survives.
    if keep.is_empty() {
        return (output.to_string(), 0);
    }
    // Heading-only markdown is the same failure mode: ATX titles survive the
    // markdown keep-set so `keep` is non-empty, but every body paragraph is
    // gone. Pass through instead of returning a TOC.
    if crate::core::triage::markdown::is_heading_only_collapse(&lines, &keep) {
        return (output.to_string(), 0);
    }
    let mut result = String::with_capacity(output.len());
    let mut consecutive_omitted: usize = 0;
    for (i, line) in lines.iter().enumerate() {
        if keep.contains(&i) {
            if consecutive_omitted > 0 {
                result.push_str(&format!(
                    "  [...{consecutive_omitted} lines omitted by triage...]\n"
                ));
                consecutive_omitted = 0;
            }
            result.push_str(line);
            result.push('\n');
        } else {
            consecutive_omitted += 1;
        }
    }
    if consecutive_omitted > 0 {
        result.push_str(&format!(
            "  [...{consecutive_omitted} lines omitted by triage...]\n"
        ));
    }
    result.push_str(&format!(
        "[lean-ctx: {removed} lines filtered by triage (level {level}) — rerun with raw=true for the unfiltered output]"
    ));
    (result, removed)
}

fn extract_task_keywords(task_class: &str, intent: &str) -> Vec<String> {
    let mut keywords = task_class
        .split(|character: char| !character.is_alphanumeric())
        .chain(intent.split(|character: char| !character.is_alphanumeric()))
        .filter(|keyword| !keyword.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    keywords.sort_unstable();
    keywords.dedup();
    keywords
}

fn is_boilerplate_line(line: &str) -> bool {
    line.starts_with("//")
        && !line.contains("TODO")
        && !line.contains("FIXME")
        && !line.contains("SAFETY")
}

#[cfg(test)]
#[path = "context_gate_tests.rs"]
mod tests;
