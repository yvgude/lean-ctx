// SPDX-License-Identifier: Apache-2.0
use super::helpers::detect_project_root_for_dashboard;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    if path != "/api/context-risk" {
        return None;
    }

    let ledger = lean_ctx::core::context_ledger::ContextLedger::load();
    let session = lean_ctx::core::session::SessionState::load_latest();

    let edited_paths: Vec<String> = session
        .as_ref()
        .map(|s| s.files_touched.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default();

    let project_root = detect_project_root_for_dashboard();

    // Load overlays early so we can suppress warnings for paths the user
    // already promoted to "full" via the dashboard's "Read Full" button.
    let overlays = lean_ctx::core::context_overlay::OverlayStore::load_project(
        &std::path::PathBuf::from(&project_root),
    );

    let has_full_view_overlay = |path: &str| -> bool {
        let target = lean_ctx::core::context_field::ContextItemId::from_file(path);
        overlays.for_item(&target).iter().any(|o| {
            matches!(
                &o.operation,
                lean_ctx::core::context_overlay::OverlayOp::SetView(
                    lean_ctx::core::context_field::ViewKind::Full
                )
            )
        })
    };

    let mut warnings: Vec<serde_json::Value> = Vec::new();

    let mut files_read_full = 0usize;
    let mut files_read_compressed = 0usize;
    let mut files_edited_after_compressed: Vec<String> = Vec::new();

    for entry in &ledger.entries {
        let is_full = entry.mode == "full" || has_full_view_overlay(&entry.path);
        if is_full {
            files_read_full += 1;
        } else {
            files_read_compressed += 1;
        }

        if !is_full
            && edited_paths
                .iter()
                .any(|ep| entry.path.ends_with(ep) || ep.ends_with(&entry.path))
        {
            files_edited_after_compressed.push(entry.path.clone());
            warnings.push(serde_json::json!({
                "severity": "high",
                "path": entry.path,
                "mode": entry.mode,
                "message": format!("Edited file was only read in '{}' mode", entry.mode),
                "suggestion": "Consider a full read before editing to ensure complete context",
            }));
        }
    }

    let pinned_count = overlays
        .all()
        .iter()
        .filter(|o| {
            matches!(
                o.operation,
                lean_ctx::core::context_overlay::OverlayOp::Pin { .. }
            )
        })
        .count();

    let excluded_count = overlays
        .all()
        .iter()
        .filter(|o| {
            matches!(
                o.operation,
                lean_ctx::core::context_overlay::OverlayOp::Exclude { .. }
            )
        })
        .count();

    let payload = serde_json::json!({
        "warnings": warnings,
        "compression_health": {
            "files_read_full": files_read_full,
            "files_read_compressed": files_read_compressed,
            "files_edited_after_compressed": files_edited_after_compressed.len(),
            "potential_risk_files": files_edited_after_compressed,
        },
        "overlay_summary": {
            "pinned": pinned_count,
            "excluded": excluded_count,
        },
    });

    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    Some(("200 OK", "application/json", json))
}
