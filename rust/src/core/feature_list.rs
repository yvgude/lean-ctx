// SPDX-License-Identifier: Apache-2.0
//! The feature list of a task: its locked acceptance tests (process-v1 §4,
//! `contracts/process-v1/feature-list.schema.json`).
//!
//! The Spec Writer writes the list until G2. Once G2 has passed (Hive sets
//! `spec_commit`) the list is locked: only `features[].passes` may change, and
//! only the G6 gate writes it, from its own re-run. Any other change is a spec
//! change and goes back to `spec`; LeanCTX refuses it.
//!
//! LeanCTX enforces the lock in two places:
//! - [`crate::core::session_rights`] refuses every session write to a locked
//!   feature list file, whatever the role (a session is never the gate);
//! - [`apply_gate_passes`] is the one writer of `passes`, and
//!   [`validate_transition`] refuses any other difference between two versions.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Schema id of a feature list.
pub const SCHEMA: &str = "leanctx.hive.process.feature-list/v1";
/// Largest feature list read.
pub const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_FEATURES: usize = 200;

/// One acceptance criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    pub id: String,
    pub description: String,
    pub steps: Vec<String>,
    pub tests: Vec<String>,
    pub passes: bool,
}

/// A feature list as the contract defines it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureList {
    pub schema: String,
    pub process: String,
    pub task: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_commit: Option<String>,
    pub test_command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2e_command: Option<String>,
    pub locked_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlocks: Option<Vec<Value>>,
    pub features: Vec<Feature>,
}

/// Who writes a new version of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writer<'a> {
    /// The G6 gate, writing `passes` from its own re-run.
    Gate,
    /// A session acting in a role (role id, e.g. `developer`).
    Role(&'a str),
}

impl FeatureList {
    /// Parse and check the structural rules of the schema.
    pub fn parse(text: &str) -> Result<Self, String> {
        let list: Self =
            serde_json::from_str(text).map_err(|e| format!("feature list is not valid: {e}"))?;
        list.check()?;
        Ok(list)
    }

    /// Read and parse a feature list file (at most [`MAX_FILE_BYTES`]).
    pub fn load(path: &Path) -> Result<Self, String> {
        let meta = std::fs::metadata(path)
            .map_err(|e| format!("feature list {} unreadable: {e}", path.display()))?;
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!(
                "feature list {} is larger than {MAX_FILE_BYTES} bytes",
                path.display()
            ));
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("feature list {} unreadable: {e}", path.display()))?;
        Self::parse(&text)
    }

    /// True once G2 has passed: from then on only the gate writes `passes`.
    pub fn is_locked(&self) -> bool {
        self.spec_commit.is_some()
    }

    fn check(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!(
                "feature list schema must be {SCHEMA} (got {})",
                self.schema
            ));
        }
        if !is_process_version(&self.process) {
            return Err(format!(
                "feature list process version must be 1.<minor> (got {})",
                self.process
            ));
        }
        check_task_ref(&self.task)?;
        if let Some(sha) = &self.spec_commit
            && !is_git_sha(sha)
        {
            return Err(format!(
                "spec_commit must be a 40-character git sha (got {sha})"
            ));
        }
        if self.test_command.trim().is_empty() || !len_in(&self.test_command, 1, 2000) {
            return Err("feature list test_command must hold 1 to 2000 characters".into());
        }
        if let Some(e2e) = &self.e2e_command
            && !len_in(e2e, 1, 2000)
        {
            return Err("feature list e2e_command must hold 1 to 2000 characters".into());
        }
        if self.locked_paths.is_empty() {
            return Err("feature list names no locked_paths".into());
        }
        let mut unique = std::collections::BTreeSet::new();
        for path in &self.locked_paths {
            if !is_repo_relative(path) || !len_in(path, 1, 500) {
                return Err(format!(
                    "locked path must be repo-relative without '..', at most 500 characters: {path}"
                ));
            }
            if !unique.insert(path.as_str()) {
                return Err(format!("locked path {path} appears twice"));
            }
        }
        for unlock in self.unlocks.iter().flatten() {
            check_unlock(unlock)?;
        }
        if self.features.is_empty() || self.features.len() > MAX_FEATURES {
            return Err(format!(
                "feature list must hold 1 to {MAX_FEATURES} features"
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for feature in &self.features {
            if !is_feature_id(&feature.id) {
                return Err(format!("feature id must match F<digits>: {}", feature.id));
            }
            if !seen.insert(feature.id.as_str()) {
                return Err(format!("feature id {} appears twice", feature.id));
            }
            if feature.steps.is_empty() || feature.tests.is_empty() {
                return Err(format!("feature {} needs steps and tests", feature.id));
            }
            if !len_in(&feature.description, 1, 2000)
                || feature.steps.iter().any(|s| !len_in(s, 1, 1000))
                || feature.tests.iter().any(|t| !len_in(t, 1, 500))
            {
                return Err(format!(
                    "feature {}: description 1-2000, steps 1-1000, tests 1-500 characters",
                    feature.id
                ));
            }
        }
        Ok(())
    }
}

fn len_in(text: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&text.chars().count())
}

/// `common.schema.json#/$defs/process_version`: `1.<minor>`.
pub(crate) fn is_process_version(raw: &str) -> bool {
    let Some(minor) = raw.strip_prefix("1.") else {
        return false;
    };
    minor == "0"
        || (!minor.is_empty()
            && minor.len() <= 4
            && !minor.starts_with('0')
            && minor.bytes().all(|b| b.is_ascii_digit()))
}

fn is_git_sha(raw: &str) -> bool {
    raw.len() == 40
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn matches(pattern: &str, raw: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|re| re.is_match(raw))
}

fn only_keys(object: &serde_json::Map<String, Value>, allowed: &[&str]) -> bool {
    object.keys().all(|k| allowed.contains(&k.as_str()))
}

/// `common.schema.json#/$defs/task_ref`.
fn check_task_ref(task: &Value) -> Result<(), String> {
    let ok = task.as_object().is_some_and(|t| {
        only_keys(t, &["vault", "number"])
            && t.get("vault")
                .and_then(Value::as_str)
                .is_some_and(|v| matches(r"^#[a-z0-9_-]{1,64}/[a-z0-9-]{2,40}$", v))
            && t.get("number")
                .and_then(Value::as_i64)
                .is_some_and(|n| (1..=2_147_483_647).contains(&n))
    });
    if ok {
        Ok(())
    } else {
        Err("feature list task must be {vault: \"#workspace/vault\", number: 1..}".into())
    }
}

const UUID: &str = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

/// `common.schema.json#/$defs/actor`: an agent or a person.
fn is_actor(actor: &Value) -> bool {
    let Some(a) = actor.as_object() else {
        return false;
    };
    let text = |key: &str| a.get(key).and_then(Value::as_str);
    match text("kind") {
        Some("agent") => {
            only_keys(
                a,
                &[
                    "kind",
                    "agent_id",
                    "worker_id",
                    "instance_id",
                    "role",
                    "queen",
                ],
            ) && text("agent_id")
                .is_some_and(|v| matches(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$", v))
                && text("instance_id").is_some_and(|v| matches(UUID, v))
                && text("role").is_some_and(|r| crate::core::session_rights::ROLE_IDS.contains(&r))
                && a.get("worker_id").is_none_or(|w| {
                    w.as_str()
                        .is_some_and(|v| matches(r"^wkr_[a-z2-7]{20}$", v))
                })
                && a.get("queen").is_none_or(Value::is_boolean)
        }
        Some("person") => {
            only_keys(a, &["kind", "person_id", "session"])
                && text("person_id").is_some_and(|v| matches(UUID, v))
                && text("session") == Some("human")
        }
        _ => false,
    }
}

/// One `unlocks[]` entry: `{by: actor, decision: dec_…, at: timestamp}`.
fn check_unlock(unlock: &Value) -> Result<(), String> {
    let ok = unlock.as_object().is_some_and(|u| {
        only_keys(u, &["by", "decision", "at"])
            && u.get("by").is_some_and(is_actor)
            && u.get("decision")
                .and_then(Value::as_str)
                .is_some_and(|d| matches(r"^dec_[0-9A-HJKMNP-TV-Z]{26}$", d))
            && u.get("at").and_then(Value::as_str).is_some_and(|t| {
                matches(
                    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?Z$",
                    t,
                )
            })
    });
    if ok {
        Ok(())
    } else {
        Err("feature list unlocks[] must be {by: actor, decision: dec_…, at: UTC timestamp}".into())
    }
}

/// A repo-relative path or glob: not absolute, no `..` component.
pub fn is_repo_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.split(['/', '\\']).any(|part| part == "..")
}

fn is_feature_id(id: &str) -> bool {
    let digits = id.strip_prefix('F').unwrap_or("");
    (1..=4).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Decide whether `writer` may turn `before` into `after`.
///
/// Before G2 (`before.spec_commit` unset) the Spec Writer may change anything
/// (Hive's G2 then judges it). After G2 the only change allowed is `passes`,
/// and only by the gate. Every refusal names the rule `feature_list_locked`.
pub fn validate_transition(
    before: &FeatureList,
    after: &FeatureList,
    writer: Writer<'_>,
) -> Result<(), String> {
    if !before.is_locked() {
        return match writer {
            Writer::Role("spec_writer") | Writer::Gate => Ok(()),
            Writer::Role(role) => Err(format!(
                "feature_list_locked: the feature list is written by the Spec Writer until G2, not by role '{role}'"
            )),
        };
    }
    let mut before_rest = before.clone();
    let mut after_rest = after.clone();
    let passes_changed = before.features.len() == after.features.len()
        && before
            .features
            .iter()
            .zip(&after.features)
            .any(|(a, b)| a.passes != b.passes);
    for feature in before_rest
        .features
        .iter_mut()
        .chain(after_rest.features.iter_mut())
    {
        feature.passes = false;
    }
    if before_rest != after_rest {
        return Err(
            "feature_list_locked: after G2 only features[].passes may change; any other change \
             is a spec change and sends the task back to spec"
                .into(),
        );
    }
    if passes_changed && writer != Writer::Gate {
        let role = match writer {
            Writer::Role(role) => role,
            Writer::Gate => "",
        };
        return Err(format!(
            "feature_list_locked: passes is written only by the G6 gate from its own re-run, not by role '{role}'"
        ));
    }
    Ok(())
}

/// The gate's write of `passes`: set each named feature's result and store the
/// list atomically. Refuses a list that is not locked yet (no G2), an unknown
/// feature id, and any result set that is not complete.
pub fn apply_gate_passes(path: &Path, results: &BTreeMap<String, bool>) -> Result<(), String> {
    let before = FeatureList::load(path)?;
    if !before.is_locked() {
        return Err(
            "feature_list_locked: the gate writes passes only after G2 (spec_commit unset)".into(),
        );
    }
    let mut after = before.clone();
    for feature in &mut after.features {
        let Some(&passes) = results.get(&feature.id) else {
            return Err(format!(
                "gate result for feature {} is missing; the gate sets every feature",
                feature.id
            ));
        };
        feature.passes = passes;
    }
    if results.len() != after.features.len() {
        return Err("gate result names a feature the list does not hold".into());
    }
    validate_transition(&before, &after, Writer::Gate)?;
    let mut text = serde_json::to_string_pretty(&after).map_err(|e| e.to_string())?;
    text.push('\n');
    let tmp = path.with_extension("json.gate-tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("gate write failed: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("gate write failed: {e}")
    })
}
