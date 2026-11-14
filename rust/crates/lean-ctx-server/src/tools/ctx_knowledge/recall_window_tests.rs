// SPDX-License-Identifier: Apache-2.0
//! #2248: a knowledge category can be listed whole through `recall` — a
//! per-call window (`limit`, `offset`), a project's own
//! `memory.knowledge.recall_facts_limit`, and every listed fact counted as
//! recalled.

use super::*;

struct EnvGuard(&'static str);

impl EnvGuard {
    fn set(name: &'static str, path: &std::path::Path) -> Self {
        lean_ctx::test_env::set_var(name, path);
        Self(name)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        lean_ctx::test_env::remove_var(self.0);
    }
}

/// An isolated data and config directory plus a project holding `count`
/// current facts in category `workflow`.
fn seeded(count: usize) -> (tempfile::TempDir, [EnvGuard; 2], String) {
    let tmp = tempfile::tempdir().unwrap();
    let data = EnvGuard::set("LEAN_CTX_DATA_DIR", &tmp.path().join("data"));
    let config = EnvGuard::set("LEAN_CTX_CONFIG_DIR", &tmp.path().join("config"));
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let root = project.to_string_lossy().to_string();
    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::load_or_create(&root);
    for i in 0..count {
        knowledge.remember(
            "workflow",
            &format!("k{i:03}"),
            &format!("v{i:03}"),
            "s1",
            0.8,
            &policy,
        );
    }
    knowledge.save().unwrap();
    (tmp, [data, config], root)
}

fn recall(root: &str, window: RecallWindow) -> String {
    match handle_outcome_windowed(
        root,
        "recall",
        Some("workflow"),
        None,
        None,
        None,
        "s1",
        None,
        None,
        None,
        None,
        None,
        window,
    ) {
        Ok(text) | Err(text) => text,
    }
}

fn keys(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|line| line.strip_prefix("  [workflow/"))
        .filter_map(|rest| rest.split(']').next())
        .map(str::to_string)
        .collect()
}

#[test]
fn recall_window_pages_a_category_past_the_policy_limit() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_tmp, _env, root) = seeded(25);

    let first = recall(&root, RecallWindow::default());
    assert!(first.contains("showing 10/25"), "{first}");
    assert!(first.contains("offset=10"), "next offset hinted: {first}");

    let second = recall(&root, RecallWindow::new(Some(10), Some(10)));
    assert!(second.contains("showing 11-20/25"), "{second}");
    assert!(second.contains("offset=20"), "next offset hinted: {second}");

    let third = recall(&root, RecallWindow::new(Some(10), Some(20)));
    assert!(third.contains("showing 21-25/25"), "{third}");
    assert!(
        !third.contains("offset="),
        "no hint on the last window: {third}"
    );

    let mut all: Vec<String> = [&first, &second, &third]
        .iter()
        .flat_map(|out| keys(out))
        .collect();
    assert_eq!(all.len(), 25, "three windows cover the category");
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 25, "no fact listed twice");

    let past = recall(&root, RecallWindow::new(Some(10), Some(25)));
    assert!(keys(&past).is_empty(), "{past}");
    assert!(past.contains("25"), "past the end names the total: {past}");
}

#[test]
fn recall_window_refreshes_every_listed_fact() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_tmp, _env, root) = seeded(25);

    // The handler persists through a deferred background save, so assert on
    // the window itself: every fact it serves is marked as recalled.
    let mut knowledge = ProjectKnowledge::load(&root).unwrap();
    let (served, total) = knowledge.recall_by_category_window("workflow", 10, 10);
    assert_eq!((served.len(), total), (10, 25));
    let served_keys: Vec<String> = served.iter().map(|f| f.key.clone()).collect();
    let unrefreshed: Vec<&str> = knowledge
        .facts
        .iter()
        .filter(|f| served_keys.contains(&f.key))
        .filter(|f| f.last_retrieved.is_none() || f.retrieval_count == 0)
        .map(|f| f.key.as_str())
        .collect();
    assert!(
        unrefreshed.is_empty(),
        "served but not recalled: {unrefreshed:?}"
    );
}

#[test]
fn recall_limit_is_bounded_by_the_hard_ceiling() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let ceiling = lean_ctx::core::budgets::KNOWLEDGE_RECALL_FACTS_MAX;
    let (_tmp, _env, root) = seeded(ceiling + 5);

    let out = recall(&root, RecallWindow::new(Some(1_000_000), None));
    assert_eq!(keys(&out).len(), ceiling, "{out}");
    assert!(
        out.contains(&format!("showing {ceiling}/{}", ceiling + 5)),
        "{out}"
    );
    assert!(out.contains(&format!("offset={ceiling}")), "{out}");
}

#[test]
fn recall_honours_the_project_recall_facts_limit() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_tmp, _env, root) = seeded(41);
    std::fs::write(
        std::path::Path::new(&root).join(".lean-ctx.toml"),
        "[memory.knowledge]\nrecall_facts_limit = 50\n",
    )
    .unwrap();

    let out = handle(
        &root,
        "recall",
        Some("workflow"),
        None,
        None,
        None,
        "s1",
        None,
        None,
        None,
        None,
        None,
    );
    assert!(out.contains("showing 41/41"), "{out}");
    assert_eq!(keys(&out).len(), 41);
}
