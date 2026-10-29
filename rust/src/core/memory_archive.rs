//! Multi-store, lossless memory archive (#995 Phase 1).
//!
//! Every memory store — facts, history, procedures, patterns — archives the
//! items it evicts here *before* dropping them, so capacity management is never
//! lossy and anything reclaimed can be restored. This is the single archive
//! subsystem behind the memory-capacity module and the recall-miss
//! rehydrate path.
//!
//! ## On-disk layout
//! - Facts live under `memory/archive/facts/<project hash>/archive-*.json`; the
//!   partition is always the request project's. Releases before 3.11.5 kept
//!   unprotected facts in one installation-wide `memory/archive/archive-*.json`
//!   without a provable project: those files are left in place, unread, and
//!   are never listed, rehydrated or restored into any project.
//! - Every other store lives under `memory/archive/<store>/<scope>/archive-*.json`,
//!   where `<scope>` is the per-project hash so a restore lands in the right
//!   project.
//!
//! ## Format
//! A single envelope ([`ArchiveEnvelope`]) is used for all stores. The item
//! collection serializes as `items`; the `facts` alias keeps legacy facts
//! archives (which used that key) deserializable.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};

/// Retention bound on archive files, kept well above the rehydrate reach so a
/// recall miss can still find recently-evicted items. Overridable via
/// `LEAN_CTX_ARCHIVE_MAX_FILES`.
const DEFAULT_MAX_ARCHIVE_FILES: usize = 16;

/// Which memory store an archive belongs to. Drives the on-disk path only — the
/// archive is generic over the item type, so this stays decoupled from the
/// concrete fact/insight/procedure/pattern structs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryStore {
    Facts,
    History,
    Procedures,
    Patterns,
}

impl MemoryStore {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryStore::Facts => "facts",
            MemoryStore::History => "history",
            MemoryStore::Procedures => "procedures",
            MemoryStore::Patterns => "patterns",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "facts" | "fact" => Some(MemoryStore::Facts),
            "history" | "insights" => Some(MemoryStore::History),
            "procedures" | "procedure" | "procs" => Some(MemoryStore::Procedures),
            "patterns" | "pattern" => Some(MemoryStore::Patterns),
            _ => None,
        }
    }

    /// All stores, for cross-store iteration (restore, reporting).
    pub fn all() -> [MemoryStore; 4] {
        [
            MemoryStore::Facts,
            MemoryStore::History,
            MemoryStore::Procedures,
            MemoryStore::Patterns,
        ]
    }

    /// Subdirectory under `memory/archive`. Facts return `None` (legacy root).
    fn subdir(self) -> Option<&'static str> {
        match self {
            MemoryStore::Facts => None,
            other => Some(other.as_str()),
        }
    }
}

/// Tunable archive bounds. `rehydrate_reach` is how many of the newest archives
/// the recall-miss path scans; it defaults to `max_files` so every retained
/// archive is actually reachable (closing the pre-#995 16-retained / 4-reachable
/// gap). Both are overridable via env for ops tuning.
#[derive(Debug, Clone, Copy)]
pub struct ArchiveConfig {
    pub max_files: usize,
    pub rehydrate_reach: usize,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_MAX_ARCHIVE_FILES,
            rehydrate_reach: DEFAULT_MAX_ARCHIVE_FILES,
        }
    }
}

impl ArchiveConfig {
    /// Read overrides from the environment. `rehydrate_reach` defaults to
    /// `max_files` and is clamped to it (cannot reach more files than retained).
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("LEAN_CTX_ARCHIVE_MAX_FILES")
            && let Ok(n) = v.parse::<usize>()
            && n > 0
        {
            cfg.max_files = n;
            cfg.rehydrate_reach = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_ARCHIVE_REHYDRATE_REACH")
            && let Ok(n) = v.parse::<usize>()
            && n > 0
        {
            cfg.rehydrate_reach = n;
        }
        cfg.rehydrate_reach = cfg.rehydrate_reach.min(cfg.max_files);
        cfg
    }
}

/// Unified archive envelope for reading every store. The collection is keyed
/// `items`; the `facts` alias keeps legacy facts archives (which used that key)
/// deserializable.
#[derive(Debug, Deserialize)]
pub struct ArchiveEnvelope<T> {
    pub archived_at: DateTime<Utc>,
    #[serde(default)]
    pub store: String,
    #[serde(default)]
    pub scope: Option<String>,
    /// The project root the archive was written for. The scope hash also
    /// covers the project's identity; this root proves the project when the
    /// identity changed (`legacy_adoption`).
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(rename = "items", alias = "facts")]
    pub items: Vec<T>,
}

/// Borrowed write-side envelope, so archiving never has to clone the evicted
/// slice. Mirrors [`ArchiveEnvelope`]'s on-disk shape exactly.
#[derive(Serialize)]
struct ArchiveEnvelopeRef<'a, T: Serialize> {
    archived_at: DateTime<Utc>,
    store: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_root: Option<&'a str>,
    items: &'a [T],
}

/// The project archives are keyed on: inside a request only its own bound
/// project — a request without one has none, never the process's working
/// directory (AP04 review S3); outside any request (the CLI) the directory
/// it runs in.
fn archive_project() -> Option<PathBuf> {
    match crate::core::policy::runtime::REQUEST_PROJECT.try_with(|slot| slot.borrow().clone()) {
        Ok(project) => project,
        Err(_) => std::env::current_dir().ok(),
    }
}

/// The scope an archive of `store` is written to and read from. Facts always
/// belong to the request project's partition; a protected project pins every
/// store to its own partition.
fn effective_scope(store: MemoryStore, scope: Option<&str>) -> Result<Option<String>, String> {
    let root = archive_project().ok_or("archive project unavailable")?;
    let root = root.to_string_lossy();
    let expected = crate::core::project_hash::hash_project_root(&root);
    if crate::core::knowledge::protection::current(&root)?.is_some() {
        if scope.is_some_and(|given| given != expected) {
            return Err("archive scope does not match protected project".into());
        }
        Ok(Some(expected))
    } else if store == MemoryStore::Facts {
        if scope.is_some_and(|given| given != expected) {
            return Err("facts archive scope does not match the request project".into());
        }
        Ok(Some(expected))
    } else {
        Ok(scope.map(str::to_owned))
    }
}

fn archive_dir(store: MemoryStore, scope: Option<&str>) -> Result<PathBuf, String> {
    let base = crate::core::data_dir::lean_ctx_data_dir()?
        .join("memory")
        .join("archive");
    let scope = effective_scope(store, scope)?;
    let dir = match (store.subdir(), scope.as_deref()) {
        (None, Some(s)) => base.join("facts").join(sanitize_scope(s)),
        (None, None) => return Err("facts archive requires a project partition".into()),
        (Some(sub), None) => base.join(sub), // store-global
        (Some(sub), Some(s)) => base.join(sub).join(sanitize_scope(s)),
    };
    Ok(dir)
}

/// Scopes are project hashes (hex). Guard against path traversal regardless,
/// so a malformed scope can never escape the archive root.
fn sanitize_scope(scope: &str) -> String {
    scope
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Archive `items` for `store`/`scope`, then prune the directory to
/// `cfg.max_files` newest. Returns the written path, or `None` when there was
/// nothing to archive. Ordering must be inspectable before writing; removal of
/// old files remains best-effort.
pub fn archive_items<T: Serialize>(
    store: MemoryStore,
    scope: Option<&str>,
    items: &[T],
    cfg: &ArchiveConfig,
) -> Result<Option<PathBuf>, String> {
    archive_items_at(store, scope, items, cfg, Utc::now())
}

fn archive_items_at<T: Serialize>(
    store: MemoryStore,
    scope: Option<&str>,
    items: &[T],
    cfg: &ArchiveConfig,
    now: DateTime<Utc>,
) -> Result<Option<PathBuf>, String> {
    if knowledge_store(store) && !items.is_empty() {
        let root = archive_project().ok_or("archive project unavailable")?;
        return crate::core::policy::runtime::with_explicit_project_source_view(
            &root,
            |authority| {
                let admission = crate::core::knowledge::protection::KnowledgeAdmission::capture(
                    &root.to_string_lossy(),
                )?;
                archive_items_inner(
                    store,
                    scope,
                    items,
                    cfg,
                    Some((admission.gateway_snapshot(), authority)),
                    Some((&admission, authority)),
                    now,
                )
            },
        )?;
    }
    archive_items_inner(store, scope, items, cfg, None, None, now)
}

pub(crate) fn archive_items_with_authority<T: Serialize>(
    store: MemoryStore,
    scope: Option<&str>,
    items: &[T],
    cfg: &ArchiveConfig,
    admission: &crate::core::context_admission::stores::StoreAdmission,
    authority: &crate::core::policy::runtime::PublicationAuthority,
) -> Result<Option<PathBuf>, String> {
    let knowledge = if knowledge_store(store) && !items.is_empty() {
        let root = archive_project().ok_or("archive project unavailable")?;
        let snapshot = crate::core::knowledge::protection::KnowledgeAdmission::capture(
            &root.to_string_lossy(),
        )?;
        if snapshot.gateway_snapshot().digest() != admission.digest() {
            return Err("archive admission changed; retry".into());
        }
        Some(snapshot)
    } else {
        None
    };
    archive_items_inner(
        store,
        scope,
        items,
        cfg,
        Some((admission, authority)),
        knowledge.as_ref().map(|snapshot| (snapshot, authority)),
        Utc::now(),
    )
}

fn knowledge_store(store: MemoryStore) -> bool {
    matches!(
        store,
        MemoryStore::Facts | MemoryStore::History | MemoryStore::Patterns
    )
}

fn admits_knowledge_archive_item(
    store: MemoryStore,
    admission: &crate::core::knowledge::protection::KnowledgeAdmission,
    value: &serde_json::Value,
) -> bool {
    match store {
        MemoryStore::Facts => admission.admits_archive_fact_value(value),
        MemoryStore::History => admission.admits_archive_history_value(value),
        MemoryStore::Patterns => admission.admits_archive_pattern_value(value),
        MemoryStore::Procedures => admission.admits_archive_value(value),
    }
}

fn admits_typed_knowledge_archive_item(
    store: MemoryStore,
    admission: &crate::core::knowledge::protection::KnowledgeAdmission,
    value: &serde_json::Value,
) -> bool {
    match store {
        MemoryStore::Facts => admission.admits_typed_archive_fact_value(value),
        MemoryStore::History => admission.admits_typed_archive_history_value(value),
        MemoryStore::Patterns => admission.admits_typed_archive_pattern_value(value),
        MemoryStore::Procedures => false,
    }
}

fn archive_store(value: &serde_json::Value) -> Option<MemoryStore> {
    match value.get("store") {
        Some(serde_json::Value::String(store)) => MemoryStore::parse(store),
        None if value.get("facts").is_some() => Some(MemoryStore::Facts),
        Some(_) | None => None,
    }
}

fn archive_items_field(value: &serde_json::Value, store: MemoryStore) -> Option<&'static str> {
    if value.get("items").is_some() {
        Some("items")
    } else if store == MemoryStore::Facts && value.get("facts").is_some() {
        Some("facts")
    } else {
        None
    }
}

fn archive_items_inner<T: Serialize>(
    store: MemoryStore,
    scope: Option<&str>,
    items: &[T],
    cfg: &ArchiveConfig,
    authorization: Option<(
        &crate::core::context_admission::stores::StoreAdmission,
        &crate::core::policy::runtime::PublicationAuthority,
    )>,
    knowledge_authorization: Option<(
        &crate::core::knowledge::protection::KnowledgeAdmission,
        &crate::core::policy::runtime::PublicationAuthority,
    )>,
    now: DateTime<Utc>,
) -> Result<Option<PathBuf>, String> {
    if items.is_empty() {
        return Ok(None);
    }
    if knowledge_store(store) && knowledge_authorization.is_none() {
        return Err("knowledge archive requires an admitted publication scope".into());
    }
    let scope = effective_scope(store, scope)?;
    let scope = scope.as_deref();
    let dir = archive_dir(store, scope)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;
    if !ordered_archive_files(&dir).1 {
        return Err("archive chronology unavailable".into());
    }

    // Sub-second suffix avoids same-second filename collisions that would
    // otherwise silently overwrite a prior archive in the same wall-clock second.
    let suffix = now.timestamp_subsec_nanos();
    let filename = format!("archive-{}-{suffix:09}.json", now.format("%Y%m%d-%H%M%S"));
    let path = dir.join(filename);

    let project_root =
        crate::core::policy::diagnostics::project().map(|root| root.to_string_lossy().into_owned());
    let envelope = ArchiveEnvelopeRef {
        archived_at: now,
        store: store.as_str(),
        scope,
        project_root: project_root.as_deref(),
        items,
    };
    let original = serde_json::to_value(&envelope).map_err(|_| "archive cannot be inspected")?;
    if let Some((admission, _)) = knowledge_authorization {
        let records = original["items"]
            .as_array()
            .ok_or("archive items are not inspectable")?;
        if !records
            .iter()
            .all(|record| admits_knowledge_archive_item(store, admission, record))
        {
            return Err("knowledge archive withheld by current policy".into());
        }
    }
    let typed_items = knowledge_authorization.map(|(admission, _)| (store, admission));
    let safe = inspect_archive(&original, typed_items)?;
    let json = serde_json::to_string_pretty(&safe).map_err(|_| "archive serialization failed")?;
    if inspect_archive(&safe, typed_items)? != safe {
        return Err("archive rules changed before publication".into());
    }
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(&dir).map_err(|_| "archive staging failed")?;
    file.write_all(json.as_bytes())
        .map_err(|_| "archive write failed")?;
    if let Some((admission, authority)) = authorization {
        admission.authorize_publication(authority)?;
    }
    if let Some((admission, authority)) = knowledge_authorization {
        admission.authorize_publication(authority)?;
    }
    // A clock collision must never overwrite an existing recovery artifact.
    file.persist_noclobber(&path)
        .map_err(|_| "archive publication failed")?;

    let (archives, ordered) = ordered_archive_files(&dir);
    if !ordered {
        return Err("archive chronology changed before retention".into());
    }
    if archives.len() > cfg.max_files {
        for old in &archives[..archives.len() - cfg.max_files] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(Some(path))
}

/// The installation-wide facts archives of releases before 3.11.5, as
/// `(path, envelope)`. They are served to no project; only the explicit
/// `lean-ctx migrate legacy-context` adoption reads them.
pub(crate) fn legacy_facts_archives() -> Vec<(PathBuf, serde_json::Value)> {
    let Ok(base) = crate::core::data_dir::lean_ctx_data_dir().map(|d| d.join("memory/archive"))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut archives: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let text = crate::core::policy::diagnostics::Target::read_file(&path)?;
            let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
            Some((path, value))
        })
        .collect();
    archives.sort_by(|a, b| a.0.cmp(&b.0));
    archives
}

/// Facts archives left in a partition that is no longer the current one of
/// the root they recorded: that project's identity (remote, package name)
/// changed since. Served to no project until adopted.
pub(crate) fn orphaned_facts_archives() -> Vec<(PathBuf, serde_json::Value)> {
    let Ok(base) =
        crate::core::data_dir::lean_ctx_data_dir().map(|d| d.join("memory/archive/facts"))
    else {
        return Vec::new();
    };
    let Ok(partitions) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut current_of: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut archives = Vec::new();
    for partition in partitions.flatten() {
        let name = partition.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(partition.path()) else {
            continue;
        };
        for path in files.flatten().map(|file| file.path()) {
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(value) = crate::core::policy::diagnostics::Target::read_file(&path)
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            else {
                continue;
            };
            let Some(root) = value["project_root"].as_str() else {
                continue;
            };
            let current = current_of
                .entry(root.to_string())
                .or_insert_with(|| crate::core::project_hash::hash_project_root(root));
            if *current != name {
                archives.push((path, value));
            }
        }
    }
    archives.sort_by(|a, b| a.0.cmp(&b.0));
    archives
}

/// The orphaned facts archives that recorded `project_root` (canonically).
pub(crate) fn drifted_facts_archives(project_root: &Path) -> Vec<(PathBuf, serde_json::Value)> {
    let canonical = |path: &Path| crate::core::pathutil::safe_canonicalize_bounded(path, 2000);
    let wanted = canonical(project_root);
    orphaned_facts_archives()
        .into_iter()
        .filter(|(_, value)| {
            value["project_root"]
                .as_str()
                .is_some_and(|root| canonical(Path::new(root)) == wanted)
        })
        .collect()
}

/// Copy legacy archived `facts` into the facts partition of `project_root`
/// (an explicit, canonical root) through the admission of a normal facts archive write. Each fact
/// is admitted on its own; the admitted ones are archived together. Returns
/// one flag per fact: `true` adopted, `false` withheld. A fact the partition
/// already holds is not archived again, so a rerun after an interrupted
/// adoption (copied, not yet journaled) leaves exactly one copy.
pub(crate) fn adopt_legacy_facts(
    project_root: &Path,
    facts: &[serde_json::Value],
) -> Result<Vec<bool>, String> {
    let root = project_root;
    crate::core::policy::runtime::with_explicit_project_source_view(root, |authority| {
        let admission = crate::core::knowledge::protection::KnowledgeAdmission::capture(
            &root.to_string_lossy(),
        )?;
        let admitted: Vec<bool> = facts
            .iter()
            .map(|fact| admits_knowledge_archive_item(MemoryStore::Facts, &admission, fact))
            .collect();
        let archived = archived_fact_identities()?;
        let kept: Vec<&serde_json::Value> = facts
            .iter()
            .zip(&admitted)
            .filter_map(|(fact, ok)| {
                (*ok && !archived.contains(&fact_identity(fact))).then_some(fact)
            })
            .collect();
        archive_items_inner(
            MemoryStore::Facts,
            None,
            &kept,
            &ArchiveConfig::from_env(),
            Some((admission.gateway_snapshot(), authority)),
            Some((&admission, authority)),
            Utc::now(),
        )?;
        Ok(admitted)
    })?
}

/// What makes an archived fact the same fact: its text fields. Numbers
/// (confidence) are left out, they need not survive a JSON round trip bit
/// for bit.
fn fact_identity(fact: &serde_json::Value) -> [String; 3] {
    ["category", "key", "value"].map(|field| fact[field].as_str().unwrap_or_default().to_owned())
}

/// The identities of the facts archived in the request project's partition.
fn archived_fact_identities() -> Result<std::collections::HashSet<[String; 3]>, String> {
    let dir = archive_dir(MemoryStore::Facts, None)?;
    Ok(ordered_archive_files(&dir)
        .0
        .iter()
        .filter_map(|path| crate::core::policy::diagnostics::Target::read_file(path))
        .filter_map(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .flat_map(|envelope| match envelope.get("items") {
            Some(serde_json::Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        })
        .map(|fact| fact_identity(&fact))
        .collect())
}

/// All archive files for `store`/`scope`, sorted by their recorded archive time.
/// Uninspectable files remain listed, but prevent retention from deleting files.
pub fn list_archives(store: MemoryStore, scope: Option<&str>) -> Vec<PathBuf> {
    let Ok(dir) = archive_dir(store, scope) else {
        return Vec::new();
    };
    ordered_archive_files(&dir).0
}

fn ordered_archive_files(dir: &Path) -> (Vec<PathBuf>, bool) {
    #[derive(Deserialize)]
    struct ArchiveTimestamp {
        archived_at: DateTime<Utc>,
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), false);
    };
    let mut complete = true;
    let mut archives = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            complete = false;
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        // Legacy six-digit names wrapped every millisecond, so their lexical
        // order cannot establish chronology. Use the bounded, no-follow reader
        // already used by restore_items, and retain only the envelope timestamp.
        let timestamp = crate::core::policy::diagnostics::Target::read_file(&path)
            .and_then(|text| serde_json::from_str::<ArchiveTimestamp>(&text).ok())
            .map(|metadata| metadata.archived_at);
        complete &= timestamp.is_some();
        archives.push((timestamp, path));
    }
    // Path order is a deterministic tie-breaker and fallback for unreadable
    // metadata; the caller must not prune unless every timestamp is known.
    archives.sort_unstable();
    (
        archives.into_iter().map(|(_, path)| path).collect(),
        complete,
    )
}

/// The newest `cfg.rehydrate_reach` archives for `store`/`scope` — the set a
/// recall miss should scan. Aligned with retention so nothing retained is
/// unreachable.
pub fn reachable_archives(
    store: MemoryStore,
    scope: Option<&str>,
    cfg: &ArchiveConfig,
) -> Vec<PathBuf> {
    let mut archives = list_archives(store, scope);
    if archives.len() > cfg.rehydrate_reach {
        archives = archives[archives.len() - cfg.rehydrate_reach..].to_vec();
    }
    archives
}

/// Restore the items from a single archive file.
pub fn restore_items<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    restore_items_inner(path, None)
}

pub(crate) fn restore_knowledge_items<T: DeserializeOwned>(
    path: &Path,
    admission: &crate::core::knowledge::protection::KnowledgeAdmission,
) -> Result<Vec<T>, String> {
    restore_items_inner(path, Some(admission))
}

fn restore_items_inner<T: DeserializeOwned>(
    path: &Path,
    supplied_admission: Option<&crate::core::knowledge::protection::KnowledgeAdmission>,
) -> Result<Vec<T>, String> {
    let data = crate::core::policy::diagnostics::Target::read_file(path)
        .ok_or("archive is unavailable or uninspectable")?;
    let original: serde_json::Value = serde_json::from_str(&data).map_err(|_| "invalid archive")?;
    let root = archive_project().ok_or("archive project unavailable")?;
    let canonical = |p: &Path| crate::core::pathutil::safe_canonicalize_bounded(p, 2000);
    let protected = crate::core::knowledge::protection::current(&root.to_string_lossy())?.is_some();
    if !protected && archive_store(&original) == Some(MemoryStore::Facts) {
        // Facts restore only from the request project's own partition; an
        // installation-wide legacy archive has no provable project.
        let expected = archive_dir(MemoryStore::Facts, None)?;
        if path.parent().map(canonical) != Some(canonical(&expected)) {
            return Err("archive belongs to a different storage scope".into());
        }
    } else if protected {
        let kind = match original["store"].as_str() {
            Some("facts") => MemoryStore::Facts,
            Some("history") => MemoryStore::History,
            Some("procedures") => MemoryStore::Procedures,
            Some("patterns") => MemoryStore::Patterns,
            _ => return Err("archive store identity unavailable".into()),
        };
        let scope = original["scope"]
            .as_str()
            .ok_or("archive has no protected scope")?;
        let expected = archive_dir(kind, Some(scope))?;
        if path.parent().map(canonical) != Some(canonical(&expected)) {
            return Err("archive belongs to a different storage scope".into());
        }
    }
    let store = archive_store(&original);
    let mut candidate = original.clone();
    let captured_admission = if store.is_some_and(knowledge_store) && supplied_admission.is_none() {
        Some(
            crate::core::knowledge::protection::KnowledgeAdmission::capture(
                &root.to_string_lossy(),
            )?,
        )
    } else {
        None
    };
    let mut typed_items = None;
    if let Some(store) = store.filter(|store| knowledge_store(*store)) {
        let admission = supplied_admission
            .or(captured_admission.as_ref())
            .ok_or("archive admission unavailable")?;
        let collection =
            archive_items_field(&candidate, store).ok_or("archive items are unavailable")?;
        let records = candidate[collection]
            .as_array_mut()
            .ok_or("archive items are invalid")?;
        records.retain(|record| admits_knowledge_archive_item(store, admission, record));
        admission.authorize_gateway()?;
        typed_items = Some((store, admission));
    }
    let safe = inspect_archive(&candidate, typed_items)?;
    if let Some((_, admission)) = typed_items {
        admission.authorize_gateway()?;
    }
    let envelope: ArchiveEnvelope<T> = serde_json::from_value(safe)
        .map_err(|_| "archive policy result is structurally invalid")?;
    Ok(envelope.items)
}

fn inspect_archive(
    original: &serde_json::Value,
    typed_items: Option<(
        MemoryStore,
        &crate::core::knowledge::protection::KnowledgeAdmission,
    )>,
) -> Result<serde_json::Value, String> {
    let root = archive_project().ok_or("archive project unavailable")?;
    let active = crate::core::knowledge::protection::current(&root.to_string_lossy())?;
    let mut inspection = original.clone();
    let mut preserved_items = Vec::new();
    if let Some((store, admission)) = typed_items {
        if let Some(collection) = archive_items_field(&inspection, store)
            && let Some(items) = inspection
                .get_mut(collection)
                .and_then(serde_json::Value::as_array_mut)
        {
            for (index, item) in items.iter_mut().enumerate() {
                if admits_typed_knowledge_archive_item(store, admission, item) {
                    preserved_items.push((index, item.clone()));
                    *item = serde_json::Value::Null;
                }
            }
        }
    }
    let mut safe = crate::core::policy::diagnostics::inspect(&inspection, active.as_deref())
        .ok_or("archive withheld by current policy")?;
    if let Some((store, _)) = typed_items {
        if let Some(collection) = archive_items_field(&safe, store)
            && let Some(items) = safe
                .get_mut(collection)
                .and_then(serde_json::Value::as_array_mut)
        {
            for (index, item) in preserved_items {
                let slot = items
                    .get_mut(index)
                    .ok_or("archive item projection changed shape")?;
                *slot = item;
            }
        }
    }
    for field in ["store", "scope", "archived_at"] {
        if safe[field] != original[field] {
            return Err("archive identity cannot be rewritten".into());
        }
    }
    let before_items = original.get("items").or_else(|| original.get("facts"));
    let after_items = safe.get("items").or_else(|| safe.get("facts"));
    if let (Some(before), Some(after)) = (
        before_items.and_then(serde_json::Value::as_array),
        after_items.and_then(serde_json::Value::as_array),
    ) {
        for (a, b) in before.iter().zip(after) {
            for field in [
                "id",
                "key",
                "category",
                "source_session",
                "supersedes",
                "from_sessions",
                "pattern_type",
            ] {
                if a.get(field) != b.get(field) {
                    return Err("archived item identity cannot be rewritten".into());
                }
            }
        }
    }
    Ok(safe)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn with_temp_data_dir<T>(f: impl FnOnce() -> T) -> T {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "lctx-archive-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        let out = f();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct Item {
        id: u32,
        label: String,
    }

    #[test]
    fn admitted_archive_rejects_unobserved_policy_before_publication() {
        with_temp_data_dir(|| {
            let project = tempfile::tempdir().expect("project root");
            let project_path = project.path().to_str().expect("UTF-8 project path");
            crate::core::policy::runtime::with_project_source_view(project_path, || {
                let authority = crate::core::policy::runtime::PublicationAuthority::capture()
                    .expect("active project authority");
                let config = crate::core::config::Config::load_arc();
                let admission = crate::core::context_admission::stores::StoreAdmission::with_policy(
                    crate::core::context_admission::AdmissionPolicy::from_config(&config),
                );
                let scope = Some("publication-guard-test");
                let result = archive_items_with_authority(
                    MemoryStore::History,
                    scope,
                    &items(1),
                    &ArchiveConfig::default(),
                    &admission,
                    &authority,
                );
                assert!(
                    result.is_err(),
                    "explicit unobserved snapshots cannot publish"
                );
                assert!(list_archives(MemoryStore::History, scope).is_empty());
            })
            .expect("source view");
        });
    }

    fn items(n: u32) -> Vec<Item> {
        (0..n)
            .map(|i| Item {
                id: i,
                label: format!("item-{i}"),
            })
            .collect()
    }

    #[test]
    fn round_trip_each_store() {
        with_temp_data_dir(|| {
            let cfg = ArchiveConfig::default();
            for store in MemoryStore::all() {
                let scope = if store == MemoryStore::Facts {
                    None
                } else {
                    Some("projhash")
                };
                let path = archive_items(store, scope, &items(3), &cfg)
                    .unwrap()
                    .expect("wrote an archive");
                let restored: Vec<Item> = restore_items(&path).unwrap();
                assert_eq!(restored, items(3), "round-trip for {}", store.as_str());
            }
        });
    }

    /// AP04 review S3: a request bound to no project has no archive; it is
    /// not keyed on the directory the process runs in.
    #[test]
    fn a_request_without_a_project_has_no_archive() {
        with_temp_data_dir(|| {
            let archived = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
                std::cell::RefCell::new(None),
                || {
                    archive_items(
                        MemoryStore::Facts,
                        None,
                        &items(1),
                        &ArchiveConfig::default(),
                    )
                },
            );
            assert!(archived.is_err(), "{archived:?}");
        });
    }

    #[test]
    fn empty_archive_is_noop() {
        with_temp_data_dir(|| {
            let cfg = ArchiveConfig::default();
            let res =
                archive_items(MemoryStore::History, Some("p"), &Vec::<Item>::new(), &cfg).unwrap();
            assert!(res.is_none());
            {
                let found = &list_archives(MemoryStore::History, Some("p"));
                assert!(found.is_empty(), "{found:?}");
            }
        });
    }

    fn in_project<T>(root: &Path, f: impl FnOnce() -> T) -> T {
        crate::core::policy::runtime::REQUEST_PROJECT
            .sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), f)
    }

    /// AP05-G1: an unprotected project's evicted facts went to one
    /// installation-wide archive, so a recall miss or `restore` in another
    /// project rehydrated them there as local facts.
    #[test]
    fn facts_archives_are_partitioned_by_request_project() {
        with_temp_data_dir(|| {
            let cfg = ArchiveConfig::default();
            let a = tempfile::tempdir().expect("project a");
            let b = tempfile::tempdir().expect("project b");
            let archived = in_project(a.path(), || {
                archive_items(MemoryStore::Facts, None, &items(2), &cfg)
                    .unwrap()
                    .expect("wrote an archive")
            });
            let base = crate::core::data_dir::lean_ctx_data_dir()
                .unwrap()
                .join("memory")
                .join("archive");
            let legacy = base.join("archive-20260101-000000-000000.json");
            std::fs::write(
                &legacy,
                r#"{"archived_at":"2026-01-01T00:00:00Z","facts":[{"id":7,"label":"old"}]}"#,
            )
            .unwrap();
            in_project(b.path(), || {
                assert!(list_archives(MemoryStore::Facts, None).is_empty());
                assert!(restore_items::<Item>(&archived).is_err());
                assert!(restore_items::<Item>(&legacy).is_err());
            });
            in_project(a.path(), || {
                assert_eq!(
                    list_archives(MemoryStore::Facts, None),
                    vec![archived.clone()]
                );
                assert_eq!(restore_items::<Item>(&archived).unwrap(), items(2));
                assert!(restore_items::<Item>(&legacy).is_err());
            });
        });
    }

    #[test]
    fn legacy_facts_field_alias_still_deserializes() {
        with_temp_data_dir(|| {
            let dir = archive_dir(MemoryStore::Facts, None).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            // A pre-#995 facts archive used the `facts` key, no store/scope.
            let legacy =
                r#"{"archived_at":"2026-01-01T00:00:00Z","facts":[{"id":7,"label":"old"}]}"#;
            let path = dir.join("archive-20260101-000000-000000.json");
            std::fs::write(&path, legacy).unwrap();
            let restored: Vec<Item> = restore_items(&path).unwrap();
            assert_eq!(
                restored,
                vec![Item {
                    id: 7,
                    label: "old".into()
                }]
            );
        });
    }

    #[test]
    fn prune_keeps_newest_max_files() {
        with_temp_data_dir(|| {
            let cfg = ArchiveConfig {
                max_files: 3,
                rehydrate_reach: 3,
            };
            // These distinct instants collide under the old nanos % 1_000_000
            // suffix and also cross its lexical-order wraparound.
            for (id, nanos) in [
                1_100_000, 1_900_000, 2_100_000, 2_900_000, 3_100_000, 3_900_000,
            ]
            .into_iter()
            .enumerate()
            {
                let batch = [Item {
                    id: u32::try_from(id).unwrap(),
                    label: format!("batch-{id}"),
                }];
                archive_items_at(
                    MemoryStore::Patterns,
                    Some("p"),
                    &batch,
                    &cfg,
                    DateTime::from_timestamp(1_700_000_000, nanos).unwrap(),
                )
                .unwrap();
            }
            let archives = list_archives(MemoryStore::Patterns, Some("p"));
            let retained: Vec<u32> = archives
                .iter()
                .map(|path| restore_items::<Item>(path).unwrap()[0].id)
                .collect();
            assert_eq!(retained, [3, 4, 5]);
        });
    }

    #[test]
    fn retention_orders_legacy_names_by_the_recorded_timestamp() {
        with_temp_data_dir(|| {
            let store = MemoryStore::Patterns;
            let dir = archive_dir(store, Some("p")).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let old_time = DateTime::from_timestamp(1_700_000_000, 1_900_000).unwrap();
            let legacy = dir.join(format!(
                "archive-{}-900000.json",
                old_time.format("%Y%m%d-%H%M%S")
            ));
            std::fs::write(
                &legacy,
                serde_json::to_vec(&ArchiveEnvelopeRef {
                    archived_at: old_time,
                    store: store.as_str(),
                    scope: Some("p"),
                    project_root: None,
                    items: &items(1),
                })
                .unwrap(),
            )
            .unwrap();
            let cfg = ArchiveConfig {
                max_files: 1,
                rehydrate_reach: 1,
            };
            let newest = archive_items_at(
                store,
                Some("p"),
                &items(2),
                &cfg,
                DateTime::from_timestamp(1_700_000_000, 2_100_000).unwrap(),
            )
            .unwrap()
            .unwrap();
            assert!(!legacy.exists());
            assert_eq!(
                list_archives(store, Some("p")).as_slice(),
                std::slice::from_ref(&newest)
            );
            assert_eq!(restore_items::<Item>(&newest).unwrap(), items(2));
        });
    }

    #[test]
    fn unreadable_archive_chronology_blocks_writes_without_pruning() {
        with_temp_data_dir(|| {
            let store = MemoryStore::Patterns;
            let dir = archive_dir(store, Some("p")).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let legacy = dir.join("archive-20231114-221320-900000.json");
            std::fs::write(&legacy, "not json").unwrap();
            let cfg = ArchiveConfig {
                max_files: 1,
                rehydrate_reach: 1,
            };
            let error = archive_items(store, Some("p"), &items(1), &cfg).unwrap_err();
            assert_eq!(error, "archive chronology unavailable");
            assert_eq!(
                list_archives(store, Some("p")).as_slice(),
                std::slice::from_ref(&legacy)
            );
            assert_eq!(std::fs::read_to_string(legacy).unwrap(), "not json");
        });
    }

    #[test]
    fn reachable_is_bounded_and_aligns_with_retention_by_default() {
        let cfg = ArchiveConfig::default();
        assert_eq!(cfg.rehydrate_reach, cfg.max_files);
    }

    #[test]
    fn from_env_reach_clamped_to_max() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_ARCHIVE_MAX_FILES", "5");
        crate::test_env::set_var("LEAN_CTX_ARCHIVE_REHYDRATE_REACH", "99");
        let cfg = ArchiveConfig::from_env();
        assert_eq!(cfg.max_files, 5);
        assert_eq!(cfg.rehydrate_reach, 5, "reach clamped to max_files");
        crate::test_env::remove_var("LEAN_CTX_ARCHIVE_MAX_FILES");
        crate::test_env::remove_var("LEAN_CTX_ARCHIVE_REHYDRATE_REACH");
    }

    #[test]
    fn store_parse_round_trips() {
        for store in MemoryStore::all() {
            assert_eq!(MemoryStore::parse(store.as_str()), Some(store));
        }
        assert_eq!(MemoryStore::parse("nope"), None);
    }

    #[test]
    fn unknown_explicit_store_does_not_fall_back_to_legacy_facts() {
        let invalid = serde_json::json!({"store":"unknown","facts":[]});
        assert_eq!(archive_store(&invalid), None);

        let legacy = serde_json::json!({"facts":[]});
        assert_eq!(archive_store(&legacy), Some(MemoryStore::Facts));
    }
}
