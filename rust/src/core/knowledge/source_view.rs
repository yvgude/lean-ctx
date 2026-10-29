// SPDX-License-Identifier: Apache-2.0
//! A source-authorized read view is not a replacement for the complete store.
use std::collections::HashMap;

use super::{FactOrigin, KnowledgeFact, ProjectKnowledge, WithheldKnowledge};

#[cfg(test)]
#[path = "source_view_tests.rs"]
mod tests;

impl ProjectKnowledge {
    pub(crate) fn admit_sources(self) -> Self {
        let root = self.project_root.clone();
        let original = self.clone();
        if let Ok(view) = crate::core::policy::runtime::with_project_source_view(&root, || {
            crate::core::providers::provenance::with_reuse_deadline(|| self.admit_sources_inner())
        }) {
            view.with_pulled_values_wrapped()
        } else {
            original.withhold_all_content()
        }
    }

    /// X5 invariant of every loaded store: the value of a fact pulled from
    /// Hive (other members' text) is a Hive wrapper. Values stored before
    /// the engine wrapped them at storage are wrapped here; a store written
    /// back (`mutate_locked`) keeps them so.
    fn with_pulled_values_wrapped(mut self) -> Self {
        use crate::core::hive::untrusted::{UntrustedKind, is_wrapped, wrap};
        for fact in &mut self.facts {
            if let Some(origin) = fact.hive_origin().map(str::to_string)
                && !is_wrapped(&fact.value)
            {
                fact.value = wrap(UntrustedKind::Knowledge, &origin, &fact.value);
            }
        }
        self
    }

    fn withhold_all_content(mut self) -> Self {
        self.withheld
            .extend(self.facts.drain(..).map(WithheldKnowledge::Fact));
        self.withheld
            .extend(self.patterns.drain(..).map(WithheldKnowledge::Pattern));
        self.withheld
            .extend(self.history.drain(..).map(WithheldKnowledge::History));
        self.withheld.extend(
            self.judged_pairs
                .drain(..)
                .map(WithheldKnowledge::JudgedPair),
        );
        self.rebuild_index();
        self
    }

    fn admit_sources_inner(mut self) -> Self {
        let protected = if let Ok(policy) = super::protection::current(&self.project_root) {
            policy.is_some()
        } else {
            return self.withhold_all_content();
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
                        self.withheld.push(WithheldKnowledge::Fact(fact));
                        continue;
                    };
                    if !seen.contains_key(&key) && seen.len() >= 32 {
                        self.withheld.push(WithheldKnowledge::Fact(fact));
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
                self.withheld.push(WithheldKnowledge::Fact(fact));
            }
        }
        self.facts = admitted;
        self.rebuild_index();
        self
    }

    /// Reconstitute the complete candidate only at the checked storage boundary.
    /// A stale view cannot resurrect a removed or changed hidden record.
    pub(super) fn complete_for_storage(&self, existing: &Self) -> Result<Self, String> {
        fn unique_json_match<'a, T: serde::Serialize>(
            items: &'a [T],
            hidden: &T,
        ) -> Result<&'a T, String> {
            let hidden = serde_json::to_value(hidden)
                .map_err(|_| "knowledge read view is not inspectable")?;
            let mut matches = items
                .iter()
                .filter(|current| serde_json::to_value(current).is_ok_and(|value| value == hidden));
            let Some(current) = matches.next() else {
                return Err("knowledge read view is stale; reload before saving".into());
            };
            if matches.next().is_some() {
                return Err("knowledge read view is ambiguous; reload before saving".into());
            }
            Ok(current)
        }

        let mut result = self.clone();
        for hidden in std::mem::take(&mut result.withheld) {
            match hidden {
                WithheldKnowledge::Fact(fact) => {
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
                WithheldKnowledge::Pattern(pattern) => {
                    result
                        .patterns
                        .push(unique_json_match(&existing.patterns, &pattern)?.clone());
                }
                WithheldKnowledge::History(history) => {
                    result
                        .history
                        .push(unique_json_match(&existing.history, &history)?.clone());
                }
                WithheldKnowledge::JudgedPair(pair) => {
                    result
                        .judged_pairs
                        .push(unique_json_match(&existing.judged_pairs, &pair)?.clone());
                }
            }
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
