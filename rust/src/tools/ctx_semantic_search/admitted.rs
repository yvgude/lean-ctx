// SPDX-License-Identifier: Apache-2.0
//! Fresh original-source admission for local lexical retrieval. Legacy indexes
//! are discovery/performance artifacts, never authorization evidence.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::core::bm25_index::BM25Index;

pub(super) const UNSUPPORTED: &str = "ERR: this search mode is withheld under content policy until its original-source provenance is verified; use local mode=bm25";

/// Wall-clock budget of one fresh source scan (#2063). A scan that cannot
/// finish in it ends with an error that says what to do, instead of holding
/// the call until the client gives up.
const SCAN_BUDGET: Duration = Duration::from_secs(90);
/// A single source that takes this long is named under `LEAN_CTX_PHASE_TIMING=1`.
const SLOW_SOURCE: Duration = Duration::from_millis(500);

const SCAN_PHASE: &str = "semantic: source scan";
const WALK_PHASE: &str = "semantic: source scan (listing files)";
const ADMIT_PHASE: &str = "semantic: source scan (reading and admitting files)";
const EXTERNAL_PHASE: &str = "semantic: source scan (persisted external items)";

pub(super) fn fresh_index(
    root: &Path,
    filter: &super::SearchFilter,
    tool: &str,
) -> Result<BM25Index, String> {
    fresh_index_within(root, filter, tool, Instant::now() + SCAN_BUDGET)
}

/// `fresh_index` that gives up once `deadline` has passed.
pub(super) fn fresh_index_within(
    root: &Path,
    filter: &super::SearchFilter,
    tool: &str,
    deadline: Instant,
) -> Result<BM25Index, String> {
    let _scan_phase = crate::core::phase_timing::enter(SCAN_PHASE);
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
    // #2063: one gateway snapshot for the whole scan, as every derived-store
    // builder takes one. Resolving the configuration again for each source
    // re-read the global and project config files once per file; a scan
    // never mixes two policies this way either.
    let gateway = crate::core::context_admission::carrier::gateway_snapshot();
    // The scope narrows the walk itself, so the file budget counts only the
    // sources the caller asked about.
    let in_scope = |relative: &str| filter.matches(relative);
    let mut walk_phase = Some(crate::core::phase_timing::enter(WALK_PHASE));
    let mut admit_phase = None;
    let built =
        BM25Index::build_from_admitted_sources(&root, filter.subdir(), &in_scope, |relative| {
            if walk_phase.take().is_some() {
                admit_phase = Some(crate::core::phase_timing::enter(ADMIT_PHASE));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "source scan did not finish within {}s; narrow it with path or path_glob, \
                     or use mode=\"dense\" or mode=\"hybrid\", which read the built index",
                    SCAN_BUDGET.as_secs()
                ));
            }
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
            let started = Instant::now();
            let content = crate::tools::ctx_read::read_file_for_tool_rooted_admitted_with(
                &root.join(relative).to_string_lossy(),
                &root.to_string_lossy(),
                tool,
                &mut allowance,
                Some(&gateway),
            );
            let spent = started.elapsed();
            if spent >= SLOW_SOURCE {
                crate::core::phase_timing::note_slow(ADMIT_PHASE, relative, spent);
            }
            remaining = remaining.saturating_sub(initial.saturating_sub(allowance));
            // Denied, unreadable and non-text sources contribute neither documents
            // nor term frequencies, snippets or names to the retrieval view.
            Ok(content.ok().map(|read| read.content))
        })?;
    drop(admit_phase);
    drop(walk_phase);
    // Provider items and other external chunks are no files the walk above
    // reads: they join an unscoped view from the persisted index, each only
    // when its source serves it to this caller now (`source_gate`).
    if filter.is_active() {
        return Ok(built);
    }
    let _external_phase = crate::core::phase_timing::enter(EXTERNAL_PHASE);
    Ok(match BM25Index::load(&root) {
        Some(stored) => built.with_served_external_from(&stored, &root),
        None => built,
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

    /// #2063: `ctx_search` with `action=semantic` and no mode (the fresh
    /// source scan) hung on Windows while `semantic-search -m dense` answered
    /// in seconds. The tool path answers a small project with a built index
    /// within a bound, and names the scan, not the built index, as its phase.
    #[test]
    fn tool_path_semantic_search_answers_a_small_project_within_a_bound() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = TestPolicyOverride::set(None);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for (name, body) in [
            (
                "slew.rs",
                "fn check_slew_target_limit() { let slew_limit = 90; }\n",
            ),
            ("mount.rs", "fn park_mount() { let parked = true; }\n"),
            ("focus.rs", "fn move_focuser() { let steps = 12; }\n"),
        ] {
            std::fs::write(root.join(name), body).unwrap();
        }
        BM25Index::build_from_directory(&root).save(&root).unwrap();
        let path = root.to_string_lossy().into_owned();
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let out = super::super::handle_for_tool(
                "ctx_search",
                "slew target limit",
                &path,
                3,
                crate::tools::CrpMode::Off,
                None,
                None,
                None,
                None,
                None,
            );
            let _ = sent.send(out);
        });
        let out = received
            .recv_timeout(Duration::from_secs(60))
            .expect("ctx_search semantic did not answer within 60s");
        assert!(out.contains("slew.rs"), "{out}");
        assert!(!out.starts_with("ERR:"), "{out}");
    }

    /// #2063: a scan that runs out of its wall-clock budget ends with an
    /// error that says what to do, instead of holding the call open.
    #[test]
    fn a_scan_past_its_deadline_stops_with_a_clear_error() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = TestPolicyOverride::set(None);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("lib.rs"), "fn deadline_canary() {}\n").unwrap();
        let filter = super::super::SearchFilter::new(None, None).unwrap();
        let error = with_project_source_view(&root.to_string_lossy(), || {
            fresh_index_within(root, &filter, "ctx_search", Instant::now())
        })
        .unwrap()
        .unwrap_err();
        assert!(error.contains("did not finish within"), "{error}");
        assert!(error.contains("path_glob"), "{error}");
    }

    #[test]
    fn filtered_retrieval_keeps_source_filtering_before_ranking() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = TestPolicyOverride::set(None);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("keep.rs"), "fn keep_canary_91() {}\n").unwrap();
        std::fs::write(root.join("skip.rs"), "fn skip_canary_92() {}\n").unwrap();
        let filter = super::super::SearchFilter::new(None, Some("keep.rs")).unwrap();
        let index = with_project_source_view(&root.to_string_lossy(), || {
            fresh_index(root, &filter, "ctx_search")
        })
        .unwrap()
        .unwrap();
        assert!(!index.search("keep_canary_91", 10).is_empty());
        assert!(index.search("skip_canary_92", 10).is_empty());
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

    /// K02/K10: a provider item in the persisted index joins the search view
    /// while its source still returns it to this caller, and once the source
    /// refuses it, it adds nothing — no hit, no document, no term statistic.
    /// Before, the view held local files only and provider items were never
    /// searchable at all.
    #[test]
    fn a_provider_item_joins_the_view_only_while_its_source_serves_it() {
        use crate::core::providers::config_provider::{ConfigProvider, schema::ProviderConfig};
        use crate::core::providers::hardened_http::redirect_tests::Server;
        use crate::core::providers::provenance::BoundResult;
        use crate::core::providers::provider_trait::ProviderParams;

        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = TestPolicyOverride::set(Some(policy("")));
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(root.join("lib.rs"), "fn local_source() {}\n").unwrap();
        // Project-local providers load only for a trusted workspace.
        crate::core::workspace_trust::trust(&root).unwrap();
        let configs = root.join(".lean-ctx/providers");
        std::fs::create_dir_all(&configs).unwrap();
        let server = Server::new(
            200,
            None,
            r#"[{"id":"1","title":"Zirconwhale outage","body":"zirconwhale login fails"}]"#,
        );
        let config_text = format!(
            "id='view-fixture'\nname='Fixture'\nbase_url='{}'\n[auth]\ntype='none'\n\
             [resources.issues]\nmethod='GET'\npath='/items'\n\
             [resources.issues.response.mapping]\nid='id'\ntitle='title'\nbody='body'\n",
            server.url
        );
        std::fs::write(configs.join("fixture.toml"), &config_text).unwrap();
        let config: ProviderConfig = toml::from_str(&config_text).unwrap();
        let provider = ConfigProvider::from_config(config).unwrap();
        let acquired =
            BoundResult::execute(&provider, "issues", &ProviderParams::default()).unwrap();
        crate::core::consolidation::apply_artifacts_to_stores(
            &crate::core::consolidation::consolidate(&acquired.chunks(&acquired.result)),
            &root.to_string_lossy(),
            &crate::core::consolidation::PrunePrior::default(),
        );

        let served = view(&root);
        assert!(
            !served.search("zirconwhale", 10).is_empty(),
            "served to a reader"
        );
        server.respond(403, "denied");
        let refused = view(&root);
        assert!(refused.search("zirconwhale", 10).is_empty());
        assert!(
            !refused
                .doc_freqs
                .keys()
                .any(|term| term.contains("zirconwhale"))
        );
        assert!(
            refused
                .chunks
                .iter()
                .all(|chunk| !chunk.file_path.contains("://"))
        );
        assert!(!refused.search("local_source", 10).is_empty());
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
