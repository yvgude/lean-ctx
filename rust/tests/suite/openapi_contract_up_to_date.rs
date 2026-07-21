//! Binds the public `OpenAPI` surface (`core::openapi::endpoints`) to the
//! Endpoints table in `docs/contracts/http-mcp-contract-v1.md`. Adding or
//! removing a public route must touch both, or this fails (EPIC 12.1).

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Parse `(METHOD, /path)` pairs from the first markdown table under the
/// `## Endpoints` heading. Skips the header/separator rows and the `/*`
/// MCP fallback (it is not an `OpenAPI` path).
fn documented_pairs(doc: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let mut in_section = false;
    for line in doc.lines() {
        if line.starts_with("## ") {
            if line.trim() == "## Endpoints" {
                in_section = true;
                continue;
            } else if in_section {
                break; // left the Endpoints section
            }
        }
        if !in_section || !line.trim_start().starts_with('|') {
            continue;
        }
        let cells: Vec<String> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        if cells.len() < 2 {
            continue;
        }
        let method = cells[0].to_uppercase();
        let path = cells[1].trim_matches('`').trim().to_string();
        if method == "METHOD" || method.starts_with("---") {
            continue;
        }
        if !path.starts_with('/') || path.contains('*') {
            continue; // header noise or the MCP fallback
        }
        out.insert((method, path));
    }
    out
}

#[test]
fn openapi_endpoints_match_contract_doc() {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = rust_dir.parent().unwrap_or(&rust_dir);
    let doc_path = repo_root.join("docs/contracts/http-mcp-contract-v1.md");

    let doc = match std::fs::read_to_string(&doc_path) {
        Ok(c) => c,
        Err(e) => {
            assert!(
                !repo_root.join("docs/contracts").exists(),
                "missing HTTP/MCP contract at {}: {e}",
                doc_path.display()
            );
            eprintln!("skipping: {} not present", doc_path.display());
            return;
        }
    };

    let documented = documented_pairs(&doc);
    let code: BTreeSet<(String, String)> = lean_ctx::core::openapi::endpoints()
        .iter()
        .map(|e| (e.method.to_uppercase(), e.path.to_string()))
        .collect();

    assert_eq!(
        code,
        documented,
        "OpenAPI inventory (core::openapi::endpoints) is out of sync with the \
         Endpoints table in {}.\nin code only: {:?}\nin doc only:  {:?}",
        doc_path.display(),
        code.difference(&documented).collect::<Vec<_>>(),
        documented.difference(&code).collect::<Vec<_>>(),
    );
}
