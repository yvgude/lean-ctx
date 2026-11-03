#[test]
fn public_docs_do_not_duplicate_mutable_tool_counts() {
    let expected_granular = lean_ctx_server::server::registry::tool_count();
    let expected_unified = lean_ctx::tool_defs::unified_tool_defs().len();

    let rust_dir = crate::engine_dir().to_path_buf();
    let repo_root = rust_dir.parent().unwrap_or(&rust_dir);

    // Mutable inventory belongs to runtime discovery and generated metadata,
    // not product or integration prose. This keeps the public SDK narrative
    // stable when the internal registry changes.
    let docs = [
        "LEANCTX_FEATURE_CATALOG.md",
        "rust/README.md",
        "README.md",
        "ARCHITECTURE.md",
        "skills/lean-ctx/SKILL.md",
        "rust/src/templates/SKILL.md",
    ];

    let mut failures: Vec<String> = Vec::new();

    for rel in docs {
        let path = repo_root.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        for needle in [
            format!("{expected_granular} MCP tools"),
            format!("{expected_granular}+ MCP tools"),
            format!("Granular MCP tools: **{expected_granular}**"),
            format!("Unified MCP tools: **{expected_unified}**"),
        ] {
            if content.contains(&needle) {
                failures.push(format!("{rel}: mutable inventory `{needle}`"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "public docs duplicate mutable runtime inventory:\n{}",
        failures.join("\n")
    );
}
