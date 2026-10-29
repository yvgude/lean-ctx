// SPDX-License-Identifier: Apache-2.0
//! Adoption of context that releases before 3.11.5 stored without a
//! provable project (AP05).
//!
//! Such context is served to no project and is never assigned to the active
//! one by guesswork. Two ways bring it back, both copying (never moving)
//! through the admission of a normal write to the target project:
//!
//! - **Automatic, only with proof:** a legacy `ctx_expand` archive whose
//!   recorded session, or a legacy archived fact whose `source_session`,
//!   resolves to a session record whose project root is exactly the request
//!   project's. Nothing else is adopted automatically.
//! - **Explicit:** the owner of this machine chooses items and a project with
//!   `lean-ctx migrate legacy-context` (`cli::migrate_legacy`).
//!
//! Both append to one journal, `migrations/legacy-context.jsonl` in the data
//! directory: timestamp, store, item id, target project hash, outcome — never
//! content. An item adopted into a project is never adopted there again.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub(crate) const STORE_FACTS_ARCHIVE: &str = "facts-archive";
pub(crate) const STORE_EXPAND_ARCHIVE: &str = "expand-archive";
pub(crate) const STORE_SHARED_CONTEXT: &str = "shared-context";
pub(crate) const STORE_KNOWLEDGE: &str = "knowledge";

/// One journal line.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct JournalLine {
    pub timestamp: String,
    pub store: String,
    pub item: String,
    pub project: String,
    pub outcome: String,
}

fn journal_path() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("migrations")
        .join("legacy-context.jsonl"))
}

pub(crate) fn read_journal() -> Vec<JournalLine> {
    journal_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `item` was adopted into the project `partition` before.
pub(crate) fn adopted(journal: &[JournalLine], item: &str, partition: &str) -> bool {
    journal
        .iter()
        .any(|line| line.item == item && line.project == partition && line.outcome == "adopted")
}

/// Append one outcome (append-only, owner-only permissions).
pub(crate) fn record(
    store: &str,
    item: &str,
    partition: &str,
    adopted: bool,
) -> Result<(), String> {
    let path = journal_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let line = JournalLine {
        timestamp: chrono::Utc::now().to_rfc3339(),
        store: store.to_string(),
        item: item.to_string(),
        project: partition.to_string(),
        outcome: if adopted { "adopted" } else { "refused" }.to_string(),
    };
    let json = serde_json::to_string(&line).map_err(|e| e.to_string())?;
    writeln!(file, "{json}").map_err(|e| format!("write {}: {e}", path.display()))
}

fn canonical(root: &str) -> PathBuf {
    crate::core::pathutil::safe_canonicalize_bounded(Path::new(root), 2000)
}

/// Bounded work per automatic pass: a request never waits long for it; what
/// is left is picked up by the next lookup.
const PASS_ITEMS: usize = 64;
const PASS_TIME: std::time::Duration = std::time::Duration::from_millis(150);

/// Process-wide state of the automatic archive pass, per data directory and
/// project (`pass_key`): the legacy and drifted candidates (one metadata scan
/// each), the project each recorded session belongs to (one session read per
/// session), and the candidates already decided.
#[derive(Default)]
struct ArchivePass {
    candidates: HashMap<String, Vec<Candidate>>,
    session_roots: HashMap<String, Option<PathBuf>>,
    decided: HashSet<(String, String)>,
    complete: HashSet<String>,
}

fn pass_key(partition: &str) -> String {
    let data = crate::core::data_dir::lean_ctx_data_dir().unwrap_or_default();
    format!("{}|{partition}", data.display())
}

#[derive(Clone)]
struct Candidate {
    id: String,
    session: Option<String>,
    recorded_root: Option<String>,
}

fn archive_pass() -> &'static Mutex<ArchivePass> {
    static ARCHIVE_PASS: OnceLock<Mutex<ArchivePass>> = OnceLock::new();
    ARCHIVE_PASS.get_or_init(|| Mutex::new(ArchivePass::default()))
}

/// The canonical project root of stored session `session_id`, cached.
fn session_root(session_id: &str) -> Option<PathBuf> {
    if let Ok(pass) = archive_pass().lock()
        && let Some(root) = pass.session_roots.get(session_id)
    {
        return root.clone();
    }
    let root = crate::core::session::SessionState::load_by_id(session_id)
        .and_then(|session| session.project_root)
        .map(|root| canonical(&root));
    if let Ok(mut pass) = archive_pass().lock() {
        pass.session_roots
            .insert(session_id.to_string(), root.clone());
    }
    root
}

/// Whether a candidate provably belongs to the project at `wanted`: its
/// recorded root is that path (an identity change moved only the hash), or
/// its recorded session is a session of exactly that project.
fn proven(candidate: &Candidate, wanted: &Path) -> bool {
    candidate
        .recorded_root
        .as_deref()
        .is_some_and(|root| canonical(root) == wanted)
        || candidate
            .session
            .as_deref()
            .and_then(session_root)
            .is_some_and(|root| root == wanted)
}

/// One automatic pass per project and process for stores without a
/// per-item budget.
fn first_pass(kind: &str, partition: &str) -> bool {
    static ADOPTION_PASSES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ADOPTION_PASSES
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .is_ok_and(|mut done| done.insert(format!("{kind}:{}", pass_key(partition))))
}

fn request_root() -> Option<String> {
    crate::core::policy::diagnostics::request_project()
        .map(|root| root.to_string_lossy().into_owned())
}

/// Adopt the legacy or drifted archive `id` into the request project if it
/// provably belongs there. `Some` is the adopted copy's id.
pub(crate) fn adopt_provable_archive(id: &str) -> Option<String> {
    let root = request_root()?;
    let entry = crate::core::archive::unbound_entry(id)
        .filter(|entry| entry.projects.is_empty() || crate::core::archive::is_drifted(entry))?;
    let candidate = Candidate {
        id: entry.id,
        session: entry.session_id,
        recorded_root: entry.project_root,
    };
    if !proven(&candidate, &canonical(&root)) {
        return None;
    }
    adopt_archive(&candidate.id, &root).ok()
}

fn adopt_archive(id: &str, root: &str) -> Result<String, String> {
    let partition = crate::core::project_hash::hash_project_root(root);
    let item = format!("{STORE_EXPAND_ARCHIVE}:{id}");
    let result = crate::core::archive::adopt_legacy_entry(id, Path::new(root));
    if !adopted(&read_journal(), &item, &partition) {
        record(STORE_EXPAND_ARCHIVE, &item, &partition, result.is_ok())?;
    }
    result
}

/// The candidates of `partition` not decided yet, at most `PASS_ITEMS`; the
/// metadata scan runs once per process, outside the lock.
fn pending_candidates(key: &str) -> Option<Vec<Candidate>> {
    let scanned = {
        let pass = archive_pass().lock().ok()?;
        if pass.complete.contains(key) {
            return None;
        }
        pass.candidates.contains_key(key)
    };
    if !scanned {
        let candidates = crate::core::archive::legacy_unscoped_entries()
            .into_iter()
            .map(|entry| Candidate {
                id: entry.id,
                session: entry.session_id,
                recorded_root: entry.project_root,
            })
            .collect();
        archive_pass()
            .lock()
            .ok()?
            .candidates
            .entry(key.to_string())
            .or_insert(candidates);
    }
    let pass = archive_pass().lock().ok()?;
    Some(
        pass.candidates
            .get(key)
            .into_iter()
            .flatten()
            .filter(|candidate| {
                !pass
                    .decided
                    .contains(&(key.to_string(), candidate.id.clone()))
            })
            .take(PASS_ITEMS)
            .cloned()
            .collect(),
    )
}

/// Adopt the legacy and drifted archives that provably belong to the request
/// project. Bounded per call (items and time); the rest follows on the next
/// lookup, so no request waits long.
pub(crate) fn auto_adopt_archives() {
    let Some(root) = request_root() else {
        return;
    };
    let partition = crate::core::project_hash::hash_project_root(&root);
    let key = pass_key(&partition);
    let Some(pending) = pending_candidates(&key) else {
        return;
    };
    let wanted = canonical(&root);
    let journal = read_journal();
    let deadline = std::time::Instant::now() + PASS_TIME;
    for candidate in &pending {
        if std::time::Instant::now() >= deadline {
            return;
        }
        let item = format!("{STORE_EXPAND_ARCHIVE}:{}", candidate.id);
        if !adopted(&journal, &item, &partition) && proven(candidate, &wanted) {
            let _ = adopt_archive(&candidate.id, &root);
        }
        if let Ok(mut pass) = archive_pass().lock() {
            pass.decided.insert((key.clone(), candidate.id.clone()));
        }
    }
    if pending.len() < PASS_ITEMS
        && let Ok(mut pass) = archive_pass().lock()
    {
        pass.complete.insert(key);
    }
}

/// Adopt the archived facts that provably belong to `project_root` into its
/// facts partition (once per process and project): legacy facts whose
/// `source_session` is a session of this project, and facts archived for
/// this very path under an earlier identity of the project.
pub(crate) fn auto_adopt_facts(project_root: &str) {
    let partition = crate::core::project_hash::hash_project_root(project_root);
    if !first_pass(STORE_FACTS_ARCHIVE, &partition) {
        return;
    }
    let wanted = canonical(project_root);
    let journal = read_journal();
    let mut items = Vec::new();
    let mut facts = Vec::new();
    for (path, envelope) in crate::core::memory_archive::legacy_facts_archives() {
        for (item, fact) in legacy_facts_of(&path, &envelope) {
            let proven = fact["source_session"]
                .as_str()
                .and_then(session_root)
                .is_some_and(|root| root == wanted);
            if proven && !adopted(&journal, &item, &partition) {
                items.push(item);
                facts.push(fact);
            }
        }
    }
    for (path, envelope) in crate::core::memory_archive::drifted_facts_archives(&wanted) {
        for (item, fact) in legacy_facts_of(&path, &envelope) {
            if !adopted(&journal, &item, &partition) {
                items.push(item);
                facts.push(fact);
            }
        }
    }
    if facts.is_empty() {
        return;
    }
    let outcome = crate::core::memory_archive::adopt_legacy_facts(Path::new(project_root), &facts);
    for (index, item) in items.iter().enumerate() {
        let ok = outcome
            .as_ref()
            .is_ok_and(|flags| flags.get(index) == Some(&true));
        let _ = record(STORE_FACTS_ARCHIVE, item, &partition, ok);
    }
}

/// Copy `ctx_memory` facts stored for this very path under an earlier
/// identity of the project into its current partition (once per process and
/// project), through the admission of a normal write.
pub(crate) fn auto_adopt_shared_context(project_root: &str) {
    let partition = crate::core::project_hash::hash_project_root(project_root);
    if !first_pass(STORE_SHARED_CONTEXT, &partition) {
        return;
    }
    let drifted = crate::core::shared_context::SharedContext::drifted_entries(project_root);
    if drifted.is_empty() {
        return;
    }
    let Ok(store) = crate::core::shared_context::SharedContext::for_project(project_root) else {
        return;
    };
    let journal = read_journal();
    for (_, entry) in drifted {
        let item = shared_context_item(&entry.id);
        if adopted(&journal, &item, &partition) {
            continue;
        }
        let ok = crate::core::context_admission::carrier::admit_for_store(&entry.content)
            .is_ok_and(|admitted| {
                store
                    .put(admitted, entry.agent.clone(), entry.category.clone())
                    .is_ok()
            });
        let _ = record(STORE_SHARED_CONTEXT, &item, &partition, ok);
    }
}

/// The journal and listing id of a shared-context fact.
pub(crate) fn shared_context_item(id: &str) -> String {
    format!("{STORE_SHARED_CONTEXT}:{}", &id[..id.len().min(16)])
}

/// Upper bound of partitions one drift lookup inspects.
const DRIFT_SCAN_LIMIT: usize = 2048;

/// The knowledge partition (`knowledge/<hash>`) that recorded `project_root`
/// (canonically) under an earlier identity of the project, if any: proof by
/// the stored root. One bounded scan per data directory and root per process.
pub(crate) fn drifted_knowledge_partition(project_root: &str) -> Option<String> {
    static KNOWLEDGE_DRIFT: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let current = crate::core::project_hash::hash_project_root(project_root);
    let wanted = canonical(project_root);
    let key = format!("{}|{}", pass_key(&current), wanted.display());
    let cache = KNOWLEDGE_DRIFT.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(found) = cache.lock().ok()?.get(&key) {
        return found.clone();
    }
    #[derive(serde::Deserialize)]
    struct Recorded {
        #[serde(default)]
        project_root: String,
    }
    let base = crate::core::data_dir::lean_ctx_data_dir()
        .ok()?
        .join("knowledge");
    let mut found: Option<(std::time::SystemTime, String)> = None;
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.flatten().take(DRIFT_SCAN_LIMIT) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == current {
                continue;
            }
            let file = entry.path().join("knowledge.json");
            let Ok(reader) = std::fs::File::open(&file) else {
                continue;
            };
            let Ok(recorded) =
                serde_json::from_reader::<_, Recorded>(std::io::BufReader::new(reader))
            else {
                continue;
            };
            if recorded.project_root.is_empty() || canonical(&recorded.project_root) != wanted {
                continue;
            }
            let modified = std::fs::metadata(&file)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if found.as_ref().is_none_or(|(newest, _)| modified > *newest) {
                found = Some((modified, name));
            }
        }
    }
    let found = found.map(|(_, name)| name);
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, found.clone());
    }
    found
}

/// Copy the files of the knowledge partition `old` that the current
/// partition of `project_root` does not have yet (knowledge, gotchas, …).
/// Never overwrites and never removes; journaled once per partition pair.
pub(crate) fn copy_knowledge_partition(old: &str, project_root: &str) -> Result<bool, String> {
    fn copy_missing(from: &Path, to: &Path) -> Result<bool, String> {
        std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
        let mut copied = false;
        for entry in std::fs::read_dir(from)
            .map_err(|e| format!("read {}: {e}", from.display()))?
            .flatten()
        {
            let source = entry.path();
            let target = to.join(entry.file_name());
            if source.is_dir() {
                copied |= copy_missing(&source, &target)?;
            } else if !target.exists() {
                std::fs::copy(&source, &target)
                    .map_err(|e| format!("copy {}: {e}", source.display()))?;
                copied = true;
            }
        }
        Ok(copied)
    }
    let current = crate::core::project_hash::hash_project_root(project_root);
    let base = crate::core::data_dir::lean_ctx_data_dir()?.join("knowledge");
    let store = base.join(&current).join("knowledge.json");
    let had_store = store.exists();
    let copied = copy_missing(&base.join(old), &base.join(&current))?;
    if !had_store && store.exists() {
        // The copy names its new partition; the recorded root stays.
        let text = std::fs::read_to_string(&store).map_err(|e| e.to_string())?;
        let mut value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| e.to_string())?;
        value["project_hash"] = serde_json::Value::String(current.clone());
        let json = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic(&store, &json)?;
    }
    let item = format!("{STORE_KNOWLEDGE}:{old}");
    if copied && !adopted(&read_journal(), &item, &current) {
        record(STORE_KNOWLEDGE, &item, &current, true)?;
    }
    Ok(copied)
}

/// The facts of one legacy facts archive with their stable item ids.
pub(crate) fn legacy_facts_of(
    path: &Path,
    envelope: &serde_json::Value,
) -> Vec<(String, serde_json::Value)> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A partition's archives are named with the partition: stems can repeat.
    let stem = match envelope["scope"].as_str() {
        Some(scope) => format!("{scope}/{stem}"),
        None => stem,
    };
    envelope
        .get("items")
        .or_else(|| envelope.get("facts"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, fact)| {
            (
                format!("{STORE_FACTS_ARCHIVE}:{stem}#{index}"),
                fact.clone(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    /// AP05 review I2: a partition hash covers the project's identity, so
    /// adding a git remote moved every partition and the project's earlier
    /// archives, `ctx_memory` facts, knowledge and shared agent knowledge
    /// were no longer visible to it.
    #[test]
    fn an_identity_change_keeps_the_projects_archives_and_memory() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        crate::test_env::set_var("LEAN_CTX_ARCHIVE", "1");
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let root_str = root.to_str().unwrap();
        let in_project = |f: &dyn Fn()| {
            crate::core::policy::runtime::REQUEST_PROJECT
                .sync_scope(std::cell::RefCell::new(Some(root.clone())), f);
        };
        let output = "identity drift marker output\n".repeat(64);
        in_project(&|| {
            crate::core::archive::store("ctx_shell", "make drift", &output, None)
                .expect("archived");
        });
        crate::core::shared_context::SharedContext::for_project(root_str)
            .unwrap()
            .put("drift-memory-marker", "codex", "fact")
            .unwrap();
        let mut knowledge = crate::core::knowledge::ProjectKnowledge::new(root_str);
        knowledge.remember(
            "decision",
            "drift-key",
            "drift knowledge survives",
            "s1",
            0.9,
            &crate::core::memory_policy::MemoryPolicy::default(),
        );
        knowledge.save().unwrap();
        crate::core::agents::AgentRegistry::new()
            .share_knowledge(
                root_str,
                "a1",
                "decision",
                &[("db".into(), "drift-shared".into())],
            )
            .unwrap();
        let before = crate::core::project_hash::hash_project_root(root_str);

        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:owner/drift.git\n",
        )
        .unwrap();
        assert_ne!(
            crate::core::project_hash::hash_project_root(root_str),
            before
        );

        in_project(&|| {
            let listed = crate::core::archive::list_entries(None);
            assert!(
                listed.iter().any(|entry| entry.command == "make drift"),
                "{listed:?}"
            );
        });
        let hint = crate::core::shared_context::session_start_hint(root_str).unwrap_or_default();
        assert!(hint.contains("drift-memory-marker"), "{hint}");
        let recalled = crate::core::knowledge::ProjectKnowledge::load(root_str).expect("knowledge");
        assert!(
            recalled
                .facts
                .iter()
                .any(|fact| fact.key == "drift-key" && fact.is_current())
        );
        let shared = crate::core::agents::AgentRegistry::new()
            .receive_shared_knowledge(root_str, "a2")
            .unwrap();
        assert!(shared.iter().any(|fact| fact.text == "db=drift-shared"));
        crate::test_env::remove_var("LEAN_CTX_ARCHIVE");
    }
}
