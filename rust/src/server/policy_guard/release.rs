// SPDX-License-Identifier: Apache-2.0
//! Final current-policy check, including lifecycle replay and response caches.
//! Never rewrite a finalized signed/cached representation: withhold it if the
//! current policy requires a different representation, then regenerate safely.

use super::{ActivePolicy, CallToolResult, ContentBlock, runtime};
use crate::core::policy::content::{MAX_PROTECTED_CONTENT_BYTES, evaluate_text};
use serde_json::Value;

pub(crate) fn release_result(tool: &str, result: CallToolResult) -> CallToolResult {
    let Some(policy) = runtime::active() else {
        return result;
    };
    let mut inspection = Inspection {
        policy: &policy,
        bytes: 0,
        nodes: 0,
    };
    let permitted = policy.tool_allowed(tool)
        && result.content.iter().all(|block| {
            block.as_text().is_some_and(|text| {
                inspection.text(&text.text)
                    && text
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
    if permitted {
        result
    } else {
        CallToolResult::error(vec![ContentBlock::text(
            "[POLICY BLOCKED] Result withheld by the current policy; a fresh authorized result is required.",
        )])
    }
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

    fn text(&mut self, text: &str) -> bool {
        self.bytes = self.bytes.saturating_add(text.len());
        self.nodes += 1;
        if self.bytes > MAX_PROTECTED_CONTENT_BYTES || self.nodes > 10_000 {
            return false;
        }
        let outcome = evaluate_text(text, self.policy);
        !outcome.blocked && outcome.text == text
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
