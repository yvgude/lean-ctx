// SPDX-License-Identifier: Apache-2.0
//! Host session state the observe hook records for the rest of the engine.
//!
//! The observe hook (`hook_handlers::observe`) learns which model and which
//! transcript the host is running from its payloads and persists them in the
//! data dir; the context radar, ledger, telemetry and conversation reader load
//! them back. Both directions live here so the file formats have one owner and
//! engine readers do not depend on the hook layer.

/// Persist the host's current model (skips background models) to
/// `detected_model.json`.
pub fn persist_detected_model(model: &str) {
    if crate::core::model_registry::is_background_model(model) {
        return;
    }

    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return;
    };
    let path = data_dir.join("detected_model.json");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let window = crate::core::model_registry::context_window_for_model(model);
    let payload = serde_json::json!({
        "model": model,
        "window_size": window,
        "detected_at": ts,
    });
    if let Ok(json) = serde_json::to_string_pretty(&payload) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// The model the host last reported, with its context window. `None` when
/// nothing was recorded or the record is older than two hours.
pub fn load_detected_model() -> Option<(String, usize)> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let path = data_dir.join("detected_model.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    let model = v.get("model")?.as_str()?.to_string();
    let window = v.get("window_size")?.as_u64()? as usize;
    let detected_at = v.get("detected_at")?.as_u64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.saturating_sub(detected_at) > 7200 {
        return None;
    }
    Some((model, window))
}

/// Persist the host's active transcript path and session identity to
/// `active_transcript.json`.
pub fn persist_transcript_path(
    path: &str,
    conversation_id: Option<&str>,
    session_id: Option<&str>,
) {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return;
    };
    let meta_path = data_dir.join("active_transcript.json");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let payload = serde_json::json!({
        "transcript_path": path,
        "conversation_id": conversation_id,
        "session_id": session_id,
        "updated_at": ts,
    });
    if let Ok(json) = serde_json::to_string_pretty(&payload) {
        let tmp = meta_path.with_extension("tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &meta_path);
        }
    }
}

pub fn load_active_transcript() -> Option<(String, Option<String>)> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let path = data_dir.join("active_transcript.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    let tp = v.get("transcript_path")?.as_str()?.to_string();
    // Prefer Cursor's `conversation_id`; fall back to the host `session_id`
    // (Claude Code / Codex / CodeBuddy) so the read-cache still has a per-session
    // identity to scope stubs against (#1004).
    let conv = v
        .get("conversation_id")
        .and_then(|c| c.as_str())
        .or_else(|| v.get("session_id").and_then(|s| s.as_str()))
        .map(String::from);
    let updated = v.get("updated_at")?.as_u64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.saturating_sub(updated) > 7200 {
        return None;
    }
    Some((tp, conv))
}
