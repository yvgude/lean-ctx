use std::path::Path;

use lean_ctx::core::knowledge::ProjectKnowledge;
use lean_ctx::core::memory_policy::MemoryPolicy;

#[test]
fn ctx_knowledge_recall_is_budgeted_and_deterministic() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().to_string());

    let project_root = tmp.path().join("proj");
    std::fs::create_dir_all(&project_root).expect("create project root");
    let project_root_str = project_root.to_string_lossy().to_string();

    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::load_or_create(&project_root_str);
    for i in 0..50 {
        knowledge.remember(
            "architecture",
            &format!("k{i:02}"),
            &format!("v{i:02}"),
            "s1",
            0.8,
            &policy,
        );
    }
    knowledge.save().expect("save knowledge");

    let out1 = lean_ctx::tools::ctx_knowledge::handle(
        &project_root_str,
        "recall",
        Some("architecture"),
        None,
        None,
        None,
        "s1",
        None,
        None,
        None,
        None,
    );
    let out2 = lean_ctx::tools::ctx_knowledge::handle(
        &project_root_str,
        "recall",
        Some("architecture"),
        None,
        None,
        None,
        "s1",
        None,
        None,
        None,
        None,
    );

    assert_eq!(out1, out2, "recall output must be deterministic");
    assert!(
        out1.contains("showing 10/50"),
        "recall header must indicate truncation"
    );

    let fact_lines = out1
        .lines()
        .filter(|l| l.starts_with("  [architecture/"))
        .count();
    assert!(fact_lines <= 10, "must not exceed recall budget");

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn ctx_knowledge_export_is_file_backed_not_json_stdout() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().to_string());

    let project_root = tmp.path().join("proj");
    std::fs::create_dir_all(&project_root).expect("create project root");
    let project_root_str = project_root.to_string_lossy().to_string();

    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::load_or_create(&project_root_str);
    knowledge.remember("arch", "db", "MySQL", "s1", 0.8, &policy);
    knowledge.save().expect("save knowledge");

    let out = lean_ctx::tools::ctx_knowledge::handle(
        &project_root_str,
        "export",
        None,
        None,
        None,
        None,
        "s1",
        None,
        None,
        None,
        None,
    );

    assert!(
        out.starts_with("Export saved: "),
        "export must return a compact confirmation"
    );
    assert!(
        !out.trim_start().starts_with('{'),
        "export must not print full JSON to stdout"
    );

    let path_str = out
        .strip_prefix("Export saved: ")
        .and_then(|s| s.split_whitespace().next())
        .expect("extract export path");
    assert!(Path::new(path_str).exists(), "export file must exist");

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn ctx_knowledge_feedback_persists_and_affects_quality_score() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().to_string());

    let project_root = tmp.path().join("proj");
    std::fs::create_dir_all(&project_root).expect("create project root");
    let project_root_str = project_root.to_string_lossy().to_string();

    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::load_or_create(&project_root_str);
    knowledge.remember("arch", "db", "MySQL", "s1", 0.8, &policy);
    knowledge.save().expect("save knowledge");

    let before = ProjectKnowledge::load_or_create(&project_root_str);
    let before_fact = before
        .facts
        .iter()
        .find(|f| f.is_current() && f.category == "arch" && f.key == "db")
        .expect("fact exists");
    let before_score = before_fact.quality_score();

    let out = lean_ctx::tools::ctx_knowledge::handle(
        &project_root_str,
        "feedback",
        Some("arch"),
        Some("db"),
        Some("up"),
        None,
        "s2",
        None,
        None,
        None,
        None,
    );
    assert!(
        out.contains("Feedback recorded"),
        "feedback output must confirm recording: {out}"
    );

    let after = ProjectKnowledge::load_or_create(&project_root_str);
    let after_fact = after
        .facts
        .iter()
        .find(|f| f.is_current() && f.category == "arch" && f.key == "db")
        .expect("fact exists");

    assert_eq!(after_fact.feedback_up, 1);
    assert_eq!(after_fact.feedback_down, 0);
    assert!(after_fact.last_feedback.is_some());
    assert!(
        after_fact.quality_score() > before_score,
        "quality score should increase after positive feedback"
    );

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn ctx_knowledge_relations_persist_and_render() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().to_string());

    let project_root = tmp.path().join("proj");
    std::fs::create_dir_all(&project_root).expect("create project root");
    let project_root_str = project_root.to_string_lossy().to_string();

    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::load_or_create(&project_root_str);
    knowledge.remember("arch", "db", "MySQL", "s1", 0.9, &policy);
    knowledge.remember("arch", "cache", "Redis", "s1", 0.9, &policy);
    knowledge.save().expect("save knowledge");

    let out = lean_ctx::tools::ctx_knowledge::handle(
        &project_root_str,
        "relate",
        Some("arch"),
        Some("db"),
        Some("depends_on"),
        Some("arch/cache"),
        "s2",
        None,
        None,
        None,
        None,
    );
    assert!(out.contains("Relation"), "relate must confirm: {out}");

    let loaded = ProjectKnowledge::load_or_create(&project_root_str);
    let graph =
        lean_ctx::core::knowledge_relations::KnowledgeRelationGraph::load(&loaded.project_hash)
            .expect("relations graph should exist");
    assert!(
        graph.edges.iter().any(|e| {
            e.from.category == "arch"
                && e.from.key == "db"
                && e.to.category == "arch"
                && e.to.key == "cache"
                && e.kind == lean_ctx::core::knowledge_relations::KnowledgeEdgeKind::DependsOn
        }),
        "expected depends_on edge in relations graph"
    );

    let list = lean_ctx::tools::ctx_knowledge::handle(
        &project_root_str,
        "relations",
        Some("arch"),
        Some("db"),
        None,
        Some("out"),
        "s2",
        None,
        None,
        None,
        None,
    );
    assert!(
        list.contains("depends_on") && list.contains("arch/cache"),
        "relations must include the edge: {list}"
    );

    let diagram = lean_ctx::tools::ctx_knowledge::handle(
        &project_root_str,
        "relations_diagram",
        Some("arch"),
        Some("db"),
        None,
        Some("out"),
        "s2",
        None,
        None,
        None,
        None,
    );
    assert!(
        diagram.contains("graph TD") && diagram.contains("depends_on"),
        "relations_diagram must return mermaid: {diagram}"
    );

    let out2 = lean_ctx::tools::ctx_knowledge::handle(
        &project_root_str,
        "unrelate",
        Some("arch"),
        Some("db"),
        Some("depends_on"),
        Some("arch/cache"),
        "s2",
        None,
        None,
        None,
        None,
    );
    assert!(
        out2.contains("removed"),
        "unrelate must confirm removal: {out2}"
    );

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}
