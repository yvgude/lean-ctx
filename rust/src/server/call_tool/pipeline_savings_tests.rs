//! Savings/triage/budget unit tests for `pipeline.rs`.
//!
//! Split out to keep `pipeline.rs` under the 1500-line LOC gate; the module
//! stays a child of `pipeline` via `#[path]`, so `use super::…` still resolves
//! to the pipeline internals it exercises.

use super::{
    apply_task_triage_filter, compression_tracker_tokens, triage_bypass_requested,
    verbatim_requested,
};

#[test]
fn test_tracker_in_pipeline() {
    let mut tracker = crate::core::savings_tracker::SessionSavingsTracker::default();
    let (raw, compressed) = compression_tracker_tokens("ctx_read", 75, 25).expect("tracked");
    tracker.record_compression(raw, compressed, "ctx_read");
    let after = tracker.session_summary();

    assert_eq!(
        (
            after.total_raw,
            after.total_compressed,
            after.savings_tokens
        ),
        (100, 25, 75)
    );
}

#[test]
fn triage_filter_rewrites_raw_output_and_tracks_removed_lines() {
    let profile = crate::core::triage::profile::TaskProfileLocal {
        confidence_milli: 500,
        context_need_milli: 400,
        ..Default::default()
    };
    let mut context = Some(crate::core::decision_loop_runtime::TaskContext {
        task_id: String::new(),
        session_id: String::new(),
        triage_class: String::new(),
        profile_intent: String::new(),
        profile_complexity: String::new(),
        filtered_lines: 0,
        start_time: std::time::Instant::now(),
    });
    let raw = format!("// boilerplate\n{}", "content\n".repeat(100));

    let filtered = apply_task_triage_filter(raw, Some(&profile), &mut context, 2);

    assert!(!filtered.starts_with("// boilerplate"));
    assert_eq!(context.as_ref().unwrap().filtered_lines, 1);
}

#[test]
fn triage_filter_fails_open_without_a_profile() {
    let raw = "content\n".repeat(100);
    let mut context = None;

    assert_eq!(
        apply_task_triage_filter(raw.clone(), None, &mut context, 2),
        raw
    );
}

#[test]
fn triage_filter_cap_zero_preserves_output_unchanged() {
    let profile = crate::core::triage::profile::TaskProfileLocal {
        confidence_milli: 500,
        context_need_milli: 200,
        ..Default::default()
    };
    let raw = format!("fn render() {{\n{}\n}}", "    token_value();\n".repeat(40));
    let mut context = None;

    assert_eq!(
        apply_task_triage_filter(raw.clone(), Some(&profile), &mut context, 0),
        raw
    );
}

#[test]
fn ctx_read_always_bypasses_second_lossy_filter() {
    let auto = serde_json::Map::from_iter([(
        "mode".to_owned(),
        serde_json::Value::String("auto".to_owned()),
    )]);
    assert!(triage_bypass_requested("ctx_read", Some(&auto)));

    let shell = serde_json::Map::new();
    assert!(!triage_bypass_requested("ctx_shell", Some(&shell)));
}

fn args(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[test]
fn raw_true_and_mode_raw_are_explicit_verbatim_requests() {
    let raw_flag = args(&[("raw", serde_json::Value::Bool(true))]);
    let raw_mode = args(&[("mode", serde_json::Value::String("raw".to_owned()))]);
    assert!(verbatim_requested("ctx_read", Some(&raw_flag)));
    assert!(verbatim_requested("ctx_read", Some(&raw_mode)));
    assert!(verbatim_requested("ctx_shell", Some(&raw_flag)));
}

#[test]
fn ordinary_reads_stay_on_the_ordinary_budget() {
    // #1582 raises the cap for an explicit escape hatch, not for the
    // everyday modes — otherwise the backstop is off for most traffic.
    for mode in ["full", "auto", "lines:1-40", "anchored", "signatures"] {
        let a = args(&[("mode", serde_json::Value::String(mode.to_owned()))]);
        assert!(
            !verbatim_requested("ctx_read", Some(&a)),
            "mode={mode} must not claim the verbatim budget"
        );
    }
    let off = args(&[("raw", serde_json::Value::Bool(false))]);
    assert!(!verbatim_requested("ctx_read", Some(&off)));
    assert!(!verbatim_requested("ctx_read", None));
}

#[test]
fn other_tools_never_claim_the_verbatim_budget() {
    let raw_flag = args(&[("raw", serde_json::Value::Bool(true))]);
    assert!(!verbatim_requested("ctx_search", Some(&raw_flag)));
    assert!(!verbatim_requested("ctx_compose", Some(&raw_flag)));
}

/// #1812: `inline` is the same request as `raw` — verbatim command output.
/// Held to the smaller backstop it was truncated at ~4k tokens while the
/// identical command with `raw=true` returned in full, and because the
/// archive line only exists on the compressed path, the cut response had no
/// recovery route at all.
#[test]
fn inline_earns_the_verbatim_budget_like_raw() {
    let inline = args(&[("inline", serde_json::Value::Bool(true))]);
    assert!(verbatim_requested("ctx_shell", Some(&inline)));

    let raw = args(&[("raw", serde_json::Value::Bool(true))]);
    assert_eq!(
        verbatim_requested("ctx_shell", Some(&inline)),
        verbatim_requested("ctx_shell", Some(&raw)),
        "inline and raw request the same thing and must be budgeted alike"
    );
}

#[test]
fn inline_false_stays_on_the_ordinary_budget() {
    let off = args(&[("inline", serde_json::Value::Bool(false))]);
    assert!(!verbatim_requested("ctx_shell", Some(&off)));
    let none = args(&[]);
    assert!(!verbatim_requested("ctx_shell", Some(&none)));
}
