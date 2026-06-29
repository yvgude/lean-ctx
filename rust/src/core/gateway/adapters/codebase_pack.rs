//! codebase-pack adapter (#1097): Repomix `pack_codebase` → a lean-ctx archive
//! handle.
//!
//! Repomix returns a one-shot summary (`directoryStructure`, totals) plus an
//! `outputId` the agent later reads via `read_repomix_output`/
//! `grep_repomix_output`. This adapter additionally persists the verbatim pack
//! into the content-addressed archive, so the agent can retrieve any slice
//! through the *single* lean-ctx path (`ctx_expand`) — the repomix `outputId`
//! stays surfaced for grep. lean-ctx becomes the unified retrieval layer.
//!
//! Deterministic (#498): the archive id is a content hash and the returned
//! summary is a pure function of the pack output.

use crate::core::tokens::count_tokens;

/// Repomix tools whose result carries a packed `outputId`.
const PACK_TOOLS: [&str; 2] = ["pack_codebase", "pack_remote_repository"];

/// If `text` is a Repomix pack result, archive it verbatim and return a compact
/// summary + a `ctx_expand` retrieval handle. Returns `None` for non-pack tools,
/// non-JSON output, or when archiving is unavailable (caller falls back).
#[must_use]
pub fn transform(server: &str, tool: &str, text: &str, budget_tokens: usize) -> Option<String> {
    if !PACK_TOOLS.contains(&tool) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let output_id = v.get("outputId").and_then(serde_json::Value::as_str)?;

    let id = crate::core::archive::store(&format!("gateway:{server}::{tool}"), tool, text, None)?;
    let tokens = count_tokens(text);

    let mut summary = String::new();
    if let Some(dir) = v
        .get("directoryStructure")
        .and_then(serde_json::Value::as_str)
    {
        summary.push_str("directoryStructure:\n");
        summary.push_str(dir.trim_end());
        summary.push('\n');
    }
    for key in ["totalFiles", "totalTokens", "totalCharacters"] {
        if let Some(n) = v.get(key) {
            summary.push_str(&format!("{key}: {n}\n"));
        }
    }
    // Keep the (possibly large) directory tree within budget; deterministic.
    let summary = super::super::postprocess::compress::to_budget(&summary, budget_tokens);

    Some(format!(
        "{summary}\nrepomix outputId: {output_id} \
         (grep via: ctx_tools call {server}::grep_repomix_output)\n{}",
        crate::core::archive::format_hint(&id, text.len(), tokens)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_json() -> String {
        serde_json::json!({
            "outputId": "rmx_abc123",
            "directoryStructure": "src/\n  main.rs\n  lib.rs\n",
            "totalFiles": 2,
            "totalTokens": 1234
        })
        .to_string()
    }

    #[test]
    fn ignores_non_pack_tools() {
        assert!(transform("repomix", "grep_repomix_output", &pack_json(), 2000).is_none());
    }

    #[test]
    fn ignores_non_json() {
        assert!(transform("repomix", "pack_codebase", "not json", 2000).is_none());
    }

    #[test]
    fn pack_becomes_handle_with_outputid() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());
        crate::test_env::set_var("LEAN_CTX_ARCHIVE", "1");

        let out = transform("repomix", "pack_codebase", &pack_json(), 2000).expect("transform");
        assert!(out.contains("rmx_abc123"), "surfaces repomix outputId");
        assert!(
            out.contains("ctx_expand"),
            "offers lean-ctx retrieval handle"
        );
        assert!(
            out.contains("directoryStructure"),
            "keeps the structure summary"
        );

        // Deterministic across calls (#498).
        let again = transform("repomix", "pack_codebase", &pack_json(), 2000).unwrap();
        assert_eq!(out, again);

        crate::test_env::remove_var("LEAN_CTX_ARCHIVE");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
