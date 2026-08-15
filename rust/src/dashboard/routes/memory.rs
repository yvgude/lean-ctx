use chrono::Utc;
use serde::Deserialize;

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

fn get_routes(path: &str, query_str: &str) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/episodes" => {
            let root = detect_project_root_for_dashboard();
            let hash = crate::core::project_hash::hash_project_root(&root);
            let store = crate::core::episodic_memory::EpisodicStore::load_or_create(&hash);
            let stats = store.stats();
            let recent: Vec<_> = store.recent(20).into_iter().cloned().collect();
            let payload = serde_json::json!({
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
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/procedures" => {
            let root = detect_project_root_for_dashboard();
            let hash = crate::core::project_hash::hash_project_root(&root);
            let store = crate::core::procedural_memory::ProceduralStore::load_or_create(&hash);
            let task = extract_query_param(query_str, "task").or_else(|| {
                crate::core::session::SessionState::load_latest_for_project_root(&root)
                    .and_then(|s| s.task.map(|t| t.description))
            });
            let suggestions: Vec<serde_json::Value> = task.as_deref().map_or(Vec::new(), |t| {
                store
                    .suggest(t)
                    .into_iter()
                    .take(10)
                    .map(|p| {
                        serde_json::json!({
                            "id": p.id,
                            "name": p.name,
                            "description": p.description,
                            "confidence": p.confidence,
                            "times_used": p.times_used,
                            "times_succeeded": p.times_succeeded,
                            "success_rate": p.success_rate(),
                            "steps": p.steps,
                            "activation_keywords": p.activation_keywords,
                            "last_used": p.last_used,
                            "created_at": p.created_at,
                        })
                    })
                    .collect()
            });
            let payload = serde_json::json!({
                "project_root": root,
                "project_hash": hash,
                "total_procedures": store.procedures.len(),
                "task": task,
                "suggestions": suggestions,
                "procedures": store.procedures,
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/session" => {
            // The status bar promises "the task your most recent agent session
            // is working on". `load_latest()` matches sessions against THIS
            // process's cwd — but the dashboard usually runs from HOME (a
            // broad root that rightly matches nothing), so it permanently
            // showed "No session" while agents were active. Fall back to the
            // most recently updated session rooted in a real project
            // (skipping broad/unsafe roots like HOME or test-sandbox temp dirs).
            let mut session = crate::core::session::SessionState::load_latest()
                .or_else(|| {
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
            // processes — MCP server, proxy, CLI). This is the source of truth.
            let summary = crate::core::savings_ledger::summary();
            let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let (today_saved, today_usd, today_baseline) = summary
                .by_day
                .iter()
                .find(|(d, ..)| d == &today)
                .map(|(_, s, u, b)| (*s, *u, *b))
                .unwrap_or((0, 0.0, 0));
            let compression_session = serde_json::json!({
                "savings_tokens": today_saved,
                "total_raw": today_baseline,
                "total_compressed": today_baseline.saturating_sub(today_saved),
                "savings_percent": if today_baseline > 0 {
                    today_saved as f64 * 100.0 / today_baseline as f64
                } else { 0.0 },
                "savings_usd": today_usd,
            });

            let global = crate::core::stats::load_for_display();
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
            let allocator = if cfg!(all(feature = "jemalloc", not(windows))) {
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
            let session_path = crate::core::data_dir::lean_ctx_data_dir()
                .ok()
                .map(|d| d.join("sessions"));
            let mut intent_data = serde_json::json!({"active": false});
            if let Some(dir) = session_path
                && let Ok(entries) = std::fs::read_dir(&dir)
            {
                let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
                for e in entries.flatten() {
                    if e.path().extension().is_some_and(|ext| ext == "json")
                        && let Ok(meta) = e.metadata()
                    {
                        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                        if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
                            newest = Some((mtime, e.path()));
                        }
                    }
                }
                if let Some((_, path)) = newest
                    && let Ok(content) = std::fs::read_to_string(&path)
                    && let Ok(session) = serde_json::from_str::<serde_json::Value>(&content)
                    && let Some(intent) = session.get("active_structured_intent")
                    && !intent.is_null()
                {
                    intent_data = serde_json::json!({
                        "active": true,
                        "intent": intent,
                        "session_file": path.file_name().unwrap_or_default().to_string_lossy(),
                    });
                }
            }
            let json = serde_json::to_string(&intent_data).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
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
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!("invalid JSON: {e}")),
            );
        }
    };
    let root = detect_project_root_for_dashboard();
    let hash = crate::core::project_hash::hash_project_root(&root);
    let mut store = crate::core::episodic_memory::EpisodicStore::load_or_create(&hash);
    let n = store.episodes.len();
    if n == 0 || req.episode_index >= n {
        return (
            "400 Bad Request",
            "application/json",
            json_err("episode_index out of range (newest-first: 0 = most recent)"),
        );
    }
    // Align with /api/episodes `recent` ordering (most recent first).
    let real_idx = n - 1 - req.episode_index;
    let new_outcome = match req.outcome.to_lowercase().as_str() {
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
    store.episodes[real_idx].outcome = new_outcome;
    if let Err(e) = store.save() {
        return (
            "500 Internal Server Error",
            "application/json",
            json_err(&e),
        );
    }
    ("200 OK", "application/json", json_ok())
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
        assert!(collect_workspaces(Utc::now(), sessions, None).is_empty());
    }
}
