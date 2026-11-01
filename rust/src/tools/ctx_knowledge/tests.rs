//! Tests for the ctx_knowledge action handlers and consolidation reports.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::core::consolidation_engine::ConsolidateOptions;
use crate::core::memory_lifecycle::LifecycleReport;
use crate::test_env::CurrentDirGuard;

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

struct ConfigDirGuard(Option<std::ffi::OsString>);

impl ConfigDirGuard {
    fn set(path: &std::path::Path) -> Self {
        let previous = std::env::var_os("LEAN_CTX_CONFIG_DIR");
        crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", path);
        Self(previous)
    }
}

impl Drop for ConfigDirGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", previous);
        } else {
            crate::test_env::remove_var("LEAN_CTX_CONFIG_DIR");
        }
    }
}

fn gateway_config(value: &str) {
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config directory");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), value).unwrap();
}

fn append_files(root: &std::path::Path, output: &mut String) {
    for entry in std::fs::read_dir(root).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            append_files(&path, output);
        } else if let Ok(text) = std::fs::read_to_string(path) {
            output.push_str(&text);
        }
    }
}

fn report(session_id: Option<String>, session_items: usize) -> KnowledgeConsolidationReport {
    KnowledgeConsolidationReport {
        session_id,
        session_items,
        imported_decisions: session_items / 2,
        imported_findings: session_items - session_items / 2,
        facts: 7,
        active_facts: 5,
        archived_facts: 2,
        patterns: 2,
        patterns_capacity_target: 6,
        patterns_compacted: 0,
        history: 3,
        history_capacity_target: 6,
        history_compacted: 1,
        lifecycle: LifecycleReport {
            decayed_count: 1,
            consolidated_count: 2,
            archived_count: 3,
            compacted_count: 4,
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
    assert!(out.contains("Facts: 5 active, 2 archived, 7 total (no limit)"));
    assert!(!out.contains("archived-to-target"));
    assert!(
        out.contains(
            "Patterns: 2 (target <= 6, compacted 0), History: 3 (target <= 6, compacted 1)"
        )
    );
    assert!(
        !out.contains("Procedures"),
        "the procedure store is retired"
    );
    assert!(out.contains("archived 3, compacted 4, remaining 5"));
    // Lossless: a run that archived items points at the restore path.
    assert!(out.contains("restore with: lean-ctx knowledge restore"));
}

#[test]
fn consolidate_dry_run_previews_without_mutating() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().unwrap();
    let _data_dir = DataDirGuard::set(data_dir.path());
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();
    let mut knowledge = ProjectKnowledge::new(&root);
    // Hysteresis (#995): reclaim triggers only at/above the cap (100), then
    // settles at the headroom target (75). 100 -> keep 75, archive 25.
    for i in 0..100 {
        knowledge
            .history
            .push(crate::core::knowledge::ConsolidatedInsight {
                summary: format!("summary {i}"),
                from_sessions: vec![format!("s{i}")],
                timestamp: Utc::now(),
            });
    }
    knowledge.save().unwrap();

    let report =
        consolidate_project_knowledge_with(&root, &ConsolidateOptions::manual().into_dry_run())
            .unwrap();

    // The preview reports the reclaim that *would* happen…
    assert!(report.dry_run);
    assert_eq!(report.history, 100);
    assert_eq!(report.history_compacted, 25);
    assert!(format_consolidation_report(&report).contains("DRY RUN"));

    // …but the store on disk is untouched.
    let reloaded = ProjectKnowledge::load(&root).unwrap();
    assert_eq!(reloaded.history.len(), 100);
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

    let report = consolidate_project_knowledge(&root).unwrap();
    let reloaded = ProjectKnowledge::load(&root).unwrap();

    assert_eq!(report.lifecycle.archived_count, 0);
    assert_eq!(report.history_compacted, 0);
    assert_eq!(reloaded.facts.len(), 150);
    assert_eq!(reloaded.history.len(), 75);
}

/// Knowledge has no fact limit (owner decision 2026-10-24): a project with
/// more healthy facts than the old default cap (200), and a legacy
/// `LEAN_CTX_KNOWLEDGE_MAX_FACTS` far below that, keeps every fact through a
/// real consolidation, and the dry run previews no fact archival either.
#[test]
fn consolidation_keeps_every_fact_past_the_old_cap() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().unwrap();
    let _data_dir = DataDirGuard::set(data_dir.path());
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();
    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::new(&root);
    for i in 0..450 {
        knowledge.remember(
            &format!("category-{i}"),
            &format!("k{i}"),
            &format!("unique stable fact value {i}"),
            "s1",
            0.8,
            &policy,
        );
    }
    knowledge.save().unwrap();
    let previous = std::env::var("LEAN_CTX_KNOWLEDGE_MAX_FACTS").ok();
    crate::test_env::set_var("LEAN_CTX_KNOWLEDGE_MAX_FACTS", "10");

    let preview =
        consolidate_project_knowledge_with(&root, &ConsolidateOptions::manual().into_dry_run());
    let real = consolidate_project_knowledge(&root);
    match previous {
        Some(value) => crate::test_env::set_var("LEAN_CTX_KNOWLEDGE_MAX_FACTS", &value),
        None => crate::test_env::remove_var("LEAN_CTX_KNOWLEDGE_MAX_FACTS"),
    }
    let (preview, real) = (preview.unwrap(), real.unwrap());

    for report in [&preview, &real] {
        assert_eq!(
            report.lifecycle.archived_count, 0,
            "dry_run={}",
            report.dry_run
        );
        assert_eq!(report.lifecycle.remaining_facts, 450);
        assert_eq!(report.active_facts, 450);
        assert!(format_consolidation_report(report).contains("450 total (no limit)"));
    }
    let reloaded = ProjectKnowledge::load(&root).unwrap();
    assert_eq!(
        reloaded.facts.iter().filter(|f| f.is_current()).count(),
        450
    );
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
    // The session imported is the caller's own (I07/S03), here the target
    // project's agent; the process CWD plays no part.
    let own = std::sync::Arc::new(tokio::sync::RwLock::new(target_session.clone()));
    let report = crate::core::session::caller::scope(Some(own), || {
        consolidate_project_knowledge(&target_root)
    })
    .unwrap();

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
    // Detached post-call threads of earlier tool tests (auto-capture, journal)
    // resolve the data dir when they write, not when they were spawned, so a
    // late one can add the runner's own checkout to this temp store while the
    // test holds the env lock. Such a store is a known project too and is
    // consolidated like any other; the test pins that both seeded projects are
    // each consolidated exactly once.
    let mut expected = vec![root_a, root_b];
    expected.sort();
    let seeded: Vec<_> = reports
        .iter()
        .filter(|(root, _)| expected.contains(root))
        .collect();
    let roots: Vec<_> = seeded.iter().map(|(root, _)| root.clone()).collect();

    assert_eq!(roots, expected);
    assert!(seeded.iter().all(|(_, report)| report.session_id.is_none()));
}

#[test]
fn all_consolidation_report_marks_empty_store_set() {
    let reports = Vec::new();

    let out = format_all_consolidation_reports(&reports);

    assert_eq!(out, "No project knowledge stores found.");
}

#[test]
fn json_and_okf_exports_withhold_unsafe_legacy_rows_without_rewriting_store() {
    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let _env_lock = crate::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().unwrap();
    let _data_dir = DataDirGuard::set(data_dir.path());
    let _config_dir = ConfigDirGuard::set(&data_dir.path().join("config"));
    gateway_config("[context_gateway]\nenabled = false\n");

    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();
    let mut knowledge = ProjectKnowledge::new(&root);
    knowledge.remember(
        "finding",
        "safe",
        "ordinary note",
        "fixture",
        0.9,
        &MemoryPolicy::default(),
    );
    knowledge.remember(
        "finding",
        "legacy-secret",
        AWS_KEY,
        "fixture",
        0.9,
        &MemoryPolicy::default(),
    );
    knowledge.save().unwrap();
    let store_path = data_dir
        .path()
        .join("knowledge")
        .join(&knowledge.project_hash)
        .join("knowledge.json");
    let original_store = std::fs::read(&store_path).unwrap();

    gateway_config("[context_gateway]\nmode = \"governed\"\n");
    let json_result = handle_export(&root);
    assert!(json_result.starts_with("Export saved:"), "{json_result}");
    let export_dir = data_dir.path().join("exports").join("knowledge");
    let mut json_output = String::new();
    for entry in std::fs::read_dir(&export_dir).unwrap().flatten() {
        json_output.push_str(&std::fs::read_to_string(entry.path()).unwrap());
    }
    assert!(json_output.contains("ordinary note"));
    assert!(!json_output.contains(AWS_KEY));

    let okf_dir = data_dir.path().join("exports").join("okf-test");
    let okf_result = handle_export_okf(&root, Some(okf_dir.to_str().unwrap()));
    assert!(
        okf_result.starts_with("OKF bundle exported:"),
        "{okf_result}"
    );
    let mut okf_output = String::new();
    append_files(&okf_dir, &mut okf_output);
    assert!(okf_output.contains("ordinary note"));
    assert!(!okf_output.contains(AWS_KEY));
    assert_eq!(std::fs::read(store_path).unwrap(), original_store);
}

#[test]
fn restore_does_not_rehydrate_unsafe_archived_knowledge_after_gateway_tightens() {
    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let _env_lock = crate::core::data_dir::test_env_lock();
    let data_dir = tempfile::tempdir().unwrap();
    let _data_dir = DataDirGuard::set(data_dir.path());
    let _config_dir = ConfigDirGuard::set(&data_dir.path().join("config"));
    gateway_config("[context_gateway]\nenabled = false\n");

    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();
    let mut knowledge = ProjectKnowledge::new(&root);
    knowledge.remember(
        "finding",
        "legacy-secret",
        AWS_KEY,
        "fixture",
        0.9,
        &MemoryPolicy::default(),
    );
    knowledge.save().unwrap();
    let archive_path = {
        let _cwd = CurrentDirGuard::enter(project.path());
        crate::core::memory_archive::archive_items(
            crate::core::memory_archive::MemoryStore::Facts,
            None,
            &knowledge.facts,
            &crate::core::memory_archive::ArchiveConfig::default(),
        )
        .unwrap()
        .unwrap()
    };
    let original_archive = std::fs::read(&archive_path).unwrap();
    knowledge.facts.clear();
    knowledge.save().unwrap();

    gateway_config("[context_gateway]\nmode = \"governed\"\n");
    let report = run_restore(
        &root,
        &RestoreOptions::new(
            Some(crate::core::memory_archive::MemoryStore::Facts),
            None,
            DEFAULT_RESTORE_LIMIT,
        ),
    )
    .unwrap();
    assert_eq!(report.total_restored(), 0);
    assert!(ProjectKnowledge::load(&root).unwrap().facts.is_empty());
    assert_eq!(std::fs::read(archive_path).unwrap(), original_archive);
}

/// The short wake-up answer needs all three: a presenter the request proved,
/// a briefing with exactly this text released to that presenter, and its hash
/// named by the caller. Building the full answer records nothing.
#[test]
fn a_wakeup_is_shortened_only_for_a_released_briefing_its_holder_confirms() {
    let text = |facts: &str| format!("{WAKEUP_HEAD}{facts}");
    let hash_of = |answer: &str| {
        answer
            .rsplit_once(WAKEUP_HASH_LINE)
            .and_then(|(_, rest)| rest.split_once(' '))
            .map(|(hash, _)| hash.to_string())
            .expect("the full answer names its hash")
    };

    // No proven presenter: the briefing as it is, with nothing to confirm.
    assert_eq!(
        wakeup_delta_for(None, None, text("ARCH: db=pg")),
        text("ARCH: db=pg")
    );

    let first = wakeup_delta_for(Some("holder-a"), None, text("ARCH: db=pg"));
    let hash = hash_of(&first);
    // Built, not released: naming the hash shortens nothing.
    let unreleased = wakeup_delta_for(Some("holder-a"), Some(&hash), text("ARCH: db=pg"));
    assert!(unreleased.starts_with(&text("ARCH: db=pg")), "{unreleased}");

    // An answer that was altered on the way out is no released briefing.
    let shortened = first.replacen("db=pg", "db=", 1);
    // Cut inside the closing line: hash and the space after it are still there.
    let cut = &first[..first.len() - 40];
    assert!(cut.contains(&format!("{hash} ")));
    let cut_and_followed = format!("{cut}\n[checkpoint]");
    note_wakeup_release_for(
        "holder-a",
        &[shortened.as_str(), cut, cut_and_followed.as_str()],
    );
    let altered = wakeup_delta_for(Some("holder-a"), Some(&hash), text("ARCH: db=pg"));
    assert!(altered.starts_with(&text("ARCH: db=pg")), "{altered}");

    // Released whole, with pipeline text around it: nothing of it is missing.
    let decorated = format!("[notice]\n{first}\n[checkpoint]");
    note_wakeup_release_for("holder-a", &[decorated.as_str()]);
    let again = wakeup_delta_for(Some("holder-a"), Some(&hash), text("ARCH: db=pg"));
    assert!(
        again.contains("unchanged") && !again.contains("db=pg"),
        "{again}"
    );
    for (holder, held, facts) in [
        (Some("holder-a"), None, "ARCH: db=pg"),
        (Some("holder-b"), Some(hash.as_str()), "ARCH: db=pg"),
        (None, Some(hash.as_str()), "ARCH: db=pg"),
        (Some("holder-a"), Some(hash.as_str()), "ARCH: db=mysql"),
    ] {
        let full = wakeup_delta_for(holder, held, text(facts));
        assert!(
            full.starts_with(&text(facts)),
            "{holder:?} {held:?}: {full}"
        );
    }
    let refusal = text(crate::core::sanitize::FENCE_REFUSAL);
    assert_eq!(
        wakeup_delta_for(Some("holder-a"), None, refusal.clone()),
        refusal
    );
}

/// AP07-G2: a bridge pull loaded and saved the project knowledge outside the
/// project lock, so facts other writers stored in between were lost.
#[test]
fn bridge_pull_keeps_facts_written_in_parallel() {
    use std::sync::{Arc, Barrier};
    const PULLERS: usize = 4;
    const WRITERS: usize = 4;
    let _data = crate::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();
    let hash = crate::core::project_hash::hash_project_root(&root);
    crate::core::knowledge_bridge::KnowledgeBridge::mutate_locked(&hash, |bridge| {
        let mut shared = ProjectKnowledge::new(&root);
        shared.remember(
            "architecture",
            "database",
            "PostgreSQL",
            "source",
            0.95,
            &crate::core::memory_policy::MemoryPolicy::default(),
        );
        bridge.publish("agent-source", &shared.facts)
    })
    .unwrap();
    let barrier = Arc::new(Barrier::new(PULLERS + WRITERS));
    let mut threads = Vec::new();
    for index in 0..PULLERS {
        let (barrier, root) = (Arc::clone(&barrier), root.clone());
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            let reply = handle_bridge_pull(&root, &format!("puller-{index}"));
            assert!(!reply.starts_with("Error"), "{reply}");
        }));
    }
    for index in 0..WRITERS {
        let (barrier, root) = (Arc::clone(&barrier), root.clone());
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            ProjectKnowledge::mutate_locked(&root, |knowledge| {
                knowledge.remember(
                    "notes",
                    &format!("writer-{index}"),
                    "kept",
                    "writer",
                    0.9,
                    &crate::core::memory_policy::MemoryPolicy::default(),
                );
            })
            .unwrap();
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let stored = ProjectKnowledge::load(&root).unwrap();
    for key in (0..WRITERS)
        .map(|index| format!("writer-{index}"))
        .chain(["database".to_string()])
    {
        assert!(
            stored.facts.iter().any(|fact| fact.key == key),
            "{key} was lost"
        );
    }
}
