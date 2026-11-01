//! Explicit cross-store restore from the lossless memory archive (#995 Phase 6).
//!
//! Every capacity reclaim (facts, history, patterns) archives the
//! evicted tail under `memory/archive/<store>/` before dropping it. This module
//! reads those archives back and merges the matching items into the live stores —
//! the user-facing "undo" for an over-eager reclaim, surfaced as
//! `lean-ctx knowledge restore` and `ctx_knowledge action=restore`.
//!
//! Idempotent: an item already present (by its store-specific identity) is never
//! duplicated, and a live fact never has its key clobbered by an older archived
//! value.

use crate::core::context_admission::stores::StoreAdmission;
use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_archive::{ArchiveConfig, MemoryStore, reachable_archives};

/// Default cap on how many items a single restore call rehydrates, across all
/// requested stores. Generous enough to undo a normal reclaim, bounded so a
/// `restore` with no query cannot resurrect an unbounded archive in one shot.
pub(crate) const DEFAULT_RESTORE_LIMIT: usize = 50;

/// Parsed `restore` request.
pub(crate) struct RestoreOptions {
    /// Stores to scan. Empty selection is treated as "all stores".
    pub stores: Vec<MemoryStore>,
    /// Optional case-insensitive substring filter on each item's text.
    pub query: Option<String>,
    /// Maximum items restored across all stores.
    pub limit: usize,
}

impl RestoreOptions {
    /// Restore from `store` (or all stores when `None`), filtered by `query`.
    pub(crate) fn new(store: Option<MemoryStore>, query: Option<String>, limit: usize) -> Self {
        Self {
            stores: store.map_or_else(|| MemoryStore::all().to_vec(), |s| vec![s]),
            query: query.filter(|q| !q.trim().is_empty()),
            limit: limit.max(1),
        }
    }
}

/// Per-store restore outcome.
pub(crate) struct StoreRestore {
    pub store: MemoryStore,
    /// Archived items that matched the query (before the dedup/limit cut).
    pub matched: usize,
    /// Items actually merged back into the live store.
    pub restored: usize,
}

/// Aggregate restore report.
pub(crate) struct RestoreReport {
    pub per_store: Vec<StoreRestore>,
    pub query: Option<String>,
}

impl RestoreReport {
    pub(crate) fn total_restored(&self) -> usize {
        self.per_store.iter().map(|s| s.restored).sum()
    }
}

/// Read + query-filter every reachable archive for `store`/`scope`, newest
/// archive first. `searchable` projects an item to the text the query matches on.
fn collect<T, S>(
    store: MemoryStore,
    scope: Option<&str>,
    cfg: &ArchiveConfig,
    query: Option<&str>,
    admission: &crate::core::knowledge::protection::KnowledgeAdmission,
    searchable: S,
) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
    S: Fn(&T) -> String,
{
    let needle = query.map(str::to_lowercase);
    let mut out = Vec::new();
    // Newest archives first: the most recent eviction is the likeliest undo target.
    for path in reachable_archives(store, scope, cfg).into_iter().rev() {
        let Ok(items) = crate::core::memory_archive::restore_knowledge_items::<T>(&path, admission)
        else {
            continue;
        };
        for it in items {
            if needle
                .as_deref()
                .is_none_or(|n| searchable(&it).to_lowercase().contains(n))
            {
                out.push(it);
            }
        }
    }
    out
}

/// Restore archived facts. Skips facts already present (same category/key/value)
/// and never resurrects an older value for a key a live fact still owns.
fn restore_facts(
    knowledge: &mut ProjectKnowledge,
    cfg: &ArchiveConfig,
    query: Option<&str>,
    remaining: &mut usize,
    admission: &crate::core::knowledge::protection::KnowledgeAdmission,
) -> StoreRestore {
    let archived = collect::<crate::core::knowledge::KnowledgeFact, _>(
        MemoryStore::Facts,
        None,
        cfg,
        None,
        admission,
        |f| format!("{} {} {}", f.category, f.key, f.value),
    );
    // Authorization precedes query matching and even the reported match count.
    // The archive remains unchanged when a source is currently unavailable.
    let mut candidate = ProjectKnowledge::new(&knowledge.project_root);
    candidate.facts = archived;
    let needle = query.map(str::to_lowercase);
    let cands: Vec<_> = candidate
        .admit_sources()
        .facts
        .into_iter()
        .filter(|f| {
            needle.as_ref().is_none_or(|needle| {
                format!("{} {} {}", f.category, f.key, f.value)
                    .to_lowercase()
                    .contains(needle)
            })
        })
        .collect();
    let matched = cands.len();
    let mut restored = 0;
    for f in cands {
        if *remaining == 0 {
            break;
        }
        let already = knowledge
            .facts
            .iter()
            .chain(knowledge.withheld.iter().filter_map(|item| match item {
                crate::core::knowledge::WithheldKnowledge::Fact(fact) => Some(fact),
                _ => None,
            }))
            .any(|e| crate::core::knowledge::source_view::same_record(e, &f));
        if already {
            continue;
        }
        let key_owned = knowledge
            .facts
            .iter()
            .chain(knowledge.withheld.iter().filter_map(|item| match item {
                crate::core::knowledge::WithheldKnowledge::Fact(fact) => Some(fact),
                _ => None,
            }))
            .any(|e| e.category == f.category && e.key == f.key && e.is_current());
        if key_owned {
            continue;
        }
        knowledge.facts.push(f);
        restored += 1;
        *remaining -= 1;
    }
    StoreRestore {
        store: MemoryStore::Facts,
        matched,
        restored,
    }
}

/// Restore archived consolidated-history insights (dedup by summary+sessions+ts).
fn restore_history(
    knowledge: &mut ProjectKnowledge,
    hash: &str,
    cfg: &ArchiveConfig,
    query: Option<&str>,
    remaining: &mut usize,
    admission: &crate::core::knowledge::protection::KnowledgeAdmission,
) -> StoreRestore {
    let cands = collect::<crate::core::knowledge::ConsolidatedInsight, _>(
        MemoryStore::History,
        Some(hash),
        cfg,
        query,
        admission,
        |h| h.summary.clone(),
    );
    let matched = cands.len();
    let mut restored = 0;
    for h in cands {
        if *remaining == 0 {
            break;
        }
        let already = knowledge.history.iter().any(|e| {
            e.summary == h.summary
                && e.from_sessions == h.from_sessions
                && e.timestamp == h.timestamp
        });
        if already {
            continue;
        }
        knowledge.history.push(h);
        restored += 1;
        *remaining -= 1;
    }
    StoreRestore {
        store: MemoryStore::History,
        matched,
        restored,
    }
}

/// Restore archived project patterns (dedup by type+description+source+created).
fn restore_patterns(
    knowledge: &mut ProjectKnowledge,
    hash: &str,
    cfg: &ArchiveConfig,
    query: Option<&str>,
    remaining: &mut usize,
    admission: &crate::core::knowledge::protection::KnowledgeAdmission,
) -> StoreRestore {
    let cands = collect::<crate::core::knowledge::ProjectPattern, _>(
        MemoryStore::Patterns,
        Some(hash),
        cfg,
        query,
        admission,
        |p| format!("{} {}", p.pattern_type, p.description),
    );
    let matched = cands.len();
    let mut restored = 0;
    for p in cands {
        if *remaining == 0 {
            break;
        }
        let already = knowledge.patterns.iter().any(|e| {
            e.pattern_type == p.pattern_type
                && e.description == p.description
                && e.source_session == p.source_session
                && e.created_at == p.created_at
        });
        if already {
            continue;
        }
        knowledge.patterns.push(p);
        restored += 1;
        *remaining -= 1;
    }
    StoreRestore {
        store: MemoryStore::Patterns,
        matched,
        restored,
    }
}

/// Restore archived items into the live stores. Facts/history/patterns share one
/// locked knowledge write. Archives of the retired procedure store stay on disk
/// untouched and are never restored.
pub(crate) fn run_restore(
    project_root: &str,
    opts: &RestoreOptions,
) -> Result<RestoreReport, String> {
    crate::core::policy::runtime::with_explicit_project_source_view(
        std::path::Path::new(project_root),
        |_| run_restore_in_view(project_root, opts),
    )?
}

fn run_restore_in_view(project_root: &str, opts: &RestoreOptions) -> Result<RestoreReport, String> {
    let admission = StoreAdmission::current();
    let knowledge_admission =
        crate::core::knowledge::protection::KnowledgeAdmission::capture(project_root)?;
    let cfg = ArchiveConfig::from_env();
    let query = opts.query.clone();
    let want = |s: MemoryStore| opts.stores.contains(&s);

    let mut per_store: Vec<StoreRestore> = Vec::new();
    let remaining = opts.limit;

    let knowledge_wanted =
        want(MemoryStore::Facts) || want(MemoryStore::History) || want(MemoryStore::Patterns);

    if want(MemoryStore::Facts) {
        // Legacy facts whose session proves this project join its partition.
        crate::core::legacy_adoption::auto_adopt_facts(project_root);
    }
    if knowledge_wanted {
        let (_k, results) = ProjectKnowledge::mutate_locked_with_admission(
            project_root,
            &knowledge_admission,
            |knowledge| {
                let hash = knowledge.project_hash.clone();
                let mut local = Vec::new();
                let mut rem = remaining;
                if want(MemoryStore::Facts) {
                    local.push(restore_facts(
                        knowledge,
                        &cfg,
                        query.as_deref(),
                        &mut rem,
                        &knowledge_admission,
                    ));
                }
                if want(MemoryStore::History) {
                    local.push(restore_history(
                        knowledge,
                        &hash,
                        &cfg,
                        query.as_deref(),
                        &mut rem,
                        &knowledge_admission,
                    ));
                }
                if want(MemoryStore::Patterns) {
                    local.push(restore_patterns(
                        knowledge,
                        &hash,
                        &cfg,
                        query.as_deref(),
                        &mut rem,
                        &knowledge_admission,
                    ));
                }
                local
            },
        )?;
        per_store.extend(results);
    }

    if !admission.is_current() {
        return Err("session admission policy changed; retry".into());
    }
    knowledge_admission.authorize_gateway()?;
    Ok(RestoreReport {
        per_store,
        query: opts.query.clone(),
    })
}

/// Deterministic, human-readable restore summary (no timestamps — #498).
pub(crate) fn format_restore_report(report: &RestoreReport) -> String {
    let total = report.total_restored();
    let scope = match &report.query {
        Some(q) => format!(" matching \"{q}\""),
        None => String::new(),
    };

    if total == 0 {
        let matched: usize = report.per_store.iter().map(|s| s.matched).sum();
        if matched == 0 {
            return format!("No archived items{scope} to restore.");
        }
        return format!(
            "Nothing restored{scope}: all {matched} matching archived item(s) are already live."
        );
    }

    let mut out = format!("Restored {total} item(s){scope} from archive:");
    for s in &report.per_store {
        if s.restored > 0 {
            out.push_str(&format!(
                "\n  {}: {} restored ({} matched)",
                s.store.as_str(),
                s.restored,
                s.matched
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge::ProjectKnowledge;
    use crate::core::memory_archive::archive_items;
    use crate::core::memory_policy::MemoryPolicy;

    /// Sandbox the data dir and hand the closure a fresh project root inside it.
    fn with_sandbox<T>(f: impl FnOnce(String) -> T) -> T {
        let isolated = crate::core::data_dir::isolated_data_dir();
        // A raw nanosecond timestamp can accidentally pass a payment-card
        // checksum. Keep this fixture's project identity free of such content
        // while exercising the normal Gateway PII policy without exemptions.
        let root = isolated.path().join("proj").to_string_lossy().to_string();
        std::fs::create_dir_all(&root).unwrap();
        f(root)
    }

    /// Archive `facts` into `root`'s partition, as the request that restores
    /// them resolves it.
    fn archive_facts_of(root: &str, facts: &[crate::core::knowledge::KnowledgeFact]) {
        let project = std::path::Path::new(root).canonicalize().unwrap();
        crate::core::policy::runtime::REQUEST_PROJECT
            .sync_scope(std::cell::RefCell::new(Some(project)), || {
                archive_items(MemoryStore::Facts, None, facts, &ArchiveConfig::default())
            })
            .unwrap();
    }

    /// Archive every current fact, then drop them from the live store — the exact
    /// state a capacity reclaim leaves behind.
    fn archive_then_drop_facts(root: &str) {
        let mut k = ProjectKnowledge::load(root).expect("knowledge");
        archive_facts_of(root, &k.facts);
        k.facts.clear();
        k.save().unwrap();
    }

    #[test]
    fn restore_facts_round_trip_is_idempotent() {
        with_sandbox(|root| {
            let policy = MemoryPolicy::default();
            let mut k = ProjectKnowledge::new(&root);
            k.remember("auth", "token", "JWT RS256", "s1", 0.9, &policy);
            k.remember("db", "engine", "Postgres", "s1", 0.8, &policy);
            k.save().unwrap();
            archive_then_drop_facts(&root);

            let opts = RestoreOptions::new(Some(MemoryStore::Facts), None, 50);
            let report = run_restore(&root, &opts).unwrap();
            assert_eq!(report.total_restored(), 2);

            let reloaded = ProjectKnowledge::load(&root).unwrap();
            assert_eq!(reloaded.facts.iter().filter(|f| f.is_current()).count(), 2);

            // Already live → second restore recovers nothing.
            let again = run_restore(&root, &opts).unwrap();
            assert_eq!(again.total_restored(), 0);
        });
    }

    #[test]
    fn restore_facts_honors_query_filter() {
        with_sandbox(|root| {
            let policy = MemoryPolicy::default();
            let mut k = ProjectKnowledge::new(&root);
            k.remember("auth", "token", "JWT RS256", "s1", 0.9, &policy);
            k.remember("db", "engine", "Postgres", "s1", 0.8, &policy);
            k.save().unwrap();
            archive_then_drop_facts(&root);

            let opts = RestoreOptions::new(Some(MemoryStore::Facts), Some("postgres".into()), 50);
            let report = run_restore(&root, &opts).unwrap();
            assert_eq!(report.total_restored(), 1);

            let reloaded = ProjectKnowledge::load(&root).unwrap();
            assert!(
                reloaded
                    .facts
                    .iter()
                    .any(|f| f.key == "engine" && f.is_current())
            );
            assert!(
                !reloaded
                    .facts
                    .iter()
                    .any(|f| f.key == "token" && f.is_current())
            );
        });
    }

    #[test]
    fn restore_never_resurrects_a_superseded_live_key() {
        with_sandbox(|root| {
            let policy = MemoryPolicy::default();
            // An older value for auth/token sits in the archive…
            let mut k = ProjectKnowledge::new(&root);
            k.remember("auth", "token", "opaque session token", "s0", 0.7, &policy);
            archive_facts_of(&root, &k.facts);
            // …while a different value currently owns that key.
            k.facts.clear();
            k.remember("auth", "token", "JWT RS256", "s1", 0.9, &policy);
            k.save().unwrap();

            let opts = RestoreOptions::new(Some(MemoryStore::Facts), None, 50);
            let report = run_restore(&root, &opts).unwrap();
            assert_eq!(
                report.total_restored(),
                0,
                "a live key must not be shadowed by an archived older value"
            );
        });
    }

    #[test]
    fn restore_patterns_round_trip_is_scoped() {
        with_sandbox(|root| {
            let policy = MemoryPolicy::default();
            let mut k = ProjectKnowledge::new(&root);
            k.add_pattern(
                "error-handling",
                "wrap IO in Result",
                Vec::new(),
                "s1",
                &policy,
            );
            k.add_pattern("naming", "snake_case modules", Vec::new(), "s1", &policy);
            archive_items(
                MemoryStore::Patterns,
                Some(&k.project_hash),
                &k.patterns,
                &ArchiveConfig::default(),
            )
            .unwrap();
            k.patterns.clear();
            k.save().unwrap();

            let opts = RestoreOptions::new(Some(MemoryStore::Patterns), None, 50);
            let report = run_restore(&root, &opts).unwrap();
            assert_eq!(report.total_restored(), 2);

            let reloaded = ProjectKnowledge::load(&root).unwrap();
            assert_eq!(reloaded.patterns.len(), 2);
        });
    }

    #[test]
    fn restore_report_is_deterministic() {
        // #498: report bodies must be byte-stable for prompt caching — no
        // timestamps, counters or run-dependent ordering in the rendered text.
        with_sandbox(|root| {
            let policy = MemoryPolicy::default();
            let mut k = ProjectKnowledge::new(&root);
            k.remember("auth", "token", "JWT", "s1", 0.9, &policy);
            k.save().unwrap();
            archive_then_drop_facts(&root);

            let opts = RestoreOptions::new(Some(MemoryStore::Facts), None, 50);
            let report = run_restore(&root, &opts).unwrap();
            assert_eq!(
                format_restore_report(&report),
                format_restore_report(&report)
            );
        });
    }

    #[test]
    fn restore_limit_caps_total_recovered() {
        with_sandbox(|root| {
            let policy = MemoryPolicy::default();
            let mut k = ProjectKnowledge::new(&root);
            for i in 0..5 {
                k.remember(
                    "finding",
                    &format!("k{i}"),
                    &format!("value {i}"),
                    "s1",
                    0.8,
                    &policy,
                );
            }
            k.save().unwrap();
            archive_then_drop_facts(&root);

            let opts = RestoreOptions::new(Some(MemoryStore::Facts), None, 2);
            let report = run_restore(&root, &opts).unwrap();
            assert_eq!(report.total_restored(), 2, "limit bounds the recovery");
        });
    }

    #[test]
    fn restore_reports_nothing_when_archive_empty() {
        with_sandbox(|root| {
            let opts = RestoreOptions::new(None, None, 50);
            let report = run_restore(&root, &opts).unwrap();
            assert_eq!(report.total_restored(), 0);
            assert!(format_restore_report(&report).contains("No archived items"));
        });
    }
}
