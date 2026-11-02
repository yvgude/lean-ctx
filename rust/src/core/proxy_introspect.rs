// SPDX-License-Identifier: Apache-2.0
//! The proxy's persisted request introspection: the proxy process writes
//! `proxy-introspect.json` (`proxy::introspect`), `gain` and the dashboard read it.

/// Load persisted proxy introspection data from disk.
/// Returns `None` if the file doesn't exist or is stale (> `max_age_secs`).
pub fn load_persisted(max_age_secs: u64) -> Option<serde_json::Value> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let path = data_dir.join("proxy-introspect.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;

    let ts = val
        .get("ts")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.saturating_sub(ts) > max_age_secs {
        return None;
    }
    Some(val)
}
