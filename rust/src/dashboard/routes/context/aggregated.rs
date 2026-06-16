pub(super) fn get_route(path: &str) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/context-summary" => Some(summary()),
        "/api/context-capabilities" => Some(capabilities()),
        "/api/context-history" => Some(history()),
        _ => None,
    }
}

/// Merges ledger + field + pressure into a single response.
fn summary() -> (&'static str, &'static str, String) {
    let ledger = crate::core::context_ledger::ContextLedger::load();
    let field = crate::core::context_field::ContextField::new();
    let pressure = ledger.pressure();
    let adjusted_saved = ledger.adjusted_total_saved();
    let eviction_candidates = ledger.eviction_candidates_by_phi(5);

    let effective_used = (pressure.utilization * ledger.window_size as f64).round() as usize;
    let budget = crate::core::context_field::TokenBudget {
        total: ledger.window_size,
        used: effective_used,
    };

    let items: Vec<serde_json::Value> = ledger
        .entries
        .iter()
        .take(50)
        .map(|e| {
            let phi = e.phi.unwrap_or_else(|| {
                field.compute_phi(&crate::core::context_field::FieldSignals {
                    relevance: 0.3,
                    ..Default::default()
                })
            });
            serde_json::json!({
                "path": e.path,
                "phi": phi,
                "state": e.state,
                "view": e.active_view,
                "mode": e.mode,
                "tokens": e.sent_tokens,
                "original_tokens": e.original_tokens,
                "kind": e.kind,
                "timestamp": e.timestamp,
                "access_count": e.access_count,
            })
        })
        .collect();

    let payload = serde_json::json!({
        "ledger": {
            "window_size": ledger.window_size,
            "entries_count": ledger.entries.len(),
            "total_tokens_sent": ledger.total_tokens_sent,
            "total_tokens_saved": ledger.total_tokens_saved,
            "total_saved_adjusted": adjusted_saved,
            "compression_ratio": ledger.compression_ratio(),
            "mode_distribution": ledger.mode_distribution(),
        },
        "pressure": {
            "utilization": pressure.utilization,
            "remaining_tokens": pressure.remaining_tokens,
            "recommendation": format!("{:?}", pressure.recommendation),
            "eviction_candidates": eviction_candidates,
        },
        "field": {
            "temperature": budget.temperature(),
            "budget_total": ledger.window_size,
            "budget_used": effective_used,
            "budget_remaining": pressure.remaining_tokens,
        },
        "items": items,
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

/// Merges client capabilities + dynamic tool state.
fn capabilities() -> (&'static str, &'static str, String) {
    let caps = crate::core::client_capabilities::load_persisted(86400)
        .unwrap_or_else(crate::core::client_capabilities::current);

    let dyn_tools = match crate::server::dynamic_tools::global().lock() {
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

    let payload = serde_json::json!({
        "client": {
            "client_id": caps.client_id,
            "tier": caps.tier(),
            "resources": caps.resources,
            "prompts": caps.prompts,
            "elicitation": caps.elicitation,
            "sampling": caps.sampling,
            "dynamic_tools": caps.dynamic_tools,
            "max_tools": caps.max_tools,
            "summary": caps.format_summary(),
        },
        "dynamic_tools": dyn_tools,
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

/// Merges introspect + radar + events + model + bounce.
fn history() -> (&'static str, &'static str, String) {
    let persisted = crate::proxy::introspect::load_persisted(300);
    let proxy_port = crate::proxy_setup::default_port();
    let addr = std::net::SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        proxy_port,
    );
    let proxy_running =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(100)).is_ok();

    let introspect = match persisted {
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
        .take(200)
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

    let rules_files: Vec<serde_json::Value> = radar
        .rules_tokens
        .files
        .iter()
        .map(|(path, tokens)| serde_json::json!({ "path": path, "tokens": tokens }))
        .collect();

    let detected = crate::hook_handlers::load_detected_model();
    let (model_name, model_window) = detected.unwrap_or_else(|| {
        let w = crate::core::context_radar::default_window_for_client(&client_id);
        ("unknown".to_string(), w)
    });

    let bounce = match crate::core::bounce_tracker::global().lock() {
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

    let payload = serde_json::json!({
        "introspect": introspect,
        "radar": {
            "breakdown": breakdown,
            "rules": {
                "files": rules_files,
                "total_tokens": radar.rules_tokens.total,
            },
            "events_total": radar.events.len(),
        },
        "events": recent_events,
        "model": {
            "model": model_name,
            "window_size": model_window,
            "client_id": client_id,
            "source": if model_name == "unknown" { "client_default" } else { "hook_detected" },
        },
        "bounce": bounce,
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}
