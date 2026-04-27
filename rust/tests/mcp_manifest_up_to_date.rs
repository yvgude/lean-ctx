use std::path::PathBuf;

use serde_json::Value;

#[test]
fn mcp_manifest_is_up_to_date() {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = rust_dir.parent().unwrap_or(&rust_dir);
    let path = repo_root.join("website/generated/mcp-tools.json");

    let Ok(content) = std::fs::read_to_string(&path) else {
        eprintln!(
            "skipping: {} not present (website/ excluded from repo)",
            path.display()
        );
        return;
    };
    let on_disk: Value = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("invalid JSON at {}: {e}", path.display()));

    let expected = lean_ctx::core::mcp_manifest::manifest_value();
    assert_eq!(
        on_disk,
        expected,
        "manifest drift at {}.\nRegenerate via:\n  cargo run --bin gen_mcp_manifest\n",
        path.display()
    );
}
