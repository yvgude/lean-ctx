// SPDX-License-Identifier: Apache-2.0
//! Mirror of a host's own agent-to-agent message (Claude Code `SendMessage`)
//! onto the local bus scratchpad, so `ctx_agent read` and the dashboard see
//! direct traffic the host delivers outside lean-ctx.
//!
//! - Local only: the entry is stored like any `ctx_agent post` — admitted
//!   (redaction, withholding) and bound before persistence — with privacy
//!   `Private`, so the Hive relay never forwards it, whatever its recipient.
//!   These are conversations between the machine's own agents.
//! - Secrets are redacted on the FULL text before the size cap, so a cut can
//!   never leave an unrecognisable secret prefix behind.
//! - One entry per tool call: the host's `tool_use_id` is the idempotency
//!   key, so a hook that fires twice (global + project settings, a retry)
//!   writes once.

use super::AgentRegistry;
use crate::core::a2a::message::{MessagePriority, PrivacyLevel};

/// Bus category of a mirrored host message.
pub const HOST_MIRROR_CATEGORY: &str = "direct";
/// Largest mirrored body in bytes, before the truncation marker.
pub const HOST_MIRROR_MAX_BYTES: usize = 8 * 1024;
const KEY_PREFIX: &str = "host-mirror:";

/// One host message to mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostMessage {
    pub project_root: String,
    pub from_agent: String,
    pub to_agent: String,
    pub body: String,
    pub tool_use_id: String,
}

/// `text` cut to at most `max` bytes on a char boundary, with a marker
/// naming how many bytes were dropped. Shorter text is returned unchanged.
#[must_use]
pub fn cap_host_message(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n[lean-ctx: truncated, {} bytes omitted]",
        &text[..cut],
        text.len() - cut
    )
}

/// Store `message` on the local bus; returns the entry id (the first id again
/// for a repeated `tool_use_id`). Errors are content-free and meant to be
/// dropped by a fail-open caller.
pub fn mirror_host_message(message: &HostMessage) -> Result<String, String> {
    let redacted = crate::core::redaction::redact_text(&message.body);
    let body = cap_host_message(&redacted, HOST_MIRROR_MAX_BYTES);
    let key = format!("{KEY_PREFIX}{}", message.tool_use_id);
    AgentRegistry::mutate_locked(|registry| {
        registry.post_message_keyed(
            Some(&message.project_root),
            &message.from_agent,
            Some(&message.to_agent),
            HOST_MIRROR_CATEGORY,
            &body,
            PrivacyLevel::Private,
            MessagePriority::Normal,
            None,
            Some(&key),
        )
    })
    .and_then(|(_, posted)| posted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(tool_use_id: &str, body: &str, root: &str) -> HostMessage {
        HostMessage {
            project_root: root.to_string(),
            from_agent: "claude:s1".to_string(),
            to_agent: "worker".to_string(),
            body: body.to_string(),
            tool_use_id: tool_use_id.to_string(),
        }
    }

    fn mirrored() -> Vec<super::super::ScratchpadEntry> {
        AgentRegistry::load_or_create()
            .scratchpad
            .into_iter()
            .filter(|e| e.category == HOST_MIRROR_CATEGORY)
            .collect()
    }

    #[test]
    fn same_tool_use_id_is_stored_once() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let first = mirror_host_message(&message("toolu_1", "build green", &root)).unwrap();
        let again = mirror_host_message(&message("toolu_1", "build green", &root)).unwrap();
        assert_eq!(first, again, "a repeated hook returns the first entry");
        mirror_host_message(&message("toolu_2", "build green", &root)).unwrap();
        let entries = mirrored();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].to_agent.as_deref(), Some("worker"));
        assert_eq!(entries[0].from_agent, "claude:s1");
        assert_eq!(
            entries[0].privacy,
            PrivacyLevel::Private,
            "mirrors never reach Hive"
        );
        assert_eq!(entries[0].message, "build green");
    }

    #[test]
    fn oversized_body_is_capped_with_a_marker() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let body = "é".repeat(HOST_MIRROR_MAX_BYTES);
        mirror_host_message(&message("toolu_big", &body, &root)).unwrap();
        let stored = &mirrored()[0].message;
        assert!(
            stored.len() < HOST_MIRROR_MAX_BYTES + 64,
            "{}",
            stored.len()
        );
        assert!(stored.ends_with(&format!(
            "[lean-ctx: truncated, {HOST_MIRROR_MAX_BYTES} bytes omitted]"
        )));
        assert_eq!(cap_host_message("short", 10), "short");
        assert_eq!(
            cap_host_message("aé", 2),
            "a\n[lean-ctx: truncated, 2 bytes omitted]"
        );
    }

    #[test]
    fn a_credential_never_reaches_the_store() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
        // The secret straddles the cap: redaction runs on the full text first.
        let body = format!("{}aws_key = {key}", "x".repeat(HOST_MIRROR_MAX_BYTES - 16));
        mirror_host_message(&message("toolu_s", &body, &root)).unwrap();
        let stored = &mirrored()[0].message;
        assert!(!stored.contains(key));
        assert!(
            !stored.contains(&key[..8]),
            "no secret prefix left by the cut"
        );
        let raw = std::fs::read_to_string(
            crate::core::data_dir::lean_ctx_data_dir()
                .unwrap()
                .join("agents/registry.json"),
        )
        .unwrap();
        assert!(!raw.contains(key));
    }
}
