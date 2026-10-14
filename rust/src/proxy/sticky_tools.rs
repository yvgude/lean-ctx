//! Sticky CCR tool injection for provider prefix-cache stability.
//!
//! Once a conversation has seen at least one CCR handle, the `ctx_expand` tool
//! definition stays in `tools[]` for the remainder of the session, even on turns
//! with no active markers. This prevents the tool-list change from busting the
//! provider prompt-cache prefix.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

const MAX_TRACKED: usize = 4096;
const EXPAND_TOOL_NAME: &str = "ctx_expand";

fn active_sessions() -> &'static Mutex<HashSet<u64>> {
    static SESSIONS: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Mark a conversation as having used CCR.
pub fn mark_ccr_active(conv_id: u64) {
    if let Ok(mut guard) = active_sessions().lock() {
        if guard.len() >= MAX_TRACKED
            && !guard.contains(&conv_id)
            && let Some(&oldest) = guard.iter().next()
        {
            guard.remove(&oldest);
        }
        guard.insert(conv_id);
    }
}

/// Returns `true` if this conversation has ever used CCR.
pub fn is_ccr_active(conv_id: u64) -> bool {
    active_sessions().lock().is_ok_and(|g| g.contains(&conv_id))
}

fn expand_tool_definition() -> Value {
    json!({
        "name": EXPAND_TOOL_NAME,
        "description": "Retrieve the original uncompressed content of a compressed tool result by its tee handle or hash.",
        "input_schema": {
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The tee path or hash handle from the compressed output."
                }
            },
            "required": ["id"]
        }
    })
}

/// Ensure `ctx_expand` is present in the `tools` array when CCR is active.
/// Returns `true` if the tool was injected.
pub fn ensure_tool_present(conv_id: u64, doc: &mut Value) -> bool {
    if !is_ccr_active(conv_id) {
        return false;
    }

    let tools = match doc.get_mut("tools") {
        Some(Value::Array(arr)) => arr,
        Some(_) => return false,
        None => {
            doc["tools"] = Value::Array(Vec::new());
            doc["tools"]
                .as_array_mut()
                .expect("tools just initialized as array")
        }
    };

    let already_present = tools.iter().any(|t| {
        t.get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| n == EXPAND_TOOL_NAME)
    });

    if already_present {
        return false;
    }

    tools.push(expand_tool_definition());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // High conv IDs reserved for these unit tests. Parallel tests in compress.rs
    // and history_prune.rs call mark_ccr_active(0); clearing the global set
    // races with them, so each test marks its own ID instead.
    const NO_CCR: u64 = 0xAA01;
    const INJECT: u64 = 0xAA02;
    const DEDUP: u64 = 0xAA03;
    const STABLE: u64 = 0xAA04;
    const STICKY: u64 = 0xAA05;

    #[test]
    fn tool_not_injected_without_ccr() {
        let mut doc = json!({"tools": [], "messages": []});
        assert!(!ensure_tool_present(NO_CCR, &mut doc));
        {
            let found = &doc["tools"].as_array().unwrap();
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    fn tool_injected_after_ccr_activation() {
        mark_ccr_active(INJECT);
        assert!(is_ccr_active(INJECT));
        let mut doc = json!({"tools": [], "messages": []});
        assert!(ensure_tool_present(INJECT, &mut doc));
        assert_eq!(doc["tools"].as_array().unwrap().len(), 1);
        assert_eq!(doc["tools"][0]["name"], "ctx_expand");
    }

    #[test]
    fn tool_not_duplicated() {
        mark_ccr_active(DEDUP);
        let mut doc = json!({"tools": [expand_tool_definition()], "messages": []});
        assert!(!ensure_tool_present(DEDUP, &mut doc));
        assert_eq!(doc["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn tools_array_stays_stable_after_ccr_activation() {
        mark_ccr_active(STABLE);
        let existing = json!({"name": "other_tool", "description": "test"});

        let mut doc1 = json!({"tools": [existing.clone()], "messages": []});
        ensure_tool_present(STABLE, &mut doc1);
        let snap1 = serde_json::to_string(&doc1["tools"]).unwrap();

        let mut doc2 = json!({"tools": [existing], "messages": []});
        ensure_tool_present(STABLE, &mut doc2);
        let snap2 = serde_json::to_string(&doc2["tools"]).unwrap();

        assert_eq!(
            snap1, snap2,
            "tool list must be byte-identical across turns"
        );
    }

    #[test]
    fn sticky_survives_turn_without_markers() {
        mark_ccr_active(STICKY);
        assert!(is_ccr_active(STICKY));
        let mut doc = json!({"tools": [], "messages": []});
        assert!(ensure_tool_present(STICKY, &mut doc));
    }

    #[test]
    fn max_tracked_does_not_panic() {
        for i in 0..(MAX_TRACKED + 100) {
            mark_ccr_active(i as u64);
        }
        assert!(active_sessions().lock().unwrap().len() <= MAX_TRACKED);
    }
}
