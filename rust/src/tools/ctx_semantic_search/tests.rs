//! Unit tests for the semantic-search stack (moved verbatim in the split).

#[cfg(test)]
mod filter_tests {
    #[allow(clippy::wildcard_imports)]
    use super::super::*;

    #[test]
    fn filter_language_rust() {
        let f = SearchFilter::new(Some(&["rust".into()]), None).unwrap();
        assert!(f.matches("src/main.rs"));
        assert!(!f.matches("src/main.ts"));
    }

    #[test]
    fn filter_path_glob() {
        let f = SearchFilter::new(None, Some("rust/src/**")).unwrap();
        assert!(f.matches("rust/src/core/mod.rs"));
        assert!(!f.matches("website/src/pages/index.astro"));
    }
}

#[cfg(test)]
mod dense_availability_tests {
    #[allow(clippy::wildcard_imports)]
    use super::super::*;

    /// #1259: a project that never built embeddings must be reported as
    /// degraded, so the default path can say so instead of calling BM25 hits
    /// "semantic".
    #[test]
    fn missing_embeddings_bin_is_reported_as_degraded() {
        let dir = std::env::temp_dir().join("lean-ctx-dense-missing-test");
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(dense_index_missing(&dir), cfg!(feature = "embeddings"));
    }
}

#[cfg(test)]
mod root_resolution_tests {
    #[allow(clippy::wildcard_imports)]
    use super::super::*;

    #[test]
    fn subdir_filter_scopes_results_to_subtree() {
        let f = SearchFilter::new(None, None)
            .unwrap()
            .with_subdir(Some("crate_a/src".to_string()));
        assert!(f.is_active());
        assert!(f.matches("crate_a/src/auth.rs"));
        assert!(f.matches("crate_a/src/nested/db.rs"));
        assert!(!f.matches("crate_b/src/auth.rs"));
        // Boundary: the prefix must be a whole path segment, not a substring.
        assert!(!f.matches("crate_a/src_extra/x.rs"));
        // Backslash input is normalized before matching.
        assert!(f.matches(r"crate_a\src\win.rs"));
    }

    #[test]
    fn subdir_filter_combines_with_extension_filter() {
        let f = SearchFilter::new(Some(&["rust".to_string()]), None)
            .unwrap()
            .with_subdir(Some("src".to_string()));
        assert!(f.matches("src/main.rs"));
        assert!(!f.matches("src/readme.md"), "wrong extension");
        assert!(!f.matches("docs/main.rs"), "outside subdir");
    }

    #[test]
    fn empty_subdir_is_no_scope() {
        let f = SearchFilter::new(None, None)
            .unwrap()
            .with_subdir(Some(String::new()));
        assert!(!f.is_active());
        assert!(f.matches("anything/here.rs"));
    }

    #[test]
    fn search_subdir_filter_derives_relative_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(search_subdir_filter(root, &sub).as_deref(), Some("a/b"));
        assert_eq!(search_subdir_filter(root, root), None);
        // A path that is not under root yields no scope.
        assert_eq!(search_subdir_filter(&sub, root), None);
    }

    #[test]
    fn resolve_search_root_promotes_subdir_to_project_root() {
        // #948: the index namespace is keyed on the project root; a subdir search
        // must resolve to that same root (and keep the subdir as a scope) instead
        // of hashing to a different, empty namespace.
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let root = crate::core::pathutil::safe_canonicalize_or_self(tmp.path());
        let sub = root.join("crate_a").join("src");
        std::fs::create_dir_all(&sub).unwrap();

        // Pin the project root so resolution is deterministic regardless of host
        // config; remove the env before asserting so a failure cannot leak it.
        crate::test_env::set_var("LEAN_CTX_PROJECT_ROOT", root.to_string_lossy().as_ref());
        let from_sub = resolve_search_root(&sub.to_string_lossy());
        let from_root = resolve_search_root(&root.to_string_lossy());
        crate::test_env::remove_var("LEAN_CTX_PROJECT_ROOT");

        let (resolved, subdir) = from_sub.unwrap();
        assert_eq!(resolved, root, "subdir must promote to the project root");
        assert_eq!(subdir.as_deref(), Some("crate_a/src"));

        let (resolved_root, subdir_root) = from_root.unwrap();
        assert_eq!(resolved_root, root);
        assert_eq!(subdir_root, None, "the root itself carries no subdir scope");
    }

    #[test]
    fn resolve_search_root_errors_on_missing_path() {
        assert!(resolve_search_root("/definitely/not/here/xyzzy-7f3a91").is_err());
    }
}

#[cfg(all(test, feature = "embeddings"))]
mod cold_start_guard_tests {
    #[allow(clippy::wildcard_imports)]
    use super::super::*;

    #[test]
    fn budget_zero_disables_guard() {
        // 0 = "always embed inline" (pre-#512 behavior), regardless of size.
        assert!(!exceeds_inline_embed_budget(1_000_000, 0));
    }

    #[test]
    fn budget_is_inclusive_and_triggers_above_threshold() {
        assert!(!exceeds_inline_embed_budget(0, 2000), "warm index: inline");
        assert!(
            !exceeds_inline_embed_budget(2000, 2000),
            "at the budget: still inline"
        );
        assert!(
            exceeds_inline_embed_budget(2001, 2000),
            "over the budget: degrade"
        );
    }

    #[test]
    fn default_threshold_positive_when_env_unset() {
        // With the env override unset the default must be a real, positive guard.
        if std::env::var_os("LEAN_CTX_HYBRID_INLINE_EMBED_MAX").is_none() {
            assert!(inline_embed_max_chunks() >= 1);
        }
    }

    #[test]
    fn dense_build_hint_always_points_at_the_cli_build() {
        let full = dense_build_hint(22_741, false);
        assert!(full.contains("lean-ctx index build-semantic"));
        assert!(full.contains("22741"));
        let compact = dense_build_hint(22_741, true);
        assert!(compact.contains("lean-ctx index build-semantic"));
        assert!(compact.contains("22741"));
    }
}

#[cfg(test)]
mod determinism_tests {
    #[allow(clippy::wildcard_imports)]
    use super::super::*;

    #[test]
    fn rrf_merge_hybrid_is_deterministic_on_ties() {
        let a = HybridResult {
            file_path: "a.rs".to_string(),
            symbol_name: "foo".to_string(),
            kind: crate::core::bm25_index::ChunkKind::Function,
            start_line: 1,
            end_line: 1,
            snippet: "a".to_string(),
            rrf_score: 0.0,
            bm25_score: None,
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        };
        let b = HybridResult {
            file_path: "b.rs".to_string(),
            symbol_name: "foo".to_string(),
            kind: crate::core::bm25_index::ChunkKind::Function,
            start_line: 1,
            end_line: 1,
            snippet: "b".to_string(),
            rrf_score: 0.0,
            bm25_score: None,
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        };

        // Two lists with swapped ranks yield identical RRF sums for a and b.
        let fused = rrf_merge_hybrid(
            vec![
                ("root".to_string(), vec![a.clone(), b.clone()]),
                ("root".to_string(), vec![b.clone(), a.clone()]),
            ],
            10,
        );

        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].file_path, "a.rs");
        assert_eq!(fused[1].file_path, "b.rs");
    }
}

#[cfg(test)]
mod dense_config_tests {
    use crate::core::hybrid_search::HybridConfig;

    /// #686: dense stays on by default — the flip is opt-in, no behavior change.
    #[test]
    fn dense_enabled_defaults_true() {
        assert!(HybridConfig::default().dense_enabled);
    }

    /// #686: `[search].dense_enabled = false` parses and leaves siblings at default.
    #[test]
    fn dense_enabled_deserializes_false() {
        let cfg: HybridConfig = toml::from_str("dense_enabled = false").unwrap();
        assert!(!cfg.dense_enabled);
        assert_eq!(cfg.bm25_candidates, 75);
        assert_eq!(cfg.splade_weight, 0.5);
    }
}

#[cfg(all(test, feature = "embeddings"))]
mod dense_toggle_tests {
    #[allow(clippy::wildcard_imports)]
    use super::super::*;
    use crate::core::bm25_index::{BM25Index, ChunkKind, CodeChunk, tokenize};
    use crate::core::hybrid_search::HybridConfig;

    fn small_index() -> BM25Index {
        BM25Index::from_chunks_for_test(vec![
            CodeChunk {
                file_path: "auth.rs".into(),
                symbol_name: "validate_token".into(),
                kind: ChunkKind::Function,
                start_line: 1,
                end_line: 10,
                content: "fn validate_token(token: &str) -> bool { check_jwt_expiry(token) }"
                    .into(),
                tokens: tokenize("fn validate_token token str bool check_jwt_expiry token"),
                token_count: 0,
            },
            CodeChunk {
                file_path: "db.rs".into(),
                symbol_name: "connect_database".into(),
                kind: ChunkKind::Function,
                start_line: 1,
                end_line: 5,
                content: "fn connect_database(url: &str) -> Pool { create_pool(url) }".into(),
                tokens: tokenize("fn connect_database url str Pool create_pool url"),
                token_count: 0,
            },
        ])
    }

    /// #686: the dense-disabled body ranks via BM25 (+ graph + rerank + SPLADE),
    /// emits a BM25 header, finds the lexical match, and crucially never loads the
    /// embedding engine or writes `embeddings.json` — the on-disk vector footprint
    /// and embed latency disappear.
    #[test]
    fn bm25_graph_search_ranks_without_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let index = small_index();
        let cfg = HybridConfig {
            dense_enabled: false,
            ..Default::default()
        };
        let filter = SearchFilter::new(None, None).unwrap();

        let out = bm25_graph_search(
            "jwt token validation",
            root,
            &index,
            5,
            false,
            &filter,
            &cfg,
        );

        assert!(
            out.contains("Semantic search (BM25"),
            "expected BM25 header, got: {out}"
        );
        assert!(
            out.contains("validate_token"),
            "expected lexical match, got: {out}"
        );
        assert!(
            !root.join("embeddings.json").exists(),
            "dense-disabled path must not persist embeddings.json"
        );
    }
}
