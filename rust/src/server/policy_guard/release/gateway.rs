// SPDX-License-Identifier: Apache-2.0
//! Context-gateway half of the release point (G01, G05).
//!
//! A tool that read a source through the admitted reader already returns
//! admitted text; releasing it again changes no byte (#498). A tool that read
//! or assembled content on its own is caught here: a plain text block is
//! delivered redacted with the content-free gateway notice in front, and
//! everything that cannot be rewritten without breaking a binding — block
//! metadata, structured content, result metadata, non-text blocks — is
//! verified only, so a finding there withholds the whole result.

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::Value;

use crate::core::context_admission::carrier::{Release, ReleaseGate, Withheld};

const MAX_NODES: usize = 10_000;
const MAX_DEPTH: usize = 64;

pub(super) fn release(mut result: CallToolResult) -> CallToolResult {
    let gate = ReleaseGate::current();
    if gate.inactive() {
        return result;
    }
    let mut check = Check {
        gate: &gate,
        nodes: 0,
        media: false,
    };
    // Bound parts first: a result that carries them is never rewritten.
    let bound = result.structured_content.is_some() || result.meta.is_some();
    let verified = result
        .structured_content
        .as_ref()
        .map_or(Ok(()), |value| check.value(value, 0, None))
        .and_then(|()| {
            result
                .meta
                .as_ref()
                .map_or(Ok(()), |meta| check.object(meta.0.iter(), 0))
        });
    if let Err(withheld) = verified {
        return blocked(&withheld);
    }
    for block in &mut result.content {
        match check.block(block, bound) {
            Ok(Some(replacement)) => *block = replacement,
            Ok(None) => {}
            Err(withheld) => return blocked(&withheld),
        }
    }
    result
}

/// The error channel (AP10-C1): a tool error's message and data reach the
/// host like a result does. Neither is a rewritable text block, so any
/// finding withholds both; the code stays and the typed, content-free notice
/// takes the message's place.
pub(super) fn release_error(error: rmcp::ErrorData) -> rmcp::ErrorData {
    let gate = ReleaseGate::current();
    if gate.inactive() {
        return error;
    }
    let mut check = Check {
        gate: &gate,
        nodes: 0,
        media: false,
    };
    let verified = check.verify(&error.message).and_then(|()| {
        error
            .data
            .as_ref()
            .map_or(Ok(()), |data| check.value(data, 0, None))
    });
    match verified {
        Ok(()) => error,
        Err(withheld) => rmcp::ErrorData::new(error.code, withheld.notice(), None),
    }
}

fn blocked(withheld: &Withheld) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(withheld.notice())])
}

struct Check<'a> {
    gate: &'a ReleaseGate,
    nodes: usize,
    /// Inside a non-text content block, whose `data`/`blob` is a payload no
    /// detector reads (the media rule decides).
    media: bool,
}

impl Check<'_> {
    /// `Ok(Some(block))`: deliver this redacted block instead.
    fn block(
        &mut self,
        block: &ContentBlock,
        bound: bool,
    ) -> Result<Option<ContentBlock>, Withheld> {
        let Some(text) = block.as_text() else {
            // Image, audio, resource: no detector reads the payload, and any
            // text riding along (URI, embedded resource text) is verified.
            // Only here is a `data`/`blob` string a media payload; anywhere
            // else (structured content, metadata, error data) it is text.
            let value = serde_json::to_value(block)
                .map_err(|_| Withheld::new(&["coverage.unsupported_media"]))?;
            self.media = true;
            let checked = self.value(&value, 0, None);
            self.media = false;
            return checked.map(|()| None);
        };
        let annotated = text.meta.is_some() || text.annotations.is_some();
        if let Some(meta) = &text.meta {
            self.object(meta.0.iter(), 0)?;
        }
        if let Some(annotations) = &text.annotations {
            let value = serde_json::to_value(annotations)
                .map_err(|_| Withheld::new(&["coverage.partial"]))?;
            self.value(&value, 0, None)?;
        }
        match self.gate.text(&text.text) {
            Release::Unchanged => Ok(None),
            Release::Withheld(withheld) => Err(withheld),
            Release::Redacted { .. } if bound || annotated => {
                Err(Withheld::new(&["release.unadmitted_content"]))
            }
            Release::Redacted { text, notice } => {
                Ok(Some(ContentBlock::text(format!("{notice}\n{text}"))))
            }
        }
    }

    fn object<'v>(
        &mut self,
        entries: impl Iterator<Item = (&'v String, &'v Value)>,
        depth: usize,
    ) -> Result<(), Withheld> {
        for (key, value) in entries {
            self.verify(key)?;
            self.value(value, depth + 1, Some(key))?;
        }
        Ok(())
    }

    fn value(&mut self, value: &Value, depth: usize, key: Option<&str>) -> Result<(), Withheld> {
        self.nodes += 1;
        if self.nodes > MAX_NODES || depth > MAX_DEPTH {
            // Not inspectable to the end: the media rule decides by mode.
            return self.gate.media(0);
        }
        match value {
            Value::String(text) if self.media && matches!(key, Some("data" | "blob")) => {
                self.gate.media(text.len() as u64)
            }
            Value::String(text) => self.verify(text),
            Value::Array(items) => items
                .iter()
                .try_for_each(|item| self.value(item, depth + 1, None)),
            Value::Object(map) => self.object(map.iter(), depth),
            Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        }
    }

    /// A string that may not be rewritten: any finding withholds.
    fn verify(&mut self, text: &str) -> Result<(), Withheld> {
        match self.gate.text(text) {
            Release::Unchanged => Ok(()),
            Release::Withheld(withheld) => Err(withheld),
            Release::Redacted { .. } => Err(Withheld::new(&["release.unadmitted_content"])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::release_result;
    use super::*;

    // Split so the repository's secret scanners never see a literal key.
    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    fn isolated(config: &str) -> crate::core::data_dir::IsolatedDataDir {
        let data = crate::core::data_dir::isolated_data_dir();
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(dir.join("config.toml"), config).expect("config");
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

    /// The universal release point without any policy pack: before, a result
    /// passed through untouched when no pack was active.
    #[test]
    fn unadmitted_tool_text_is_redacted_or_withheld_by_mode_and_admitted_text_is_untouched() {
        let raw = format!("aws_key = {AWS_KEY}\nrest of the output\n");

        {
            let _data = isolated("");
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let clean = CallToolResult::success(vec![ContentBlock::text("fn main() {}\n")]);
            assert_eq!(
                serde_json::to_value(release_result("ctx_graph", clean.clone())).unwrap(),
                serde_json::to_value(&clean).unwrap(),
                "allowed content keeps its bytes"
            );

            // Developer: delivered redacted with the typed notice.
            let released = release_result(
                "ctx_graph",
                CallToolResult::success(vec![ContentBlock::text(&raw)]),
            );
            let delivered = text_of(&released);
            assert!(!delivered.contains(AWS_KEY), "old code returned the key");
            assert!(delivered.contains("secret(s) redacted"), "{delivered}");
            assert!(delivered.contains("rest of the output"));
            // Idempotent: releasing the released result changes nothing.
            assert_eq!(text_of(&release_result("ctx_graph", released)), delivered);

            // A copy in structured content cannot be rewritten: withheld.
            let mut structured = CallToolResult::success(vec![ContentBlock::text("ok")]);
            structured.structured_content = Some(serde_json::json!({"rows": [raw]}));
            let blocked = release_result("ctx_graph", structured);
            assert_eq!(blocked.is_error, Some(true));
            assert!(blocked.structured_content.is_none());
            assert!(text_of(&blocked).contains("release.unadmitted_content"));
        }

        // Governed with blocking secrets: nothing is delivered.
        let _data = isolated("[context_gateway]\nmode = \"governed\"\nsecrets = \"block\"\n");
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let blocked = release_result(
            "ctx_graph",
            CallToolResult::success(vec![ContentBlock::text(&raw)]),
        );
        assert_eq!(blocked.is_error, Some(true));
        let notice = text_of(&blocked);
        assert!(notice.contains("secret.blocked"), "{notice}");
        assert!(!notice.contains(AWS_KEY) && !notice.contains("rest of the output"));
    }

    /// AP10-C1: without a pack a tool error passed the release point
    /// untouched, so a secret echoed into its message or data reached the
    /// host. Now any finding there withholds both; the code is kept.
    #[test]
    fn a_secret_in_a_tool_error_is_withheld_without_a_pack() {
        let _data = isolated("");
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let clean = rmcp::ErrorData::invalid_params("path: not found", None);
        assert_eq!(
            super::super::release_outcome("ctx_git_read", Err(clean.clone())),
            Err(clean)
        );
        for (message, data) in [
            (format!("ctx_git_read failed: {AWS_KEY}"), None),
            (
                "ctx_git_read failed".to_owned(),
                Some(serde_json::json!({ "stderr": [format!("key={AWS_KEY}")] })),
            ),
            // `data`/`blob` in error data are text, not a media payload.
            (
                "ctx_git_read failed".to_owned(),
                Some(serde_json::json!({ "data": AWS_KEY, "nested": { "blob": AWS_KEY } })),
            ),
        ] {
            let error = rmcp::ErrorData::internal_error(message, data);
            let code = error.code;
            let released = super::super::release_outcome("ctx_git_read", Err(error))
                .expect_err("stays an error");
            assert_eq!(released.code, code);
            assert!(released.data.is_none());
            assert!(!released.message.contains(AWS_KEY), "{}", released.message);
            assert!(!released.message.contains("ctx_git_read failed"));
        }
    }
}
