// SPDX-License-Identifier: Apache-2.0
//! Plans the runtime answers are checked against the request and the bound
//! project before any of them runs, then executed with the engine's own code
//! paths and read back. Anything unknown, outside the project, or not what
//! the user asked for is a 409.

use std::collections::{BTreeSet, HashMap};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::core::knowledge::{ImportMerge, KnowledgeFact, ProjectKnowledge};

use super::request::{BridgeRequest, Op, is_fact_ref, is_profile_name, is_safe_webhook};
use super::state::ProState;
use super::wire::{AlertRecord, AppliedItem, Baseline, FileOut, Plan, Profile, ProjectColumn};
use super::{
    GUARD_KINDS, ProError, ProResult, ProjectScope, collect, export, ledger, plain_text,
    security_fields, snapshots,
};

/// At most this many plans in one answer.
const MAX_PLANS: usize = 8;
const SNAPSHOT_HINT: &str = "A snapshot was saved first; restore it under Memory Care → Snapshots.";

/// Desktop and webhook delivery of alerts.
pub(crate) trait Notifier: Send + Sync {
    fn desktop(&self, text: &str) -> bool;
    fn webhook(&self, url: &str, body: &[u8]) -> bool;
}

/// The engine's notifier: the native desktop notification and an https
/// POST without redirects and with a timeout.
pub(crate) struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn desktop(&self, text: &str) -> bool {
        crate::core::value::notify::send("LeanCTX", text)
    }

    fn webhook(&self, url: &str, body: &[u8]) -> bool {
        if !is_safe_webhook(url) {
            return false;
        }
        let timeout = Some(std::time::Duration::from_secs(6));
        let agent =
            crate::core::http_client::ureq_agent_without_redirects(timeout, timeout, timeout);
        agent
            .post(url)
            .header("Content-Type", "application/json")
            .send(body)
            .is_ok_and(|r| r.status().is_success())
    }
}

/// What one plan produced.
pub(crate) enum Outcome {
    Applied(Value),
    File(FileOut),
}

pub(crate) struct Ctx<'a> {
    pub(crate) scope: &'a ProjectScope,
    pub(crate) request: &'a BridgeRequest,
    pub(crate) now: DateTime<Utc>,
    pub(crate) notifier: &'a dyn Notifier,
}

/// Typed plans the op allows, or 409.
pub(crate) fn parse_plans(op: Op, raw: Vec<Value>) -> ProResult<Vec<Plan>> {
    if raw.is_empty() || raw.len() > MAX_PLANS {
        return Err(ProError::conflict("Unexpected number of planned changes"));
    }
    raw.into_iter()
        .map(|value| {
            let plan: Plan = serde_json::from_value(value)
                .map_err(|_| ProError::conflict("Unknown planned change"))?;
            if !op.allowed_effects().contains(&plan.effect()) {
                return Err(ProError::conflict(
                    "Planned change does not fit the request",
                ));
            }
            Ok(plan)
        })
        .collect()
}

fn ensure(condition: bool, message: &str) -> ProResult<()> {
    if condition {
        Ok(())
    } else {
        Err(ProError::conflict(message))
    }
}

fn is_time(value: &str) -> bool {
    DateTime::parse_from_rfc3339(value).is_ok()
}

fn fact_ref(fact: &KnowledgeFact) -> String {
    format!("{}/{}", fact.category, fact.key)
}

fn current_map(facts: &[KnowledgeFact]) -> HashMap<String, KnowledgeFact> {
    facts
        .iter()
        .filter(|f| f.is_current())
        .map(|f| (fact_ref(f), f.clone()))
        .collect()
}

/// A restore against the live store: what comes back, what is reset.
pub(crate) struct RestorePlan {
    snapshot: HashMap<String, KnowledgeFact>,
    now: HashMap<String, KnowledgeFact>,
    pub(crate) missing: Vec<String>,
    pub(crate) changed: Vec<String>,
}

pub(crate) fn restore_plan(scope: &ProjectScope, id: &str) -> ProResult<RestorePlan> {
    let snapshot = current_map(&snapshots::read_facts(scope, id)?);
    let records = collect::knowledge_records(scope);
    let now = current_map(&records);
    let mut missing: Vec<String> = snapshot
        .keys()
        .filter(|r| !now.contains_key(*r))
        .cloned()
        .collect();
    let mut changed: Vec<String> = snapshot
        .iter()
        .filter(|(r, f)| now.get(*r).is_some_and(|n| n.value != f.value))
        .map(|(r, _)| r.clone())
        .collect();
    missing.sort();
    changed.sort();
    // No room check: knowledge has no fact limit, so a restore never pushes
    // other facts out.
    Ok(RestorePlan {
        snapshot,
        now,
        missing,
        changed,
    })
}

fn same_set(a: &[String], b: &[String]) -> bool {
    a.iter().collect::<BTreeSet<_>>() == b.iter().collect::<BTreeSet<_>>()
}

fn requested_profile(ctx: &Ctx<'_>) -> Option<Profile> {
    let name = ctx.request.b_str("name");
    ProState::new(ctx.scope)
        .profiles()
        .into_iter()
        .find(|p| p.name == name)
}

/// Checks one plan against the request and the project, without effects.
pub(crate) fn validate(ctx: &Ctx<'_>, plan: &Plan) -> ProResult<()> {
    let request = ctx.request;
    let state = ProState::new(ctx.scope);
    match plan {
        Plan::AlertsSettings { settings } => {
            let stored = state.alert_settings();
            let wanted = |key: &str| request.body.get(key);
            let kinds: Vec<Value> = settings.kinds.iter().map(|k| json!(k)).collect();
            ensure(
                settings
                    .kinds
                    .iter()
                    .all(|k| GUARD_KINDS.contains(&k.as_str()))
                    && settings.kinds.iter().collect::<BTreeSet<_>>().len() == settings.kinds.len(),
                "Alert kinds outside the guard list",
            )?;
            ensure(
                settings.webhook.is_empty() || is_safe_webhook(&settings.webhook),
                "Unsafe webhook",
            )?;
            let matches = |key: &str, planned: Value, stored: Value| {
                wanted(key).map_or(planned == stored, |w| *w == planned)
            };
            ensure(
                matches("enabled", json!(settings.enabled), json!(stored.enabled))
                    && matches("desktop", json!(settings.desktop), json!(stored.desktop))
                    && matches("webhook", json!(settings.webhook), json!(stored.webhook))
                    && matches("kinds", Value::Array(kinds), json!(stored.kinds)),
                "Alert settings differ from the request",
            )
        }
        Plan::AlertsNotify {
            at,
            text,
            counts,
            test,
            desktop,
            webhook,
        } => {
            let stored = state.alert_settings();
            ensure(*test == (request.op == Op::AlertsTest), "Unexpected alert")?;
            ensure(*test || stored.enabled, "Alerts are off")?;
            ensure(is_time(at) && plain_text(text, 240), "Invalid alert text")?;
            ensure(
                counts.iter().all(|(k, n)| {
                    GUARD_KINDS.contains(&k.as_str()) && (0..=1_000_000_000).contains(n)
                }),
                "Alert counts outside the guard list",
            )?;
            ensure(
                *desktop == stored.desktop && *webhook == stored.webhook,
                "Alert channels differ from the settings",
            )
        }
        Plan::ProfilesPut {
            name,
            values,
            saved_at,
        } => {
            ensure(
                request.b_str("action") == "save" && name == request.b_str("name"),
                "Profile differs from the request",
            )?;
            ensure(
                is_profile_name(name) && is_time(saved_at),
                "Invalid profile",
            )?;
            for (key, value) in values {
                let field = security_fields::lookup(key)
                    .ok_or_else(|| ProError::conflict("Profile holds an unmanaged setting"))?;
                security_fields::wire_of(field, value)?;
            }
            Ok(())
        }
        Plan::ProfilesDelete { name } => ensure(
            request.b_str("action") == "delete" && name == request.b_str("name"),
            "Profile differs from the request",
        ),
        Plan::ConfigSet {
            key,
            wire,
            expected,
            weakening,
        } => {
            ensure(
                request.b_str("action") == "apply",
                "Unexpected setting change",
            )?;
            let field = security_fields::lookup(key)
                .ok_or_else(|| ProError::conflict("This setting is not managed here"))?;
            ensure(
                security_fields::wire_of(field, expected)? == *wire
                    && security_fields::expected_of(field, wire) == *expected,
                "Setting value does not match its wire form",
            )?;
            let profile =
                requested_profile(ctx).ok_or_else(|| ProError::not_found("Unknown profile"))?;
            let stored = profile
                .values
                .get(key)
                .map(|v| security_fields::wire_of(field, v))
                .transpose()?;
            ensure(
                stored.as_deref() == Some(wire.as_str()),
                "Setting is not part of the profile",
            )?;
            let before = security_fields::effective(key).unwrap_or_default();
            let lowers = security_fields::weakening(field, &before, wire);
            if lowers && !request.b_true("confirm_weaker") {
                return Err(ProError::conflict(
                    "This lowers protection. Confirm the change to continue.",
                ));
            }
            ensure(lowers == *weakening, "Protection change misreported")
        }
        Plan::MemorySnapshot { reason } => ensure(
            reason == request.b_str("reason"),
            "Snapshot differs from the request",
        ),
        Plan::MemoryRestore {
            id,
            missing,
            changed,
        } => {
            ensure(
                id == request.b_str("id"),
                "Snapshot differs from the request",
            )?;
            let plan = restore_plan(ctx.scope, id)?;
            ensure(
                same_set(missing, &plan.missing) && same_set(changed, &plan.changed),
                "Memory changed; compare the snapshot again",
            )
        }
        Plan::MemoryForget { refs, reason } => {
            let asked: Vec<String> = request
                .body
                .get("refs")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            ensure(
                same_set(refs, &asked) && reason == request.b_str("reason"),
                "Cleanup differs from the request",
            )?;
            ensure(refs.iter().all(|r| is_fact_ref(r)), "Invalid insight key")?;
            let current = current_map(&collect::knowledge_records(ctx.scope));
            ensure(
                refs.iter().all(|r| current.contains_key(r)),
                "Memory changed; refresh before cleaning up",
            )
        }
        Plan::MemoryConsolidate { run } => ensure(
            *run && request.b_true("run"),
            "Consolidation differs from the request",
        ),
        Plan::ContextOverlay { action, path } => {
            ensure(
                action == request.b_str("action") && path == request.b_str("path"),
                "Rule differs from the request",
            )?;
            ensure(
                ctx.scope.jailed(path)?.is_file(),
                "The path is not a file of this project",
            )?;
            ensure(
                collect::heatmap(ctx.scope).contains_key(path),
                "The path is not a current recommendation",
            )
        }
        Plan::AdaptiveRecord { item } => ensure(
            item.path == request.b_str("path") && item.action == request.b_str("action"),
            "Record differs from the request",
        ),
        Plan::AdaptiveDismiss { path } => {
            ensure(
                path == request.b_str("path"),
                "Dismissal differs from the request",
            )?;
            ctx.scope.jailed(path).map(|_| ())
        }
        Plan::LedgerExport {
            format,
            file_name,
            mime,
            filters,
            project_column,
        } => {
            ensure(format == request.q("format"), "Export format differs")?;
            ensure(
                *file_name == export::file_name(ctx.now.date_naive(), format)
                    && mime == export::mime_of(format),
                "Export file differs",
            )?;
            let mine = ledger::filters(ctx.scope, &ctx.request.query)?;
            let normalize = |f: &super::wire::LedgerFilters| {
                let mut f = f.clone();
                if let Some(scope) = f.scope.as_mut() {
                    scope.sort();
                    scope.dedup();
                }
                f
            };
            ensure(
                normalize(filters) == normalize(&mine),
                "Export filters differ",
            )?;
            let labels = ctx.scope.labels();
            ensure(
                match project_column {
                    ProjectColumn::Fixed { name } => *name == ctx.scope.label,
                    ProjectColumn::Labels { labels: planned } => {
                        planned.iter().all(|l| labels.contains(l))
                    }
                },
                "Export project column outside this project",
            )
        }
    }
}

fn snapshot_now(ctx: &Ctx<'_>, reason: &str) -> ProResult<super::wire::SnapshotMeta> {
    let knowledge = ProjectKnowledge::load_or_create(&ctx.scope.root);
    snapshots::create(ctx.scope, &knowledge, reason, ctx.now)
}

fn policy() -> ProResult<crate::core::memory_policy::MemoryPolicy> {
    crate::core::config::Config::load()
        .memory_policy_effective()
        .map_err(|_| ProError::internal("Invalid memory policy"))
}

fn import(ctx: &Ctx<'_>, facts: Vec<KnowledgeFact>, merge: ImportMerge) -> ProResult<()> {
    if facts.is_empty() {
        return Ok(());
    }
    ProjectKnowledge::mutate_locked(&ctx.scope.root, |k| {
        k.import_facts(facts, merge, "dashboard-pro");
    })
    .map(|_| ())
    .map_err(|_| ProError::internal(format!("The restore was not saved. {SNAPSHOT_HINT}")))
}

/// Executes one validated plan and reads it back.
pub(crate) fn execute(ctx: &Ctx<'_>, plan: Plan) -> ProResult<Outcome> {
    let state = ProState::new(ctx.scope);
    let effect = plan.effect();
    let applied = |readback: Value| {
        Ok(Outcome::Applied(
            json!({ "effect": effect, "readback": readback }),
        ))
    };
    match plan {
        Plan::AlertsSettings { settings } => {
            state.put_alert_settings(&settings)?;
            let back = state.alert_settings();
            ensure(back == settings, "Alert settings did not read back")?;
            applied(json!({ "settings": back }))
        }
        Plan::AlertsNotify {
            at,
            text,
            counts,
            test,
            desktop,
            webhook,
        } => {
            let mut channels = Vec::new();
            if desktop {
                channels.push(
                    if ctx.notifier.desktop(&text) {
                        "desktop"
                    } else {
                        "desktop failed"
                    }
                    .to_string(),
                );
            }
            if !webhook.is_empty() {
                // Counts only: never content, paths or project names.
                let body = json!({ "source": "leanctx", "test": test, "counts": counts, "at": at });
                let sent = ctx.notifier.webhook(&webhook, body.to_string().as_bytes());
                channels.push(if sent { "webhook" } else { "webhook failed" }.to_string());
            }
            let record = AlertRecord {
                at,
                text,
                channels,
                test,
            };
            state.append_alert(record.clone())?;
            ensure(
                state.alerts_file().history.last() == Some(&record),
                "Alert history did not read back",
            )?;
            applied(json!({ "alert": record }))
        }
        Plan::ProfilesPut {
            name,
            values,
            saved_at,
        } => {
            let profile = Profile {
                name: name.clone(),
                values,
                saved_at,
            };
            state.put_profile(profile.clone())?;
            ensure(
                state.profiles().contains(&profile),
                "Profile did not read back",
            )?;
            applied(json!({ "profiles": state.profiles() }))
        }
        Plan::ProfilesDelete { name } => {
            state.delete_profile(&name)?;
            ensure(
                !state.profiles().iter().any(|p| p.name == name),
                "Profile was not deleted",
            )?;
            applied(json!({ "profiles": state.profiles() }))
        }
        Plan::ConfigSet {
            key,
            wire,
            expected,
            weakening,
        } => {
            crate::core::config::setter::set_by_key(&key, &wire)
                .map_err(|e| ProError::conflict(format!("The setting was refused: {e}")))?;
            let field = security_fields::lookup(&key)
                .ok_or_else(|| ProError::conflict("This setting is not managed here"))?;
            let after =
                security_fields::effective(&key).map(|w| security_fields::expected_of(field, &w));
            ensure(
                after.as_ref() == Some(&expected),
                "The Engine did not report the new value. A setting in this project's own configuration may take precedence over the device setting.",
            )?;
            applied(json!({ "key": key, "value": expected, "weakened": weakening }))
        }
        Plan::MemorySnapshot { reason } => {
            let meta = snapshot_now(ctx, &reason)?;
            ensure(
                snapshots::list(ctx.scope, snapshots::LIST_LIMIT)
                    .iter()
                    .any(|m| m.id == meta.id),
                "Snapshot did not read back",
            )?;
            applied(json!({ "snapshot": meta }))
        }
        Plan::MemoryRestore { id, .. } => {
            let plan = restore_plan(ctx.scope, &id)?;
            let safety = snapshot_now(ctx, "Before restoring a snapshot")?;
            let pick = |refs: &[String]| -> Vec<KnowledgeFact> {
                refs.iter()
                    .filter_map(|r| plan.snapshot.get(r).cloned())
                    .collect()
            };
            import(ctx, pick(&plan.missing), ImportMerge::SkipExisting)?;
            import(ctx, pick(&plan.changed), ImportMerge::Replace)?;
            let after = current_map(&collect::knowledge_records(ctx.scope));
            let lost = plan.now.keys().any(|r| !after.contains_key(r));
            let differs = plan
                .snapshot
                .iter()
                .any(|(r, f)| after.get(r).is_none_or(|a| a.value != f.value));
            if lost || differs {
                return Err(ProError::conflict(format!(
                    "The restore did not read back completely. {SNAPSHOT_HINT}"
                )));
            }
            applied(json!({
                "restored": plan.missing.len(),
                "updated": plan.changed.len(),
                "insights": after.len(),
                "safety_snapshot": safety.id,
            }))
        }
        Plan::MemoryForget { refs, reason } => {
            let safety = snapshot_now(ctx, &reason)?;
            let policy = policy()?;
            let pairs: Vec<(String, String)> = refs
                .iter()
                .filter_map(|r| r.split_once('/'))
                .map(|(c, k)| (c.to_string(), k.to_string()))
                .collect();
            ProjectKnowledge::mutate_locked(&ctx.scope.root, |k| {
                let mut removed = false;
                for (category, key) in &pairs {
                    removed |= k.remove_fact(category, key);
                }
                if removed {
                    let _ = k.run_memory_lifecycle(&policy);
                }
            })
            .map_err(|_| {
                ProError::internal(format!("The removal was not saved. {SNAPSHOT_HINT}"))
            })?;
            let left = current_map(&collect::knowledge_records(ctx.scope));
            if refs.iter().any(|r| left.contains_key(r)) {
                return Err(ProError::conflict(format!(
                    "Some insights were not removed. {SNAPSHOT_HINT}"
                )));
            }
            applied(json!({ "removed": refs.len(), "safety_snapshot": safety.id }))
        }
        Plan::MemoryConsolidate { .. } => {
            let safety = snapshot_now(ctx, "Before consolidation")?;
            let report = crate::tools::ctx_knowledge::consolidate_project_knowledge_with(
                &ctx.scope.root,
                &crate::core::consolidation_engine::ConsolidateOptions::manual(),
            )
            .map_err(|_| ProError::internal(format!("Consolidation failed. {SNAPSHOT_HINT}")))?;
            let text = crate::tools::ctx_knowledge::format_consolidation_report(&report);
            applied(json!({
                "summary": consolidation_summary(&text),
                "safety_snapshot": safety.id,
            }))
        }
        Plan::ContextOverlay { action, path } => {
            use crate::core::context_overlay::{
                ContextOverlay, OverlayAuthor, OverlayOp, OverlayScope, OverlayStore,
            };
            let root = std::path::PathBuf::from(&ctx.scope.root);
            let target = crate::core::context_field::ContextItemId::from_file(&path);
            let operation = if action == "compress" {
                OverlayOp::SetView(crate::core::context_field::ViewKind::Map)
            } else {
                OverlayOp::Exclude {
                    reason: "adaptive recommendation".into(),
                }
            };
            let mut store = OverlayStore::load_project(&root);
            store.add(ContextOverlay::new(
                target.clone(),
                operation.clone(),
                OverlayScope::Project,
                String::new(),
                OverlayAuthor::User,
            ));
            store
                .save_project(&root)
                .map_err(|_| ProError::internal("The rule was not saved"))?;
            let back = OverlayStore::load_project(&root);
            ensure(
                back.all()
                    .iter()
                    .any(|o| o.target == target && o.operation == operation),
                "The rule did not read back",
            )?;
            applied(json!({ "action": action, "path": path }))
        }
        Plan::AdaptiveRecord { item } => {
            let heat: super::wire::HeatmapEntry = collect::heatmap(ctx.scope)
                .remove(&item.path)
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let original = heat.total_original_tokens.unwrap_or(0);
            let saved = heat.total_tokens_saved.unwrap_or(0);
            let record = AppliedItem {
                path: item.path,
                action: item.action,
                applied_at: ctx.now.to_rfc3339(),
                baseline: Baseline {
                    reads: heat.access_count.unwrap_or(0),
                    original,
                    saved,
                    ratio: (original > 0)
                        .then(|| ((saved as f64 / original as f64) * 1000.0).round() / 1000.0),
                },
            };
            let mut adaptive = state.adaptive();
            adaptive.applied.push(record.clone());
            state.put_adaptive(adaptive)?;
            ensure(
                state.adaptive().applied.last() == Some(&record),
                "The record did not read back",
            )?;
            applied(json!({ "item": record }))
        }
        Plan::AdaptiveDismiss { path } => {
            let mut adaptive = state.adaptive();
            if !adaptive.dismissed.contains(&path) {
                adaptive.dismissed.push(path.clone());
            }
            // A dismissed file is no longer an applied map-view recommendation.
            adaptive
                .applied
                .retain(|item| !(item.path == path && item.action == "compress"));
            state.put_adaptive(adaptive)?;
            let back = state.adaptive();
            ensure(
                back.dismissed.contains(&path)
                    && !back
                        .applied
                        .iter()
                        .any(|item| item.path == path && item.action == "compress"),
                "The dismissal did not read back",
            )?;
            applied(json!({ "path": path }))
        }
        Plan::LedgerExport {
            format,
            file_name,
            mime,
            filters,
            project_column,
        } => {
            let bytes = export::build(ctx.scope, &filters, &format, &project_column)?;
            Ok(Outcome::File(FileOut {
                name: file_name,
                mime,
                bytes_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
            }))
        }
    }
}

fn plural(count: i64, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

/// Plain-English summary of a real consolidation run's report.
pub(crate) fn consolidation_summary(output: &str) -> Value {
    let number = |pattern: &str| -> i64 {
        regex::Regex::new(pattern)
            .ok()
            .and_then(|re| re.captures(output))
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0)
    };
    let merged = number(r"consolidated (\d+)");
    let archived = number(r"archived-to-target (\d+)")
        .max(number(r"Lifecycle:[^\n]*?archived (\d+)"))
        + number(r"Patterns:[^\n]*?compacted (\d+)")
        + number(r"History:[^\n]*?compacted (\d+)");
    let kept = match number(r"remaining (\d+)") {
        0 => number(r"Facts: (\d+) active"),
        n => n,
    };
    let session = number(r"Session import: \S+ \((\d+) item");
    let mut lines = Vec::new();
    if merged > 0 {
        lines.push(format!(
            "{} were merged into one.",
            plural(merged, "similar insight")
        ));
    }
    if archived > 0 {
        lines.push(format!(
            "{} were archived; archived items can be restored.",
            plural(archived, "item")
        ));
    }
    if session > 0 {
        lines.push(format!(
            "{} from the current session were added.",
            plural(session, "item")
        ));
    }
    if lines.is_empty() {
        lines.push("Nothing needed consolidating.".into());
    }
    lines.push(format!("{} stayed in memory.", plural(kept, "insight")));
    json!({ "merged": merged, "archived": archived, "kept": kept, "preview": false, "lines": lines })
}

/// Checks every plan first, then runs them in order.
pub(crate) fn apply(ctx: &Ctx<'_>, plans: Vec<Plan>) -> ProResult<Vec<Outcome>> {
    for plan in &plans {
        validate(ctx, plan)?;
    }
    plans.into_iter().map(|plan| execute(ctx, plan)).collect()
}
