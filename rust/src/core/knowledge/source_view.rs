// SPDX-License-Identifier: Apache-2.0
//! A source-authorized read view is not a replacement for the complete store.
use std::collections::HashMap;

use super::{FactOrigin, KnowledgeFact, ProjectKnowledge};

#[cfg(test)]
#[path = "source_view_tests.rs"]
mod tests;

impl ProjectKnowledge {
    pub(crate) fn admit_sources(self) -> Self {
        let root = self.project_root.clone();
        let mut original = self.clone();
        if let Ok(view) = crate::core::policy::runtime::with_project_source_view(&root, || {
            crate::core::providers::provenance::with_reuse_deadline(|| self.admit_sources_inner())
        }) {
            view
        } else {
            original.withheld.append(&mut original.facts);
            original.rebuild_index();
            original
        }
    }

    fn admit_sources_inner(mut self) -> Self {
        let protected = if let Ok(policy) = super::protection::current(&self.project_root) {
            policy.is_some()
        } else {
            self.withheld.append(&mut self.facts);
            self.rebuild_index();
            return self;
        };
        let mut seen: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        let mut admitted = Vec::new();
        for fact in std::mem::take(&mut self.facts) {
            let allowed = match &fact.origin {
                FactOrigin::Local => true,
                FactOrigin::Unverified => !protected,
                // A derived value needs its own verifiable derivation before
                // source-backed inputs can authorize it, even outside policy mode.
                FactOrigin::Derived(_) => false,
                FactOrigin::Provider(origin) => {
                    let Some(key) = crate::core::providers::provenance::digest(origin) else {
                        self.withheld.push(fact);
                        continue;
                    };
                    if !seen.contains_key(&key) && seen.len() >= 32 {
                        self.withheld.push(fact);
                        continue;
                    }
                    let current = seen.entry(key).or_insert_with(|| {
                        origin
                            .current_facts(&self.project_root)
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|f| {
                                let value = crate::core::policy::content::protect_active(&f.value)
                                    .ok()?
                                    .into_owned();
                                Some((f.category, f.key, value))
                            })
                            .collect()
                    });
                    current.iter().any(|(category, key, value)| {
                        category == &fact.category && key == &fact.key && value == &fact.value
                    })
                }
            };
            if allowed {
                admitted.push(fact);
            } else {
                self.withheld.push(fact);
            }
        }
        self.facts = admitted;
        self.rebuild_index();
        self
    }

    /// Reconstitute the complete candidate only at the checked storage boundary.
    /// A stale view cannot resurrect a removed or changed hidden record.
    pub(super) fn complete_for_storage(&self, existing: &Self) -> Result<Self, String> {
        let mut result = self.clone();
        for fact in std::mem::take(&mut result.withheld) {
            let mut matches = existing.facts.iter().filter(|current| {
                same_record(current, &fact)
                    && current.created_at == fact.created_at
                    && current.source_session == fact.source_session
            });
            let Some(current) = matches.next() else {
                return Err("knowledge read view is stale; reload before saving".into());
            };
            if matches.next().is_some() {
                return Err("knowledge read view is ambiguous; reload before saving".into());
            }
            // Keep the latest counters and validity state; never restore an old
            // hidden copy over a concurrent update.
            result.facts.push(current.clone());
        }
        result.rebuild_index();
        Ok(result)
    }
}

pub(crate) fn imported_origin(origin: &FactOrigin) -> FactOrigin {
    match origin {
        // Source dependencies survive import; the next read reacquires them.
        FactOrigin::Provider(source) => FactOrigin::Provider(source.clone()),
        FactOrigin::Local | FactOrigin::Unverified => FactOrigin::Unverified,
        FactOrigin::Derived(sources) => {
            FactOrigin::Derived(sources.iter().map(imported_origin).collect())
        }
    }
}

pub(crate) fn same_record(a: &KnowledgeFact, b: &KnowledgeFact) -> bool {
    a.category == b.category && a.key == b.key && a.value == b.value && a.origin == b.origin
}
