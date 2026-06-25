use crate::dashboard::routes::helpers::detect_project_root_for_dashboard;

pub(super) fn get_route(path: &str) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/context-bounce" => Some(bounce()),
        "/api/context-client" => Some(client()),
        "/api/context-dynamic-tools" => Some(dynamic_tools()),
        "/api/context-introspect" => Some(introspect()),
        "/api/context-radar" => Some(radar()),
        "/api/context-transcript" => Some(transcript()),
        "/api/context-model" => Some(model()),
        "/api/context-events" => Some(events()),
        "/api/context-triage" => Some(build_triage_response()),
        _ => None,
    }
}

fn bounce() -> (&'static str, &'static str, String) {
    let payload = match crate::core::bounce_tracker::global().lock() {
        Ok(bt) => {
            serde_json::json!({
                "summary": bt.format_summary(),
                "total_bounces": bt.total_bounces(),
                "total_wasted_tokens": bt.total_wasted_tokens(),
                "per_extension": bt.per_extension_json(),
            })
        }
        _ => {
            serde_json::json!({ "error": "lock failed" })
        }
    };
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn client() -> (&'static str, &'static str, String) {
    let caps = crate::core::client_capabilities::load_persisted(86400)
        .unwrap_or_else(crate::core::client_capabilities::current);
    let payload = serde_json::json!({
        "client_id": caps.client_id,
        "tier": caps.tier(),
        "resources": caps.resources,
        "prompts": caps.prompts,
        "elicitation": caps.elicitation,
        "sampling": caps.sampling,
        "dynamic_tools": caps.dynamic_tools,
        "max_tools": caps.max_tools,
        "summary": caps.format_summary(),
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn dynamic_tools() -> (&'static str, &'static str, String) {
    let payload = match crate::server::dynamic_tools::global().lock() {
        Ok(state) => {
            serde_json::json!({
                "active_categories": state.active_categories(),
                "all_categories": crate::server::dynamic_tools::DynamicToolState::all_categories(),
                "supports_list_changed": state.supports_list_changed(),
            })
        }
        _ => {
            serde_json::json!({ "error": "lock failed" })
        }
    };
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn introspect() -> (&'static str, &'static str, String) {
    let persisted = crate::proxy::introspect::load_persisted(300);
    let proxy_port = crate::proxy_setup::default_port();
    let addr = std::net::SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        proxy_port,
    );
    let proxy_running =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(100)).is_ok();

    let payload = match persisted {
        Some(mut val) => {
            if let Some(obj) = val.as_object_mut() {
                obj.insert("proxy_running".into(), proxy_running.into());
            }
            val
        }
        None => serde_json::json!({
            "proxy_active": false,
            "proxy_running": proxy_running,
        }),
    };
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn radar() -> (&'static str, &'static str, String) {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let client_id = crate::core::client_capabilities::load_persisted(86400)
        .map_or_else(|| "cursor".to_string(), |c| c.client_id);
    let window = crate::core::context_radar::default_window_for_client(&client_id);
    let radar = crate::core::context_radar::ContextRadar::load(&data_dir, window);
    let breakdown = radar.budget_breakdown();
    let recent_events: Vec<serde_json::Value> = radar
        .events
        .iter()
        .rev()
        .take(100)
        .map(|e| {
            serde_json::json!({
                "ts": e.ts,
                "event_type": e.event_type,
                "tokens": e.tokens,
                "tool_name": e.tool_name,
                "detail": e.detail,
            })
        })
        .collect();
    let rules_files: Vec<serde_json::Value> = radar
        .rules_tokens
        .files
        .iter()
        .map(|(path, tokens)| serde_json::json!({ "path": path, "tokens": tokens }))
        .collect();
    let payload = serde_json::json!({
        "breakdown": breakdown,
        "rules": {
            "files": rules_files,
            "total_tokens": radar.rules_tokens.total,
        },
        "events_total": radar.events.len(),
        "recent_events": recent_events,
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn transcript() -> (&'static str, &'static str, String) {
    let transcript = crate::hook_handlers::load_active_transcript();
    if let Some((path, conv_id)) = transcript {
        let tp = std::path::Path::new(&path);
        if tp.exists() {
            if let Ok(raw) = std::fs::read_to_string(tp) {
                let messages: Vec<serde_json::Value> = raw
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .filter_map(|entry| {
                        let role = entry.get("role")?.as_str()?;
                        let msg = entry.get("message")?;
                        let content = msg.get("content")?;
                        let text = if let Some(s) = content.as_str() {
                            s.to_string()
                        } else if let Some(arr) = content.as_array() {
                            let parts: Vec<String> = arr
                                .iter()
                                .filter_map(|p| {
                                    if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                                        p.get("text").and_then(|t| t.as_str()).map(String::from)
                                    } else if p.get("type").and_then(|t| t.as_str())
                                        == Some("tool_use")
                                    {
                                        let name = p
                                            .get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("tool");
                                        Some(format!("[Tool: {name}]"))
                                    } else if p.get("type").and_then(|t| t.as_str())
                                        == Some("tool_result")
                                    {
                                        Some("[Tool Result]".to_string())
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            if parts.is_empty() {
                                return None;
                            }
                            parts.join("\n")
                        } else {
                            content.to_string()
                        };
                        if text.is_empty() {
                            return None;
                        }
                        let tokens = text.len() / 4;
                        let capped = if text.len() > 50000 {
                            format!("{}…", &text[..text.floor_char_boundary(50000)])
                        } else {
                            text
                        };
                        Some(serde_json::json!({
                            "role": role,
                            "text": capped,
                            "tokens": tokens,
                        }))
                    })
                    .collect();
                let payload = serde_json::json!({
                    "conversation_id": conv_id,
                    "transcript_path": path,
                    "messages": messages,
                    "total": messages.len(),
                });
                let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
                ("200 OK", "application/json", json)
            } else {
                (
                    "200 OK",
                    "application/json",
                    r#"{"messages":[],"error":"unreadable"}"#.to_string(),
                )
            }
        } else {
            (
                "200 OK",
                "application/json",
                r#"{"messages":[],"error":"file_not_found"}"#.to_string(),
            )
        }
    } else {
        (
            "200 OK",
            "application/json",
            r#"{"messages":[],"error":"no_transcript"}"#.to_string(),
        )
    }
}

fn model() -> (&'static str, &'static str, String) {
    let detected = crate::hook_handlers::load_detected_model();
    let client = crate::core::client_capabilities::load_persisted(86400)
        .map_or_else(|| "unknown".to_string(), |c| c.client_id);
    let (model, window) = detected.unwrap_or_else(|| {
        let w = crate::core::context_radar::default_window_for_client(&client);
        ("unknown".to_string(), w)
    });
    let payload = serde_json::json!({
        "model": model,
        "window_size": window,
        "client_id": client,
        "source": if model == "unknown" { "client_default" } else { "hook_detected" },
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn events() -> (&'static str, &'static str, String) {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let client_id = crate::core::client_capabilities::load_persisted(86400)
        .map_or_else(|| "cursor".to_string(), |c| c.client_id);
    let window = crate::core::context_radar::default_window_for_client(&client_id);
    let radar = crate::core::context_radar::ContextRadar::load(&data_dir, window);
    let events: Vec<serde_json::Value> = radar
        .events
        .iter()
        .rev()
        .take(500)
        .map(|e| {
            serde_json::json!({
                "ts": e.ts,
                "event_type": e.event_type,
                "tokens": e.tokens,
                "tool_name": e.tool_name,
                "detail": e.detail,
                "content": e.content,
                "model": e.model,
                "conversation_id": e.conversation_id,
            })
        })
        .collect();
    let json = serde_json::to_string(&events).unwrap_or_else(|_| "[]".to_string());
    ("200 OK", "application/json", json)
}

// --- Triage helpers ---

fn budget_band(utilization: f64) -> &'static str {
    let yellow = std::env::var("LCTX_BAND_YELLOW")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(50.0);
    let orange = std::env::var("LCTX_BAND_ORANGE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(75.0);
    let red = std::env::var("LCTX_BAND_RED")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(90.0);

    let pct = utilization * 100.0;
    if pct >= red {
        "red"
    } else if pct >= orange {
        "orange"
    } else if pct >= yellow {
        "yellow"
    } else {
        "green"
    }
}

fn band_recommendation(band: &str) -> &'static str {
    match band {
        "green" => "Optimal usage — no action needed",
        "yellow" => "Moderate pressure — consider compressed reads for new files",
        "orange" => "High pressure — review top eviction candidates",
        "red" => "Critical pressure — compact or create handoff pack",
        _ => "",
    }
}

fn compute_eviction_score(
    sent_tokens: usize,
    max_tokens: usize,
    timestamp: i64,
    now: i64,
    access_count: u32,
    pinned: bool,
) -> f64 {
    if pinned {
        return 0.0;
    }
    let token_ratio = if max_tokens > 0 {
        sent_tokens as f64 / max_tokens as f64
    } else {
        0.0
    };
    let age_secs = (now - timestamp).max(0) as f64;
    let recency = 1.0 / (1.0 + (age_secs / 600.0));
    let frequency = 1.0 / (1.0 + f64::from(access_count));
    (token_ratio * 0.5 + (1.0 - recency) * 0.3 + frequency * 0.2).min(1.0)
}

fn build_triage_response() -> (&'static str, &'static str, String) {
    let ledger = crate::core::context_ledger::ContextLedger::load();
    let pressure = ledger.pressure();
    let heatmap = crate::core::heatmap::HeatMap::load();
    let session = crate::core::session::SessionState::load_latest();
    let mi = crate::hook_handlers::load_detected_model();

    let window_size = mi.as_ref().map_or(ledger.window_size, |m| m.1);
    let band = budget_band(pressure.utilization);
    let recommendation = band_recommendation(band);

    let now = chrono::Utc::now().timestamp();

    let project_root = detect_project_root_for_dashboard();
    let overlays = crate::core::context_overlay::OverlayStore::load_project(
        &std::path::PathBuf::from(&project_root),
    );

    let pinned_paths: std::collections::HashSet<String> = overlays
        .all()
        .iter()
        .filter(|o| {
            matches!(
                o.operation,
                crate::core::context_overlay::OverlayOp::Pin { .. }
            )
        })
        .map(|o| o.target.as_str().to_string())
        .collect();

    let edited_paths: Vec<String> = session
        .as_ref()
        .map(|s| s.files_touched.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default();

    let max_tokens = ledger
        .entries
        .iter()
        .map(|e| e.sent_tokens)
        .max()
        .unwrap_or(1);

    // Git working-set signal (#497): files with uncommitted changes are the
    // active task — keep them in context longer.
    let git_signals = crate::core::git_signals::collect(&project_root);
    // Editor focus signal (#500): the file open in the editor right now.
    let editor_signal =
        crate::core::editor_signal::load_fresh(crate::core::editor_signal::FRESHNESS_SECS);

    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let client_id = crate::core::client_capabilities::load_persisted(86400)
        .map_or_else(|| "cursor".to_string(), |c| c.client_id);
    let radar_window = crate::core::context_radar::default_window_for_client(&client_id);
    let radar = crate::core::context_radar::ContextRadar::load(&data_dir, radar_window);

    let mut items: Vec<serde_json::Value> = ledger
        .entries
        .iter()
        .map(|entry| {
            let heat = heatmap.entries.get(&entry.path);
            let access_count = heat.map_or(1, |h| h.access_count);
            let last_access_ts = heat
                .and_then(|h| chrono::DateTime::parse_from_rfc3339(&h.last_access).ok())
                .map_or(entry.timestamp, |dt| dt.timestamp());

            let is_pinned = pinned_paths.contains(&entry.path)
                || entry.state == Some(crate::core::context_field::ContextState::Pinned);

            let git_recency = git_signals.recency_for(&entry.path, &project_root);
            let diag_details = crate::core::diagnostics_store::details_for(&entry.path);
            let has_active_error = diag_details
                .iter()
                .any(|(_, sev, _)| *sev == crate::core::diagnostics_store::Severity::Error);
            let editor_active = editor_signal
                .as_ref()
                .is_some_and(|s| crate::core::editor_signal::boost_for(s, &entry.path) >= 0.30);
            let eviction_score = {
                let base = compute_eviction_score(
                    entry.sent_tokens,
                    max_tokens,
                    last_access_ts,
                    now,
                    access_count,
                    is_pinned,
                );
                let mut adjusted = base;
                if git_recency > 0.8 {
                    adjusted -= 0.2;
                }
                // A file with an active build error is the task — keep it (#499).
                if has_active_error {
                    adjusted -= 0.3;
                }
                // The developer is looking at this file right now (#500).
                if editor_active {
                    adjusted -= 0.25;
                }
                adjusted.max(0.0)
            };

            let compression_pct = if entry.original_tokens > 0 {
                ((entry.original_tokens - entry.sent_tokens) as f64 / entry.original_tokens as f64
                    * 100.0)
                    .round() as u32
            } else {
                0
            };

            let source_trail = build_source_trail(entry, &radar, is_pinned);

            let mut risk_flags: Vec<serde_json::Value> = Vec::new();
            if entry.mode != "full"
                && edited_paths
                    .iter()
                    .any(|ep| entry.path.ends_with(ep) || ep.ends_with(&entry.path))
            {
                risk_flags.push(serde_json::json!({
                    "type": "edited_after_compressed",
                    "message": format!("Edited but only read in '{}' mode", entry.mode),
                }));
            }
            if has_active_error {
                risk_flags.push(serde_json::json!({
                    "type": "active_error",
                    "message": "File has an active compiler/linter error",
                }));
            }

            serde_json::json!({
                "path": entry.path,
                "kind": entry.kind,
                "tokens_sent": entry.sent_tokens,
                "tokens_original": entry.original_tokens,
                "compression_pct": compression_pct,
                "mode": entry.mode,
                "phi": entry.phi,
                "pinned": is_pinned,
                "last_accessed_ts": last_access_ts,
                "access_count": access_count,
                "eviction_score": (eviction_score * 1000.0).round() / 1000.0,
                "git_recency": (git_recency * 100.0).round() / 100.0,
                "editor_active": editor_active,
                "diagnostics": diag_details
                    .iter()
                    .map(|(line, sev, msg)| serde_json::json!({
                        "line": line,
                        "severity": match sev {
                            crate::core::diagnostics_store::Severity::Error => "error",
                            crate::core::diagnostics_store::Severity::Warning => "warning",
                        },
                        "message": msg,
                    }))
                    .collect::<Vec<_>>(),
                "source_trail": source_trail,
                "risk_flags": risk_flags,
                "state": entry.state,
            })
        })
        .collect();

    items.sort_by(|a, b| {
        let sa = a["eviction_score"].as_f64().unwrap_or(0.0);
        let sb = b["eviction_score"].as_f64().unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    let actions = build_action_recommendations(&items, band);

    let payload = serde_json::json!({
        "budget": {
            "window_size": window_size,
            "used": pressure.utilization * window_size as f64,
            "utilization": (pressure.utilization * 1000.0).round() / 1000.0,
            "remaining_tokens": pressure.remaining_tokens,
            "band": band,
            "recommendation": recommendation,
        },
        "items": items,
        "actions": actions,
        "summary": {
            "total_files": ledger.entries.len(),
            "total_tokens_sent": ledger.total_tokens_sent,
            "total_tokens_saved": ledger.total_tokens_saved,
            "pinned_count": pinned_paths.len(),
            "risk_count": items.iter().filter(|i| !i["risk_flags"].as_array().is_none_or(Vec::is_empty)).count(),
        },
    });

    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn build_source_trail(
    entry: &crate::core::context_ledger::LedgerEntry,
    radar: &crate::core::context_radar::ContextRadar,
    is_pinned: bool,
) -> Vec<serde_json::Value> {
    let mut trail: Vec<serde_json::Value> = Vec::new();

    if is_pinned {
        trail.push(serde_json::json!({
            "type": "pinned",
            "detail": "Pinned by user or policy",
        }));
    }

    if let Some(ref prov) = entry.provenance {
        trail.push(serde_json::json!({
            "type": "provenance",
            "tool": prov.tool,
            "agent": prov.agent_id,
            "client": prov.client_name,
            "ts": prov.timestamp,
        }));
    }

    let matching_events: Vec<&crate::core::context_radar::RadarEvent> = radar
        .events
        .iter()
        .filter(|e| {
            e.detail.as_deref().is_some_and(|d| d.contains(&entry.path))
                || e.tool_name
                    .as_deref()
                    .is_some_and(|t| t == "ctx_read" || t == "ctx_multi_read")
                    && e.detail.as_deref().is_some_and(|d| d.contains(&entry.path))
        })
        .collect();

    for ev in matching_events.iter().take(3) {
        trail.push(serde_json::json!({
            "type": "event",
            "event_type": ev.event_type,
            "tool": ev.tool_name,
            "ts": ev.ts,
            "tokens": ev.tokens,
        }));
    }

    trail
}

fn build_action_recommendations(items: &[serde_json::Value], band: &str) -> Vec<serde_json::Value> {
    if band == "green" {
        return Vec::new();
    }

    let mut actions: Vec<serde_json::Value> = Vec::new();
    let max_actions = match band {
        "red" => 5,
        "orange" => 3,
        _ => 2,
    };

    for item in items.iter().take(20) {
        if actions.len() >= max_actions {
            break;
        }

        let pinned = item["pinned"].as_bool().unwrap_or(false);
        if pinned {
            continue;
        }

        let tokens = item["tokens_sent"].as_u64().unwrap_or(0);
        let mode = item["mode"].as_str().unwrap_or("");
        let path = item["path"].as_str().unwrap_or("");
        let eviction_score = item["eviction_score"].as_f64().unwrap_or(0.0);
        // Never recommend compressing/evicting a file the build is failing on (#499).
        let has_error_flag = item["risk_flags"]
            .as_array()
            .is_some_and(|f| f.iter().any(|r| r["type"] == "active_error"));
        if has_error_flag {
            continue;
        }

        if mode == "full" && tokens > 500 {
            let estimated_savings = (tokens as f64 * 0.6).round() as u64;
            actions.push(serde_json::json!({
                "type": "compress",
                "path": path,
                "from_mode": "full",
                "to_mode": "map",
                "reason": "Full read can be compressed to map mode",
                "estimated_savings": estimated_savings,
            }));
        } else if eviction_score > 0.5 && tokens > 200 {
            actions.push(serde_json::json!({
                "type": "evict",
                "path": path,
                "reason": format!("Low relevance (score {:.2}), {} tokens", eviction_score, tokens),
                "estimated_savings": tokens,
            }));
        }
    }

    for item in items {
        if actions.len() >= max_actions {
            break;
        }
        let Some(flags) = item["risk_flags"].as_array() else {
            continue;
        };
        if flags.is_empty() {
            continue;
        }
        let path = item["path"].as_str().unwrap_or("");
        let mode = item["mode"].as_str().unwrap_or("");
        let has_error = flags.iter().any(|r| r["type"] == "active_error");
        // An error file that's only in context compressed needs a full read
        // to expose the failing region (#499); edited-after-compressed keeps
        // its original recommendation.
        let reason = if has_error && mode != "full" {
            "File has an active build error but only a compressed read in context — full read recommended"
        } else if has_error {
            continue;
        } else {
            "File was edited after compressed read — full read recommended"
        };
        actions.push(serde_json::json!({
            "type": "full_read",
            "path": path,
            "reason": reason,
            "estimated_savings": 0,
        }));
    }

    actions
}
