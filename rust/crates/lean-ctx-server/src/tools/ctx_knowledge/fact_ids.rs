// SPDX-License-Identifier: Apache-2.0
//! What every `local-read/1` knowledge document must agree on — `knowledge
//! list`, `knowledge graph|related` and the freshness listings (`stale`,
//! `verify`, `dedupe`, `outdated`, `conflicts`): one fact id per fact, one
//! read-noise rule, one timestamp format. All of them use these, never
//! their own copies (the CLI re-exports them as `cli::knowledge_shared`).

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};

use lean_ctx::core::knowledge::{FactOrigin, KnowledgeFact, fact_version_id_v1};

/// RFC 3339 UTC, whole seconds, `Z` (`2026-11-04T09:12:33Z`).
pub fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Automatic read-noise: machine-derived facts (`auto:` category or key,
/// written by auto-capture from tool output) and `finding`s that only
/// record a read, a search or a lookup in the producer's own wording.
pub fn is_read_noise(f: &KnowledgeFact) -> bool {
    lean_ctx::core::knowledge::is_machine_derived(&f.category, &f.key)
        || (f.category.eq_ignore_ascii_case("finding")
            && lean_ctx::core::auto_findings::is_activity_record(f.value.trim()))
}

/// The id of a superseded version, or of a second current fact on the
/// same key: `<category>/<key>@<version>`.
pub fn version_id(f: &KnowledgeFact) -> String {
    format!("{}/{}@{}", f.category, f.key, fact_version_id_v1(f))
}

/// The ids of `current` (current facts), in the same order. Facts on one
/// `<category>/<key>`: the `local` one keeps the plain id (else the newest,
/// then the lower version), every other one gets [`version_id`]. Unique
/// for distinct facts.
pub fn current_ids(current: &[&KnowledgeFact]) -> Vec<String> {
    let mut groups: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (i, f) in current.iter().enumerate() {
        groups.entry((&f.category, &f.key)).or_default().push(i);
    }
    let mut ids = vec![String::new(); current.len()];
    for ((category, key), mut members) in groups {
        members.sort_by(|&a, &b| {
            let (a, b) = (current[a], current[b]);
            let rank = |f: &KnowledgeFact| u8::from(!matches!(f.origin, FactOrigin::Local));
            rank(a)
                .cmp(&rank(b))
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| fact_version_id_v1(a).cmp(&fact_version_id_v1(b)))
        });
        for (n, i) in members.into_iter().enumerate() {
            ids[i] = if n == 0 {
                format!("{category}/{key}")
            } else {
                version_id(current[i])
            };
        }
    }
    ids
}
