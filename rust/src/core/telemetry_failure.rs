// SPDX-License-Identifier: Apache-2.0

//! Why a built-in tool call failed, as a closed class for telemetry.
//!
//! Only the class leaves the machine; the message it is derived from (which
//! can name files, commands or values) never does. Matching is ordered from
//! the most specific signal to the most generic one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// Arguments missing, malformed or of the wrong type.
    InvalidInput,
    /// File, symbol, job or tool does not exist.
    NotFound,
    /// Path outside the jail, read-only target, OS permission.
    Permission,
    /// Refused by a LeanCTX policy, allowlist or gateway rule.
    PolicyBlocked,
    /// The call or the command it ran timed out.
    Timeout,
    /// An edit no longer matches the file (stale anchor, ambiguous match).
    Conflict,
    /// Input or output beyond a size or budget limit.
    TooLarge,
    /// A dependency (daemon, provider, network, runtime) is not available.
    Unavailable,
    /// Anything else.
    Other,
}

pub const FAILURE_KINDS: [FailureKind; 9] = [
    FailureKind::InvalidInput,
    FailureKind::NotFound,
    FailureKind::Permission,
    FailureKind::PolicyBlocked,
    FailureKind::Timeout,
    FailureKind::Conflict,
    FailureKind::TooLarge,
    FailureKind::Unavailable,
    FailureKind::Other,
];

impl FailureKind {
    pub fn index(self) -> usize {
        FAILURE_KINDS
            .iter()
            .position(|kind| *kind == self)
            .unwrap_or(FAILURE_KINDS.len() - 1)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::Permission => "permission",
            Self::PolicyBlocked => "policy_blocked",
            Self::Timeout => "timeout",
            Self::Conflict => "conflict",
            Self::TooLarge => "too_large",
            Self::Unavailable => "unavailable",
            Self::Other => "other",
        }
    }
}

const RULES: &[(FailureKind, &[&str])] = &[
    (
        FailureKind::PolicyBlocked,
        &[
            "[policy blocked]",
            "policy blocked",
            "blocked by policy",
            "denied by policy",
            "allowlist",
            "not permitted by",
            "gateway denied",
            "egress denied",
        ],
    ),
    (
        FailureKind::Timeout,
        &["timed out", "timeout", "deadline exceeded"],
    ),
    (
        FailureKind::Conflict,
        &[
            "old_string",
            "not unique",
            "no unique",
            "conflict",
            "mismatch",
            "stale",
            "changed since",
            "hash does not match",
            "anchor",
        ],
    ),
    (
        FailureKind::Permission,
        &[
            "permission denied",
            "access denied",
            "outside the project",
            "outside project",
            "outside of",
            "path jail",
            "escapes project root",
            "read-only",
            "readonly",
            "operation not permitted",
        ],
    ),
    (
        FailureKind::NotFound,
        &[
            "not found",
            "no such file",
            "does not exist",
            "doesn't exist",
            "unknown tool",
            "no matches",
            "no such",
        ],
    ),
    (
        FailureKind::TooLarge,
        &[
            "too large",
            "too long",
            "exceeds",
            "limit reached",
            "over budget",
        ],
    ),
    (
        FailureKind::Unavailable,
        &[
            "unavailable",
            "not running",
            "connection refused",
            "not installed",
            "not configured",
            "is disabled",
            "not enabled",
            "failed to connect",
            "network",
        ],
    ),
    (
        FailureKind::InvalidInput,
        &[
            "invalid",
            "missing required",
            "missing field",
            "required parameter",
            "unknown field",
            "unknown parameter",
            "unknown action",
            "expected",
            "must be",
            "cannot parse",
            "failed to parse",
        ],
    ),
];

/// Classify a failure message; the message itself is discarded.
pub fn classify(message: &str) -> FailureKind {
    let lowered = message.to_ascii_lowercase();
    RULES
        .iter()
        .find(|(_, needles)| needles.iter().any(|needle| contains_word(&lowered, needle)))
        .map_or(FailureKind::Other, |(kind, _)| *kind)
}

/// `needle` in `haystack` starting at a word boundary, so `unexpected` is not
/// read as `expected`.
fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(at, _)| {
        haystack[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric())
    })
}

/// Failure counts indexed like [`FAILURE_KINDS`].
pub type KindCounts = [u64; FAILURE_KINDS.len()];

/// A failed call as telemetry sees it: its class and, when one survives
/// scrubbing, the template of its message (see [`super::failure_template`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub template: Option<String>,
}

impl Failure {
    /// Derive class and template locally; the message itself is dropped.
    pub fn from_message(message: &str) -> Self {
        Self {
            kind: classify(message),
            template: super::failure_template::message_template(message),
        }
    }

    pub fn of_kind(kind: FailureKind) -> Self {
        Self {
            kind,
            template: None,
        }
    }
}

pub fn sub_kinds(observed: &KindCounts, baseline: &KindCounts) -> KindCounts {
    std::array::from_fn(|index| observed[index].saturating_sub(baseline[index]))
}

pub fn add_kinds(total: &mut KindCounts, delta: &KindCounts) {
    for (slot, added) in total.iter_mut().zip(delta) {
        *slot = slot.saturating_add(*added);
    }
}

/// Non-zero classes for the wire, capped so they never exceed `failures`.
pub fn wire_kinds(counts: &KindCounts, failures: u64) -> Option<BTreeMap<FailureKind, u64>> {
    let mut remaining = failures;
    let map: BTreeMap<_, _> = FAILURE_KINDS
        .iter()
        .zip(counts)
        .filter_map(|(kind, count)| {
            let count = (*count).min(remaining);
            remaining -= count;
            (count > 0).then_some((*kind, count))
        })
        .collect();
    (!map.is_empty()).then_some(map)
}

/// Failure templates reported per tool and day.
pub const MAX_FAILURE_MESSAGES: usize = 5;

/// The most frequent scrubbed templates of `tool` (keys `tool\ttemplate`),
/// counts capped so they never exceed `failures`.
pub fn wire_messages(
    messages: &BTreeMap<String, u64>,
    tool: &str,
    failures: u64,
) -> Option<Vec<crate::core::telemetry_v2::FailureMessage>> {
    let prefix = format!("{tool}\t");
    let mut found: Vec<(&str, u64)> = messages
        .iter()
        .filter_map(|(key, count)| Some((key.strip_prefix(&prefix)?, *count)))
        .filter(|(template, count)| *count > 0 && !template.is_empty())
        .collect();
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mut remaining = failures;
    let list: Vec<_> = found
        .into_iter()
        .take(MAX_FAILURE_MESSAGES)
        .filter_map(|(template, count)| {
            let count = count.min(remaining);
            remaining -= count;
            (count > 0).then(|| crate::core::telemetry_v2::FailureMessage {
                template: template.to_string(),
                count,
            })
        })
        .collect();
    (!list.is_empty()).then_some(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_map_to_closed_classes_most_specific_first() {
        for (message, kind) in [
            (
                "[POLICY BLOCKED] command not in allowlist",
                FailureKind::PolicyBlocked,
            ),
            ("command timed out after 120s", FailureKind::Timeout),
            ("old_string not found in file", FailureKind::Conflict),
            (
                "CONFLICT: line 12 hash mismatch, re-read",
                FailureKind::Conflict,
            ),
            ("path escapes project root: /etc", FailureKind::Permission),
            ("Permission denied (os error 13)", FailureKind::Permission),
            (
                "No such file or directory (os error 2)",
                FailureKind::NotFound,
            ),
            ("file is too large to read in full", FailureKind::TooLarge),
            ("daemon not running", FailureKind::Unavailable),
            (
                "missing required parameter 'path'",
                FailureKind::InvalidInput,
            ),
            ("invalid mode: banana", FailureKind::InvalidInput),
            ("something odd happened", FailureKind::Other),
            ("", FailureKind::Other),
        ] {
            assert_eq!(classify(message), kind, "{message}");
        }
    }

    #[test]
    fn every_kind_has_a_stable_wire_name_and_index() {
        for (index, kind) in FAILURE_KINDS.iter().enumerate() {
            assert_eq!(kind.index(), index);
            assert_eq!(
                serde_json::to_string(kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
    }
}
