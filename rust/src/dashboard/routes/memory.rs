use chrono::Utc;
use serde::Deserialize;
use std::path::Path;

use super::helpers::{detect_project_root_for_dashboard, extract_query_param, json_err, json_ok};

pub(super) fn handle(
    path: &str,
    query_str: &str,
    method: &str,
    body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/session/note" if method.eq_ignore_ascii_case("POST") => Some(post_session_note(body)),
        "/api/episodes/annotate" if method.eq_ignore_ascii_case("POST") => {
            Some(post_episodes_annotate(body))
        }
        _ => get_routes(path, query_str),
    }
}

fn with_dashboard_project<T>(
    root: &str,
    operation: impl FnOnce(
        &str,
        &crate::core::context_admission::stores::StoreAdmission,
        &crate::core::policy::runtime::PublicationAuthority,
    ) -> Result<T, String>,
) -> Result<T, String> {
    let root_path = Path::new(root);
    if crate::core::pathutil::is_broad_or_unsafe_root(root_path) {
        return Err("dashboard project scope unavailable".into());
    }
    let canonical_root = root_path
        .canonicalize()
        .map_err(|_| "dashboard project scope unavailable".to_string())?;
    let project_hash = crate::core::project_hash::hash_project_root(root);
    let admission = crate::core::context_admission::stores::StoreAdmission::current();

    crate::core::policy::runtime::with_explicit_project_source_view(&canonical_root, |authority| {
        let result = operation(&project_hash, &admission, authority)?;
        admission.authorize_publication(authority)?;
        Ok(result)
    })?
}

fn admitted_dashboard_text(text: &str) -> Result<String, String> {
    let result = crate::core::context_admission::admit_tool_output(
        text,
        crate::core::context_admission::OutputCompleteness::complete(),
    );
    result
        .text
        .ok_or_else(|| "dashboard content withheld by current policy".to_string())
}

fn admitted_dashboard_json(value: &serde_json::Value) -> Result<String, String> {
    let raw =
        serde_json::to_string(value).map_err(|_| "dashboard response unavailable".to_string())?;
    let admitted = admitted_dashboard_text(&raw)?;
    if admitted != raw {
        return Err("dashboard response would require policy redaction".into());
    }
    Ok(raw)
}

fn dashboard_json_response(
    response: Result<String, String>,
) -> (&'static str, &'static str, String) {
    match response {
        Ok(json) => ("200 OK", "application/json", json),
        Err(_) => (
            "503 Service Unavailable",
            "application/json",
            json_err("dashboard memory unavailable under current policy"),
        ),
    }
}

fn episodes_json(root: &str) -> Result<String, String> {
    with_dashboard_project(root, |hash, admission, _authority| {
        let store = crate::core::episodic_memory::EpisodicStore::load_admitted(hash, admission)?;
        let stats = store.stats();
        let recent: Vec<_> = store.recent(20).into_iter().cloned().collect();
        admitted_dashboard_json(&serde_json::json!({
            "project_root": root,
            "project_hash": hash,
            "stats": {
                "total_episodes": stats.total_episodes,
                "successes": stats.successes,
                "failures": stats.failures,
                "success_rate": stats.success_rate,
                "total_tokens": stats.total_tokens,
            },
            "recent": recent,
        }))
    })
}

fn procedures_json(root: &str, query_str: &str) -> Result<String, String> {
    with_dashboard_project(root, |hash, admission, _authority| {
        let store =
            crate::core::procedural_memory::ProceduralStore::load_admitted(hash, admission)?;
        let raw_task = extract_query_param(query_str, "task").or_else(|| {
            crate::core::session::SessionState::load_latest_for_project_root(root)
                .and_then(|session| session.task.map(|task| task.description))
        });
        let task = raw_task
            .map(|task| admitted_dashboard_text(&task))
            .transpose()?;
        let suggestions: Vec<serde_json::Value> = task.as_deref().map_or(Vec::new(), |task| {
            store
                .suggest(task)
                .into_iter()
                .take(10)
                .map(|procedure| {
                    serde_json::json!({
                        "id": procedure.id,
                        "name": procedure.name,
                        "description": procedure.description,
                        "confidence": procedure.confidence,
                        "times_used": procedure.times_used,
                        "times_succeeded": procedure.times_succeeded,
                        "success_rate": procedure.success_rate(),
                        "steps": procedure.steps,
                        "activation_keywords": procedure.activation_keywords,
                        "last_used": procedure.last_used,
                        "created_at": procedure.created_at,
                    })
                })
                .collect()
        });
        admitted_dashboard_json(&serde_json::json!({
            "project_root": root,
            "project_hash": hash,
            "total_procedures": store.procedures.len(),
            "task": task,
            "suggestions": suggestions,
            "procedures": store.procedures,
        }))
    })
}

fn intent_json(root: &str) -> Result<String, String> {
    with_dashboard_project(root, |_hash, _admission, _authority| {
        let session = crate::core::session::SessionState::load_latest_for_project_root(root)
            .map(|session| crate::core::session::admission::for_delivery(&session))
            .transpose()?
            .filter(|session| session.active_structured_intent.is_some());
        let payload = match session {
            Some(session) => serde_json::json!({
                "active": true,
                "intent": session.active_structured_intent,
            }),
            None => serde_json::json!({ "active": false }),
        };
        admitted_dashboard_json(&payload)
    })
}

/// #1284: today's (UTC) totals from metering.jsonl. Unlike the savings ledger,
/// metering records EVERY ctx_* call including zero-saving ones, so this is
/// the honest denominator. Only today's tail of the file is read; the result is
/// cached on (file length, day) because the Live view polls every few seconds.
/// Today's metered totals: the device's, or for a named project its share.
fn today_metering_summary(
    scope: Option<&crate::core::savings_ledger::ProjectScope>,
) -> crate::core::metering::DayTotals {
    use crate::core::metering::{DayTotals, MeterStore};
    use std::sync::Mutex;
    static CACHE: Mutex<Option<(u64, chrono::NaiveDate, DayTotals)>> = Mutex::new(None);

    if let Some(scope) = scope {
        let Ok(store) = MeterStore::from_data_dir() else {
            return DayTotals::default();
        };
        let keys = scope.repo_hashes();
        return store.day_totals_where(Utc::now().date_naive(), |key| {
            key.is_some_and(|key| keys.iter().any(|k| k == key))
        });
    }

    let Ok(store) = MeterStore::from_data_dir() else {
        return DayTotals::default();
    };
    let len = std::fs::metadata(store.path())
        .map(|m| m.len())
        .unwrap_or(0);
    let today = Utc::now().date_naive();
    if let Ok(guard) = CACHE.lock()
        && let Some((cached_len, cached_day, totals)) = *guard
        && cached_len == len
        && cached_day == today
    {
        return totals;
    }
    let totals = store.day_totals(today);
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((len, today, totals));
    }
    totals
}

fn get_routes(path: &str, query_str: &str) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/episodes" => {
            let root = detect_project_root_for_dashboard();
            Some(dashboard_json_response(episodes_json(&root)))
        }
        "/api/procedures" => {
            let root = detect_project_root_for_dashboard();
            Some(dashboard_json_response(procedures_json(&root, query_str)))
        }
        "/api/session" => {
            // The status bar promises "the task your most recent agent session
            // is working on". `load_latest()` matches sessions against THIS
            // process's cwd — but the dashboard usually runs from HOME (a
            // broad root that rightly matches nothing), so it permanently
            // showed "No session" while agents were active. Fall back to the
            // most recently updated session rooted in a real project
            // (skipping broad/unsafe roots like HOME or test-sandbox temp dirs).
            // A read for a named project (`X-LeanCTX-Project`) never falls
            // back to another project's session.
            let named_project = matches!(
                super::helpers::dashboard_project_binding().1,
                super::helpers::DashboardProjectSource::Request
            );
            let mut session = crate::core::session::SessionState::load_latest()
                .or_else(|| {
                    if named_project {
                        return None;
                    }
                    crate::core::session::SessionState::list_sessions()
                        .iter()
                        .find_map(|summary| {
                            let sess = crate::core::session::SessionState::load_by_id(&summary.id)?;
                            let root = sess.project_root.as_deref()?;
                            if crate::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(
                                root,
                            )) {
                                return None;
                            }
                            Some(sess)
                        })
                })
                .unwrap_or_default();

            // Capture session-only stats BEFORE global enrichment.
            let session_stats =
                serde_json::to_value(&session.stats).unwrap_or_else(|_| serde_json::json!({}));

            // Today's verified savings from the savings ledger (written by all
            // processes — MCP server, proxy, CLI). This is the source of truth;
            // for a named project, only its events.
            let summary = crate::core::savings_ledger::summary_for(
                super::helpers::request_ledger_scope().as_ref(),
            );
            let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let (today_saved, today_usd, today_baseline) = summary
                .by_day
                .iter()
                .find(|(d, ..)| d == &today)
                .map(|(_, s, u, b)| (*s, *u, *b))
                .unwrap_or((0, 0.0, 0));
            // #1284: the ledger only records calls that saved something, so its
            // percentage is "of compression events", not "of everything the
            // agent did". Ship the metering view (ALL metered ctx_* calls,
            // zero-saving ones included) and the native-passthrough count next
            // to it so the panel can label the denominator honestly.
            let metered = today_metering_summary(super::helpers::request_ledger_scope().as_ref());
            let (metered_input, metered_saved) = (metered.input_tokens, metered.savings_tokens);
            let reach =
                crate::core::gain::reach::reach_view(crate::core::gain::reach::ReachCounts {
                    routed_calls: metered.routed_calls,
                    native_passthrough_calls: metered.native_passthrough_calls,
                });
            let compression_session = serde_json::json!({
                "savings_tokens": today_saved,
                "total_raw": today_baseline,
                "total_compressed": today_baseline.saturating_sub(today_saved),
                "savings_percent": if today_baseline > 0 {
                    today_saved as f64 * 100.0 / today_baseline as f64
                } else { 0.0 },
                "savings_usd": today_usd,
                // #1284 honesty fields (today, UTC):
                "scope": "savings ledger — compression events only; metered_* covers every metered ctx_* call incl. zero-saving ones",
                "metered_input_tokens": metered_input,
                "metered_saved_tokens": metered_saved,
                "savings_percent_of_metered": if metered_input > 0 {
                    metered_saved as f64 * 100.0 / metered_input as f64
                } else { 0.0 },
                "native_passthrough_calls": metered.native_passthrough_calls,
                "reach": reach,
            });

            // A named project's session is enriched with that project's own
            // counters, never the device's.
            let global = match super::helpers::request_ledger_scope() {
                Some(scope) if named_project => crate::core::stats::load_for_project(&scope),
                _ => crate::core::stats::load_for_display(),
            };
            let g_cmds = global.total_commands;
            let g_input = global.total_input_tokens;
            let g_output = global.total_output_tokens;
            let g_saved = g_input.saturating_sub(g_output);
            if g_cmds > session.stats.total_tool_calls as u64 {
                session.stats.total_tool_calls = g_cmds as u32;
            }
            if g_saved > session.stats.total_tokens_saved {
                session.stats.total_tokens_saved = g_saved;
            }
            if g_input > session.stats.total_tokens_input {
                session.stats.total_tokens_input = g_input;
            }
            if let Some(lu) = &global.last_use
                && let Ok(ts) = chrono::DateTime::parse_from_rfc3339(lu)
            {
                let utc = ts.with_timezone(&chrono::Utc);
                if utc > session.updated_at {
                    session.updated_at = utc;
                }
            }
            let mut payload =
                serde_json::to_value(&session).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(object) = payload.as_object_mut() {
                object.insert("session_stats".to_string(), session_stats);
                object.insert("compression_session".to_string(), compression_session);
            }
            let json = serde_json::to_string(&payload)
                .unwrap_or_else(|_| "{\"error\":\"failed to serialize session\"}".to_string());
            Some(("200 OK", "application/json", json))
        }
        // Multi-window visibility (GH #694): every workspace that has a
        // session, newest first, so users with several IDE windows can see
        // which projects lean-ctx serves and how fresh each one is.
        "/api/workspaces" => {
            let last_use = crate::core::stats::load_for_display()
                .last_use
                .as_deref()
                .and_then(|lu| chrono::DateTime::parse_from_rfc3339(lu).ok())
                .map(|ts| ts.with_timezone(&Utc));
            let workspaces = collect_workspaces(
                Utc::now(),
                crate::core::session::SessionState::list_sessions(),
                last_use,
            );
            let json = serde_json::to_string(&serde_json::json!({ "workspaces": workspaces }))
                .unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/memory" => {
            let snap = crate::core::memory_guard::MemorySnapshot::capture();
            let allocator = if cfg!(all(
                feature = "jemalloc",
                not(windows),
                not(target_env = "musl")
            )) {
                "jemalloc (dirty_decay: 1s)"
            } else {
                "system"
            };
            let payload = if let Some(s) = snap {
                serde_json::json!({
                    "rss_bytes": s.rss_bytes,
                    "rss_mb": format!("{:.1}", s.rss_bytes as f64 / 1_048_576.0),
                    "peak_rss_bytes": s.peak_rss_bytes,
                    "peak_rss_mb": format!("{:.1}", s.peak_rss_bytes as f64 / 1_048_576.0),
                    "system_ram_bytes": s.system_ram_bytes,
                    "system_ram_gb": format!("{:.1}", s.system_ram_bytes as f64 / 1_073_741_824.0),
                    "rss_limit_bytes": s.rss_limit_bytes,
                    "rss_limit_mb": format!("{:.1}", s.rss_limit_bytes as f64 / 1_048_576.0),
                    "rss_percent": format!("{:.2}", s.rss_percent),
                    "pressure_level": s.pressure_level,
                    "allocator": allocator,
                    "max_sessions": crate::core::context_os::SharedSessionStore::max_sessions(),
                })
            } else {
                serde_json::json!({
                    "available": false,
                    "reason": "RSS monitoring not supported on this platform",
                    "allocator": allocator,
                })
            };
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/intent" => {
            let root = detect_project_root_for_dashboard();
            Some(dashboard_json_response(intent_json(&root)))
        }
        _ => None,
    }
}

#[derive(Deserialize)]
struct SessionNoteReq {
    note: String,
}

fn post_session_note(body: &str) -> (&'static str, &'static str, String) {
    let req: SessionNoteReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!("invalid JSON: {e}")),
            );
        }
    };
    let note = req.note.trim();
    if note.is_empty() {
        return (
            "400 Bad Request",
            "application/json",
            json_err("note must not be empty"),
        );
    }
    let Some(mut session) = crate::core::session::SessionState::load_latest() else {
        return (
            "400 Bad Request",
            "application/json",
            json_err("no session to attach note to"),
        );
    };
    let now = Utc::now();
    session.updated_at = now;
    session.progress.push(crate::core::session::ProgressEntry {
        action: "note".to_string(),
        detail: Some(note.to_string()),
        timestamp: now,
    });
    if let Err(e) = session.save() {
        return (
            "500 Internal Server Error",
            "application/json",
            json_err(&e),
        );
    }
    ("200 OK", "application/json", json_ok())
}

#[derive(Deserialize)]
struct EpisodeAnnotateReq {
    episode_index: usize,
    outcome: String,
}

fn post_episodes_annotate(body: &str) -> (&'static str, &'static str, String) {
    let req: EpisodeAnnotateReq = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(error) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!("invalid JSON: {error}")),
            );
        }
    };
    let outcome = match req.outcome.to_lowercase().as_str() {
        "success" => crate::core::episodic_memory::Outcome::Success {
            tests_passed: false,
        },
        "failure" => crate::core::episodic_memory::Outcome::Failure {
            error: "annotated failure".to_string(),
        },
        "neutral" => crate::core::episodic_memory::Outcome::Unknown,
        _ => {
            return (
                "400 Bad Request",
                "application/json",
                json_err("outcome must be success, failure, or neutral"),
            );
        }
    };

    let root = detect_project_root_for_dashboard();
    match annotate_episode_in_project(&root, req.episode_index, outcome) {
        Ok(true) => ("200 OK", "application/json", json_ok()),
        Ok(false) => (
            "400 Bad Request",
            "application/json",
            json_err("episode_index out of range (newest-first: 0 = most recent)"),
        ),
        Err(_) => (
            "503 Service Unavailable",
            "application/json",
            json_err("episode annotation unavailable under current policy"),
        ),
    }
}

fn annotate_episode_in_project(
    root: &str,
    episode_index: usize,
    outcome: crate::core::episodic_memory::Outcome,
) -> Result<bool, String> {
    with_dashboard_project(root, |project_hash, admission, _authority| {
        let (_, updated) = crate::core::episodic_memory::EpisodicStore::mutate_locked_admitted(
            project_hash,
            admission,
            |text| crate::core::session::admission::admit_store_text(admission, text),
            |store| {
                let len = store.episodes.len();
                if len == 0 || episode_index >= len {
                    return Ok(false);
                }
                let real_idx = len - 1 - episode_index;
                store.episodes[real_idx].outcome = outcome;
                Ok(true)
            },
        )?;
        Ok(updated)
    })
}

/// Canonical grouping key for a workspace root (#717). Sessions persisted on
/// Windows carry `C:\proj`, `C:/proj` and `c:/proj` variants of the same
/// workspace; the previous exact-string dedup rendered duplicate cards where
/// the stale twin sat on "idle" right next to the active one. Lexical-only —
/// no filesystem access, because the daemon may not be allowed to stat the
/// path (TCC, #356) and the root may even belong to another machine.
/// Drive-letter paths are Windows paths and case-insensitive as a whole.
fn workspace_canonical_key(root: &str) -> String {
    let p = crate::core::pathutil::normalize_tool_path_lexical(root);
    let is_drive_path =
        p.len() >= 2 && p.as_bytes()[0].is_ascii_alphabetic() && p.as_bytes()[1] == b':';
    if is_drive_path {
        p.to_ascii_lowercase()
    } else {
        p
    }
}

/// Builds the `/api/workspaces` entries: one card per canonical workspace
/// (#717), newest first. Within a group the freshest session wins
/// `updated_at`/root/task and counters take the max across variants. The
/// globally freshest workspace additionally absorbs `stats.last_use`
/// (`last_use` parameter) — the proxy bumps that on every request while
/// session JSONs lag behind the batch-save window, which is exactly the
/// "active session shown as idle" gap from the report. Same heuristic as
/// `/api/session`.
fn collect_workspaces(
    now: chrono::DateTime<Utc>,
    sessions: Vec<crate::core::session::SessionSummary>,
    last_use: Option<chrono::DateTime<Utc>>,
) -> Vec<serde_json::Value> {
    struct Group {
        root: String,
        updated_at: chrono::DateTime<Utc>,
        task: Option<String>,
        tool_calls: u32,
        tokens_saved: u64,
    }
    let mut groups: std::collections::HashMap<String, Group> = std::collections::HashMap::new();
    for s in sessions {
        let Some(root) = s.project_root.clone().filter(|r| !r.is_empty()) else {
            continue;
        };
        if crate::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(&root)) {
            continue;
        }
        let key = workspace_canonical_key(&root);
        // Render the normalized form so Windows separator variants display
        // uniformly regardless of which session happens to be freshest.
        let display_root = crate::core::pathutil::normalize_tool_path_lexical(&root);
        match groups.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let g = e.get_mut();
                if s.updated_at > g.updated_at {
                    g.updated_at = s.updated_at;
                    g.root = display_root;
                    if s.task.is_some() {
                        g.task.clone_from(&s.task);
                    }
                }
                if g.task.is_none() {
                    g.task.clone_from(&s.task);
                }
                g.tool_calls = g.tool_calls.max(s.tool_calls);
                g.tokens_saved = g.tokens_saved.max(s.tokens_saved);
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Group {
                    root: display_root,
                    updated_at: s.updated_at,
                    task: s.task,
                    tool_calls: s.tool_calls,
                    tokens_saved: s.tokens_saved,
                });
            }
        }
    }
    let mut list: Vec<Group> = groups.into_values().collect();
    list.sort_by_key(|g| std::cmp::Reverse(g.updated_at));
    if let Some(first) = list.first_mut()
        && let Some(lu) = last_use
        && lu > first.updated_at
    {
        first.updated_at = lu;
    }
    list.into_iter()
        .map(|g| {
            let age_minutes = now.signed_duration_since(g.updated_at).num_minutes();
            let status = if age_minutes < 10 {
                "active"
            } else if age_minutes < 24 * 60 {
                "idle"
            } else {
                "stale"
            };
            let name = std::path::Path::new(&g.root)
                .file_name()
                .map_or_else(|| g.root.clone(), |n| n.to_string_lossy().into_owned());
            serde_json::json!({
                "root": g.root,
                "name": name,
                "status": status,
                "last_activity": g.updated_at,
                "age_minutes": age_minutes,
                "task": g.task,
                "tool_calls": g.tool_calls,
                "tokens_saved": g.tokens_saved,
            })
        })
        .collect()
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    use crate::core::session::SessionSummary;
    use chrono::Duration;

    fn summary(root: &str, updated_min_ago: i64, calls: u32, task: Option<&str>) -> SessionSummary {
        let ts = Utc::now() - Duration::minutes(updated_min_ago);
        SessionSummary {
            id: format!("s-{root}-{updated_min_ago}"),
            started_at: ts,
            updated_at: ts,
            version: 1,
            task: task.map(str::to_string),
            tool_calls: calls,
            tokens_saved: calls as u64 * 100,
            project_root: Some(root.to_string()),
        }
    }

    #[test]
    fn canonical_key_folds_windows_variants() {
        // #717: all spellings of the same Windows workspace share one key.
        let variants = [
            r"C:\Users\dev\proj",
            "C:/Users/dev/proj",
            "c:/Users/dev/proj",
            r"c:\Users\dev\proj/",
        ];
        let keys: std::collections::HashSet<String> = variants
            .iter()
            .map(|v| workspace_canonical_key(v))
            .collect();
        assert_eq!(keys.len(), 1, "expected one canonical key, got {keys:?}");
        // Unix paths stay case-sensitive (a real FS distinction there).
        assert_ne!(
            workspace_canonical_key("/home/dev/Proj"),
            workspace_canonical_key("/home/dev/proj")
        );
    }

    #[test]
    fn windows_path_variants_dedupe_to_one_entry() {
        // #717 repro: the same workspace persisted under three spellings must
        // render one card carrying the freshest timestamp and max counters.
        let sessions = vec![
            summary(r"C:\Users\dev\proj", 120, 40, None),
            summary("c:/Users/dev/proj", 60, 10, Some("older task")),
            summary("C:/Users/dev/proj", 2, 25, Some("current task")),
        ];
        let out = collect_workspaces(Utc::now(), sessions, None);
        assert_eq!(out.len(), 1, "expected dedupe to one entry: {out:?}");
        let ws = &out[0];
        assert_eq!(ws["status"], "active");
        assert_eq!(ws["task"], "current task");
        assert_eq!(ws["tool_calls"], 40);
        assert_eq!(ws["root"], "C:/Users/dev/proj");
    }

    #[test]
    fn stats_last_use_promotes_freshest_workspace() {
        // #717: batch-save lag means the session JSON can be minutes old
        // while the proxy is actively serving — stats.last_use closes the gap.
        let sessions = vec![
            summary("/home/dev/alpha", 30, 5, None),
            summary("/home/dev/beta", 90, 3, None),
        ];
        let out = collect_workspaces(Utc::now(), sessions.clone(), Some(Utc::now()));
        assert_eq!(out[0]["root"], "/home/dev/alpha");
        assert_eq!(
            out[0]["status"], "active",
            "last_use must lift idle → active"
        );
        // Second workspace keeps its own (idle) freshness.
        assert_eq!(out[1]["status"], "idle");
        // Without last_use the same input stays idle.
        let out = collect_workspaces(Utc::now(), sessions, None);
        assert_eq!(out[0]["status"], "idle");
    }

    #[test]
    fn broad_roots_and_rootless_sessions_are_skipped() {
        let mut no_root = summary("/home/dev/proj", 5, 1, None);
        no_root.project_root = None;
        let sessions = vec![no_root, summary("/", 5, 1, None)];
        {
            let found = &collect_workspaces(Utc::now(), sessions, None);
            assert!(found.is_empty(), "{found:?}");
        }
    }
}

#[cfg(test)]
mod dashboard_memory_admission_tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;
    use crate::core::episodic_memory::{Episode, EpisodicStore, Outcome};
    use crate::core::procedural_memory::{ProceduralStore, Procedure};

    fn episode(id: &str, task_description: &str) -> Episode {
        Episode {
            id: id.to_string(),
            session_id: "dashboard-test-session".to_string(),
            timestamp: Utc::now(),
            task_description: task_description.to_string(),
            actions: Vec::new(),
            outcome: Outcome::Unknown,
            affected_files: Vec::new(),
            summary: "safe summary".to_string(),
            duration_secs: 0,
            tokens_used: 0,
            agent_id: None,
        }
    }

    #[test]
    fn dashboard_intent_reads_only_admitted_project_session() {
        let _isolated = isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let root = project.path().to_str().expect("UTF-8 project root");
        let mut session = crate::core::session::SessionState::new();
        session.project_root = Some(root.to_string());
        session.shell_cwd = session.project_root.clone();
        let mut intent = crate::core::intent_engine::StructuredIntent::from_query("ordinary task");
        intent.keywords = vec!["TOP SECRET\nprivate intent".to_string()];
        session.active_structured_intent = Some(intent);

        let sessions_dir = crate::core::data_dir::lean_ctx_data_dir()
            .expect("isolated data directory")
            .join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions directory");
        let path = sessions_dir.join(format!("{}.json", session.id));
        std::fs::write(
            &path,
            serde_json::to_string(&session).expect("serialize raw fixture"),
        )
        .expect("write raw session fixture");

        let body = intent_json(root).expect("admitted intent response");
        assert!(!body.contains("TOP SECRET"), "restricted intent escaped");
        let payload: serde_json::Value =
            serde_json::from_str(&body).expect("dashboard response remains JSON");
        assert_eq!(payload["active"], true);
        assert!(payload["intent"]["keywords"].as_array().unwrap().is_empty());
    }

    #[test]
    fn dashboard_episode_read_and_annotation_admit_before_delivery_and_write() {
        let _isolated = isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let root = project.path().to_str().expect("UTF-8 project root");
        let hash = crate::core::project_hash::hash_project_root(root);
        let mut store = EpisodicStore::new(&hash);
        store.episodes = vec![
            episode("restricted-old", "TOP SECRET\nprivate finding"),
            episode("safe-new", "ordinary task"),
        ];
        store.save().expect("write raw fixture");

        let body = episodes_json(root).expect("admitted dashboard response");
        assert!(!body.contains("TOP SECRET"), "restricted content escaped");
        let json: serde_json::Value =
            serde_json::from_str(&body).expect("dashboard response remains JSON");
        assert_eq!(json["recent"].as_array().unwrap().len(), 1);
        assert_eq!(json["recent"][0]["task_description"], "ordinary task");

        assert!(
            annotate_episode_in_project(
                root,
                0,
                Outcome::Failure {
                    error: "annotated failure".to_string(),
                },
            )
            .expect("admitted annotation")
        );
        let persisted = EpisodicStore::load(&hash).expect("cleaned store persisted");
        assert_eq!(persisted.episodes.len(), 1);
        assert_eq!(persisted.episodes[0].task_description, "ordinary task");
        assert_eq!(
            persisted.episodes[0].outcome,
            Outcome::Failure {
                error: "annotated failure".to_string(),
            }
        );
    }

    #[test]
    fn dashboard_procedure_read_omits_restricted_records() {
        let _isolated = isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let root = project.path().to_str().expect("UTF-8 project root");
        let hash = crate::core::project_hash::hash_project_root(root);
        let mut store = ProceduralStore::new(&hash);
        store.procedures.push(Procedure {
            id: "restricted-procedure".to_string(),
            name: "TOP SECRET\nprivate workflow".to_string(),
            description: "safe description".to_string(),
            steps: Vec::new(),
            activation_keywords: Vec::new(),
            confidence: 0.5,
            times_used: 1,
            times_succeeded: 1,
            last_used: Utc::now(),
            project_specific: true,
            created_at: Utc::now(),
        });
        store.save().expect("write raw fixture");

        let body = procedures_json(root, "").expect("admitted dashboard response");
        assert!(!body.contains("TOP SECRET"), "restricted content escaped");
        let json: serde_json::Value =
            serde_json::from_str(&body).expect("dashboard response remains JSON");
        assert_eq!(json["procedures"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn dashboard_rejects_policy_change_before_returning_content() {
        let _isolated = isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let root = project.path().to_str().expect("UTF-8 project root");
        let config_dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        std::fs::write(
            config_dir.join("config.toml"),
            "[context_gateway]\nenabled = true\nmode = \"developer\"\n",
        )
        .expect("initial gateway policy");

        let result = with_dashboard_project(root, |_hash, _admission, _authority| {
            std::fs::write(
                config_dir.join("config.toml"),
                "[context_gateway]\nenabled = true\nmode = \"governed\"\n",
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        });
        assert!(result.is_err(), "stale policy snapshot was accepted");
    }

    #[test]
    fn dashboard_rejects_foreign_request_project() {
        let _isolated = isolated_data_dir();
        let project = tempfile::tempdir().expect("selected project");
        let foreign = tempfile::tempdir().expect("foreign project");
        let selected_root = project.path().to_str().expect("selected root");
        let result = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(foreign.path().to_path_buf())),
            || intent_json(selected_root),
        );
        assert!(result.is_err(), "foreign request root was accepted");
    }
}
