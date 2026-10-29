// SPDX-License-Identifier: Apache-2.0
//! Bounded, project-scoped inputs per op. Every collector reads the bound
//! project's own stores and its own Pro partition; device-wide sources
//! (audit trail, receipts, ledger, host session files) are reduced to counts
//! and narrowed to the project where the source records it.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use super::request::{BridgeRequest, Op};
use super::state::ProState;
use super::wire::{GuardEvent, HeatmapEntry};
use super::{ProResult, ProjectScope, guard, host_usage, ledger, security_fields, snapshots};

/// The engine routes some inputs come from (`/api/knowledge-relations`,
/// `/api/procedures`, …), already bound to the dashboard's project.
pub(crate) trait RouteSource {
    fn get(&self, path: &str) -> Option<Value>;
}

const MAX_EPISODES: usize = 500;
const MAX_HISTORY: usize = 100;
/// Heatmap entries passed, most read first.
const MAX_HEAT_ENTRIES: usize = 2_000;
const MAX_AGENTS: usize = 500;
const MAX_EXISTENCE_CHECKS: usize = 5_000;
const HEATMAP_LIMIT: u64 = 64 << 20;

/// Everything collected for one op.
pub(crate) struct Collected {
    pub(crate) inputs: Map<String, Value>,
}

pub(crate) fn max_facts() -> usize {
    crate::core::config::Config::load()
        .memory_policy_effective()
        .map_or(super::DEFAULT_MAX_FACTS, |p| p.knowledge.max_facts)
}

/// Every record of the project's knowledge store, history included.
pub(crate) fn knowledge_records(
    scope: &ProjectScope,
) -> Vec<crate::core::knowledge::KnowledgeFact> {
    crate::core::knowledge::ProjectKnowledge::load_or_create(&scope.root).facts
}

/// The newest `max_facts` current facts.
fn current_facts(scope: &ProjectScope) -> Value {
    let mut facts: Vec<_> = knowledge_records(scope)
        .into_iter()
        .filter(crate::core::knowledge::KnowledgeFact::is_current)
        .collect();
    let skip = facts.len().saturating_sub(max_facts());
    facts.drain(..skip);
    serde_json::to_value(facts).unwrap_or(Value::Array(Vec::new()))
}

/// Every record, history included (compare and restore count them all; the
/// frame limit bounds the request).
fn all_facts(scope: &ProjectScope) -> Value {
    serde_json::to_value(knowledge_records(scope)).unwrap_or(Value::Array(Vec::new()))
}

fn snapshot_list(scope: &ProjectScope) -> Value {
    serde_json::to_value(snapshots::list(scope, snapshots::LIST_LIMIT)).unwrap_or(Value::Null)
}

/// The guard timeline of the project in scope (its agent sessions), or of
/// the whole trail for the device scope.
fn guard_inputs(scope: &ProjectScope) -> ProResult<(Vec<Value>, Vec<GuardEvent>)> {
    let trail = scope.data_dir.join("audit").join("trail.jsonl");
    if scope.is_device() {
        guard::timeline(&trail)
    } else {
        guard::timeline_for(&trail, Some(&scope.sessions))
    }
}

/// Heatmap entries under the project, keyed by project-relative path.
pub(crate) fn heatmap(scope: &ProjectScope) -> Map<String, Value> {
    let Some(bytes) = super::read_bounded(&scope.state_dir.join("heatmap.json"), HEATMAP_LIMIT)
    else {
        return Map::new();
    };
    let Ok(data) = serde_json::from_slice::<Value>(&bytes) else {
        return Map::new();
    };
    let entries = data
        .get("entries")
        .and_then(Value::as_object)
        .or_else(|| data.as_object())
        .cloned()
        .unwrap_or_default();
    let prefixes = root_prefixes(scope);
    entries
        .into_iter()
        .filter(|(_, entry)| entry.is_object())
        .filter_map(|(path, entry)| {
            let relative = prefixes
                .iter()
                .find_map(|p| path.strip_prefix(p.as_str()))?;
            let entry: HeatmapEntry = serde_json::from_value(entry).ok()?;
            Some((relative.to_string(), serde_json::to_value(entry).ok()?))
        })
        .collect()
}

fn root_prefixes(scope: &ProjectScope) -> Vec<String> {
    let sep = std::path::MAIN_SEPARATOR;
    let mut prefixes = vec![format!("{}{sep}", scope.root.trim_end_matches(sep))];
    let canonical = format!(
        "{}{sep}",
        scope.canonical.to_string_lossy().trim_end_matches(sep)
    );
    if !prefixes.contains(&canonical) {
        prefixes.push(canonical);
    }
    prefixes
}

fn view_costs(scope: &ProjectScope) -> Map<String, Value> {
    let Some(data) =
        super::read_bounded(&scope.state_dir.join("context_ledger.json"), HEATMAP_LIMIT)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return Map::new();
    };
    let prefixes = root_prefixes(scope);
    let mut out = Map::new();
    for entry in data
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let path = entry.get("path").and_then(Value::as_str).unwrap_or("");
        let estimates = entry
            .get("view_costs")
            .and_then(|v| v.get("estimates"))
            .filter(|e| e.as_object().is_some_and(|o| !o.is_empty()));
        if let (Some(relative), Some(estimates)) = (
            prefixes.iter().find_map(|p| path.strip_prefix(p.as_str())),
            estimates,
        ) {
            out.insert(
                relative.to_string(),
                json!({
                    "estimates": estimates,
                    "active": entry.get("active_view").cloned().unwrap_or(Value::Null),
                    "state": entry.get("state").cloned().unwrap_or(Value::Null),
                }),
            );
        }
    }
    out
}

/// Project-relative paths of `candidates` that are regular files of the project.
fn existing(scope: &ProjectScope, candidates: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut found = BTreeSet::new();
    for relative in candidates.into_iter().take(MAX_EXISTENCE_CHECKS) {
        if let Ok(path) = scope.jailed(&relative)
            && path.is_file()
        {
            found.insert(relative);
        }
    }
    found.into_iter().collect()
}

fn adaptive_inputs(
    scope: &ProjectScope,
    routes: &dyn RouteSource,
    state: &ProState,
) -> ProResult<Map<String, Value>> {
    let mut ranked: Vec<(String, Value)> = heatmap(scope).into_iter().collect();
    ranked.sort_by_key(|(_, e)| std::cmp::Reverse(e.get("access_count").and_then(Value::as_i64)));
    ranked.truncate(MAX_HEAT_ENTRIES);
    let heat: Map<String, Value> = ranked.into_iter().collect();
    let paths = existing(scope, heat.keys().cloned());
    let views: Map<String, Value> = view_costs(scope)
        .into_iter()
        .filter(|(path, _)| heat.contains_key(path))
        .collect();
    let price = ledger::recorded_price(scope)?.map(|p| p * 1e6);
    let mut inputs = Map::new();
    inputs.insert("heatmap".into(), Value::Object(heat));
    inputs.insert("view_costs".into(), Value::Object(views));
    inputs.insert(
        "overlay_history".into(),
        routes
            .get("/api/context-overlay-history")
            .unwrap_or(Value::Array(Vec::new())),
    );
    inputs.insert("price_per_m".into(), json!(price));
    inputs.insert(
        "adaptive_state".into(),
        serde_json::to_value(state.adaptive()).unwrap_or(Value::Null),
    );
    inputs.insert("existing_paths".into(), json!(paths));
    Ok(inputs)
}

fn episodes(scope: &ProjectScope) -> Value {
    let root = scope.canonical.clone();
    let hash = scope.partition.clone();
    let loaded =
        crate::core::policy::runtime::with_explicit_project_source_view(&root, |authority| {
            let admission = crate::core::context_admission::stores::StoreAdmission::current();
            let store =
                crate::core::episodic_memory::EpisodicStore::load_admitted(&hash, &admission)?;
            admission.authorize_publication(authority)?;
            let skip = store.episodes.len().saturating_sub(MAX_EPISODES);
            serde_json::to_value(&store.episodes[skip..]).map_err(|e| e.to_string())
        });
    match loaded {
        Ok(Ok(value)) => value,
        _ => Value::Array(Vec::new()),
    }
}

fn agents(scope: &ProjectScope) -> Value {
    let registry: Value = super::read_json(&scope.data_dir.join("agents").join("registry.json"))
        .unwrap_or(Value::Null);
    let list = registry
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Value::Array(
        list.into_iter()
            .take(MAX_AGENTS)
            .map(|a| {
                json!({
                    "agent_id": a.get("agent_id").cloned().unwrap_or(Value::Null),
                    "agent_type": a.get("agent_type").cloned().unwrap_or(Value::Null),
                    "transport": a.get("transport").cloned().unwrap_or(Value::Null),
                })
            })
            .collect(),
    )
}

/// Every project-relative tail of the paths episodes name.
fn episode_paths(scope: &ProjectScope, episodes: &Value) -> Vec<String> {
    let root = scope.canonical.to_string_lossy().into_owned();
    let mut candidates = BTreeSet::new();
    for episode in episodes.as_array().into_iter().flatten() {
        for raw in episode
            .get("affected_files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let raw = raw.strip_prefix(&format!("{root}/")).unwrap_or(raw);
            let parts: Vec<&str> = raw.split('/').filter(|p| !p.is_empty()).collect();
            for start in 0..parts.len() {
                candidates.insert(parts[start..].join("/"));
            }
        }
        if candidates.len() >= MAX_EXISTENCE_CHECKS {
            break;
        }
    }
    existing(scope, candidates)
}

/// The inputs `op` needs. `guard_new` is only set for `alerts.evaluate`.
pub(crate) fn inputs(
    request: &BridgeRequest,
    scope: &ProjectScope,
    routes: &dyn RouteSource,
    now: DateTime<Utc>,
    guard_new: Option<Vec<GuardEvent>>,
) -> ProResult<Collected> {
    let state = ProState::new(scope);
    let mut inputs = Map::new();
    let mut put = |key: &str, value: Value| {
        inputs.insert(key.to_string(), value);
    };
    match request.op {
        Op::SecurityReport => {
            let (days, recent) = guard_inputs(scope)?;
            let since = match request.q("days").parse::<i64>() {
                Ok(days) => now.timestamp() - days * 86_400,
                Err(_) => 0,
            };
            put("guard_days", Value::Array(days));
            put("guard_recent", json!(recent));
            put(
                "egress_rows",
                guard::egress_rows(&scope.data_dir.join("gateway").join("receipts"), since),
            );
            put("alerts_state", state.alerts_state());
            put("profiles", json!(state.profiles()));
        }
        Op::MemoryAnalyze => {
            put("facts", current_facts(scope));
            // Only contradictions are analyzed.
            let edges: Vec<Value> = routes
                .get("/api/knowledge-relations")
                .and_then(|r| r.get("edges").and_then(Value::as_array).cloned())
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.get("kind").and_then(Value::as_str) == Some("contradicts"))
                .collect();
            put("relations", json!({ "edges": edges }));
            put("snapshots", snapshot_list(scope));
            put("max_facts", json!(max_facts()));
        }
        Op::MemoryCompare => {
            let saved = snapshots::read_facts(scope, request.q("id"))?;
            put(
                "snapshot_facts",
                serde_json::to_value(saved).unwrap_or(Value::Null),
            );
            put("facts", all_facts(scope));
            put("max_facts", json!(max_facts()));
        }
        Op::AdaptiveState => {
            for (key, value) in adaptive_inputs(scope, routes, &state)? {
                put(&key, value);
            }
        }
        Op::ReportBuild => {
            let (days, chain) = ledger::report(scope)?;
            let (guard_days, _) = guard_inputs(scope)?;
            put(
                "ledger_days",
                serde_json::to_value(days).unwrap_or(Value::Null),
            );
            put("chain", serde_json::to_value(chain).unwrap_or(Value::Null));
            put("guard_days", Value::Array(guard_days));
            put("roi", routes.get("/api/roi").unwrap_or(Value::Null));
            put("spend", routes.get("/api/spend").unwrap_or(Value::Null));
        }
        Op::LedgerBrowse => {
            let filters = ledger::filters(scope, &request.query)?;
            let offset = request.q("offset").parse().unwrap_or(0);
            let limit = request.q("limit").parse().unwrap_or(50);
            let (page, facets, totals) = ledger::browse(scope, &filters, offset, limit)?;
            put("ledger_page", page);
            put("ledger_facets", facets);
            put("ledger_totals", totals);
        }
        Op::LedgerExport => {}
        Op::SpendTrend => {
            put(
                "proxy_days",
                serde_json::to_value(ledger::proxy_days(scope)?).unwrap_or(Value::Null),
            );
            let roots = vec![
                scope.root.clone(),
                scope.canonical.to_string_lossy().into_owned(),
            ];
            put(
                "host_usage",
                scope.home.as_deref().map_or(Value::Null, |home: &Path| {
                    if scope.is_device() {
                        // Every project's sessions, each under its own root.
                        let files: Vec<_> = host_usage::collect_assigned(
                            home,
                            &scope.projects,
                            now,
                            host_usage::WINDOW_DAYS,
                        )
                        .into_iter()
                        .map(|(index, mut file)| {
                            file.project.clone_from(&scope.projects[index]);
                            file
                        })
                        .collect();
                        json!(files)
                    } else {
                        json!(host_usage::collect(home, &roots, now))
                    }
                }),
            );
        }
        Op::WorkAnalysis => {
            let episodes = episodes(scope);
            let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(&scope.root);
            let skip = knowledge.history.len().saturating_sub(MAX_HISTORY);
            put(
                "knowledge",
                json!({
                    "project_hash": knowledge.project_hash,
                    "facts": current_facts(scope),
                    "history": serde_json::to_value(&knowledge.history[skip..]).unwrap_or(Value::Null),
                }),
            );
            put("project_root", json!(scope.canonical.to_string_lossy()));
            let mut homes: Vec<String> = Vec::new();
            if let Some(home) = scope.home.as_deref() {
                homes.push(home.to_string_lossy().into_owned());
                if let Ok(real) = home.canonicalize() {
                    homes.push(real.to_string_lossy().into_owned());
                }
                homes.dedup();
            }
            put("homes", json!(homes));
            put(
                "utc_offset_seconds",
                json!(chrono::Local::now().offset().local_minus_utc()),
            );
            for (key, route) in [
                ("procedures", "/api/procedures"),
                ("gotchas", "/api/gotchas"),
                ("learning", "/api/learning"),
            ] {
                put(key, routes.get(route).unwrap_or(Value::Null));
            }
            put("existing_paths", json!(episode_paths(scope, &episodes)));
            put("episodes", episodes);
            put("agents", agents(scope));
        }
        Op::AlertsUpdate | Op::AlertsTest | Op::AlertsEvaluate => {
            let (_, recent) = guard_inputs(scope)?;
            put("alerts_state", state.alerts_state());
            put("guard_recent", json!(recent));
            if request.op == Op::AlertsEvaluate {
                put("guard_new", json!(guard_new.unwrap_or_default()));
            }
        }
        Op::Profiles => {
            put("profiles", json!(state.profiles()));
            put(
                "security_config",
                Value::Object(security_fields::security_config()),
            );
        }
        Op::MemorySnapshot | Op::MemoryForget | Op::MemoryRestore => {
            if request.op == Op::MemoryRestore {
                put("facts", all_facts(scope));
            } else {
                put("facts", current_facts(scope));
            }
            put("snapshots", snapshot_list(scope));
            put("max_facts", json!(max_facts()));
            if request.op == Op::MemoryRestore {
                let saved = snapshots::read_facts(scope, request.b_str("id"))?;
                put(
                    "snapshot_facts",
                    serde_json::to_value(saved).unwrap_or(Value::Null),
                );
            }
        }
        Op::MemoryConsolidate => {
            if !request.b_true("run") {
                put("consolidate_dry_run", json!(consolidate_preview(scope)?));
            }
        }
        Op::AdaptiveApply | Op::AdaptiveDismiss => {
            // Without `recommendations` the runtime computes the current ones
            // from the adaptive.state inputs.
            for (key, value) in adaptive_inputs(scope, routes, &state)? {
                put(&key, value);
            }
        }
    }
    Ok(Collected { inputs })
}

/// `knowledge consolidate --dry-run` for the bound project.
pub(crate) fn consolidate_preview(scope: &ProjectScope) -> ProResult<String> {
    use crate::core::consolidation_engine::ConsolidateOptions;
    crate::tools::ctx_knowledge::consolidate_project_knowledge_with(
        &scope.root,
        &ConsolidateOptions::manual().into_dry_run(),
    )
    .map(|report| crate::tools::ctx_knowledge::format_consolidation_report(&report))
    .map_err(|_| super::ProError::internal("Consolidation preview failed"))
}
