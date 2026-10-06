//! Tests for the ctx_knowledge action handlers and consolidation reports.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::core::consolidation_engine::ConsolidateOptions;
use crate::core::memory_lifecycle::LifecycleReport;
use crate::core::procedural_memory::ProceduralStore;
use crate::core::procedural_memory::Procedure;

struct CurrentDirGuard {
    previous: std::path::PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl CurrentDirGuard {
    fn enter(dir: &std::path::Path) -> Self {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let lock = LOCK.get_or_init(|| std::sync::Mutex::new(()));
        let guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        Self {
            previous,
            _lock: guard,
        }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.previous).unwrap();
    }
}

struct DataDirGuard;

impl DataDirGuard {
    fn set(path: &std::path::Path) -> Self {
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", path);
        Self
    }
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

fn report(session_id: Option<String>, session_items: usize) -> KnowledgeConsolidationReport {
    KnowledgeConsolidationReport {
        session_id,
        session_items,
        imported_decisions: session_items / 2,
        imported_findings: session_items - session_items / 2,
        curation: None,
        facts: 7,
        active_facts: 5,
        archived_facts: 2,
        fact_capacity_target: 6,
        fact_capacity_archived: 1,
        patterns: 2,
        patterns_capacity_target: 6,
        patterns_compacted: 0,
        history: 3,
        history_capacity_target: 6,
        history_compacted: 1,
        procedures: 4,
        procedure_capacity_target: 6,
        procedures_compacted: 2,
        lifecycle: LifecycleReport {
            decayed_count: 1,
            consolidated_count: 2,
            archived_count: 3,
            compacted_count: 4,
            capacity_archived: 1,
            remaining_facts: 5,
        },
        dry_run: false,
    }
}

#[test]
fn consolidation_report_marks_no_session_import() {
    let out = format_consolidation_report(&report(None, 0));

    assert!(out.contains("Session import: none (no active session)"));
    assert!(out.contains("Lifecycle: decayed 1, consolidated 2"));
}

#[test]
fn consolidation_report_includes_session_and_lifecycle_stats() {
    let out = format_consolidation_report(&report(Some("s1".to_string()), 6));

    assert!(out.contains("Session import: s1 (6 item(s))"));
    assert!(
        out.contains("Facts: 5 active, 2 archived, 7 total (target <= 6, archived-to-target 1)")
    );
    assert!(
        out.contains(
            "Patterns: 2 (target <= 6, compacted 0), History: 3 (target <= 6, compacted 1)"
        )
    );
    assert!(out.contains("Procedures: 4 (target <= 6, compacted 2)"));
    assert!(out.contains("archived 3, compacted 4, remaining 5"));
    // Lossless: a run that archived items points at the restore path.
    assert!(out.contains("restore with: lean-ctx knowledge restore"));
}

fn test_procedure(id: usize, confidence: f32) -> Procedure {
    Procedure {
        id: format!("p-{id}"),
        name: format!("workflow-{id}"),
        description: "test workflow".to_string(),
        steps: Vec::new(),
        activation_keywords: Vec::new(),
        confidence,
        times_used: id as u32,
        times_succeeded: id as u32,
        last_used: Utc::now(),
        project_specific: true,
        created_at: Utc::now(),
    }
}

#[test]
fn consolidation_compacts_procedures_above_target() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().unwrap();
    let _data_dir = DataDirGuard::set(data_dir.path());
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();
    let project_hash = ProjectKnowledge::new(&root).project_hash;
    let mut store = ProceduralStore::new(&project_hash);
    // Hysteresis (#995): reclaim triggers only at/above the cap (100), then
    // settles at the headroom target (75). 100 -> keep 75, archive 25.
    for i in 0..100 {
        store.procedures.push(test_procedure(i, i as f32 / 100.0));
    }
    store.save().unwrap();

    let report = consolidate_project_knowledge(&root).unwrap();
    let reloaded = ProceduralStore::load(&project_hash).unwrap();

    assert_eq!(report.procedures, 75);
    assert_eq!(report.procedure_capacity_target, 75);
    assert_eq!(report.procedures_compacted, 25);
    assert_eq!(reloaded.procedures.len(), 75);
    // Lowest-retention procedures (smallest id/confidence) are the ones evicted.
    assert!(!reloaded.procedures.iter().any(|p| p.id == "p-0"));
    assert!(!reloaded.procedures.iter().any(|p| p.id == "p-24"));
    assert!(reloaded.procedures.iter().any(|p| p.id == "p-99"));
}

#[test]
fn consolidate_dry_run_previews_without_mutating() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().unwrap();
    let _data_dir = DataDirGuard::set(data_dir.path());
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();
    let project_hash = ProjectKnowledge::new(&root).project_hash;

    let mut store = ProceduralStore::new(&project_hash);
    for i in 0..100 {
        store.procedures.push(test_procedure(i, i as f32 / 100.0));
    }
    store.save().unwrap();

    let report =
        consolidate_project_knowledge_with(&root, &ConsolidateOptions::manual().into_dry_run())
            .unwrap();

    // The preview reports the reclaim that *would* happen…
    assert!(report.dry_run);
    assert_eq!(report.procedures, 100);
    assert_eq!(report.procedures_compacted, 25);
    assert!(format_consolidation_report(&report).contains("DRY RUN"));

    // …but the store on disk is byte-for-byte untouched.
    let reloaded = ProceduralStore::load(&project_hash).unwrap();
    assert_eq!(reloaded.procedures.len(), 100);
}

#[test]
fn consolidation_does_not_capacity_compact_at_twenty_five_percent_free() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().unwrap();
    let _data_dir = DataDirGuard::set(data_dir.path());
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();
    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::new(&root);

    for i in 0..150 {
        knowledge.remember(
            &format!("category-{i}"),
            &format!("k{i}"),
            &format!("unique stable fact value {i}"),
            "s1",
            0.8,
            &policy,
        );
    }
    for i in 0..75 {
        knowledge
            .history
            .push(crate::core::knowledge::ConsolidatedInsight {
                summary: format!("summary {i}"),
                from_sessions: vec![format!("s{i}")],
                timestamp: Utc::now(),
            });
    }
    knowledge.save().unwrap();

    let mut procedures = ProceduralStore::new(&knowledge.project_hash);
    for i in 0..75 {
        procedures
            .procedures
            .push(test_procedure(i, i as f32 / 100.0));
    }
    procedures.save().unwrap();

    let report = consolidate_project_knowledge(&root).unwrap();
    let reloaded = ProjectKnowledge::load(&root).unwrap();
    let reloaded_procedures = ProceduralStore::load(&knowledge.project_hash).unwrap();

    assert_eq!(report.fact_capacity_archived, 0);
    assert_eq!(report.history_compacted, 0);
    assert_eq!(report.procedures_compacted, 0);
    assert_eq!(reloaded.facts.len(), 150);
    assert_eq!(reloaded.history.len(), 75);
    assert_eq!(reloaded_procedures.procedures.len(), 75);
}

#[test]
fn consolidation_loads_session_for_requested_project_root() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().unwrap();
    let _data_dir = DataDirGuard::set(data_dir.path());
    let cwd_project = tempfile::tempdir().unwrap();
    let target_project = tempfile::tempdir().unwrap();
    let cwd_root = cwd_project.path().to_string_lossy().to_string();
    let target_root = target_project.path().to_string_lossy().to_string();

    let mut cwd_session = SessionState::new();
    cwd_session.project_root = Some(cwd_root);
    cwd_session.add_finding(None, None, "wrong cwd finding");
    cwd_session.save().unwrap();

    let mut target_session = SessionState::new();
    target_session.project_root = Some(target_root.clone());
    target_session.add_finding(None, None, "target project error: assertion failed in auth");
    target_session.save().unwrap();

    let _cwd = CurrentDirGuard::enter(cwd_project.path());
    let report = consolidate_project_knowledge(&target_root).unwrap();

    assert_eq!(
        report.session_id.as_deref(),
        Some(target_session.id.as_str())
    );
    assert_eq!(report.session_items, 1);

    let knowledge = ProjectKnowledge::load(&target_root).unwrap();
    assert!(
        knowledge
            .facts
            .iter()
            .any(|f| f.value == "target project error: assertion failed in auth")
    );
    assert!(
        !knowledge
            .facts
            .iter()
            .any(|f| f.value == "wrong cwd finding")
    );
}

#[test]
fn consolidate_all_project_knowledge_runs_every_known_project() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().unwrap();
    let _data_dir = DataDirGuard::set(data_dir.path());
    let project_a = tempfile::tempdir().unwrap();
    let project_b = tempfile::tempdir().unwrap();
    let root_a = project_a.path().to_string_lossy().to_string();
    let root_b = project_b.path().to_string_lossy().to_string();
    let policy = MemoryPolicy::default();

    let mut knowledge_a = ProjectKnowledge::new(&root_a);
    knowledge_a.remember("finding", "a", "project a fact", "s1", 0.8, &policy);
    knowledge_a.save().unwrap();

    let mut knowledge_b = ProjectKnowledge::new(&root_b);
    knowledge_b.remember("finding", "b", "project b fact", "s1", 0.8, &policy);
    knowledge_b.save().unwrap();

    let reports = consolidate_all_project_knowledge_with(&ConsolidateOptions::manual()).unwrap();
    let roots: Vec<_> = reports.iter().map(|(root, _)| root.clone()).collect();
    let mut expected = vec![root_a, root_b];
    expected.sort();

    assert_eq!(roots, expected);
    assert_eq!(reports.len(), 2);
    assert!(
        reports
            .iter()
            .all(|(_, report)| report.session_id.is_none())
    );
}

#[test]
fn all_consolidation_report_marks_empty_store_set() {
    let reports = Vec::new();

    let out = format_all_consolidation_reports(&reports);

    assert_eq!(out, "No project knowledge stores found.");
}
