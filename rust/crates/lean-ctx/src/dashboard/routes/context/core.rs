// SPDX-License-Identifier: Apache-2.0
pub(super) fn get_route(path: &str) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/context-ledger" => Some(ledger()),
        "/api/context-field" => Some(field()),
        "/api/context-pressure" => Some(pressure()),
        _ => None,
    }
}

fn ledger() -> (&'static str, &'static str, String) {
    let ledger = lean_ctx::core::context_ledger::ContextLedger::load();
    let pressure = ledger.pressure();
    let payload = serde_json::json!({
        "window_size": ledger.window_size,
        "entries_count": ledger.entries.len(),
        "total_tokens_sent": ledger.total_tokens_sent,
        "total_tokens_saved": ledger.total_tokens_saved,
        "compression_ratio": ledger.compression_ratio(),
        "pressure": {
            "utilization": pressure.utilization,
            "remaining_tokens": pressure.remaining_tokens,
            "recommendation": format!("{:?}", pressure.recommendation),
        },
        "mode_distribution": ledger.mode_distribution(),
        "entries": ledger.entries.iter().take(50).collect::<Vec<_>>(),
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn field() -> (&'static str, &'static str, String) {
    let ledger = lean_ctx::core::context_ledger::ContextLedger::load();
    let field = lean_ctx::core::context_field::ContextField::new();
    let pressure = ledger.pressure();
    let effective_used = (pressure.utilization * ledger.window_size as f64).round() as usize;
    let budget = lean_ctx::core::context_field::TokenBudget {
        total: ledger.window_size,
        used: effective_used,
    };
    let items: Vec<serde_json::Value> = ledger
        .entries
        .iter()
        .map(|e| {
            let phi = e.phi.unwrap_or_else(|| {
                field.compute_phi(&lean_ctx::core::context_field::FieldSignals {
                    relevance: 0.3,
                    ..Default::default()
                })
            });
            serde_json::json!({
                "path": e.path,
                "phi": phi,
                "state": e.state,
                "view": e.active_view,
                "tokens": e.sent_tokens,
                "kind": e.kind,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "temperature": budget.temperature(),
        "budget_total": ledger.window_size,
        "budget_used": effective_used,
        "budget_remaining": pressure.remaining_tokens,
        "items": items,
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn pressure() -> (&'static str, &'static str, String) {
    let ledger = lean_ctx::core::context_ledger::ContextLedger::load();
    let pressure = ledger.pressure();
    let adjusted_saved = ledger.adjusted_total_saved();
    let eviction_candidates = ledger.eviction_candidates_by_phi(5);
    let payload = serde_json::json!({
        "utilization": pressure.utilization,
        "remaining_tokens": pressure.remaining_tokens,
        "recommendation": format!("{:?}", pressure.recommendation),
        "total_sent": ledger.total_tokens_sent,
        "total_saved_raw": ledger.total_tokens_saved,
        "total_saved_adjusted": adjusted_saved,
        "window_size": ledger.window_size,
        "eviction_candidates": eviction_candidates,
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}
