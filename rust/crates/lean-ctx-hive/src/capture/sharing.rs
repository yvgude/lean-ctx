// SPDX-License-Identifier: Apache-2.0
//! Whether a fact just remembered goes to the team, decided when it is
//! remembered (3.12.3): `ctx_knowledge remember` and `lean-ctx knowledge
//! remember` say `"local only: <reason>"` with how to fix it, instead of a
//! plain OK that the capture gate later turns into a silent rejection.
//!
//! This checks what a person or agent can fix now (the statement rules and
//! a missing reference); the gate makes the final decision at sync (it
//! also checks that each reference exists, screening, budget).

use serde::Serialize;

use super::{ReasonCode, rules};
use crate::state::HiveStore;
use crate::vault::VaultMap;

/// Where a remembered fact goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Sharing {
    /// Goes to the vault with the next sync (once its sources check out).
    NextSync { vault: String },
    /// Stays on this machine: the gate would reject it for `reason`.
    LocalOnly {
        reason: ReasonCode,
        hint: &'static str,
    },
    /// Sharing knowledge is switched off on this machine.
    Off,
    /// The project is not shared with the workspace yet.
    NotShared,
}

impl Sharing {
    /// The line under a `remember` answer; `org` names the workspace.
    pub fn line(&self, org: &str) -> String {
        match self {
            Self::NextSync { vault } => {
                format!("Goes to {vault} with the next sync (once its source checks out).")
            }
            Self::LocalOnly { reason, hint } => format!(
                "Saved on this machine only. Not shared with {org}: {hint}. (local only: {})",
                reason.as_str()
            ),
            Self::Off => format!(
                "Saved on this machine only: sharing knowledge with {org} is switched off on this machine (`hive.auto_capture`)."
            ),
            Self::NotShared => format!(
                "Ready to share, but this project is not shared with {org} yet: run `lean-ctx hive connect` here."
            ),
        }
    }
}

/// The statement rules and the reference rule for `value`; `has_ref`: the
/// call named a source (refs or anchors), or the fact already has anchors.
pub fn problem(value: &str, has_ref: bool) -> Option<ReasonCode> {
    rules::check_statement(value.trim())
        .err()
        .or((!has_ref).then_some(ReasonCode::NoReference))
}

/// Where `value`, just remembered in `project_root`, goes. `None` on a
/// machine not connected to Hive (nothing to say).
pub fn decide(
    store: &HiveStore,
    config: &crate::HiveConfig,
    project_root: &str,
    value: &str,
    has_ref: bool,
) -> Option<Sharing> {
    store.instance().ok().flatten()?;
    if let Some(reason) = problem(value, has_ref) {
        return Some(Sharing::LocalOnly {
            reason,
            hint: reason.plain(),
        });
    }
    if !crate::sync::place::capture_allowed(config) {
        return Some(Sharing::Off);
    }
    Some(
        match VaultMap::load(store.data_dir()).for_root(project_root) {
            Some(vault) => Sharing::NextSync {
                vault: format!("{}/{}", vault.ns, vault.name),
            },
            None => Sharing::NotShared,
        },
    )
}

/// [`decide`] for this machine, as the line under a `remember` answer
/// (`None`: not connected to Hive). `has_ref` as in [`problem`]; a fact
/// LeanCTX anchored to code carries that reference itself.
pub fn note(project_root: &str, value: &str, has_ref: bool) -> Option<String> {
    let store = HiveStore::open().ok()?;
    let instance = store.instance().ok().flatten()?;
    let anchored =
        lean_ctx::core::knowledge::ProjectKnowledge::load(project_root).is_some_and(|k| {
            k.facts
                .iter()
                .any(|f| f.is_current() && f.value == value && !f.anchors.is_empty())
        });
    let config = lean_ctx::core::config::Config::load().hive.clone();
    decide(&store, &config, project_root, value, has_ref || anchored)
        .map(|sharing| sharing.line(&instance.org_name))
}

// ---------------------------------------------------------------------------
// Facts the gate kept local at sync, for the sync status (local only).

/// At most this many rejected facts are kept per vault (the newest).
pub const MAX_REJECTED: usize = 200;

/// One fact the capture gate kept on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct RejectedFact {
    pub category: String,
    pub key: String,
    pub reason: ReasonCode,
    /// How to fix it ([`ReasonCode::plain`]).
    #[serde(default)]
    pub hint: String,
    /// When the gate rejected it (RFC 3339).
    pub at: String,
}

fn rejected_file(vault_id: &str) -> String {
    let name: String = vault_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    format!("{name}.rejected.json")
}

fn load_rejected(data_dir: &std::path::Path, vault_id: &str) -> Vec<RejectedFact> {
    std::fs::read(data_dir.join("capture").join(rejected_file(vault_id)))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Note what the gate refused in one sync of `vault_id` (`dropped` as
/// [`crate::knowledge::learn::LearnPlan::dropped`]: `category/key`,
/// reason). A fact refused again replaces its earlier row.
pub fn record_rejected(
    data_dir: &std::path::Path,
    vault_id: &str,
    dropped: &[(String, ReasonCode)],
    now: chrono::DateTime<chrono::Utc>,
) {
    // Only what a person or agent can fix; a duplicate or a full batch is
    // no rejection of the fact.
    let fixable = |reason: &ReasonCode| {
        !matches!(
            reason,
            ReasonCode::Duplicate | ReasonCode::NearDuplicateMerged | ReasonCode::BudgetBatchFull
        )
    };
    if dropped.iter().all(|(_, r)| !fixable(r)) {
        return;
    }
    let at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut rows = load_rejected(data_dir, vault_id);
    for (label, reason) in dropped.iter().filter(|(_, r)| fixable(r)) {
        let (category, key) = label.split_once('/').unwrap_or((label.as_str(), ""));
        rows.retain(|row| !(row.category == category && row.key == key));
        rows.push(RejectedFact {
            category: category.to_owned(),
            key: key.to_owned(),
            reason: *reason,
            hint: reason.plain().to_owned(),
            at: at.clone(),
        });
    }
    if rows.len() > MAX_REJECTED {
        let extra = rows.len() - MAX_REJECTED;
        rows.drain(..extra);
    }
    if let Ok(bytes) = serde_json::to_vec(&rows) {
        let _ = crate::state::write_private(
            &data_dir.join("capture"),
            &rejected_file(vault_id),
            &bytes,
        );
    }
}

/// The rejected facts of `vault_id` still waiting for a fix: the fact is
/// current in `knowledge` and was not changed since (a changed fact is
/// considered again by the next sync).
pub fn rejected_now(
    data_dir: &std::path::Path,
    vault_id: &str,
    knowledge: Option<&lean_ctx::core::knowledge::ProjectKnowledge>,
) -> Vec<RejectedFact> {
    let rows = load_rejected(data_dir, vault_id);
    let Some(knowledge) = knowledge else {
        return rows;
    };
    rows.into_iter()
        .filter(|row| {
            let Ok(at) = chrono::DateTime::parse_from_rfc3339(&row.at) else {
                return true;
            };
            knowledge.facts.iter().any(|f| {
                f.is_current() && f.category == row.category && f.key == row.key && {
                    let changed = f
                        .capture
                        .changed_at
                        .map_or(f.created_at, |c| c.max(f.created_at));
                    changed <= at
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejections_are_kept_per_fact_and_only_the_fixable_ones() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        record_rejected(
            dir.path(),
            "v-1",
            &[
                ("gotcha/a".into(), ReasonCode::NotSentence),
                ("gotcha/b".into(), ReasonCode::Duplicate),
            ],
            now,
        );
        record_rejected(
            dir.path(),
            "v-1",
            &[("gotcha/a".into(), ReasonCode::NoReference)],
            now,
        );
        let rows = rejected_now(dir.path(), "v-1", None);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].category.as_str(), rows[0].key.as_str()),
            ("gotcha", "a")
        );
        assert_eq!(rows[0].reason, ReasonCode::NoReference);
        assert!(rows[0].hint.contains("--ref"));
        assert!(rejected_now(dir.path(), "v-2", None).is_empty());
    }

    #[test]
    fn a_fragment_or_a_fact_without_a_source_stays_local_with_the_fix() {
        assert_eq!(
            problem("not a sentence, just a lowercase fragment here", true),
            Some(ReasonCode::NotSentence)
        );
        assert_eq!(problem("Too short.", true), Some(ReasonCode::TooShort));
        assert_eq!(
            problem(
                "The daemon syncs every shared vault in the background.",
                false
            ),
            Some(ReasonCode::NoReference)
        );
        assert_eq!(
            problem(
                "The daemon syncs every shared vault in the background.",
                true
            ),
            None
        );
        let local = Sharing::LocalOnly {
            reason: ReasonCode::NoReference,
            hint: ReasonCode::NoReference.plain(),
        };
        let line = local.line("Acme");
        assert!(line.contains("local only: no_reference"), "{line}");
        assert!(line.contains("--ref file:"), "the fix is named: {line}");
    }

    #[test]
    fn not_connected_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
        assert_eq!(
            decide(
                &store,
                &crate::HiveConfig::default(),
                "/x",
                "not a sentence",
                false
            ),
            None
        );
    }

    #[test]
    fn the_json_shape_names_state_reason_and_hint() {
        let local = Sharing::LocalOnly {
            reason: ReasonCode::NotSentence,
            hint: ReasonCode::NotSentence.plain(),
        };
        let value = serde_json::to_value(&local).unwrap();
        assert_eq!(value["state"], "local_only");
        assert_eq!(value["reason"], "not_sentence");
        assert!(value["hint"].as_str().unwrap().contains("capital letter"));
        assert_eq!(
            serde_json::to_value(Sharing::NextSync {
                vault: "a/b".into()
            })
            .unwrap(),
            serde_json::json!({"state": "next_sync", "vault": "a/b"})
        );
    }
}
