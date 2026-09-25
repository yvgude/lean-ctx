//! User-only value recaps for Claude-compatible hosts (`systemMessage`).
//!
//! A `systemMessage` is shown to the user and never enters the model's
//! context — the only hook channel the value surface may use. Stop counts the
//! host's turns and closes recap windows; SessionStart baselines a new host
//! session and offers the weekly digest or a last-session recap. All policy
//! (cadence, thresholds, wording) lives in [`crate::core::value::recap`].

use std::path::Path;

use crate::core::config::{Config, ValueDisplayMode};
use crate::core::value::{format::Style, recap, snapshot};

/// The `systemMessage` for this hook payload, if it has one to show.
pub(super) fn system_message(input: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let event = v.get("hook_event_name")?.as_str()?;
    if !matches!(event, "Stop" | "SessionStart") || !host_shows_system_message(&v) {
        return None;
    }
    let cfg = Config::load_arc().value_display.clone();
    if cfg.effective_mode() == ValueDisplayMode::Off {
        return None;
    }
    let host_session = v.get("session_id")?.as_str()?;
    let dir = snapshot::value_dir()?;
    let snap = v
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .and_then(|cwd| snapshot::load_for_dir_in(&dir, Path::new(cwd)));
    // A host message is plain text: no ANSI, glyphs unless LEAN_CTX_ASCII.
    let style = Style {
        color: false,
        ..Style::from_env()
    };
    let now = chrono::Utc::now();
    if event == "Stop" {
        // A Stop hook that already blocked once is a continuation of the
        // same turn, not a new one.
        if v.get("stop_hook_active")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return None;
        }
        return recap::on_turn_end(&dir, host_session, snap.as_ref(), &cfg, now, style);
    }
    let source = v.get("source").and_then(|s| s.as_str()).unwrap_or("");
    recap::on_session_start(&dir, host_session, source, snap.as_ref(), &cfg, now, style)
}

/// Claude Code and CodeBuddy render `systemMessage`. Cursor
/// (`conversation_id`) and Codex use other hook schemas and get nothing.
fn host_shows_system_message(v: &serde_json::Value) -> bool {
    v.get("conversation_id").is_none()
        && std::env::var_os("CODEX_THREAD_ID").is_none()
        && std::env::var_os("CODEX_PROFILE").is_none()
}

/// The single JSON object a hook prints: hosts parse exactly one, so the
/// SessionStart rules (`additionalContext`, model-visible) and the value recap
/// (`systemMessage`, user-only) must share it.
pub(super) fn hook_output(
    event: &str,
    context: Option<&str>,
    system_message: Option<&str>,
) -> Option<String> {
    let mut out = serde_json::Map::new();
    if let Some(msg) = system_message {
        out.insert("systemMessage".into(), msg.into());
    }
    if let Some(ctx) = context {
        out.insert(
            "hookSpecificOutput".into(),
            serde_json::json!({ "hookEventName": event, "additionalContext": ctx }),
        );
    }
    (!out.is_empty()).then(|| serde_json::Value::Object(out).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_start_output_carries_both_channels_in_one_object() {
        let json = hook_output("SessionStart", Some("rules"), Some("◆ lean-ctx · x")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["systemMessage"], "◆ lean-ctx · x");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "rules");
        assert!(!json.contains('\n'), "one line, one object");
    }

    #[test]
    fn the_recap_never_lands_in_additional_context() {
        let json = hook_output("Stop", None, Some("◆ lean-ctx · x")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("hookSpecificOutput").is_none());
        assert_eq!(v["systemMessage"], "◆ lean-ctx · x");
        assert_eq!(hook_output("Stop", None, None), None);
    }

    #[test]
    fn cursor_payloads_get_no_system_message() {
        let cursor = serde_json::json!({ "conversation_id": "c", "hook_event_name": "stop" });
        assert!(!host_shows_system_message(&cursor));
        assert_eq!(system_message(&cursor.to_string()), None);
    }

    #[test]
    fn other_events_get_no_system_message() {
        let prompt = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "s",
            "cwd": "/"
        });
        assert_eq!(system_message(&prompt.to_string()), None);
    }
}
