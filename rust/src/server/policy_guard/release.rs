// SPDX-License-Identifier: Apache-2.0
//! Final current-policy check, including lifecycle replay and response caches.
//! Never rewrite a finalized signed/cached representation: withhold it if the
//! current policy requires a different representation, then regenerate safely.

//!
//! Two layers, both mandatory for every tool result (registered tools,
//! `ctx_call` invokers and bridged foreign tools all return through here):
//! the project policy pack when one is active, then the context gateway
//! ([`gateway`]), which runs whether or not a pack exists.

//!
//! One exception to "never rewrite", shared with the gateway half: a PLAIN
//! text block (no block metadata or annotations, in a result without
//! structured content or result metadata) whose only pending change is the
//! built-in secret mask is delivered with the secret masked — with a pack
//! exactly as without one. A secret alone never withholds a result. Whatever
//! a rule of the pack itself would block or rewrite, and every bound part, is
//! withheld as before.

use super::{ActivePolicy, CallToolResult, ContentBlock, runtime};
use crate::core::policy::content::{
    MAX_PROTECTED_CONTENT_BYTES, ReleaseCheck, evaluate_text, release_check,
};
use serde_json::Value;

mod gateway;

/// The release point for both channels of a tool call. Under an active pack
/// the dispatcher replaces every error with a fixed text; without one the
/// error's message and data pass the gateway like a result (AP10-C1).
pub(crate) fn release_outcome(
    tool: &str,
    outcome: Result<CallToolResult, rmcp::ErrorData>,
) -> Result<CallToolResult, rmcp::ErrorData> {
    outcome
        .map(|result| release_result(tool, result))
        .map_err(gateway::release_error)
}

pub(crate) fn release_result(tool: &str, result: CallToolResult) -> CallToolResult {
    let Some(policy) = runtime::active() else {
        return gateway::release(result);
    };
    // The pack decides on the original, before the gateway can mask a signal.
    let Some(mask_pending) = policy_release(&policy, tool, &result, true) else {
        return withheld();
    };
    // The gateway sees the original too: its own rule for a secret (mask,
    // block) is not pre-empted by the pack's mask.
    let mut released = gateway::release(result);
    if !mask_pending {
        return released;
    }
    // What the gateway left in place (switched off, secrets not redacted) the
    // pack still masks. Same masker, same labels: text the gateway masked is
    // unchanged by this pass.
    for block in &mut released.content {
        if let Some(text) = block.as_text() {
            let masked = crate::core::redaction::redact_text(&text.text);
            if masked != text.text {
                *block = ContentBlock::text(masked);
            }
        }
    }
    // The delivered representation is one the pack leaves exactly as it is.
    match policy_release(&policy, tool, &released, false) {
        Some(false) => released,
        _ => withheld(),
    }
}

fn withheld() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        "[POLICY BLOCKED] Result withheld by the current policy; a fresh authorized result is required.",
    )])
}

/// `None`: withheld. `Some(mask_pending)`: permitted; `mask_pending` says a
/// plain text block still carries a built-in secret to mask (only reported
/// with `maskable`; without it such a block is withheld like any rewrite).
fn policy_release(
    policy: &ActivePolicy,
    tool: &str,
    result: &CallToolResult,
    maskable: bool,
) -> Option<bool> {
    let mut inspection = Inspection {
        policy,
        bytes: 0,
        nodes: 0,
    };
    let bound = result.structured_content.is_some() || result.meta.is_some();
    let mut mask_pending = false;
    let permitted = policy.tool_allowed(tool)
        && result.content.iter().all(|block| {
            block.as_text().is_some_and(|text| {
                let plain = maskable && !bound && text.meta.is_none() && text.annotations.is_none();
                (if plain {
                    inspection.plain_text(&text.text, &mut mask_pending)
                } else {
                    inspection.text(&text.text)
                }) && text
                    .meta
                    .as_ref()
                    .is_none_or(|meta| inspection.metadata(meta))
                    && text.annotations.as_ref().is_none_or(|annotations| {
                        annotations
                            .audience
                            .as_ref()
                            .is_none_or(|roles| roles.len() <= 10_000)
                            && serde_json::to_value(annotations)
                                .is_ok_and(|value| inspection.value(&value, 0))
                    })
            })
        })
        && result
            .structured_content
            .as_ref()
            .is_none_or(|value| inspection.value(value, 0))
        && result
            .meta
            .as_ref()
            .is_none_or(|meta| inspection.metadata(meta));
    permitted.then_some(mask_pending)
}

struct Inspection<'a> {
    policy: &'a ActivePolicy,
    bytes: usize,
    nodes: usize,
}

impl Inspection<'_> {
    fn metadata(&mut self, meta: &rmcp::model::Meta) -> bool {
        meta.0
            .iter()
            .all(|(key, value)| self.text(key) && self.value(value, 0))
    }

    fn bounded(&mut self, text: &str) -> bool {
        self.bytes = self.bytes.saturating_add(text.len());
        self.nodes += 1;
        self.bytes <= MAX_PROTECTED_CONTENT_BYTES && self.nodes <= 10_000
    }

    fn text(&mut self, text: &str) -> bool {
        if !self.bounded(text) {
            return false;
        }
        let outcome = evaluate_text(text, self.policy);
        !outcome.blocked && outcome.text == text
    }

    /// A plain text block: also permitted when only the built-in secret mask
    /// is outstanding, which is then recorded in `mask_pending`.
    fn plain_text(&mut self, text: &str, mask_pending: &mut bool) -> bool {
        if !self.bounded(text) {
            return false;
        }
        match release_check(text, self.policy) {
            ReleaseCheck::Unchanged => true,
            ReleaseCheck::BuiltinSecretOnly => {
                *mask_pending = true;
                true
            }
            ReleaseCheck::Refused => false,
        }
    }

    fn value(&mut self, value: &Value, depth: usize) -> bool {
        self.nodes += 1;
        if self.nodes > 10_000 || depth > 64 {
            return false;
        }
        match value {
            Value::String(text) => self.text(text),
            Value::Array(values) => values.iter().all(|value| self.value(value, depth + 1)),
            Value::Object(values) => values
                .iter()
                .all(|(key, value)| self.text(key) && self.value(value, depth + 1)),
            _ => self.text(&value.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::policy::{parse, resolve};

    fn mask() -> runtime::TestPolicyOverride {
        let source = "name = 'release'\nversion = '1.0.0'\ndescription = 'test'\n[redaction]\ncustomer = 'CUS-[0-9]{4}'\n";
        runtime::TestPolicyOverride::set(Some(resolve(&parse(source).unwrap()).unwrap()))
    }

    #[test]
    fn stale_finalized_text_is_withheld_and_safe_result_preserved() {
        let _policy = mask();
        let unsafe_result = CallToolResult::success(vec![ContentBlock::text("CUS-1234")]);
        let blocked = release_result("ctx_read", unsafe_result);
        assert_eq!(blocked.is_error, Some(true));
        assert!(
            !serde_json::to_string(&blocked)
                .unwrap()
                .contains("CUS-1234")
        );
        let safe = CallToolResult::success(vec![ContentBlock::text("[REDACTED:customer]")]);
        assert_eq!(
            serde_json::to_value(release_result("ctx_read", safe.clone())).unwrap(),
            serde_json::to_value(safe).unwrap()
        );
    }

    // Split so the repository's secret scanners never see a literal key.
    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    fn config(toml: &str) -> crate::core::data_dir::IsolatedDataDir {
        let data = crate::core::data_dir::isolated_data_dir();
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(dir.join("config.toml"), toml).expect("config");
        data
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Under an active pack a plain text block with a built-in secret was
    /// withheld whole ("the pack would rewrite it"), while without a pack the
    /// gateway half masked it. Now it is delivered with the secret masked once
    /// under the one label of its kind, whether or not the gateway masks; the
    /// released result is a fixed point. A rule that withholds still does.
    #[test]
    fn a_built_in_secret_in_plain_text_is_masked_under_a_pack_never_withheld() {
        let raw = format!("aws_key = {AWS_KEY}\nrest of the output\n");
        let masked = "aws_key = [REDACTED:AWS key]\nrest of the output\n";
        for toml in [
            "",
            "[context_gateway]\nmode = \"governed\"\n",
            // The gateway does not mask here; the pack still does.
            "[context_gateway]\nenabled = false\n",
        ] {
            let _data = config(toml);
            let _policy = mask();
            let released = release_result(
                "ctx_graph",
                CallToolResult::success(vec![ContentBlock::text(&raw)]),
            );
            assert_ne!(released.is_error, Some(true), "{toml}: withheld");
            let delivered = text_of(&released);
            assert!(delivered.ends_with(masked), "{toml}: {delivered}");
            assert_eq!(delivered.matches("[REDACTED:").count(), 1, "{delivered}");
            // Idempotent: releasing the released result changes nothing.
            assert_eq!(
                text_of(&release_result("ctx_graph", released)),
                delivered,
                "{toml}"
            );

            // A rule of the pack itself is not a secret mask: still withheld.
            let own_rule = release_result(
                "ctx_graph",
                CallToolResult::success(vec![ContentBlock::text(format!("{raw}CUS-1234\n"))]),
            );
            assert_eq!(own_rule.is_error, Some(true), "{toml}");
            assert!(!text_of(&own_rule).contains(AWS_KEY));

            // A bound copy cannot be rewritten: withheld, as before.
            let mut bound = CallToolResult::success(vec![ContentBlock::text("ok")]);
            bound.structured_content = Some(serde_json::json!({ "rows": [raw] }));
            let bound = release_result("ctx_graph", bound);
            assert_eq!(bound.is_error, Some(true), "{toml}");
            assert!(bound.structured_content.is_none());
        }

        // A gateway rule that blocks secrets withholds, with a pack too.
        let _data = config("[context_gateway]\nmode = \"governed\"\nsecrets = \"block\"\n");
        let _policy = mask();
        let blocked = release_result(
            "ctx_graph",
            CallToolResult::success(vec![ContentBlock::text(&raw)]),
        );
        assert_eq!(blocked.is_error, Some(true));
        let notice = text_of(&blocked);
        assert!(notice.contains("secret.blocked"), "{notice}");
        assert!(!notice.contains(AWS_KEY) && !notice.contains("rest of the output"));
    }

    #[test]
    fn structured_and_metadata_copies_cannot_bypass_release_inspection() {
        let _policy = mask();
        for metadata in [false, true] {
            let mut result = CallToolResult::success(vec![ContentBlock::text("safe")]);
            let extra = serde_json::json!({"nested": [{"record": "line one\nCUS-1234"}]});
            if metadata {
                let mut meta = rmcp::model::Meta::new();
                meta.0.insert("fixture".into(), extra);
                result.meta = Some(meta);
            } else {
                result.structured_content = Some(extra);
            }
            let blocked = release_result("ctx_read", result);
            assert_eq!(blocked.is_error, Some(true));
            assert!(blocked.meta.is_none() && blocked.structured_content.is_none());
            assert!(
                !serde_json::to_string(&blocked)
                    .unwrap()
                    .contains("CUS-1234")
            );
        }
    }

    #[test]
    fn text_block_metadata_cannot_bypass_release_inspection() {
        let _policy = mask();
        let mut meta = rmcp::model::Meta::new();
        meta.0
            .insert("customer".into(), serde_json::json!("CUS-1234"));
        let block = ContentBlock::Text(rmcp::model::TextContent::new("safe").with_meta(meta));
        let blocked = release_result("ctx_read", CallToolResult::success(vec![block]));
        assert_eq!(blocked.is_error, Some(true));
        assert!(
            !serde_json::to_string(&blocked)
                .unwrap()
                .contains("CUS-1234")
        );
    }

    #[test]
    fn current_tool_revocation_denies_previously_safe_results() {
        let source = "name = 'release'\nversion = '1.0.0'\ndescription = 'test'\n[context]\ndeny_tools = ['ctx_read']\n";
        let _policy =
            runtime::TestPolicyOverride::set(Some(resolve(&parse(source).unwrap()).unwrap()));
        assert_eq!(
            release_result(
                "ctx_read",
                CallToolResult::success(vec![ContentBlock::text("safe")])
            )
            .is_error,
            Some(true)
        );
    }
}
