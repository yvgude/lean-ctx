// SPDX-License-Identifier: Apache-2.0
//! `/api/signals` — the Live Signals panel data source (#505, #507).
//!
//! Surfaces what the closed-loop signal stores currently know: editor focus,
//! active build diagnostics, git working set, per-path bounce memory, the
//! cumulative auto-mode decision sources, and the two learning trends
//! (bounce rate, output echo). Everything here is read from disk or cheap
//! subprocess calls — the dashboard runs in its own process and must not
//! depend on MCP-server in-memory state.

use crate::dashboard::routes::helpers::detect_project_root_for_dashboard;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    if path != "/api/signals" {
        return None;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    // Editor focus (#500): fresh = within the ranking freshness window.
    let editor = {
        let raw = lean_ctx::core::editor_signal::load_raw_for_status();
        match raw {
            Some(s) if s.active_file.is_some() => {
                let age = now.saturating_sub(s.updated_at);
                serde_json::json!({
                    "active_file": s.active_file,
                    "age_secs": age,
                    "fresh": age <= lean_ctx::core::editor_signal::FRESHNESS_SECS,
                })
            }
            _ => serde_json::json!({ "active_file": null, "age_secs": null, "fresh": false }),
        }
    };

    // Active diagnostics (#499): TTL/pruning happens inside the store.
    let diagnostics = {
        let snap = lean_ctx::core::diagnostics_store::snapshot_detailed();
        let errors = snap
            .iter()
            .filter(|(_, _, s, _, _)| *s == lean_ctx::core::diagnostics_store::Severity::Error)
            .count();
        let warnings = snap.len() - errors;
        let mut files: Vec<&str> = snap.iter().map(|(p, _, _, _, _)| p.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        files.truncate(8);
        let details: Vec<serde_json::Value> = snap
            .iter()
            .take(20)
            .map(|(path, line, sev, msg, tool)| {
                let short = path.rsplit('/').next().unwrap_or(path);
                serde_json::json!({
                    "file": short,
                    "path": path,
                    "line": line,
                    "severity": if *sev == lean_ctx::core::diagnostics_store::Severity::Error { "error" } else { "warning" },
                    "message": msg,
                    "tool": tool,
                })
            })
            .collect();
        serde_json::json!({ "errors": errors, "warnings": warnings, "files": files, "details": details })
    };

    // Git working set (#497).
    let git = {
        let project_root = detect_project_root_for_dashboard();
        let signals = lean_ctx::core::git_signals::collect(&project_root);
        let uncommitted = signals.recency.values().filter(|v| **v >= 1.0).count();
        serde_json::json!({
            "uncommitted": uncommitted,
            "churned": signals.churn.len(),
        })
    };

    // Per-path bounce memory (#496).
    let (tracked, forced) = lean_ctx::core::path_mode_memory::disk_summary();
    let bounce_memory = serde_json::json!({
        "tracked_paths": tracked,
        "forced_full_paths": forced,
    });

    // Cumulative auto-mode decision sources (#496/#505) — flushed by the
    // MCP/CLI processes, read here from disk.
    let auto_sources: Vec<serde_json::Value> =
        lean_ctx::core::auto_mode_resolver::persisted_source_counts()
            .into_iter()
            .map(|(s, n)| serde_json::json!([s, n]))
            .collect();

    // Learning trends (#507), last 14 days.
    let bounce_trend: Vec<serde_json::Value> =
        lean_ctx::core::savings_ledger::daily_bounce_trend(14)
            .into_iter()
            .map(|(d, b, r)| serde_json::json!([d, b, r]))
            .collect();
    let echo_trend: Vec<serde_json::Value> = lean_ctx::core::output_echo::load_stats()
        .daily_trend(14)
        .into_iter()
        .map(|(d, ratio, n)| serde_json::json!([d, (ratio * 1000.0).round() / 1000.0, n]))
        .collect();

    let payload = serde_json::json!({
        "editor": editor,
        "diagnostics": diagnostics,
        "git": git,
        "bounce_memory": bounce_memory,
        "auto_mode_sources": auto_sources,
        "bounce_trend": bounce_trend,
        "echo_trend": echo_trend,
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    Some(("200 OK", "application/json", json))
}
