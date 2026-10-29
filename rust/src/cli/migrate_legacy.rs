// SPDX-License-Identifier: Apache-2.0

//! `lean-ctx migrate legacy-context` — explicit adoption of context that
//! releases before 3.11.5 stored without a provable project.
//!
//! Such context is served to no project (AP05): it is never assigned to the
//! active one automatically. The owner of this machine lists it here and
//! copies chosen items into one chosen project. Every copy passes the
//! admission of a normal write to that project's store; a refused item is
//! reported and not copied. Legacy files are never changed or deleted. Each
//! adoption is appended to the journal of `core::legacy_adoption`, which
//! also makes a repeated adoption into the same project a no-op. Items with
//! provable provenance are also adopted automatically there.

use std::path::{Path, PathBuf};

const USAGE: &str = "Usage: lean-ctx migrate legacy-context [--list]\n       \
     lean-ctx migrate legacy-context --adopt --project <root> --items <id,id,...|all> [--dry-run]";

/// The legacy stores this command reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyStore {
    /// `~/.local/share/lean-ctx/shared_context.jsonl` (`ctx_memory`).
    SharedContext,
    /// `memory/archive/archive-*.json` (archived knowledge facts).
    FactsArchive,
    /// Unbound `ctx_expand` archives without a project.
    ExpandArchive,
    /// Handoff ledgers without a project.
    HandoffLedger,
    /// Quarantined installation-wide agent shared knowledge.
    SharedKnowledge,
}

impl LegacyStore {
    fn as_str(self) -> &'static str {
        match self {
            Self::SharedContext => "shared-context",
            Self::FactsArchive => "facts-archive",
            Self::ExpandArchive => "expand-archive",
            Self::HandoffLedger => "handoff-ledger",
            Self::SharedKnowledge => "shared-knowledge",
        }
    }
}

/// What an item is adopted from; read again at adoption.
#[derive(Debug, Clone)]
enum Source {
    SharedContext {
        content: String,
        agent: String,
        category: String,
    },
    Fact(serde_json::Value),
    Archive(String),
    Ledger(PathBuf),
    SharedFact {
        from: String,
        category: String,
        key: String,
        value: String,
    },
}

/// One legacy item as the local owner sees it in the terminal.
#[derive(Debug, Clone)]
pub(crate) struct LegacyItem {
    pub store: LegacyStore,
    pub id: String,
    pub title: String,
    pub path: PathBuf,
    pub date: String,
    source: Source,
}

/// The result of adopting one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Adopted,
    AlreadyAdopted,
    WouldAdopt,
    Refused(String),
}

impl Outcome {
    fn label(&self) -> String {
        match self {
            Self::Adopted => "adopted".into(),
            Self::AlreadyAdopted => "already adopted (no-op)".into(),
            Self::WouldAdopt => "would adopt (admission runs at --adopt)".into(),
            Self::Refused(reason) => format!("refused: {reason}"),
        }
    }
}

fn title(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(59).collect::<String>())
    } else {
        line.to_string()
    }
}

fn shared_context_item(
    path: &Path,
    entry: crate::core::shared_context::SharedContextEntry,
    note: &str,
) -> LegacyItem {
    LegacyItem {
        store: LegacyStore::SharedContext,
        id: crate::core::legacy_adoption::shared_context_item(&entry.id),
        title: format!(
            "[{} · {}] {}{note}",
            entry.category,
            entry.agent,
            title(&entry.content)
        ),
        path: path.to_path_buf(),
        date: chrono::DateTime::from_timestamp(i64::try_from(entry.timestamp).unwrap_or(0), 0)
            .map(|at| at.to_rfc3339())
            .unwrap_or_default(),
        source: Source::SharedContext {
            content: entry.content,
            agent: entry.agent,
            category: entry.category,
        },
    }
}

/// The pre-3.11.5 installation-wide file, then facts left in a partition
/// that is no longer the current one of the root they recorded.
fn shared_context_items() -> Vec<LegacyItem> {
    let mut items = Vec::new();
    if let Some(store) = crate::core::shared_context::SharedContext::legacy() {
        let path = store.path().to_path_buf();
        let mut entries = store.get_recent(usize::MAX).unwrap_or_default();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        items.extend(
            entries
                .into_iter()
                .map(|entry| shared_context_item(&path, entry, "")),
        );
    }
    for (path, entry) in crate::core::shared_context::SharedContext::orphaned_entries() {
        items.push(shared_context_item(&path, entry, IDENTITY_CHANGED));
    }
    items
}

/// Marks an item whose project identity (remote, package name) changed.
const IDENTITY_CHANGED: &str = " (project identity changed)";

fn facts_items(
    archives: Vec<(PathBuf, serde_json::Value)>,
    note: &str,
    items: &mut Vec<LegacyItem>,
) {
    for (path, envelope) in archives {
        let date = envelope["archived_at"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        for (id, fact) in crate::core::legacy_adoption::legacy_facts_of(&path, &envelope) {
            let category = fact["category"].as_str().unwrap_or("?");
            let key = fact["key"].as_str().unwrap_or("?");
            items.push(LegacyItem {
                store: LegacyStore::FactsArchive,
                id,
                title: format!("{category}/{key}{note}"),
                path: path.clone(),
                date: date.clone(),
                source: Source::Fact(fact),
            });
        }
    }
}

fn facts_archive_items() -> Vec<LegacyItem> {
    let mut items = Vec::new();
    facts_items(
        crate::core::memory_archive::legacy_facts_archives(),
        "",
        &mut items,
    );
    facts_items(
        crate::core::memory_archive::orphaned_facts_archives(),
        IDENTITY_CHANGED,
        &mut items,
    );
    items
}

fn expand_archive_items() -> Vec<LegacyItem> {
    let base = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.join("archives"))
        .unwrap_or_default();
    let mut entries = crate::core::archive::legacy_unscoped_entries();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    entries
        .into_iter()
        .map(|entry| LegacyItem {
            store: LegacyStore::ExpandArchive,
            id: format!("expand-archive:{}", entry.id),
            title: format!(
                "{} · {}{}",
                entry.tool,
                title(&entry.command),
                if crate::core::archive::is_drifted(&entry) {
                    IDENTITY_CHANGED
                } else {
                    ""
                }
            ),
            path: base.join(&entry.id[..2]),
            date: entry.created_at.to_rfc3339(),
            source: Source::Archive(entry.id),
        })
        .collect()
}

fn handoff_items() -> Vec<LegacyItem> {
    crate::core::handoff_ledger::projectless_ledgers()
        .into_iter()
        .map(|(path, ledger)| LegacyItem {
            store: LegacyStore::HandoffLedger,
            id: format!(
                "handoff-ledger:{}",
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            title: format!(
                "handoff by {}",
                ledger.agent_id.as_deref().unwrap_or("unknown agent")
            ),
            path: path.clone(),
            date: ledger.created_at.clone(),
            source: Source::Ledger(path),
        })
        .collect()
}

fn shared_knowledge_items() -> Vec<LegacyItem> {
    let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.join("agents").join("shared_knowledge").join("quarantine"))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let mut items = Vec::new();
    for path in files {
        let Some(facts) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<serde_json::Value>>(&text).ok())
        else {
            continue;
        };
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        for (index, fact) in facts.into_iter().enumerate() {
            let field = |name: &str| fact[name].as_str().unwrap_or_default().to_string();
            items.push(LegacyItem {
                store: LegacyStore::SharedKnowledge,
                id: format!("shared-knowledge:{stem}#{index}"),
                title: format!(
                    "{}/{} (from {})",
                    field("category"),
                    field("key"),
                    field("from_agent")
                ),
                path: path.clone(),
                date: field("timestamp"),
                source: Source::SharedFact {
                    from: field("from_agent"),
                    category: field("category"),
                    key: field("key"),
                    value: field("value"),
                },
            });
        }
    }
    items
}

/// Every legacy item on this machine, store by store, in a stable order.
pub(crate) fn list_items() -> Vec<LegacyItem> {
    let mut items = shared_context_items();
    items.extend(facts_archive_items());
    items.extend(expand_archive_items());
    items.extend(handoff_items());
    items.extend(shared_knowledge_items());
    items
}

/// A project root the owner may adopt into: an existing directory that is
/// not a broad root (home, `/`, agent config).
fn adoption_root(project: &str) -> Result<PathBuf, String> {
    let root = Path::new(project)
        .canonicalize()
        .map_err(|e| format!("project {project}: {e}"))?;
    if !root.is_dir() || crate::core::pathutil::is_broad_or_unsafe_root(&root) {
        return Err(format!("{} is not a project root", root.display()));
    }
    Ok(root)
}

/// Copy one item into `root` through the admission of a normal write there.
fn adopt_one(item: &LegacyItem, root: &Path) -> Outcome {
    let root_str = root.to_string_lossy();
    let result: Result<(), String> = match &item.source {
        Source::SharedContext {
            content,
            agent,
            category,
        } => crate::core::context_admission::carrier::admit_for_store(content)
            .map_err(|withheld| withheld.message("memory content"))
            .and_then(|admitted| {
                crate::core::shared_context::SharedContext::for_project(&root_str)?
                    .put(admitted, agent.clone(), category.clone())
                    .map(|_| ())
            }),
        Source::Fact(fact) => {
            match crate::core::memory_archive::adopt_legacy_facts(root, std::slice::from_ref(fact))
            {
                Ok(flags) if flags.first() == Some(&true) => Ok(()),
                Ok(_) => Err("withheld by the current policy".into()),
                Err(error) => Err(error),
            }
        }
        Source::Archive(id) => crate::core::archive::adopt_legacy_entry(id, root).map(|_| ()),
        Source::Ledger(path) => {
            crate::core::handoff_ledger::adopt_projectless_ledger(path, &root_str).map(|_| ())
        }
        Source::SharedFact {
            from,
            category,
            key,
            value,
        } => crate::core::agents::AgentRegistry::mutate_locked(|registry| {
            registry.adopt_shared_fact(
                &root_str,
                &adopted_fact_id(&item.id),
                from,
                category,
                (key.clone(), value.clone()),
            )
        })
        .and_then(|(_, shared)| shared),
    };
    result.map_or_else(Outcome::Refused, |()| Outcome::Adopted)
}

/// The stable id of an adopted shared fact, derived from its legacy item.
fn adopted_fact_id(item: &str) -> String {
    format!("adopted-{}", &blake3::hash(item.as_bytes()).to_hex()[..16])
}

/// Test-only crash point between an item's write and its journal line.
#[cfg(test)]
pub(crate) mod failpoint {
    use std::cell::Cell;

    thread_local! {
        static AFTER_WRITE: Cell<bool> = const { Cell::new(false) };
    }

    /// The next adoption on this thread stops after its write.
    pub(crate) fn arm_after_write() {
        AFTER_WRITE.with(|armed| armed.set(true));
    }

    pub(super) fn after_write() -> Result<(), String> {
        if AFTER_WRITE.with(|armed| armed.replace(false)) {
            return Err("injected crash after the write".into());
        }
        Ok(())
    }
}

/// Adopt the items named by `selection` (ids, or `all`) into `project`.
/// With `dry_run` nothing is written: the result says what would happen.
pub(crate) fn adopt(
    project: &str,
    selection: &[String],
    dry_run: bool,
) -> Result<Vec<(LegacyItem, Outcome)>, String> {
    let root = adoption_root(project)?;
    let root_str = root.to_string_lossy().into_owned();
    let partition = crate::core::project_hash::hash_project_root(&root_str);
    let items = list_items();
    let all = selection.iter().any(|id| id == "all");
    if !all
        && let Some(unknown) = selection
            .iter()
            .find(|id| !items.iter().any(|item| &item.id == *id))
    {
        return Err(format!("unknown legacy item '{unknown}' (see --list)"));
    }
    let journal = crate::core::legacy_adoption::read_journal();
    let mut report = Vec::new();
    for item in items
        .into_iter()
        .filter(|item| all || selection.contains(&item.id))
    {
        let done = crate::core::legacy_adoption::adopted(&journal, &item.id, &partition);
        let outcome = if done {
            Outcome::AlreadyAdopted
        } else if dry_run {
            Outcome::WouldAdopt
        } else {
            crate::core::policy::runtime::REQUEST_PROJECT
                .sync_scope(std::cell::RefCell::new(Some(root.clone())), || {
                    adopt_one(&item, &root)
                })
        };
        // Every write is idempotent per item and project (content id, stable
        // adoption id or an existing copy): a crash after the write and
        // before its journal line makes the rerun write nothing twice.
        if !dry_run && !done {
            #[cfg(test)]
            failpoint::after_write()?;
            crate::core::legacy_adoption::record(
                item.store.as_str(),
                &item.id,
                &partition,
                outcome == Outcome::Adopted,
            )?;
        }
        report.push((item, outcome));
    }
    Ok(report)
}

fn print_list(items: &[LegacyItem]) {
    if items.is_empty() {
        println!("No legacy context without a project on this machine.");
        return;
    }
    for store in [
        LegacyStore::SharedContext,
        LegacyStore::FactsArchive,
        LegacyStore::ExpandArchive,
        LegacyStore::HandoffLedger,
        LegacyStore::SharedKnowledge,
    ] {
        let of_store: Vec<_> = items.iter().filter(|item| item.store == store).collect();
        if of_store.is_empty() {
            continue;
        }
        println!("{} ({} item(s))", store.as_str(), of_store.len());
        for item in of_store {
            println!(
                "  {}  {}  {}  [{}]",
                item.id,
                item.date,
                item.title,
                item.path.display()
            );
        }
    }
    println!(
        "\nNothing here is served to any project. Adopt chosen items with:\n  \
         lean-ctx migrate legacy-context --adopt --project <root> --items <id,...|all> [--dry-run]"
    );
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|pos| args.get(pos + 1))
        .cloned()
}

/// `lean-ctx migrate legacy-context …`; `args` follow the source name.
pub(crate) fn run(args: &[String]) -> Result<(), String> {
    if !args.iter().any(|arg| arg == "--adopt") {
        print_list(&list_items());
        return Ok(());
    }
    let project = flag_value(args, "--project").ok_or(USAGE)?;
    let selection: Vec<String> = flag_value(args, "--items")
        .ok_or(USAGE)?
        .split(',')
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    if selection.is_empty() {
        return Err(USAGE.into());
    }
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let report = adopt(&project, &selection, dry_run)?;
    if dry_run {
        println!("[dry-run] nothing is written.");
    }
    for (item, outcome) in &report {
        println!("  {}  {}", item.id, outcome.label());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AP05 follow-up: legacy context is served to no project; the owner
    /// adopts it explicitly into one. Adoption is visible there only,
    /// idempotent, and a dry run writes nothing.
    #[test]
    fn legacy_items_are_listed_and_adopted_into_one_project_only() {
        let data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let home = data.path().join("home");
        let previous_home = std::env::var_os("HOME");
        crate::test_env::set_var("HOME", &home);
        let legacy = crate::core::shared_context::SharedContext::legacy().unwrap();
        legacy
            .put(
                "legacy-marker retries live in net/retry.rs",
                "codex",
                "fact",
            )
            .unwrap();
        let before = std::fs::read(legacy.path()).unwrap();
        let p1 = tempfile::tempdir().unwrap();
        let p2 = tempfile::tempdir().unwrap();
        let p1_root = p1.path().canonicalize().unwrap();
        let p1_root = p1_root.to_str().unwrap();
        let p2_root = p2.path().to_str().unwrap();

        let listed: Vec<_> = list_items()
            .into_iter()
            .filter(|item| item.store == LegacyStore::SharedContext)
            .collect();
        assert_eq!(listed.len(), 1);
        let id = listed[0].id.clone();
        let visible = |root: &str| {
            crate::core::shared_context::SharedContext::for_project(root)
                .unwrap()
                .get_recent(10)
                .unwrap()
                .len()
        };

        let dry = adopt(p1_root, std::slice::from_ref(&id), true).unwrap();
        assert_eq!(dry[0].1, Outcome::WouldAdopt);
        assert_eq!(visible(p1_root), 0);
        assert!(crate::core::legacy_adoption::read_journal().is_empty());

        let adopted = adopt(p1_root, std::slice::from_ref(&id), false).unwrap();
        assert_eq!(adopted[0].1, Outcome::Adopted);
        assert_eq!(visible(p1_root), 1);
        assert_eq!(visible(p2_root), 0);

        let again = adopt(p1_root, std::slice::from_ref(&id), false).unwrap();
        assert_eq!(again[0].1, Outcome::AlreadyAdopted);
        assert_eq!(visible(p1_root), 1);
        assert_eq!(crate::core::legacy_adoption::read_journal().len(), 1);
        assert_eq!(std::fs::read(legacy.path()).unwrap(), before);

        // M04: an older client writes to the installation-wide file again
        // after the adoption, in its own format (no project, none of the
        // newer fields). It is read, listed as legacy and served to no one.
        let mut old_client = std::fs::OpenOptions::new()
            .append(true)
            .open(legacy.path())
            .unwrap();
        std::io::Write::write_all(
            &mut old_client,
            br#"{"id":"n1-old-client","content":"old-client-marker","agent":"old","timestamp":1,"category":"fact","access_count":0,"last_accessed":1}
"#,
        )
        .unwrap();
        assert_eq!(visible(p1_root), 1);
        assert_eq!(visible(p2_root), 0);
        let relisted: Vec<_> = list_items()
            .into_iter()
            .filter(|item| item.store == LegacyStore::SharedContext)
            .map(|item| item.id)
            .collect();
        assert_eq!(relisted.len(), 2);
        assert!(relisted.contains(&id));
        let again = adopt(p1_root, std::slice::from_ref(&id), false).unwrap();
        assert_eq!(again[0].1, Outcome::AlreadyAdopted);

        match previous_home {
            Some(home) => crate::test_env::set_var("HOME", home),
            None => crate::test_env::remove_var("HOME"),
        }
    }

    /// M03: a crash after an item's write and before its journal line made
    /// the rerun adopt it again: a second shared fact, a second archived
    /// fact. Each rerun now completes the adoption with exactly one copy.
    #[test]
    fn an_adoption_interrupted_before_its_journal_line_leaves_one_copy() {
        let data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let previous_home = std::env::var_os("HOME");
        crate::test_env::set_var("HOME", data.path().join("home"));
        let p1 = tempfile::tempdir().unwrap();
        let p1_root = p1.path().canonicalize().unwrap();
        let p1_str = p1_root.to_str().unwrap();

        crate::core::shared_context::SharedContext::legacy()
            .unwrap()
            .put("legacy-crash-marker", "codex", "fact")
            .unwrap();
        let shared = crate::core::paths::data_dir()
            .unwrap()
            .join("shared_knowledge.json");
        std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
        std::fs::write(
            &shared,
            r#"[{"from_agent":"old","category":"c","key":"k","value":"crash-shared","timestamp":"2026-10-01T00:00:00Z","received_by":[]}]"#,
        )
        .unwrap();
        // Any access moves the legacy file into quarantine.
        crate::core::agents::AgentRegistry::new()
            .receive_shared_knowledge(p1_str, "nobody")
            .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut knowledge =
            crate::core::knowledge::ProjectKnowledge::new(scratch.path().to_str().unwrap());
        knowledge.remember(
            "decision",
            "crash-fact-key",
            "crash fact value",
            "s1",
            0.9,
            &crate::core::memory_policy::MemoryPolicy::default(),
        );
        let fact = serde_json::to_value(&knowledge.facts[0]).unwrap();
        let archive = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap()
            .join("memory/archive");
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::write(
            archive.join("archive-legacy.json"),
            serde_json::json!({"archived_at": "2026-09-01T00:00:00Z", "facts": [fact]}).to_string(),
        )
        .unwrap();

        let ids: Vec<String> = list_items()
            .into_iter()
            .filter(|item| {
                matches!(
                    item.store,
                    LegacyStore::SharedContext
                        | LegacyStore::SharedKnowledge
                        | LegacyStore::FactsArchive
                )
            })
            .map(|item| item.id)
            .collect();
        assert_eq!(ids.len(), 3, "{ids:?}");
        for id in &ids {
            failpoint::arm_after_write();
            assert!(adopt(p1_str, std::slice::from_ref(id), false).is_err());
            let rerun = adopt(p1_str, std::slice::from_ref(id), false).unwrap();
            assert_eq!(rerun[0].1, Outcome::Adopted, "{id}");
            let again = adopt(p1_str, std::slice::from_ref(id), false).unwrap();
            assert_eq!(again[0].1, Outcome::AlreadyAdopted, "{id}");
        }

        let memory = crate::core::shared_context::SharedContext::for_project(p1_str)
            .unwrap()
            .get_recent(10)
            .unwrap();
        assert_eq!(memory.len(), 1);
        let received = crate::core::agents::AgentRegistry::new()
            .receive_shared_knowledge(p1_str, "reader")
            .unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].text, "k=crash-shared");
        let archived = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(p1_root.clone())),
            || {
                crate::core::memory_archive::list_archives(
                    crate::core::memory_archive::MemoryStore::Facts,
                    None,
                )
                .iter()
                .map(|path| {
                    let envelope: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
                    envelope["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|item| item["key"] == "crash-fact-key")
                        .count()
                })
                .sum::<usize>()
            },
        );
        assert_eq!(archived, 1);
        assert_eq!(crate::core::legacy_adoption::read_journal().len(), 3);

        match previous_home {
            Some(home) => crate::test_env::set_var("HOME", home),
            None => crate::test_env::remove_var("HOME"),
        }
    }
}
