//! Drift gate for the code-generated reference appendices.
//!
//! `docs/reference/generated/*.md` are rendered from the in-code single sources
//! of truth (MCP registry + `Config` schema). If a new MCP tool or config key
//! is added without regenerating, this test fails — mirroring the CI
//! `gen_docs --check` step so the gap is also caught by `cargo test`.

use lean_ctx::core::reference_docs;

#[test]
fn generated_reference_docs_are_committed_and_current() {
    let dir = reference_docs::generated_dir();
    for (name, expected) in reference_docs::generated_docs() {
        let path = dir.join(name);
        let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "missing generated doc {} ({e}). Run: cargo run --example gen_docs --features dev-tools",
                path.display()
            )
        });
        assert!(
            reference_docs::content_matches(&on_disk, &expected),
            "{} is out of date. Run: cargo run --example gen_docs --features dev-tools",
            path.display()
        );
    }
}

#[test]
fn mcp_tool_count_matches_registry() {
    // The MCP-tool appendix is derived from the manifest, which is derived from
    // the registry — guard that the registry count is what we document.
    let manifest = lean_ctx::core::mcp_manifest::manifest_value();
    let granular = manifest["counts"]["granular"].as_u64().unwrap_or(0) as usize;
    assert_eq!(
        granular,
        lean_ctx::server::registry::tool_count(),
        "manifest granular tool count must equal registry tool_count()"
    );
}
