use std::path::PathBuf;

#[test]
fn docs_tool_counts_match_manifest() {
    let registry = lean_ctx::server::registry::build_registry();
    let expected_granular = registry.len();
    let expected_unified = lean_ctx::tool_defs::unified_tool_defs().len();

    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = rust_dir.parent().unwrap_or(&rust_dir);

    // Exact-count files must match the runtime count
    let exact_checks: Vec<(&str, Vec<String>)> = vec![
        (
            "LEANCTX_FEATURE_CATALOG.md",
            vec![
                format!("Granular MCP tools: **{}**", expected_granular),
                format!("Unified MCP tools: **{}**", expected_unified),
                format!("## Granular MCP Tools ({})", expected_granular),
            ],
        ),
        (
            "rust/README.md",
            vec![
                format!("{} MCP tools", expected_granular),
                format!("## {}+ MCP Tools", expected_granular),
            ],
        ),
    ];

    // Approximate-count files use "N+" format (marketing docs).
    // VISION.md is intentionally absent: the public manifesto carries no tool
    // counts since the vision-docs consolidation (numbers live in README,
    // ARCHITECTURE and the internal source of truth).
    let approx_checks: Vec<(&str, &str)> = vec![
        ("README.md", "MCP tools"),
        ("ARCHITECTURE.md", "tools"),
        ("skills/lean-ctx/SKILL.md", "MCP tools"),
        ("rust/src/templates/SKILL.md", "MCP tools"),
    ];

    let mut failures: Vec<String> = Vec::new();

    for (rel, must_contain) in exact_checks {
        let path = repo_root.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        for needle in must_contain {
            if !content.contains(&needle) {
                failures.push(format!("{rel}: missing `{needle}`"));
            }
        }
    }

    for (rel, suffix) in approx_checks {
        let path = repo_root.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let has_count = content.contains(&format!("{expected_granular} {suffix}"))
            || content.contains(&format!("{expected_granular}+ {suffix}"));
        if !has_count {
            failures.push(format!(
                "{rel}: missing `{expected_granular} {suffix}` or `{expected_granular}+ {suffix}`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "docs/tool-count drift detected (expected_granular={expected_granular}, expected_unified={expected_unified}):\n{}",
        failures.join("\n")
    );
}
