use std::path::PathBuf;

use serde_json::Value;

#[test]
fn mcp_manifest_is_up_to_date() {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = rust_dir.parent().unwrap_or(&rust_dir);
    let path = repo_root.join("website/generated/mcp-tools.json");

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            // If the repo contains `website/`, the manifest is part of our SSOT and MUST exist.
            // (We keep the skip for minimal checkouts that exclude `website/` entirely.)
            assert!(
                !repo_root.join("website").exists(),
                "missing manifest at {}: {e}\nRun:\n  cargo run --example gen_mcp_manifest --features dev-tools\n",
                path.display()
            );
            eprintln!(
                "skipping: {} not present (website/ excluded)",
                path.display()
            );
            return;
        }
    };
    let on_disk: Value = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("invalid JSON at {}: {e}", path.display()));

    let expected = lean_ctx::core::mcp_manifest::manifest_value();
    assert_eq!(
        on_disk,
        expected,
        "manifest drift at {}.\nRegenerate via:\n  cargo run --example gen_mcp_manifest --features dev-tools\n",
        path.display()
    );
}
