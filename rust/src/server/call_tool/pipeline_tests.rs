// SPDX-License-Identifier: Apache-2.0

use super::{
    apply_task_triage_filter, background_cursor_token, background_status_delta,
    compression_tracker_tokens, triage_bypass_requested,
};

#[test]
fn background_status_delta_is_utf8_safe_and_request_scoped() {
    let full = "aé🙂z";
    let prefix_cursor = background_cursor_token("job-a", "aé");
    let full_cursor = background_cursor_token("job-a", full);
    assert_eq!(
        background_status_delta("job-a", full, Some(&prefix_cursor), true),
        ("🙂z".to_string(), full_cursor.clone(), false)
    );
    assert_eq!(
        background_status_delta("job-a", full, Some(&full_cursor), true),
        (String::new(), full_cursor.clone(), false)
    );
    assert_eq!(
        background_status_delta("job-b", full, Some(&prefix_cursor), true),
        (
            full.to_string(),
            background_cursor_token("job-b", full),
            true
        )
    );
    assert_eq!(
        background_status_delta("job-a", "ax🙂z", Some(&prefix_cursor), true),
        (
            "ax🙂z".to_string(),
            background_cursor_token("job-a", "ax🙂z"),
            true
        )
    );
    assert_eq!(
        background_status_delta("job-a", full, Some(&prefix_cursor), false),
        (full.to_string(), full_cursor, false)
    );
}

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
