//! Binds the documented capabilities contract to the code SSOT: the machine-
//! readable key list in `docs/contracts/capabilities-contract-v1.md` must equal
//! `server_capabilities::TOP_LEVEL_KEYS`. Keeps the doc honest as the payload
//! evolves (EPIC 12.1).

use std::path::PathBuf;

const START: &str = "<!-- capabilities-top-level-keys -->";
const END: &str = "<!-- /capabilities-top-level-keys -->";

#[test]
fn capabilities_doc_keys_match_code() {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = rust_dir.parent().unwrap_or(&rust_dir);
    let doc = repo_root.join("docs/contracts/capabilities-contract-v1.md");

    let content = match std::fs::read_to_string(&doc) {
        Ok(c) => c,
        Err(e) => {
            // Minimal checkouts may exclude docs/; only enforce when present.
            assert!(
                !repo_root.join("docs/contracts").exists(),
                "missing capabilities contract at {}: {e}",
                doc.display()
            );
            eprintln!("skipping: {} not present", doc.display());
            return;
        }
    };

    let start = content
        .find(START)
        .unwrap_or_else(|| panic!("missing `{START}` marker in {}", doc.display()))
        + START.len();
    let end = content
        .find(END)
        .unwrap_or_else(|| panic!("missing `{END}` marker in {}", doc.display()));
    assert!(start <= end, "malformed key markers in {}", doc.display());

    let mut documented: Vec<String> = content[start..end]
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    documented.sort();

    let mut code: Vec<String> = lean_ctx::core::server_capabilities::TOP_LEVEL_KEYS
        .iter()
        .map(ToString::to_string)
        .collect();
    code.sort();

    assert_eq!(
        documented,
        code,
        "capabilities contract doc {} is out of sync with \
         server_capabilities::TOP_LEVEL_KEYS.\nUpdate the `{START}` block to match.",
        doc.display()
    );
}
