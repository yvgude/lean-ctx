// SPDX-License-Identifier: Apache-2.0
//! Fresh original-source admission for local lexical retrieval. Legacy indexes
//! are discovery/performance artifacts, never authorization evidence.

use std::path::Path;

use crate::core::bm25_index::BM25Index;

pub(super) const UNSUPPORTED: &str = "ERR: this search mode is withheld under content policy until its original-source provenance is verified; use local mode=bm25";

pub(super) fn fresh_index(
    root: &Path,
    filter: &super::SearchFilter,
    tool: &str,
) -> Result<BM25Index, String> {
    let role = crate::core::roles::active_role();
    // Compose ranking is a source search: a role that explicitly denies
    // ctx_search must not receive search results through ctx_compose.
    let search_denied = tool == "ctx_compose"
        && role
            .tools
            .denied
            .iter()
            .any(|denied| denied == "ctx_search");
    if !matches!(tool, "ctx_search" | "ctx_semantic_search" | "ctx_compose")
        || !role.is_tool_allowed(tool)
        || search_denied
        || crate::core::policy::runtime::active().is_some_and(|policy| !policy.tool_allowed(tool))
    {
        return Err("source search is not authorized".into());
    }
    let root = root.canonicalize().map_err(|_| "source root unavailable")?;
    let authority = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .ok_or("source authority unavailable")?
        .canonicalize()
        .map_err(|_| "source authority unavailable")?;
    if !root.starts_with(&authority) {
        return Err("source root is outside the request authority".into());
    }
    // lean-ctx: bounded synchronous fresh view; replace with a digest- and
    // authority-bound admitted cache only when that cache has equivalent proof.
    let mut remaining = 64 * 1024 * 1024usize;
    BM25Index::build_from_admitted_sources(&root, |relative| {
        if !filter.matches(relative) {
            return Ok(None);
        }
        // Reserve a full per-source allowance. Otherwise an ordinary source
        // larger than the final remainder could be silently omitted as though
        // it were individually uninspectable, yielding a partial ranked corpus.
        if remaining < 2 * 1024 * 1024 {
            return Err("source byte budget exceeded; narrow the search scope".into());
        }
        let initial = remaining.min(2 * 1024 * 1024);
        let mut allowance = initial;
        let content = crate::tools::ctx_read::read_file_for_tool_rooted_budgeted(
            &root.join(relative).to_string_lossy(),
            &root.to_string_lossy(),
            tool,
            &mut allowance,
        );
        remaining = remaining.saturating_sub(initial.saturating_sub(allowance));
        // Denied, unreadable and non-text sources contribute neither documents
        // nor term frequencies, snippets or names to the retrieval view.
        Ok(content.ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::policy::runtime::{TestPolicyOverride, with_project_source_view};

    fn policy(extra: &str) -> crate::core::policy::ResolvedPolicy {
        crate::core::policy::load(&format!(
            "name = \"search-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{extra}"
        ))
        .unwrap()
    }

    fn view(root: &Path) -> BM25Index {
        let filter = super::super::SearchFilter::new(None, None).unwrap();
        with_project_source_view(&root.to_string_lossy(), || {
            fresh_index(root, &filter, "ctx_search")
        })
        .unwrap()
        .unwrap()
    }

    #[test]
    fn original_source_is_filtered_before_ranking_and_rechecked_without_metadata_change() {
        let _env = crate::core::data_dir::test_env_lock();
        let _policy = TestPolicyOverride::set(Some(policy(
            "[filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )));
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let source = root.join("auth.rs");
        let allowed = "// PUBLIC      \nfn login_canary_7319() { let answer = 73; }\n";
        let blocked = allowed.replace("PUBLIC      ", "CONFIDENTIAL");
        assert_eq!(allowed.len(), blocked.len());
        std::fs::write(&source, allowed).unwrap();
        let first = view(root);
        assert!(!first.search("login_canary_7319", 10).is_empty());
        let before = std::fs::metadata(&source).unwrap();
        std::fs::write(&source, blocked).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(before.modified().unwrap())
            .unwrap();
        let after = std::fs::metadata(&source).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        let denied = view(root);
        assert_eq!(denied.doc_count, 0);
        assert!(denied.search("login_canary_7319", 10).is_empty());
        assert!(denied.doc_freqs.is_empty());
        std::fs::write(&source, allowed).unwrap();
        assert!(!view(root).search("login_canary_7319", 10).is_empty());
    }

    #[test]
    fn redacted_values_cannot_influence_matching_or_corpus_statistics() {
        let _env = crate::core::data_dir::test_env_lock();
        let _policy =
            TestPolicyOverride::set(Some(policy("[redaction]\ncustomer = \"ZQX7319\"\n")));
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("auth.rs"),
            "fn authenticate() { let customer = \"ZQX7319\"; }\n",
        )
        .unwrap();
        let index = view(temp.path());
        assert!(!index.search("authenticate", 10).is_empty());
        assert!(index.search("ZQX7319", 10).is_empty());
        assert!(!index.doc_freqs.keys().any(|term| term.contains("zqx")));
        assert!(
            index
                .chunks
                .iter()
                .all(|chunk| !chunk.content.contains("ZQX7319"))
        );
    }

    #[test]
    fn bound_project_cannot_be_replaced_by_search_path() {
        let _env = crate::core::data_dir::test_env_lock();
        let _policy = TestPolicyOverride::set(None);
        let authority = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("outside.rs"),
            "fn outside_canary() {}\n",
        )
        .unwrap();
        let result = with_project_source_view(&authority.path().to_string_lossy(), || {
            super::super::search_hits(
                "outside_canary",
                &outside.path().to_string_lossy(),
                10,
                "bm25",
                None,
                None,
            )
        })
        .unwrap();
        assert!(
            result
                .unwrap_err()
                .contains("outside the request authority")
        );
    }

    #[test]
    fn legacy_alias_uses_its_own_explicit_tool_permission() {
        let _env = crate::core::data_dir::test_env_lock();
        let _policy = TestPolicyOverride::set(Some(policy(
            "[context]\nallow_tools = [\"ctx_semantic_search\"]\n",
        )));
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("auth.rs"), "fn legacy_canary() {}\n").unwrap();
        let path = temp.path().to_string_lossy();
        let legacy = super::super::handle_for_tool(
            "ctx_semantic_search",
            "legacy_canary",
            &path,
            10,
            crate::tools::CrpMode::Off,
            None,
            None,
            Some("bm25"),
            None,
            None,
        );
        assert!(legacy.contains("auth.rs"));
        let canonical = super::super::search_hits("legacy_canary", &path, 10, "bm25", None, None);
        assert!(canonical.unwrap_err().contains("not authorized"));
    }

    #[test]
    fn protected_legacy_modes_do_not_build_or_return_a_raw_index() {
        let _env = crate::core::data_dir::test_env_lock();
        let _policy = TestPolicyOverride::set(Some(policy("")));
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().to_string_lossy();
        std::fs::write(temp.path().join("auth.rs"), "fn authenticate() {}\n").unwrap();
        for mode in ["dense", "hybrid"] {
            assert!(
                super::super::search_hits("authenticate", &path, 10, mode, None, None)
                    .unwrap_err()
                    .contains("withheld")
            );
        }
        assert!(super::super::handle_reindex(&path).contains("withheld"));
        assert!(super::super::handle_reindex_artifacts(&path, true).contains("withheld"));
        assert!(
            super::super::handle_find_related("auth.rs", 1, &path, 10, crate::tools::CrpMode::Off)
                .contains("withheld")
        );
        assert!(!BM25Index::index_file_path(temp.path()).exists());
    }
}
