//! L2 handle/spill (#1094): when downstream output is larger than the budget,
//! store the verbatim blob in the content-addressed archive and hand the model a
//! compact summary plus a `ctx_expand` retrieval handle instead of the full
//! payload. This generalizes what Repomix (`outputId`) and Headroom (CCR) each
//! do for themselves to *every* addon, through one lean-ctx retrieval path.
//!
//! Determinism (#498): the archive id is a content hash and the summary is a
//! pure function of the content, so the returned text is byte-stable.

use crate::core::tokens::count_tokens;

/// Leading lines kept in the inline summary above the retrieval hint.
const SUMMARY_LINES: usize = 20;

/// If `text` exceeds `budget_tokens`, archive it verbatim and return a summary +
/// `ctx_expand` handle. Returns `None` when the output fits (caller keeps it) or
/// when archiving is disabled (caller falls back to L1 / identity).
#[must_use]
pub fn maybe_spill(server: &str, tool: &str, text: &str, budget_tokens: usize) -> Option<String> {
    let tokens = count_tokens(text);
    if tokens <= budget_tokens {
        return None;
    }
    let source = format!("gateway:{server}::{tool}");
    let id = crate::core::archive::store(&source, tool, text, None)?;
    Some(format_handle(&id, text, tokens))
}

/// Compact, deterministic summary: the first [`SUMMARY_LINES`] lines followed by
/// the archive retrieval hint (`ctx_expand(id="…")`).
fn format_handle(id: &str, text: &str, tokens: usize) -> String {
    let head: String = text
        .lines()
        .take(SUMMARY_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let hint = crate::core::archive::format_hint(id, text.len(), tokens);
    format!("{head}\n…\n{hint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_budget_does_not_spill() {
        assert!(maybe_spill("s", "t", "tiny", 1000).is_none());
    }

    #[test]
    fn over_budget_spills_and_returns_handle() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());
        crate::test_env::set_var("LEAN_CTX_ARCHIVE", "1");

        let big: String = (0..5000).map(|i| format!("payload line {i}\n")).collect();
        let out = maybe_spill("repomix", "pack_codebase", &big, 100)
            .expect("oversized output should spill when archive is enabled");
        assert!(out.contains("ctx_expand"), "must offer a retrieval handle");
        assert!(
            out.contains("payload line 0"),
            "must include a head summary"
        );

        // Deterministic: same content → same content-addressed id (#498).
        let again = maybe_spill("repomix", "pack_codebase", &big, 100).unwrap();
        assert_eq!(out, again);

        crate::test_env::remove_var("LEAN_CTX_ARCHIVE");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
