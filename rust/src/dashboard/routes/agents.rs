use std::collections::HashMap;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    method: &str,
    body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/mcp" => {
            let json = build_mcp_tools_json();
            Some(("200 OK", "application/json", json))
        }
        "/api/agents" => {
            let json = build_agents_json();
            Some(("200 OK", "application/json", json))
        }
        "/api/agents/sessions" if method.eq_ignore_ascii_case("POST") => {
            Some(handle_logical_session_presence(body))
        }
        "/api/agents/sessions" => Some((
            "405 Method Not Allowed",
            "application/json",
            r#"{"error":"method not allowed"}"#.to_string(),
        )),
        "/api/events" => {
            let evs = crate::core::events::load_events_from_file(200);
            let json = serde_json::to_string(&evs).unwrap_or_else(|_| "[]".to_string());
            Some(("200 OK", "application/json", json))
        }
        p if p.starts_with("/api/events/") => {
            let id_str = &p["/api/events/".len()..];
            if let Ok(id) = id_str.parse::<u64>() {
                let evs = crate::core::events::load_events_from_file(500);
                if let Some(ev) = evs.iter().find(|e| e.id == id) {
                    let json = serde_json::to_string(ev).unwrap_or_else(|_| "{}".to_string());
                    Some(("200 OK", "application/json", json))
                } else {
                    Some((
                        "404 Not Found",
                        "application/json",
                        r#"{"error":"event not found"}"#.to_string(),
                    ))
                }
            } else {
                Some((
                    "400 Bad Request",
                    "application/json",
                    r#"{"error":"invalid event id"}"#.to_string(),
                ))
            }
        }
        _ => None,
    }
}

fn build_agents_json() -> String {
    let mut registry = crate::core::agents::AgentRegistry::load_or_create();
    let project_roots: std::collections::BTreeSet<_> = registry
        .agents
        .iter()
        .map(|agent| agent.project_root.clone())
        .filter(|root| !root.is_empty())
        .collect();
    let cfg = crate::core::config::Config::load().agents;
    registry.cleanup_stale(cfg.presence_ttl_hours);
    registry.cleanup_stale_logical_sessions(cfg.logical_session_ttl_seconds);

    let live_agents: Vec<_> = registry
        .agents
        .iter()
        .filter(|agent| {
            agent.status != crate::core::agents::AgentStatus::Finished
                && agent.process_identity.as_ref().is_some_and(|identity| {
                    crate::ipc::process::matches_identity(agent.pid, identity)
                })
        })
        .collect();
    let transports: Vec<serde_json::Value> = live_agents
        .iter()
        .map(|a| {
            let age_min = (chrono::Utc::now() - a.last_active).num_minutes().max(0);
            serde_json::json!({
                "id": a.agent_id,
                "project_root": a.project_root,
                "type": a.agent_type,
                "role": a.role,
                "status": format!("{}", a.status),
                "status_message": a.status_message,
                "last_active_minutes_ago": age_min,
                "pid": a.pid
            })
        })
        .collect();

    let logical_sessions: Vec<serde_json::Value> = registry
        .logical_sessions
        .iter()
        .map(|session| {
            let heartbeat_age_seconds = (chrono::Utc::now() - session.last_heartbeat)
                .num_seconds()
                .max(0);
            serde_json::json!({
                "source": session.source,
                "workspace": session.workspace,
                "session_id": session.session_id,
                "opened_at": session.opened_at,
                "last_heartbeat": session.last_heartbeat,
                "heartbeat_age_seconds": heartbeat_age_seconds
            })
        })
        .collect();
    let logical_session_count = registry
        .logical_session_telemetry_seen
        .then_some(logical_sessions.len());

    // Retained history is not an inbox backlog. A pending delivery must have
    // a live, project-scoped recipient that has not read the message.
    let now = chrono::Utc::now();
    let pending_msgs = registry
        .scratchpad
        .iter()
        .filter(|message| {
            message.expires_at.is_none_or(|expiry| expiry > now)
                && live_agents.iter().any(|agent| {
                    pending_for_recipient(message, &agent.agent_id, &agent.project_root)
                })
        })
        .count();
    let shared_dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".lean-ctx"))
        .join("agents")
        .join("shared");
    let shared_count = if shared_dir.exists() {
        std::fs::read_dir(&shared_dir).map_or(0, std::iter::Iterator::count)
    } else {
        0
    };

    serde_json::json!({
        "transports": transports,
        "project_roots": project_roots,
        "transport_count": transports.len(),
        "logical_sessions": logical_sessions,
        "logical_session_count": logical_session_count,
        "logical_session_presence_available": registry.logical_session_telemetry_seen,
        "recent_tool_activity": infer_agents_from_events(),
        // Compatibility for older dashboard clients: agents always means transports.
        "agents": transports,
        "total_active": transports.len(),
        "pending_messages": pending_msgs,
        "retained_messages": registry.scratchpad.len(),
        "execution_presence_available": false,
        "shared_contexts": shared_count
    })
    .to_string()
}

fn pending_for_recipient(
    message: &crate::core::agents::ScratchpadEntry,
    agent_id: &str,
    project_root: &str,
) -> bool {
    message.project_root.as_deref() == Some(project_root)
        && message.to_agent.as_deref().is_none_or(|id| id == agent_id)
        && message.from_agent != agent_id
        && !message.read_by.iter().any(|id| id == agent_id)
}

#[derive(serde::Deserialize)]
struct LogicalSessionPresenceRequest {
    event: String,
    source: String,
    workspace: String,
    session_id: String,
}

fn handle_logical_session_presence(body: &str) -> (&'static str, &'static str, String) {
    let request: LogicalSessionPresenceRequest = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(_) => {
            return (
                "400 Bad Request",
                "application/json",
                r#"{"error":"invalid JSON body"}"#.to_string(),
            );
        }
    };

    if !valid_presence_field(&request.source, 64)
        || !valid_presence_field(&request.workspace, 4096)
        || !valid_presence_field(&request.session_id, 256)
    {
        return (
            "400 Bad Request",
            "application/json",
            r#"{"error":"presence fields are empty, too long, or contain control characters"}"#
                .to_string(),
        );
    }

    let event = request.event.as_str();
    if !matches!(event, "open" | "heartbeat" | "close") {
        return (
            "400 Bad Request",
            "application/json",
            r#"{"error":"event must be open, heartbeat, or close"}"#.to_string(),
        );
    }

    let result = crate::core::agents::AgentRegistry::record_logical_session_presence(
        event,
        &request.source,
        &request.workspace,
        &request.session_id,
    );

    match result {
        Ok(()) => (
            "200 OK",
            "application/json",
            serde_json::json!({"ok": true, "event": event}).to_string(),
        ),
        Err(error) => (
            "500 Internal Server Error",
            "application/json",
            serde_json::json!({"error": error}).to_string(),
        ),
    }
}

fn valid_presence_field(value: &str, max_len: usize) -> bool {
    !value.is_empty() && value.len() <= max_len && !value.chars().any(char::is_control)
}

/// Event timestamps are written by `events.rs` with `chrono::Local::now()` and
/// carry no offset, so they MUST be interpreted as *local* time. Reading them
/// as UTC made agents appear "active 2h in the future" on UTC+2 machines
/// (`last_active_minutes_ago = -119`, GL #479 D3).
fn local_event_ts_to_utc(ts: chrono::NaiveDateTime) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone as _;
    match chrono::Local.from_local_datetime(&ts) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => {
            t.with_timezone(&chrono::Utc)
        }
        // A nonexistent local time (DST spring-forward gap): fall back to UTC,
        // which is at most one DST shift off — never negative by hours.
        chrono::LocalResult::None => ts.and_utc(),
    }
}

fn infer_agents_from_events() -> Vec<serde_json::Value> {
    let evts = crate::core::events::load_events_from_file(200);
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::minutes(30);

    let mut recent_tool_count: u64 = 0;
    let mut latest_ts: Option<chrono::NaiveDateTime> = None;

    for ev in &evts {
        let ts_str = &ev.timestamp;
        if let Ok(ts) = chrono::NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%dT%H:%M:%S%.f") {
            let aware = local_event_ts_to_utc(ts);
            if aware >= cutoff {
                if matches!(&ev.kind, crate::core::events::EventKind::ToolCall { .. }) {
                    recent_tool_count += 1;
                }
                if latest_ts.is_none_or(|prev| ts > prev) {
                    latest_ts = Some(ts);
                }
            }
        }
    }

    if recent_tool_count == 0 {
        return Vec::new();
    }

    // Clamp at zero: clock skew must never render a negative age.
    let age_min = latest_ts.map_or(0, |ts| {
        (now - local_event_ts_to_utc(ts)).num_minutes().max(0)
    });

    // #717: same freshness threshold as /api/workspaces — the two panels
    // used to disagree ("active" here, "idle" there) for the same session.
    let status = if age_min < 10 { "active" } else { "idle" };

    vec![serde_json::json!({
        "id": "lean-ctx-session",
        "type": "lean-ctx",
        "role": "context-engine",
        "status": status,
        "status_message": format!("{} tool calls in last 30min", recent_tool_count),
        "last_active_minutes_ago": age_min,
        "pid": std::process::id(),
        "inferred": true
    })]
}

fn build_mcp_tools_json() -> String {
    // All-time per-tool aggregates from stats.json — the same source Home and
    // ROI use. The event log only holds the last N events, which silently
    // understated these counters as a pseudo all-time view (#492).
    let store = crate::core::stats::load_for_display();

    let mut tool_stats: HashMap<String, ToolAgg> = HashMap::new();

    for (name, cmd) in &store.commands {
        let entry = tool_stats.entry(name.clone()).or_default();
        entry.calls += cmd.count;
        entry.tokens_saved += cmd.input_tokens.saturating_sub(cmd.output_tokens);
        entry.tokens_original += cmd.input_tokens;
    }

    let known_tools: &[(&str, &str)] = &[
        ("ctx_read", "Read files with 10 compression modes"),
        ("ctx_search", "Search code with compact results"),
        ("ctx_shell", "Shell commands with pattern compression"),
        ("ctx_tree", "Compact directory maps"),
        ("ctx_overview", "Project overview with dependency graph"),
        ("ctx_session", "Session management and state tracking"),
        ("ctx_compress", "Compress context when budget is tight"),
        ("ctx_metrics", "Token savings and performance metrics"),
        ("ctx_control", "Context overlays: pin, exclude, priority"),
        ("ctx_plan", "Context-aware planning with budget estimation"),
    ];

    let mut tools: Vec<serde_json::Value> = Vec::new();

    for &(name, description) in known_tools {
        let stats = tool_stats.remove(name);
        let (calls, saved, original) =
            stats.map_or((0, 0, 0), |s| (s.calls, s.tokens_saved, s.tokens_original));
        tools.push(serde_json::json!({
            "name": name,
            "description": description,
            "call_count": calls,
            "tokens_saved": saved,
            "tokens_original": original
        }));
    }

    for (name, stats) in &tool_stats {
        tools.push(serde_json::json!({
            "name": name,
            "description": "",
            "call_count": stats.calls,
            "tokens_saved": stats.tokens_saved,
            "tokens_original": stats.tokens_original
        }));
    }

    serde_json::json!({ "tools": tools }).to_string()
}

#[derive(Default)]
struct ToolAgg {
    calls: u64,
    tokens_saved: u64,
    tokens_original: u64,
}

#[cfg(test)]
mod tests {
    use super::{handle, local_event_ts_to_utc};

    #[test]
    fn pending_delivery_is_recipient_and_project_scoped() {
        let mut message: crate::core::agents::ScratchpadEntry =
            serde_json::from_value(serde_json::json!({
                "id": "message-1", "from_agent": "sender", "to_agent": "worker",
                "category": "request", "message": "task", "project_root": "/repo",
                "timestamp": "2026-01-01T00:00:00Z", "read_by": []
            }))
            .expect("valid message");
        assert!(super::pending_for_recipient(&message, "worker", "/repo"));
        assert!(!super::pending_for_recipient(&message, "other", "/repo"));
        assert!(!super::pending_for_recipient(&message, "worker", "/other"));
        message.read_by.push("worker".into());
        assert!(!super::pending_for_recipient(&message, "worker", "/repo"));
        message.to_agent = None;
        assert!(super::pending_for_recipient(&message, "other", "/repo"));
        assert!(!super::pending_for_recipient(&message, "sender", "/repo"));
        message.project_root = None;
        assert!(!super::pending_for_recipient(&message, "other", "/repo"));
    }

    /// GL #479 D3: event timestamps are local wall-clock strings; interpreting
    /// "now" as local must yield an age of ~0 — not a negative UTC-offset age.
    #[test]
    fn local_event_ts_age_is_never_hours_in_the_future() {
        let now_local = chrono::Local::now().naive_local();
        let aware = local_event_ts_to_utc(now_local);
        let age_min = (chrono::Utc::now() - aware).num_minutes();
        assert!(
            (-1..=1).contains(&age_min),
            "a just-written event must be ~0 minutes old, got {age_min}"
        );
    }

    #[test]
    fn agents_route_does_not_wait_for_registry_writer() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let agents_dir = isolated.path().join("agents");
        std::fs::create_dir_all(&agents_dir).expect("agents dir");
        std::fs::write(agents_dir.join("registry.lock"), b"held").expect("registry lock");

        let started = std::time::Instant::now();
        let (status, _, _) = handle("/api/agents", "", "GET", "").expect("agents route");

        assert_eq!(status, "200 OK");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "read-only agents route must not wait for a registry writer"
        );
    }

    #[test]
    fn explicit_session_presence_is_reported_separately_from_transports() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let body = serde_json::json!({
            "event": "open",
            "source": "vscode",
            "workspace": "/repo",
            "session_id": "chat-1"
        })
        .to_string();

        let (status, _, _) =
            handle("/api/agents/sessions", "", "POST", &body).expect("presence route");
        assert_eq!(status, "200 OK");

        let (_, _, json) = handle("/api/agents", "", "GET", "").expect("agents route");
        let payload: serde_json::Value = serde_json::from_str(&json).expect("agents JSON");
        assert_eq!(payload["transport_count"], 0);
        assert_eq!(payload["logical_session_count"], 1);
        assert_eq!(payload["logical_sessions"][0]["session_id"], "chat-1");
        assert_eq!(payload["logical_session_presence_available"], true);
    }

    #[test]
    fn session_presence_route_validates_method_event_and_fields() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let request = |event: &str, source: &str| {
            serde_json::json!({
                "event": event,
                "source": source,
                "workspace": "/repo",
                "session_id": "chat-1"
            })
            .to_string()
        };

        let (status, _, _) = handle(
            "/api/agents/sessions",
            "",
            "GET",
            &request("open", "vscode"),
        )
        .expect("presence route");
        assert_eq!(status, "405 Method Not Allowed");

        let (status, _, _) = handle(
            "/api/agents/sessions",
            "",
            "POST",
            &request("activity", "vscode"),
        )
        .expect("presence route");
        assert_eq!(status, "400 Bad Request");

        let (status, _, _) = handle(
            "/api/agents/sessions",
            "",
            "POST",
            &request("open", "vs\ncode"),
        )
        .expect("presence route");
        assert_eq!(status, "400 Bad Request");
    }
}
