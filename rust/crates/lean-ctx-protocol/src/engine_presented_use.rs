// SPDX-License-Identifier: Apache-2.0

//! `context-presented-use`: what the holder of a presenter capability asks
//! LATER about text that was delivered under a presented lineage.
//!
//! Additive to the v2 materialization exchange; neither that exchange nor v1
//! changes. The capability is never part of this JSON: it reaches the Engine
//! through `--presenter-capability-file`.
//!
//! A request may carry a source constraint — the ids of exactly the sources
//! the sender has authorized the request for. The Engine then answers only
//! for a lineage of exactly these sources, and nothing is checked, stored or
//! returned for any other. An Engine that does not know a member of the
//! request refuses it unread.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    ENGINE_PRESENTER_REF_PREFIX, MAX_ENGINE_SOURCE_MATERIALIZED_CONTEXT_BYTES,
    MAX_ENGINE_SOURCE_PLAN_SOURCES, MAX_IDENTIFIER_LENGTH, SourceId, ValidationError,
};

/// Schema id of the presented-use exchange.
pub const ENGINE_PRESENTED_USE_V1_SCHEMA_ID: &str = "leanctx.engine-presented-use/v1";
/// Lowercase hex characters of a recovery id.
pub const ENGINE_RECOVERY_ID_HEX_CHARS: usize = 64;

/// A delivered text is at most one materialized context.
const MAX_TEXT_BYTES: usize = MAX_ENGINE_SOURCE_MATERIALIZED_CONTEXT_BYTES;
/// The source constraint as JSON: every id a string of at most two bytes a
/// byte (quote and backslash are escaped; control characters are not ids),
/// with its quotes and separator.
const MAX_SOURCE_CONSTRAINT_BYTES: usize =
    MAX_ENGINE_SOURCE_PLAN_SOURCES * (2 * MAX_IDENTIFIER_LENGTH + 3);
/// Bound of a serialized use request: the delivered text as a JSON string
/// (quotes, backslashes and line breaks take two bytes), the source
/// constraint and the envelope.
pub const MAX_ENGINE_PRESENTED_USE_REQUEST_BYTES: usize =
    2 * MAX_ENGINE_SOURCE_MATERIALIZED_CONTEXT_BYTES + MAX_SOURCE_CONSTRAINT_BYTES + 1024;

const PRESENTER_REF_HEX_CHARS: usize = 32;
const MAX_REASON_CODE_BYTES: usize = 48;

fn lower_hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_recovery_id(text: &str) -> bool {
    lower_hex(text, ENGINE_RECOVERY_ID_HEX_CHARS)
}

/// A refusal reason is one of the Engine's fixed, content-free codes:
/// `lineage.*` or `recovery.*`, lowercase letters and underscores.
fn is_reason(reason: &str) -> bool {
    ["lineage.", "recovery."].iter().any(|prefix| {
        reason.strip_prefix(prefix).is_some_and(|code| {
            !code.is_empty()
                && code.len() <= MAX_REASON_CODE_BYTES
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        })
    })
}

/// What the holder of a capability asks (the `use` member).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "mode")]
pub enum EnginePresentedUseV1 {
    /// May this exact delivered text still be used by this holder now?
    Check { content: String },
    /// `Check`, and keep a recovery copy of the text bound to its lineage.
    Retain { content: String },
    /// Hand back the text of a recovery copy, under current authority only.
    Expand { recovery_id: String },
}

impl EnginePresentedUseV1 {
    /// Form only: a non-empty text within the materialization bound, or a
    /// canonical recovery id.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let well_formed = match self {
            Self::Check { content } | Self::Retain { content } => {
                !content.is_empty() && content.len() <= MAX_TEXT_BYTES
            }
            Self::Expand { recovery_id } => is_recovery_id(recovery_id),
        };
        if well_formed { Ok(()) } else { Err(invalid()) }
    }
}

/// Request of `context-presented-use`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnginePresentedUseRequestV1 {
    pub schema_version: u32,
    /// The source constraint: the ids of EXACTLY the sources the sender has
    /// authorized this request for. Set equality, not inclusion — a lineage
    /// with fewer sources belongs to another text than the one the sender
    /// authorized, and a lineage with more derives from a source nobody
    /// authorized for this request. Absent: no additional constraint (a
    /// local host without an authorizing caller in front of the Engine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_source_ids: Option<Vec<SourceId>>,
    #[serde(rename = "use")]
    pub operation: EnginePresentedUseV1,
}

impl EnginePresentedUseRequestV1 {
    /// Validate the envelope, the constraint (a non-empty, bounded set of
    /// distinct ids) and the form of what is asked.
    pub fn validate_payload(&self) -> Result<(), ValidationError> {
        if self.schema_version != 1 {
            return Err(invalid());
        }
        if let Some(ids) = &self.authorized_source_ids {
            let distinct: BTreeSet<&SourceId> = ids.iter().collect();
            if ids.is_empty()
                || ids.len() > MAX_ENGINE_SOURCE_PLAN_SOURCES
                || distinct.len() != ids.len()
            {
                return Err(invalid());
            }
        }
        self.operation.validate()
    }
}

/// Answer of `context-presented-use`. `usable: false` carries a content-free
/// reason and nothing else; source ids, recovery id and text are present
/// only when usable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnginePresentedUseResponseV1 {
    pub schema_version: u32,
    /// Content-free reference of the presenter entry that asked.
    pub presenter_ref: String,
    pub usable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Ids of the caller-supplied sources the text derives from, sorted.
    pub source_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl EnginePresentedUseResponseV1 {
    /// Validate the form of this answer as an answer to `asked`: text comes
    /// back for `expand` only, a recovery id for `retain` and `expand`, and a
    /// refusal carries its reason and nothing else. Form only: whether the
    /// answer is by the presenter that asked, and free of its capability, is
    /// for the caller that holds both.
    pub fn validate_for(&self, asked: &EnginePresentedUseV1) -> Result<(), ValidationError> {
        let reference_ok = self
            .presenter_ref
            .strip_prefix(ENGINE_PRESENTER_REF_PREFIX)
            .is_some_and(|hex| lower_hex(hex, PRESENTER_REF_HEX_CHARS));
        let body_ok = if self.usable {
            self.is_usable_answer_to(asked)
        } else {
            self.is_refusal()
        };
        if self.schema_version == 1 && reference_ok && body_ok {
            Ok(())
        } else {
            Err(invalid())
        }
    }

    fn is_refusal(&self) -> bool {
        self.reason.as_deref().is_some_and(is_reason)
            && self.source_ids.is_empty()
            && self.recovery_id.is_none()
            && self.content.is_none()
    }

    fn is_usable_answer_to(&self, asked: &EnginePresentedUseV1) -> bool {
        // A lineage names every source once: a repeated id could stand in
        // for a source the answer does not name.
        let distinct: BTreeSet<&str> = self.source_ids.iter().map(String::as_str).collect();
        let sources_ok = !self.source_ids.is_empty()
            && self.source_ids.len() <= MAX_ENGINE_SOURCE_PLAN_SOURCES
            && distinct.len() == self.source_ids.len()
            && self
                .source_ids
                .iter()
                .all(|id| !id.is_empty() && id.len() <= MAX_IDENTIFIER_LENGTH);
        if self.reason.is_some() || !sources_ok {
            return false;
        }
        match asked {
            EnginePresentedUseV1::Check { .. } => {
                self.recovery_id.is_none() && self.content.is_none()
            }
            EnginePresentedUseV1::Retain { .. } => {
                self.recovery_id.as_deref().is_some_and(is_recovery_id) && self.content.is_none()
            }
            EnginePresentedUseV1::Expand { recovery_id } => {
                let text_ok = self
                    .content
                    .as_deref()
                    .is_some_and(|text| text.len() <= MAX_TEXT_BYTES);
                self.recovery_id.as_deref() == Some(recovery_id.as_str()) && text_ok
            }
        }
    }
}

fn invalid() -> ValidationError {
    ValidationError::new("invalid Engine presented use envelope")
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A use request is closed and bounded: an unknown member anywhere, a
    /// constraint that is not a non-empty bounded set of distinct ids, and a
    /// malformed text or recovery id are all unreadable — never narrowed,
    /// widened or ignored. Without this an Engine could answer a request
    /// whose constraint it did not understand as if there were none.
    #[test]
    fn a_use_request_is_closed_and_its_source_constraint_bounded() {
        let accepted = |request: Value| {
            serde_json::from_value::<EnginePresentedUseRequestV1>(request)
                .is_ok_and(|request| request.validate_payload().is_ok())
        };
        let check = json!({"mode": "check", "content": "ledger"});
        let plain = |version: u32, asked: Value| json!({"schema_version": version, "use": asked});
        let constrained = |ids: Value| json!({"schema_version": 1, "authorized_source_ids": ids, "use": check.clone()});
        let too_many: Vec<String> = (0..=MAX_ENGINE_SOURCE_PLAN_SOURCES)
            .map(|index| format!("source-{index}"))
            .collect();
        let extra_member = json!({"schema_version": 1, "use": check.clone(), "capability": "x"});
        let extra_in_use = json!({"mode": "check", "content": "x", "also": 1});
        let empty_text = json!({"mode": "retain", "content": ""});
        let path_as_id = json!({"mode": "expand", "recovery_id": "../x"});
        for (case, request, expected) in [
            ("no constraint", plain(1, check.clone()), true),
            ("a set of sources", constrained(json!(["b", "a"])), true),
            ("an empty set", constrained(json!([])), false),
            (
                "a source named twice",
                constrained(json!(["a", "a"])),
                false,
            ),
            ("an empty id", constrained(json!(["a", ""])), false),
            (
                "more sources than a plan has",
                constrained(json!(too_many)),
                false,
            ),
            (
                "a constraint that is no set",
                constrained(json!("a")),
                false,
            ),
            ("an unknown member", extra_member, false),
            (
                "an unknown member of the use",
                plain(1, extra_in_use),
                false,
            ),
            ("another schema version", plain(2, check.clone()), false),
            ("an empty text", plain(1, empty_text), false),
            ("a recovery id that is a path", plain(1, path_as_id), false),
        ] {
            assert_eq!(accepted(request), expected, "{case}");
        }

        // An answer is accepted only in the form of an answer to what was
        // asked: text for `expand` alone, nothing beside a refusal's reason.
        let recovery_id = "5e".repeat(ENGINE_RECOVERY_ID_HEX_CHARS / 2);
        let expand = EnginePresentedUseV1::Expand {
            recovery_id: recovery_id.clone(),
        };
        let answer = |usable: bool, reason: Option<&str>| EnginePresentedUseResponseV1 {
            schema_version: 1,
            presenter_ref: format!("{ENGINE_PRESENTER_REF_PREFIX}{}", "0a".repeat(16)),
            usable,
            reason: reason.map(str::to_owned),
            source_ids: vec!["source-one".to_owned()],
            recovery_id: Some(recovery_id.clone()),
            content: Some("ledger".to_owned()),
        };
        let retain = EnginePresentedUseV1::Retain {
            content: "ledger".to_owned(),
        };
        assert!(answer(true, None).validate_for(&expand).is_ok());
        assert!(answer(true, None).validate_for(&retain).is_err());
        let mut twice = answer(true, None);
        twice.source_ids = vec!["source-one".to_owned(), "source-one".to_owned()];
        assert!(twice.validate_for(&expand).is_err(), "a source named twice");
        assert!(
            answer(false, Some("lineage.unknown"))
                .validate_for(&expand)
                .is_err(),
            "text beside a refusal"
        );
    }
}
