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
    // SAFETY: this integration-test binary contains a single `#[test]`, so no

    // other thread reads or writes the environment concurrently.
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

/// One `lean-ctx` process in `project`, sharing the data directories under
/// `shared` with every other process of the test; never the developer's
/// daemon or state.
fn cli(shared: &std::path::Path, project: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .env("LEAN_CTX_ACTIVE", "1")
        .env("__LEAN_CTX_SKIP_EVENTS", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .env("DO_NOT_TRACK", "1")
        .env_remove("CLAUDECODE")
        .env_remove("LEAN_CTX_PROJECT_ROOT")
        .env("LEAN_CTX_CONVERSATION_SCOPE", "0");
    for (name, directory) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("LEAN_CTX_CONFIG_DIR", "config"),
        ("LEAN_CTX_DATA_DIR", "data"),
        ("LEAN_CTX_STATE_DIR", "state"),
        ("LEAN_CTX_CACHE_DIR", "cache"),
    ] {
        let path = shared.join(directory);
        std::fs::create_dir_all(&path).unwrap();
        command.env(name, path);
    }
    command
}

/// ABNAHME I01/I02 across independent processes, then after a restart:
/// processes of two projects run at the same time against ONE data
/// directory and write the same fact keys. Every acknowledged fact stays in
/// its own project; a later process of one project neither recalls nor finds
/// the other project's value, nor a hint that it exists.
#[test]
fn independent_processes_of_two_projects_keep_the_same_keys_apart() {
    const KEYS: [&str; 3] = ["db", "queue", "auth"];
    let shared = tempfile::tempdir().unwrap();
    let shared = shared.path().canonicalize().unwrap();
    let projects: Vec<(&str, std::path::PathBuf)> = ["p1", "p2"]
        .into_iter()
        .map(|name| {
            let root = shared.join("work").join(name);
            std::fs::create_dir_all(&root).unwrap();
            (name, root)
        })
        .collect();

    // All writers are started before any is awaited.
    let shared_dirs = shared.as_path();
    let writers: Vec<_> = projects
        .iter()
        .flat_map(|(name, root)| {
            KEYS.iter().map(move |key| {
                cli(shared_dirs, root)
                    .args(["knowledge", "remember", &format!("canary-{name}-{key}")])
                    .args(["--category", "decision", "--key", key])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
        })
        .collect();
    for writer in writers {
        let output = writer.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Restarted processes, one per project.
    let run = |root: &std::path::Path, args: &[&str]| {
        let output = cli(&shared, root).args(args).output().unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };
    for (name, root) in &projects {
        let other = if *name == "p1" { "p2" } else { "p1" };
        let recalled = run(root, &["knowledge", "recall", "--category", "decision"]);
        for key in KEYS {
            assert!(
                recalled.contains(&format!("canary-{name}-{key}")),
                "{name} lost {key}: {recalled}"
            );
        }
        let searched = run(root, &["knowledge", "search", "canary"]);
        let foreign = format!("canary-{other}");
        assert!(
            !recalled.contains(&foreign) && !searched.contains(&foreign),
            "{recalled}{searched}"
        );
        // Searching for the other project's exact value says what a search
        // for a value that never existed says.
        assert_eq!(
            run(
                root,
                &["knowledge", "search", &format!("canary-{other}-db")]
            )
            .replace(&format!("canary-{other}-db"), "QUERY"),
            run(root, &["knowledge", "search", "canary-nowhere-db"])
                .replace("canary-nowhere-db", "QUERY"),
        );
    }
}
