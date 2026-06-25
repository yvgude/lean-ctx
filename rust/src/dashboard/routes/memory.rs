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
            let global = crate::core::stats::load();
            let g_cmds = global.total_commands;
            let g_input = global.total_input_tokens;
            let g_output = global.total_output_tokens;
            let g_saved = g_input.saturating_sub(g_output);
            if g_cmds > u64::from(session.stats.total_tool_calls) {
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
            let json = serde_json::to_string(&session)
                .unwrap_or_else(|_| "{\"error\":\"failed to serialize session\"}".to_string());
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
