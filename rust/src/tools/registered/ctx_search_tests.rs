// SPDX-License-Identifier: Apache-2.0
use super::{
    SearchAction, ToolContext, ext_to_include, handle_symbol, normalize_query_keys,
    resolve_query_cap,
};
use serde_json::{Map, Value, json};

fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .cloned()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

#[test]
fn explicit_action_selects_engine() {
    // #509: an explicit action always wins, including synonyms.
    assert_eq!(
        SearchAction::resolve(&args(&[("action", json!("semantic"))])),
        SearchAction::Semantic
    );
    assert_eq!(
        SearchAction::resolve(&args(&[("action", json!("symbol"))])),
        SearchAction::Symbol
    );
    assert_eq!(
        SearchAction::resolve(&args(&[("action", json!("grep"))])),
        SearchAction::Regex
    );
    assert_eq!(
        SearchAction::resolve(&args(&[("action", json!("related"))])),
        SearchAction::FindRelated
    );
    assert_eq!(
        SearchAction::resolve(&args(&[("action", json!("reindex"))])),
        SearchAction::MovedReindex
    );
}

/// #1624: `ctx_search` was unusable in read-only client modes (Devin Plan
/// mode, Cursor's restricted contexts) because MCP annotations are per
/// *tool*, and the single mutating action `reindex` withheld
/// `readOnlyHint` from every search call. The rebuild belongs to
/// `ctx_index`, which already owns it and advertises itself as mutating.
#[test]
fn search_is_annotated_read_only_so_plan_mode_can_call_it() {
    use crate::server::tool_trait::McpTool;
    let defs = crate::tool_defs::apply_tool_annotations(vec![super::CtxSearchTool.tool_def()]);
    let annotations = defs[0]
        .annotations
        .as_ref()
        .expect("ctx_search must carry annotations at all");
    assert_eq!(
        annotations.read_only_hint,
        Some(true),
        "without readOnlyHint a read-only client mode refuses the call before dispatch"
    );
    assert_ne!(
        annotations.destructive_hint,
        Some(true),
        "search destroys nothing"
    );
}

/// The published action list must not advertise what the tool no longer
/// does — an enum still naming `reindex` would send agents into an error.
#[test]
fn reindex_is_gone_from_the_published_action_list() {
    use crate::server::tool_trait::McpTool;
    let def = super::CtxSearchTool.tool_def();
    let schema = serde_json::to_value(&*def.input_schema).expect("schema");
    let actions = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action enum")
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    assert!(
        !actions.contains(&"reindex"),
        "reindex must not be offered by a read-only tool: {actions:?}"
    );
    assert!(
        !def.description
            .as_deref()
            .unwrap_or_default()
            .contains("reindex"),
        "the description must not advertise it either"
    );
    for kept in ["regex", "semantic", "symbol", "find_related"] {
        assert!(
            actions.contains(&kept),
            "the read-only actions must all survive: {actions:?}"
        );
    }
}

/// A call that still asks for `reindex` gets the replacement, not a bare
/// rejection: agents that learned the old spelling must be able to recover
/// from the error text alone.
#[test]
fn a_reindex_call_is_answered_with_the_replacement_command() {
    let error = super::moved_reindex_error();
    assert!(
        error.message.contains("ctx_index(action=\"build-full\")"),
        "the error must carry the exact replacement call: {}",
        error.message
    );
    assert!(
        error.message.contains("read-only"),
        "and say why it moved, so the change does not look arbitrary: {}",
        error.message
    );
}

#[test]
fn action_inferred_from_fields_for_backward_compat() {
    // Pre-#509 call sites set only one of these fields and no action.
    assert_eq!(
        SearchAction::resolve(&args(&[("pattern", json!("fn .*"))])),
        SearchAction::Regex
    );
    assert_eq!(
        SearchAction::resolve(&args(&[("query", json!("user auth"))])),
        SearchAction::Semantic
    );
    assert_eq!(
        SearchAction::resolve(&args(&[("name", json!("handle"))])),
        SearchAction::Symbol
    );
    assert_eq!(
        SearchAction::resolve(&args(&[("file_path", json!("a.rs")), ("line", json!(10))])),
        SearchAction::FindRelated
    );
}

#[test]
fn handle_infers_symbol_action() {
    // A bare `handle` (no action) must route to the symbol engine.
    assert_eq!(
        SearchAction::resolve(&args(&[("handle", json!("src/lib.rs#Config::load@L22"))])),
        SearchAction::Symbol
    );
}

#[test]
fn pattern_wins_over_query_and_unknown_action_falls_back_to_inference() {
    // A regex caller that also carries a stray query must stay regex.
    assert_eq!(
        SearchAction::resolve(&args(&[("pattern", json!("x")), ("query", json!("y"))])),
        SearchAction::Regex
    );
    // Unknown action value → infer from fields (here: symbol).
    assert_eq!(
        SearchAction::resolve(&args(&[("action", json!("bogus")), ("name", json!("f"))])),
        SearchAction::Symbol
    );
    // Nothing recognizable → default regex (the empty-call default).
    assert_eq!(SearchAction::resolve(&args(&[])), SearchAction::Regex);
}

#[test]
fn symbol_action_takes_the_name_from_symbol_query_or_pattern() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "pub fn load_config() {}\n").unwrap();
    let ctx = ToolContext {
        project_root: dir.path().to_string_lossy().into_owned(),
        ..ToolContext::default()
    };
    for key in ["symbol", "query", "pattern"] {
        let call = args(&[("action", json!("symbol")), (key, json!("load_config"))]);
        if let Err(error) = handle_symbol(&call, &ctx) {
            assert!(
                !error.message.contains("name or handle is required"),
                "`{key}` must name the symbol: {}",
                error.message
            );
        }
    }
    let Err(error) = handle_symbol(&args(&[("action", json!("symbol"))]), &ctx) else {
        panic!("no name at all must still be rejected");
    };
    assert!(error.message.contains("name or handle is required"));
}

#[test]
fn ext_alias_bare_extension_becomes_glob() {
    assert_eq!(ext_to_include("rs"), "*.rs");
    assert_eq!(ext_to_include("ts"), "*.ts");
}

#[test]
fn ext_alias_strips_leading_dot() {
    assert_eq!(ext_to_include(".rs"), "*.rs");
    assert_eq!(ext_to_include(".tsx"), "*.tsx");
}

#[test]
fn ext_alias_passes_through_glob_like_values() {
    // Already a glob/path → keep verbatim, don't double-wrap.
    assert_eq!(ext_to_include("*.rs"), "*.rs");
    assert_eq!(ext_to_include("*.{rs,ts}"), "*.{rs,ts}");
    assert_eq!(ext_to_include("src/**/*.tsx"), "src/**/*.tsx");
}

#[test]
fn lenient_fallback_uses_unknown_string_key_as_pattern() {
    use super::{KNOWN_KEYS, get_str};

    // Simulate Gemma sending {"search_term": "fn main"} — an unknown key
    // with a string value should be picked up by the lenient fallback.
    let a = args(&[("search_term", json!("fn main"))]);
    let pattern = get_str(&a, "pattern").or_else(|| {
        a.iter()
            .find(|(k, v)| !KNOWN_KEYS.contains(&k.as_str()) && v.is_string())
            .and_then(|(_, v)| v.as_str().map(String::from))
    });
    assert_eq!(pattern, Some("fn main".to_string()));
}

#[test]
fn lenient_fallback_does_not_grab_known_keys() {
    use super::{KNOWN_KEYS, get_str};

    // If only known keys are present (but pattern is missing), fallback
    // should NOT pick them up — it returns None.
    let a = args(&[("path", json!("/src")), ("max_results", json!(10))]);
    let pattern = get_str(&a, "pattern").or_else(|| {
        a.iter()
            .find(|(k, v)| !KNOWN_KEYS.contains(&k.as_str()) && v.is_string())
            .and_then(|(_, v)| v.as_str().map(String::from))
    });
    assert_eq!(pattern, None);
}

/// #1625: a `max_results` inside a query object was read by nothing. The
/// caller saw exactly half the default budget per query and no indication
/// that the value they had written was ignored.
#[test]
fn per_query_max_results_overrides_its_share_of_the_shared_budget() {
    // Two queries under the default budget of 20 → 10 each.
    let share = 10;
    assert_eq!(
        resolve_query_cap(&args(&[("pattern", json!("x"))]), share, 0),
        Ok(share),
        "without its own cap a query keeps its share of the shared budget"
    );
    assert_eq!(
        resolve_query_cap(
            &args(&[("pattern", json!("x")), ("max_results", json!(20))]),
            share,
            0
        ),
        Ok(20),
        "a per-query cap must win over the split"
    );
    assert_eq!(
        resolve_query_cap(
            &args(&[("pattern", json!("x")), ("max_results", json!(9_000))]),
            share,
            0
        ),
        Ok(500),
        "a per-query cap may not escape the 500 ceiling the top level has"
    );
}

#[test]
fn non_positive_per_query_max_results_is_rejected_not_ignored() {
    let error = resolve_query_cap(
        &args(&[("pattern", json!("x")), ("max_results", json!(0))]),
        10,
        1,
    )
    .expect_err("zero is not a usable cap");
    assert!(
        error.contains("queries[2].max_results"),
        "the error must point at the offending entry, 1-based: {error}"
    );
}

/// Agents guess key names; an unambiguous near-miss costs no extra request
/// (focus 2026-10-15), anything else is still named rather than dropped
/// (#1625), since `queries.items` stays untyped in the schema.
#[test]
fn query_keys_canonicalize_near_misses_and_name_the_rest() {
    let ok = normalize_query_keys(
        &args(&[
            ("regex", json!("x")),
            ("maxResults", json!(20)),
            ("path", json!("src")),
        ]),
        0,
    )
    .expect("near-misses and the per-query path are accepted");
    assert_eq!(ok.get("pattern"), Some(&json!("x")));
    assert_eq!(ok.get("max_results"), Some(&json!(20)));
    assert_eq!(ok.get("path"), Some(&json!("src")));
    let both = normalize_query_keys(&args(&[("pattern", json!("a")), ("query", json!("b"))]), 0)
        .expect("alias next to its canonical key");
    assert_eq!(both.get("pattern"), Some(&json!("a")), "canonical key wins");
    let error = normalize_query_keys(
        &args(&[("pattern", json!("x")), ("caseSensitive", json!(true))]),
        1,
    )
    .expect_err("a key with no unambiguous meaning must not be silently dropped");
    assert!(
        error.contains("queries[2]: unknown key 'caseSensitive'"),
        "the offending key must be named: {error}"
    );
    assert!(
        error.contains("max_results"),
        "the accepted set must be listed so the fix is obvious: {error}"
    );
}

/// Other search tools spell the pattern `regex`/`query` and the file
/// filter `glob`; a query entry written that way means the same thing.
#[test]
fn query_entries_accept_other_tools_spellings() {
    for (key, value) in [
        ("regex", json!("x")),
        ("query", json!("x")),
        ("glob", json!("*.rs")),
    ] {
        assert!(
            normalize_query_keys(&args(&[("pattern", json!("x")), (key, value)]), 0).is_ok(),
            "{key} must be accepted"
        );
    }
}

/// The published schema must describe the shared-budget semantics the
/// reporter had to reverse-engineer from the results — and must do it
/// without paying for a fully expanded `queries.items` object.
///
/// Typing every `items` key (`additionalProperties: false` + six property
/// entries) cost ~52 tokens on *every turn of every session* and broke
/// `minimal_arm_per_turn_prefix_stays_within_budget`. Its only benefit is
/// client-side rejection of a rare malformed call, which
/// `normalize_query_keys` already catches at dispatch with a better message
/// (it names the offending key and the accepted set). In a tool whose
/// purpose is context economy, the per-turn cost loses. The one line kept
/// is the shared-budget note, because that is the misconception #1625 was
/// actually made of.
#[test]
fn schema_documents_the_shared_budget_without_paying_for_typed_items() {
    use crate::server::tool_trait::McpTool;
    let def = super::CtxSearchTool.tool_def();
    let schema = serde_json::to_value(&*def.input_schema).expect("schema");
    let top = schema["properties"]["max_results"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(
        top.contains("shared across queries"),
        "the top-level budget must announce that it is split across queries: {top}"
    );
    assert!(
        schema["properties"]["queries"]["items"]["properties"].is_null(),
        "queries.items must stay unexpanded — the per-turn token cost is not \
         worth duplicating a check normalize_query_keys already makes"
    );
}
