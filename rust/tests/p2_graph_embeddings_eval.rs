use lean_ctx::core::graph_context::{build_graph_context, GraphContextOptions};

#[test]
fn graph_context_must_include_direct_and_transitive_deps() {
    if cfg!(windows) {
        return;
    }
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("mkdir data");
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().to_string());

    let project_root = tmp.path().join("proj");
    std::fs::create_dir_all(project_root.join("src")).expect("mkdir src");

    std::fs::write(
        project_root.join("src/a.ts"),
        r#"import { b } from "./b";
export const a = b + 1;
"#,
    )
    .expect("write a.ts");
    std::fs::write(
        project_root.join("src/b.ts"),
        r#"import { c } from "./c";
export const b = c + 1;
"#,
    )
    .expect("write b.ts");
    std::fs::write(project_root.join("src/c.ts"), "export const c = 1;\n").expect("write c.ts");

    let opts = GraphContextOptions {
        token_budget: 5000,
        max_files: 5,
        max_edges: 50,
        max_depth: 2,
        allow_build: true,
    };

    let primary = project_root.join("src/a.ts");
    let ctx = build_graph_context(
        primary.to_string_lossy().as_ref(),
        project_root.to_string_lossy().as_ref(),
        Some(opts),
    )
    .expect("graph context");

    let paths: Vec<String> = ctx.related_files.iter().map(|rf| rf.path.clone()).collect();
    assert!(
        paths.contains(&"src/b.ts".to_string()),
        "expected direct dependency src/b.ts, got {paths:?}"
    );
    assert!(
        paths.contains(&"src/c.ts".to_string()),
        "expected transitive dependency src/c.ts, got {paths:?}"
    );

    std::env::remove_var("LEAN_CTX_DATA_DIR");
}

#[cfg(feature = "embeddings")]
#[test]
fn knowledge_embeddings_semantic_search_must_include_expected() {
    use lean_ctx::core::knowledge_embedding::KnowledgeEmbeddingIndex;

    let idx = {
        let mut idx = KnowledgeEmbeddingIndex::new("projhash");
        idx.upsert("arch", "db", vec![1.0, 0.0, 0.0]);
        idx.upsert("arch", "cache", vec![0.0, 1.0, 0.0]);
        idx
    };

    let query = vec![1.0, 0.0, 0.0];
    let hits = idx.semantic_search(&query, 2);
    assert!(!hits.is_empty(), "expected at least one semantic hit");
    assert_eq!(hits[0].0.key, "db");
}
