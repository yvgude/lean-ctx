// Integration tests for Issue #326: parallel MCP `remember` calls clobbered
// each other (lost updates) and could leave `knowledge.json` with trailing
// JSON garbage. The fix serializes the read-modify-write under a per-project
// lock and writes the file atomically (temp + rename).

use lean_ctx::core::knowledge::ProjectKnowledge;
use lean_ctx::core::memory_policy::MemoryPolicy;

/// Sets an isolated data dir for this test binary so the real `~/.lean-ctx`
/// store is never touched.
fn isolate_data_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", dir.path()) };
    dir
}

// A single `#[test]` keeps the process-global `LEAN_CTX_DATA_DIR` override
// race-free (cargo runs test fns in this binary on parallel threads).
#[test]
fn parallel_writes_are_lossless_atomic_and_clean() {
    let _data = isolate_data_dir();
    parallel_remember_preserves_every_fact_and_valid_json();
    save_is_atomic_and_leaves_no_temp_files();
}

fn parallel_remember_preserves_every_fact_and_valid_json() {
    // A real, unique project directory (no identity markers → stable hash).
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();

    // Seven distinct facts written from seven threads at once — the exact
    // scenario from the bug report.
    let facts = [
        ("architecture", "project-purpose", "context layer"),
        ("architecture", "example-projects", "samples dir"),
        ("architecture", "skill-pattern", "skill md"),
        ("deployment", "build-run", "cargo build"),
        ("conventions", "env-shell", "bash"),
        ("conventions", "repo-boundary", "single repo"),
        ("conventions", "lean-ctx-note", "use mcp"),
    ];

    let handles: Vec<_> = facts
        .iter()
        .map(|(cat, key, val)| {
            let root = root.clone();
            let cat = (*cat).to_string();
            let key = (*key).to_string();
            let val = (*val).to_string();
            std::thread::spawn(move || {
                let policy = MemoryPolicy::default();
                ProjectKnowledge::mutate_locked(&root, |k| {
                    k.remember(&cat, &key, &val, "sess", 0.8, &policy);
                })
                .expect("mutate_locked must succeed");
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    // All seven facts must survive — no lost updates.
    let knowledge = ProjectKnowledge::load(&root).expect("store must exist");
    for (cat, key, _) in &facts {
        assert!(
            knowledge
                .facts
                .iter()
                .any(|f| &f.category == cat && &f.key == key),
            "fact {cat}/{key} was lost to a write race"
        );
    }
    assert_eq!(knowledge.facts.len(), facts.len(), "expected all 7 facts");

    // The on-disk file must be valid JSON (no trailing garbage).
    let data_dir = lean_ctx::core::data_dir::lean_ctx_data_dir().unwrap();
    let path = data_dir
        .join("knowledge")
        .join(&knowledge.project_hash)
        .join("knowledge.json");
    let content = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str::<serde_json::Value>(&content)
        .expect("knowledge.json must be valid JSON after concurrent writes");
}

fn save_is_atomic_and_leaves_no_temp_files() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();

    let policy = MemoryPolicy::default();
    let (knowledge, ()) = ProjectKnowledge::mutate_locked(&root, |k| {
        k.remember("conventions", "lang", "rust", "sess", 0.9, &policy);
    })
    .unwrap();

    let data_dir = lean_ctx::core::data_dir::lean_ctx_data_dir().unwrap();
    let dir = data_dir.join("knowledge").join(&knowledge.project_hash);

    // The committed file exists and parses…
    let content = std::fs::read_to_string(dir.join("knowledge.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&content).unwrap();

    // …and the atomic write left no `.tmp.*` siblings behind.
    let leftover = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().contains(".tmp."));
    assert!(!leftover, "atomic write must not leave temp files");
}
