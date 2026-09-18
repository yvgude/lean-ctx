use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::ranking::hash_project_root;
use super::types::{ConsolidatedInsight, KnowledgeFact, ProjectKnowledge, ProjectPattern};
use crate::core::memory_policy::MemoryPolicy;

fn knowledge_dir(project_hash: &str) -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("knowledge")
        .join(project_hash))
}

/// Per-project-hash mutex registry. Serializes the read-modify-write cycle of
/// `mutate_locked` so concurrent `remember` calls within a single process (e.g.
/// parallel MCP tool calls) cannot clobber each other (issue #326). The outer
/// map lock is held only briefly to clone the inner `Arc`; the inner lock is
/// held across the load → mutate → save cycle.
fn knowledge_lock(project_hash: &str) -> Arc<Mutex<()>> {
    static KNOWLEDGE_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = KNOWLEDGE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .entry(project_hash.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Acquires an exclusive, cross-process advisory lock for a project's
/// knowledge store. The returned file handle holds the lock until it is
/// dropped; the OS releases it automatically if the process exits (even on
/// crash), so there are no stale locks. This serializes the read-modify-write
/// cycle across *separate processes* (parallel CLI invocations, CLI + daemon +
/// MCP server), complementing the in-process mutex (issue #326).
fn acquire_file_lock(dir: &Path) -> Option<std::fs::File> {
    use fs2::FileExt;
    let lock_path = dir.join(".knowledge.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
    }
    // Blocks until every other process holding the lock releases it. A failure
    // here (unsupported FS, etc.) degrades to the in-process lock only.
    file.lock_exclusive().ok()?;
    Some(file)
}

/// Atomically writes `json` to `path` by writing to a unique temp file in the
/// same directory and renaming it into place. `rename` is atomic on every
/// supported platform (and replaces the target on Windows), so readers and
/// concurrent writers never observe a half-written file — preventing the
/// trailing-garbage JSON corruption reported in issue #326.
fn write_json_atomic(dir: &Path, path: &Path, json: &str) -> Result<(), String> {
    let unique = format!(
        "knowledge.json.tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let tmp = dir.join(unique);
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

/// Marker that identifies a fact as machine-derived rather than curated.
///
/// Both automatic producers already carry it: `extract_session_facts` tags the
/// *category* (`auto:decision`, `auto:pattern`, `auto:blocker`) and
/// `auto_capture` derives an `auto:`-prefixed *key*. Nothing a person writes
/// through `ctx_knowledge(action="remember")` does.
pub(crate) const MACHINE_DERIVED_PREFIX: &str = "auto:";

/// Whether this fact was derived by lean-ctx rather than written by a person.
pub(crate) fn is_machine_derived(category: &str, key: &str) -> bool {
    category.trim_start().starts_with(MACHINE_DERIVED_PREFIX)
        || key.trim_start().starts_with(MACHINE_DERIVED_PREFIX)
}

/// Whether a machine-derived fact may enter the durable store right now.
///
/// #1802: `auto_capture = false` was honoured by exactly one producer
/// (`auto_capture::capture_finding`) and ignored by the other
/// (`extract_session_facts`, reached from both the consolidation engine and
/// the session *save* path) — so a single MCP call re-materialised every
/// deleted `auto:*` fact, complete with its original `created_at`, and a
/// curated store could not be kept clean.
///
/// The gate therefore lives here, at the store's own ingestion points, and not
/// at the producers: a check placed at a call site only covers the call sites
/// that exist when it is written, which is exactly how this defect arose.
/// Session state itself is left untouched — it is ephemeral, capacity-capped,
/// and backs handoff, session recap and metrics, none of which this key
/// disables.
pub(crate) fn machine_derived_writes_allowed() -> bool {
    crate::core::auto_capture::is_enabled()
}

impl ProjectKnowledge {
    /// Return the most recent active decision-like facts for session handoff.
    /// Adds a pre-built fact, coalescing repeated observations into confirmations.
    pub fn add_fact(&mut self, mut fact: KnowledgeFact) -> bool {
        let trimmed_cat = fact.category.trim().to_string();
        fact.category = trimmed_cat;
        let trimmed_val = fact.value.trim().to_string();
        fact.value = trimmed_val;
        if fact.category.is_empty() || fact.value.is_empty() {
            return false;
        }
        // #1802: refuse machine-derived facts while auto-capture is off. Placed
        // before the coalescing branch below so a disabled run cannot even
        // refresh `last_confirmed` on facts a previous enabled run left behind.
        if is_machine_derived(&fact.category, &fact.key) && !machine_derived_writes_allowed() {
            return false;
        }

        if let Some(existing) = self
            .facts
            .iter_mut()
            .find(|existing| existing.category == fact.category && existing.value == fact.value)
        {
            if fact.last_confirmed > existing.last_confirmed {
                existing.last_confirmed = fact.last_confirmed;
            }
            existing.confidence = existing.confidence.max(fact.confidence);
            existing.confirmation_count = existing.confirmation_count.saturating_add(1);
            existing.update_fidelity();
        } else {
            fact.update_fidelity();
            self.facts.push(fact);
            self.rebuild_index();
        }

        self.updated_at = Utc::now();
        true
    }

    pub fn recent_decisions(&self, limit: usize) -> Vec<KnowledgeFact> {
        let mut decisions: Vec<KnowledgeFact> = self
            .facts
            .iter()
            .filter(|fact| {
                let category = fact.category.to_ascii_lowercase();
                fact.is_current()
                    && (category.contains("decision")
                        || category.contains("architecture")
                        || category.contains("solution"))
            })
            .cloned()
            .collect();
        decisions.sort_by(|a, b| {
            b.last_confirmed
                .cmp(&a.last_confirmed)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        decisions.truncate(limit);
        decisions
    }
    /// Keyword-based knowledge search across all facts.
    /// Serves as the API surface for semantic retrieval; currently falls back
    /// to substring matching. When the embedding model is loaded, this will
    /// use HNSW vector search instead.
    pub fn search_semantic(&self, query: &str, limit: usize) -> Vec<KnowledgeFact> {
        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, &KnowledgeFact)> = self
            .facts
            .iter()
            .filter(|f| f.valid_until.is_none_or(|t| t > chrono::Utc::now()))
            .map(|fact| {
                let haystack = format!(
                    "{} {} {}",
                    fact.category.to_lowercase(),
                    fact.value.to_lowercase(),
                    fact.key.to_lowercase(),
                );
                let score = terms.iter().filter(|t| haystack.contains(*t)).count();
                (score, fact)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, f)| f.clone())
            .collect()
    }

    pub fn list_project_roots() -> Result<Vec<String>, String> {
        let base = crate::core::data_dir::lean_ctx_data_dir()?.join("knowledge");
        if !base.exists() {
            return Ok(Vec::new());
        }

        let mut roots = Vec::new();
        let mut seen = HashSet::new();
        let entries = std::fs::read_dir(&base).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let path = entry.path().join("knowledge.json");
            if !path.is_file() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(knowledge) = serde_json::from_str::<Self>(&content) else {
                continue;
            };
            if seen.insert(knowledge.project_root.clone()) {
                roots.push(knowledge.project_root);
            }
        }

        roots.sort();
        Ok(roots)
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = knowledge_dir(&self.project_hash)?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }

        let path = dir.join("knowledge.json");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_json_atomic(&dir, &path, &json)?;
        Ok(())
    }

    /// Runs `f` while holding this project's locks — the in-process per-hash
    /// mutex *and* the cross-process advisory file lock — without loading or
    /// saving the knowledge JSON itself. [`mutate_locked`](Self::mutate_locked)
    /// is built on this, and side-car stores that must stay consistent with the
    /// facts (today: the embedding index) call it directly so their
    /// read-modify-write is serialized against parallel
    /// `remember`/`remove`/`reindex`. That side-car write used to run lock-free,
    /// so concurrent callers clobbered each other's embeddings and pruned
    /// just-stored vectors, degrading semantic recall (issue #412, a #326
    /// follow-up).
    pub(crate) fn with_project_lock<T>(project_root: &str, f: impl FnOnce() -> T) -> T {
        let hash = hash_project_root(project_root);
        let lock = knowledge_lock(&hash);
        let _guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Cross-process lock: create the dir up front so the lock file has a
        // home, then block until any other process releases it. Held for the
        // whole critical section via `_file_lock`'s lifetime.
        let _file_lock = match knowledge_dir(&hash) {
            Ok(dir) => {
                let _ = std::fs::create_dir_all(&dir);
                acquire_file_lock(&dir)
            }
            Err(_) => None,
        };

        f()
    }

    /// Runs a read-modify-write cycle under `with_project_lock`, then saves
    /// atomically. The knowledge is (re)loaded *inside* the lock so
    /// the closure always operates on the latest on-disk state; this is what
    /// prevents lost updates when several `remember` calls run in parallel —
    /// whether as threads in one process (parallel MCP calls) or as separate
    /// processes (parallel CLI invocations, CLI + daemon + MCP server) — see
    /// issue #326. Returns the persisted knowledge plus the closure's return
    /// value so the caller can build a response from the committed state.
    pub fn mutate_locked<T>(
        project_root: &str,
        f: impl FnOnce(&mut Self) -> T,
    ) -> Result<(Self, T), String> {
        Self::with_project_lock(project_root, || {
            let mut knowledge = Self::load_or_create(project_root);
            let out = f(&mut knowledge);
            knowledge.save()?;
            Ok((knowledge, out))
        })
    }

    pub fn load(project_root: &str) -> Option<Self> {
        let hash = hash_project_root(project_root);
        let dir = knowledge_dir(&hash).ok()?;
        let path = dir.join("knowledge.json");

        if let Ok(content) = std::fs::read_to_string(&path) {
            let size = content.len();
            if size > 1_000_000 {
                tracing::warn!(
                    "knowledge.json is large ({:.1} MB) — recall may be slow. \
                     Consider running ctx_knowledge(action=\"consolidate\") to compact it.",
                    size as f64 / 1_048_576.0,
                );
            }
            if let Ok(mut k) = serde_json::from_str::<Self>(&content) {
                k.rebuild_index();
                return Some(k);
            }
        }

        let old_hash = crate::core::project_hash::hash_path_only(project_root);
        if old_hash != hash {
            crate::core::project_hash::migrate_if_needed(&old_hash, &hash, project_root);
            if let Ok(content) = std::fs::read_to_string(&path)
                && let Ok(mut k) = serde_json::from_str::<Self>(&content)
            {
                k.project_hash = hash;
                k.rebuild_index();
                let _ = k.save();
                return Some(k);
            }
        }

        // Migrate stores created before path normalization (issue #325): on
        // Windows the CLI keyed its store by a backslash path, splitting it from
        // the forward-slash MCP store. Pull any such legacy store into the
        // canonical (normalized) location so facts converge.
        for legacy_hash in crate::core::project_hash::legacy_unnormalized_hashes(project_root) {
            if legacy_hash == hash {
                continue;
            }
            crate::core::project_hash::migrate_if_needed(&legacy_hash, &hash, project_root);
            if let Ok(content) = std::fs::read_to_string(&path)
                && let Ok(mut k) = serde_json::from_str::<Self>(&content)
            {
                k.project_hash = hash;
                k.rebuild_index();
                let _ = k.save();
                return Some(k);
            }
        }

        None
    }

    pub fn load_or_create(project_root: &str) -> Self {
        Self::load(project_root).unwrap_or_else(|| Self::new(project_root))
    }

    /// Migrates legacy knowledge that was accidentally stored under an empty project_root ("")
    /// into the given `target_root`. Keeps a timestamped backup of the legacy file.
    pub fn migrate_legacy_empty_root(
        target_root: &str,
        policy: &MemoryPolicy,
    ) -> Result<bool, String> {
        if target_root.trim().is_empty() {
            return Ok(false);
        }

        let Some(legacy) = Self::load("") else {
            return Ok(false);
        };

        if !legacy.project_root.trim().is_empty() {
            return Ok(false);
        }
        if legacy.facts.is_empty() && legacy.patterns.is_empty() && legacy.history.is_empty() {
            return Ok(false);
        }

        let mut target = Self::load_or_create(target_root);

        fn fact_key(f: &KnowledgeFact) -> String {
            format!(
                "{}|{}|{}|{}|{}",
                f.category, f.key, f.value, f.source_session, f.created_at
            )
        }
        fn pattern_key(p: &ProjectPattern) -> String {
            format!(
                "{}|{}|{}|{}",
                p.pattern_type, p.description, p.source_session, p.created_at
            )
        }
        fn history_key(h: &ConsolidatedInsight) -> String {
            format!(
                "{}|{}|{}",
                h.summary,
                h.from_sessions.join(","),
                h.timestamp
            )
        }

        let mut seen_facts: std::collections::HashSet<String> =
            target.facts.iter().map(fact_key).collect();
        for f in legacy.facts {
            if seen_facts.insert(fact_key(&f)) {
                target.facts.push(f);
            }
        }

        let mut seen_patterns: std::collections::HashSet<String> =
            target.patterns.iter().map(pattern_key).collect();
        for p in legacy.patterns {
            if seen_patterns.insert(pattern_key(&p)) {
                target.patterns.push(p);
            }
        }

        let mut seen_history: std::collections::HashSet<String> =
            target.history.iter().map(history_key).collect();
        for h in legacy.history {
            if seen_history.insert(history_key(&h)) {
                target.history.push(h);
            }
        }

        target.facts.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.confidence.total_cmp(&a.confidence))
        });
        if target.facts.len() > policy.knowledge.max_facts {
            target.facts.truncate(policy.knowledge.max_facts);
        }
        target
            .patterns
            .sort_by_key(|x| std::cmp::Reverse(x.created_at));
        if target.patterns.len() > policy.knowledge.max_patterns {
            target.patterns.truncate(policy.knowledge.max_patterns);
        }
        target
            .history
            .sort_by_key(|x| std::cmp::Reverse(x.timestamp));
        if target.history.len() > policy.knowledge.max_history {
            target.history.truncate(policy.knowledge.max_history);
        }

        target.updated_at = Utc::now();
        target.save()?;

        let legacy_hash = crate::core::project_hash::hash_path_only("");
        let legacy_dir = knowledge_dir(&legacy_hash)?;
        let legacy_path = legacy_dir.join("knowledge.json");
        if legacy_path.exists() {
            let ts = Utc::now().format("%Y%m%d-%H%M%S");
            let backup = legacy_dir.join(format!("knowledge.legacy-empty-root.{ts}.json"));
            std::fs::rename(&legacy_path, &backup).map_err(|e| e.to_string())?;
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs2::FileExt;

    // --- #1802: `auto_capture = false` must hold at the store, not the callers ---

    fn machine_fact(category: &str, key: &str) -> KnowledgeFact {
        let now = chrono::Utc::now();
        KnowledgeFact {
            category: category.to_string(),
            key: key.to_string(),
            value: "Finding: run-guards.ts: Read run-guards.ts (92L)".to_string(),
            source_session: "session-1".to_string(),
            confidence: 0.8,
            created_at: now,
            last_confirmed: now,
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: None,
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
            privacy: Default::default(),
            sensitivity: Default::default(),
            imported_from: None,
            archetype: Default::default(),
            fidelity: None,
            revision_count: 0,
        }
    }

    #[test]
    fn machine_derived_is_recognised_in_either_ingestion_shape() {
        // `extract_session_facts` marks the category; `auto_capture` marks the
        // key. Both producers must be recognisable by one predicate.
        assert!(is_machine_derived("auto:blocker", "run-guards.ts"));
        assert!(is_machine_derived("blocker", "auto:config.rs"));
        assert!(!is_machine_derived("konventionen", "naming"));
        assert!(!is_machine_derived("ops", "deploy"));
    }

    #[test]
    fn a_curated_fact_is_never_affected_by_the_gate() {
        // The key disables *automatic* capture. A fact a person wrote must be
        // storable whatever the flag says, or the feature would be a footgun.
        let mut knowledge = ProjectKnowledge::new("/tmp/project");
        assert!(
            knowledge.add_fact(machine_fact("konventionen", "naming")),
            "curated facts must always be accepted"
        );
    }

    #[test]
    fn disabled_capture_blocks_every_machine_derived_shape() {
        // The regression: a single MCP call re-materialised deleted `auto:*`
        // facts because only one of two producers consulted the flag. Asserting
        // at the store covers both, and any producer added later.
        let _env = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_AUTO_CAPTURE", "0");

        let mut knowledge = ProjectKnowledge::new("/tmp/project");
        let category_marked = knowledge.add_fact(machine_fact("auto:blocker", "run-guards.ts"));
        let key_marked = knowledge.add_fact(machine_fact("blocker", "auto:config.rs"));

        crate::test_env::remove_var("LEAN_CTX_AUTO_CAPTURE");

        assert!(!category_marked, "auto:* category must be refused");
        assert!(!key_marked, "auto:* key must be refused");
        assert!(
            knowledge.facts.is_empty(),
            "nothing machine-derived may reach the durable store"
        );
    }

    #[test]
    fn enabled_capture_still_stores_machine_derived_facts() {
        let _env = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_AUTO_CAPTURE", "1");

        let mut knowledge = ProjectKnowledge::new("/tmp/project");
        let stored = knowledge.add_fact(machine_fact("auto:blocker", "run-guards.ts"));

        crate::test_env::remove_var("LEAN_CTX_AUTO_CAPTURE");

        assert!(stored, "the default behaviour must be unchanged");
    }

    #[test]
    fn disabled_capture_does_not_refresh_existing_machine_facts() {
        // Facts from an earlier enabled run stay on disk — the fix stops new
        // writes, it does not delete history. But a disabled run must not keep
        // touching them either, or they would look perpetually fresh to the
        // lifecycle and never age out.
        let _env = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_AUTO_CAPTURE", "1");
        let mut knowledge = ProjectKnowledge::new("/tmp/project");
        knowledge.add_fact(machine_fact("auto:blocker", "run-guards.ts"));
        let confirmed_while_enabled = knowledge.facts[0].last_confirmed;

        crate::test_env::set_var("LEAN_CTX_AUTO_CAPTURE", "0");
        let refreshed = knowledge.add_fact(machine_fact("auto:blocker", "run-guards.ts"));
        crate::test_env::remove_var("LEAN_CTX_AUTO_CAPTURE");

        assert!(!refreshed);
        assert_eq!(knowledge.facts.len(), 1);
        assert_eq!(
            knowledge.facts[0].last_confirmed, confirmed_while_enabled,
            "a refused write must not refresh the existing fact"
        );
    }

    #[test]
    fn file_lock_is_exclusive_across_handles() {
        // flock-style locks are tied to the open file description, so two
        // independent `open()`s in the same process behave like two separate
        // processes: while the first holds the exclusive lock, the second must
        // fail to acquire it. This validates the cross-process guarantee that
        // protects parallel CLI writes (issue #326).
        let dir = tempfile::tempdir().unwrap();
        let held = acquire_file_lock(dir.path()).expect("first lock must succeed");

        let second = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.path().join(".knowledge.lock"))
            .unwrap();
        assert!(
            second.try_lock_exclusive().is_err(),
            "a second handle must not acquire the lock while it is held"
        );

        drop(held);
        // `close()` releases the flock synchronously, so the lock IS free here.
        // Under heavy parallel test load, however, a single non-blocking
        // `try_lock_exclusive()` can momentarily observe `EWOULDBLOCK` from
        // scheduling jitter. A short bounded retry removes that flake without
        // weakening the guarantee: the lock must become acquirable again.
        let mut reacquired = false;
        for _ in 0..50 {
            if second.try_lock_exclusive().is_ok() {
                reacquired = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            reacquired,
            "lock must be acquirable within 500ms of release"
        );
    }

    #[test]
    fn write_json_atomic_leaves_valid_file_and_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("knowledge.json");
        write_json_atomic(dir.path(), &path, "{\"ok\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"ok\":true}");
        let leftover = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains(".tmp."));
        assert!(!leftover, "no temp file should remain");
    }

    #[test]
    fn load_rebuilds_ephemeral_index_without_changing_json() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let root = "/tmp/knowledge-index-load";
        let policy = MemoryPolicy::default();
        let mut knowledge = ProjectKnowledge::new(root);
        knowledge.remember(
            "architecture",
            "database",
            "PostgreSQL",
            "test",
            0.9,
            &policy,
        );
        knowledge.save().unwrap();

        let loaded = ProjectKnowledge::load(root).expect("saved knowledge should load");
        assert!(loaded.index.token_positions.contains_key("postgresql"));
        assert_eq!(loaded.recall("postgresql").len(), 1);

        let json = serde_json::to_string(&loaded).unwrap();
        assert!(!json.contains("\"index\""));
    }
}
