// SPDX-License-Identifier: Apache-2.0
//! Current project policy at durable knowledge boundaries.
use std::path::Path;
use std::sync::Arc;

use super::{
    ConsolidatedInsight, KnowledgeFact, ProjectKnowledge, ProjectPattern, WithheldKnowledge,
};
use crate::core::context_admission::stores::StoreAdmission;
use crate::core::policy::{diagnostics, runtime};

#[cfg(test)]
#[path = "protection_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "protection_transition_tests.rs"]
mod transition_tests;

pub(crate) fn current(root: &str) -> Result<Option<Arc<runtime::ActivePolicy>>, String> {
    if let Some(request) = diagnostics::request_project()
        && crate::core::pathutil::safe_canonicalize_bounded(&request, 2000)
            != crate::core::pathutil::safe_canonicalize_bounded(Path::new(root), 2000)
    {
        return Err("knowledge project does not match bound request".into());
    }
    let policy = runtime::for_project(Path::new(root))?;
    if policy
        .as_ref()
        .is_some_and(|p| !p.tool_allowed("ctx_knowledge"))
    {
        return Err("knowledge access withheld by current policy".into());
    }
    Ok(policy)
}

#[derive(Clone, Copy)]
enum TypedSessionField {
    Generic,
    SourceSession,
    FromSessions,
}

/// One policy snapshot used for a complete knowledge operation. Text is never
/// rewritten in a persisted record: any blocked or changed string withholds the
/// complete record so its version identity and lineage remain truthful.
pub(crate) struct KnowledgeAdmission {
    root: String,
    gateway: StoreAdmission,
    project: Option<Arc<runtime::ActivePolicy>>,
    project_digest: Option<[u8; 32]>,
    /// Both policies of this snapshot: the key of remembered verdicts.
    rules: [u8; 32],
}

/// Remembered `exact_record` verdicts, keyed by the rules and the record's
/// exact bytes. A record is admitted the same way under the same rules, so
/// a store that grows without limit runs the detectors only on records that
/// are new or changed since (owner decision 24.10.: no knowledge limits;
/// every load used to admit every fact again). Changed rules give new keys.
static VERDICTS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<[u8; 32], bool>>> =
    std::sync::LazyLock::new(Default::default);
/// Past this many verdicts the memory starts over (about 40 bytes each).
const VERDICTS_MAX: usize = 4_000_000;

impl KnowledgeAdmission {
    pub(crate) fn capture(root: &str) -> Result<Self, String> {
        let project = current(root)?;
        let project_digest = project.as_deref().map(policy_digest).transpose()?;
        let gateway = StoreAdmission::current();
        let mut rules = blake3::Hasher::new();
        rules.update(gateway.digest().as_bytes());
        rules.update(&[u8::from(project_digest.is_some())]);
        rules.update(project_digest.as_ref().map_or(&[][..], |d| &d[..]));
        Ok(Self {
            root: root.to_owned(),
            gateway,
            project,
            project_digest,
            rules: *rules.finalize().as_bytes(),
        })
    }

    fn apply_project(&self, text: &str) -> Option<String> {
        let Some(policy) = &self.project else {
            return Some(text.to_owned());
        };
        if !policy.tool_allowed("ctx_knowledge") {
            return None;
        }
        let outcome = crate::core::policy::content::evaluate_text(text, policy);
        (!outcome.blocked).then_some(outcome.text)
    }

    /// Compose project and global rules over the original text in both orders.
    /// If they disagree, fail closed instead of losing a rule's original signal.
    pub(crate) fn admit_text(&self, original: &str) -> Option<String> {
        let project_original = self.apply_project(original)?;
        let gateway_original = self.gateway.admit_text(original)?;
        let gateway_then_project = self.apply_project(&gateway_original)?;
        let project_then_gateway = self.gateway.admit_text(&project_original)?;
        (gateway_then_project == project_then_gateway).then_some(gateway_then_project)
    }

    fn exact_text(&self, original: &str) -> bool {
        self.admit_text(original).as_deref() == Some(original)
    }

    fn exact_value(&self, value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(text) => self.exact_text(text),
            serde_json::Value::Array(values) => values.iter().all(|value| self.exact_value(value)),
            serde_json::Value::Object(values) => values
                .iter()
                .all(|(key, value)| self.exact_text(key) && self.exact_value(value)),
            _ => true,
        }
    }

    fn exact_session_identity(&self, text: &str) -> bool {
        let Some(id) = crate::core::session::parse_engine_session_id(text) else {
            return self.exact_text(text);
        };
        let project_allows = self.project.as_deref().is_none_or(|policy| {
            crate::core::policy::content::protect_engine_identity_with_active(id, policy)
                .is_ok_and(|admitted| admitted.as_ref() == text)
        });
        project_allows && self.gateway.admit_engine_session_id(id).as_deref() == Some(text)
    }

    fn exact_record_value(
        &self,
        value: &serde_json::Value,
        typed_session_field: TypedSessionField,
    ) -> bool {
        let Some(fields) = value.as_object() else {
            return self.exact_value(value);
        };
        fields.iter().all(|(key, value)| {
            let admitted = match (typed_session_field, key.as_str()) {
                (TypedSessionField::SourceSession, "source_session") => value
                    .as_str()
                    .is_some_and(|text| self.exact_session_identity(text)),
                (TypedSessionField::FromSessions, "from_sessions") => {
                    value.as_array().is_some_and(|items| {
                        items.iter().all(|item| {
                            item.as_str()
                                .is_some_and(|text| self.exact_session_identity(text))
                        })
                    })
                }
                _ => self.exact_value(value),
            };
            self.exact_text(key) && admitted
        })
    }

    fn exact_record<T: serde::Serialize>(
        &self,
        record: &T,
        typed_session_field: TypedSessionField,
    ) -> bool {
        let Ok(bytes) = serde_json::to_vec(record) else {
            return false;
        };
        let mut key = blake3::Hasher::new();
        key.update(&self.rules);
        key.update(&[typed_session_field as u8]);
        key.update(&bytes);
        let key = *key.finalize().as_bytes();
        let remembered = VERDICTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .copied();
        if let Some(verdict) = remembered {
            return verdict;
        }
        let verdict = serde_json::from_slice::<serde_json::Value>(&bytes)
            .is_ok_and(|value| self.exact_record_value(&value, typed_session_field));
        let mut verdicts = VERDICTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if verdicts.len() >= VERDICTS_MAX {
            verdicts.clear();
        }
        verdicts.insert(key, verdict);
        verdict
    }

    fn admits_archive_record_with_generic_fallback<T>(
        &self,
        value: &serde_json::Value,
        typed_session_field: TypedSessionField,
    ) -> bool
    where
        T: serde::de::DeserializeOwned + serde::Serialize,
    {
        let Ok(record) = serde_json::from_value::<T>(value.clone()) else {
            return self.exact_value(value);
        };
        let Some(input_fields) = value.as_object() else {
            return self.exact_value(value);
        };
        let Ok(known_record) = serde_json::to_value(record) else {
            return false;
        };
        let Some(known_fields) = known_record.as_object() else {
            return false;
        };
        if input_fields.len() != known_fields.len()
            || input_fields
                .keys()
                .any(|key| !known_fields.contains_key(key))
            || known_fields
                .keys()
                .any(|key| !input_fields.contains_key(key))
        {
            return self.exact_value(value);
        }
        self.exact_record_value(value, typed_session_field)
    }

    fn admits_complete_typed_archive_record<T>(
        &self,
        value: &serde_json::Value,
        typed_session_field: TypedSessionField,
    ) -> bool
    where
        T: serde::de::DeserializeOwned + serde::Serialize,
    {
        let Ok(record) = serde_json::from_value::<T>(value.clone()) else {
            return false;
        };
        let Some(input_fields) = value.as_object() else {
            return false;
        };
        let Ok(known_record) = serde_json::to_value(record) else {
            return false;
        };
        let Some(known_fields) = known_record.as_object() else {
            return false;
        };
        if input_fields.len() != known_fields.len()
            || input_fields
                .keys()
                .any(|key| !known_fields.contains_key(key))
            || known_fields
                .keys()
                .any(|key| !input_fields.contains_key(key))
        {
            return false;
        }
        self.exact_record_value(value, typed_session_field)
    }

    pub(crate) fn admits_archive_fact_value(&self, value: &serde_json::Value) -> bool {
        self.admits_archive_record_with_generic_fallback::<KnowledgeFact>(
            value,
            TypedSessionField::SourceSession,
        )
    }

    pub(crate) fn admits_archive_history_value(&self, value: &serde_json::Value) -> bool {
        self.admits_archive_record_with_generic_fallback::<ConsolidatedInsight>(
            value,
            TypedSessionField::FromSessions,
        )
    }

    pub(crate) fn admits_archive_pattern_value(&self, value: &serde_json::Value) -> bool {
        self.admits_archive_record_with_generic_fallback::<ProjectPattern>(
            value,
            TypedSessionField::SourceSession,
        )
    }

    pub(crate) fn admits_typed_archive_fact_value(&self, value: &serde_json::Value) -> bool {
        self.admits_complete_typed_archive_record::<KnowledgeFact>(
            value,
            TypedSessionField::SourceSession,
        )
    }

    pub(crate) fn admits_typed_archive_history_value(&self, value: &serde_json::Value) -> bool {
        self.admits_complete_typed_archive_record::<ConsolidatedInsight>(
            value,
            TypedSessionField::FromSessions,
        )
    }

    pub(crate) fn admits_typed_archive_pattern_value(&self, value: &serde_json::Value) -> bool {
        self.admits_complete_typed_archive_record::<ProjectPattern>(
            value,
            TypedSessionField::SourceSession,
        )
    }

    pub(crate) fn view(&self, knowledge: &ProjectKnowledge) -> Result<ProjectKnowledge, String> {
        if !self.exact_text(&knowledge.project_root) || !self.exact_text(&knowledge.project_hash) {
            return Err("knowledge storage identity is withheld by current policy".into());
        }
        let mut result = knowledge.clone();
        result.facts.clear();
        result.patterns.clear();
        result.history.clear();
        result.judged_pairs.clear();
        for fact in &knowledge.facts {
            if self.exact_record(fact, TypedSessionField::SourceSession) {
                result.facts.push(fact.clone());
            } else {
                result.withheld.push(WithheldKnowledge::Fact(fact.clone()));
            }
        }
        for pattern in &knowledge.patterns {
            if self.exact_record(pattern, TypedSessionField::SourceSession) {
                result.patterns.push(pattern.clone());
            } else {
                result
                    .withheld
                    .push(WithheldKnowledge::Pattern(pattern.clone()));
            }
        }
        for history in &knowledge.history {
            if self.exact_record(history, TypedSessionField::FromSessions) {
                result.history.push(history.clone());
            } else {
                result
                    .withheld
                    .push(WithheldKnowledge::History(history.clone()));
            }
        }
        for pair in &knowledge.judged_pairs {
            if self.exact_record(pair, TypedSessionField::Generic) {
                result.judged_pairs.push(pair.clone());
            } else {
                result
                    .withheld
                    .push(WithheldKnowledge::JudgedPair(pair.clone()));
            }
        }
        result.rebuild_index();
        Ok(result)
    }

    pub(crate) fn require_safe(&self, knowledge: &ProjectKnowledge) -> Result<(), String> {
        if !self.exact_text(&knowledge.project_root)
            || !self.exact_text(&knowledge.project_hash)
            || !knowledge
                .facts
                .iter()
                .all(|item| self.exact_record(item, TypedSessionField::SourceSession))
            || !knowledge
                .patterns
                .iter()
                .all(|item| self.exact_record(item, TypedSessionField::SourceSession))
            || !knowledge
                .history
                .iter()
                .all(|item| self.exact_record(item, TypedSessionField::FromSessions))
            || !knowledge
                .judged_pairs
                .iter()
                .all(|item| self.exact_record(item, TypedSessionField::Generic))
            || !knowledge.withheld.iter().all(|item| match item {
                WithheldKnowledge::Fact(item) => {
                    self.exact_record(item, TypedSessionField::SourceSession)
                }
                WithheldKnowledge::Pattern(item) => {
                    self.exact_record(item, TypedSessionField::SourceSession)
                }
                WithheldKnowledge::History(item) => {
                    self.exact_record(item, TypedSessionField::FromSessions)
                }
                WithheldKnowledge::JudgedPair(item) => {
                    self.exact_record(item, TypedSessionField::Generic)
                }
            })
        {
            return Err("knowledge requires an explicit policy-safe migration".into());
        }
        Ok(())
    }

    pub(crate) fn gateway_snapshot(&self) -> &StoreAdmission {
        &self.gateway
    }

    pub(crate) fn admits_archive_value(&self, value: &serde_json::Value) -> bool {
        self.exact_value(value)
    }

    pub(crate) fn authorize_gateway(&self) -> Result<(), String> {
        if !self.gateway.is_current() || StoreAdmission::current().digest() != self.gateway.digest()
        {
            return Err("knowledge Gateway policy changed; retry".into());
        }
        let project = current(&self.root)?;
        if project.as_deref().map(policy_digest).transpose()? != self.project_digest {
            return Err("knowledge project policy changed; retry".into());
        }
        Ok(())
    }

    pub(crate) fn authorize_publication(
        &self,
        authority: &runtime::PublicationAuthority,
    ) -> Result<(), String> {
        self.authorize_gateway()?;
        authority.verify()
    }
}

fn policy_digest(policy: &runtime::ActivePolicy) -> Result<[u8; 32], String> {
    let bytes = serde_json::to_vec(&policy.resolved).map_err(|_| "knowledge policy unavailable")?;
    Ok(*blake3::hash(&bytes).as_bytes())
}
