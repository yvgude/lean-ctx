use std::io::Write;
use std::time::Instant;

use lean_ctx::core::context_radar::{ContextRadar, RadarEvent, default_window_for_client};

fn make_event(event_type: &str, tokens: usize, tool_name: Option<&str>) -> RadarEvent {
    RadarEvent {
        ts: 1700000000,
        event_type: event_type.to_string(),
        tokens,
        tool_name: tool_name.map(String::from),
        detail: None,
        content: None,
        model: None,
        conversation_id: None,
    }
}

fn write_jsonl(dir: &std::path::Path, events: &[RadarEvent]) {
    let path = dir.join("context_radar.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    for ev in events {
        let line = serde_json::to_string(ev).unwrap();
        writeln!(f, "{line}").unwrap();
    }
}

// ---------------------------------------------------------------------------
// Performance: load + budget_breakdown with increasing event counts
// ---------------------------------------------------------------------------

#[test]
fn perf_radar_load_100_events() {
    let dir = tempfile::tempdir().unwrap();
    let events: Vec<RadarEvent> = (0..100)
        .map(|i| make_event("mcp_call", 50 + i % 200, Some("ctx_read")))
        .collect();
    write_jsonl(dir.path(), &events);

    let start = Instant::now();
    let radar = ContextRadar::load(dir.path(), 200_000);
    let elapsed = start.elapsed();

    assert_eq!(radar.events.len(), 100);
    let b = radar.budget_breakdown();
    assert!(b.lean_ctx_tool_tokens > 0);
    assert!(elapsed.as_millis() < 100, "100 events took {elapsed:?}");
}

#[test]
fn perf_radar_load_10k_events() {
    let dir = tempfile::tempdir().unwrap();
    let events: Vec<RadarEvent> = (0..10_000)
        .map(|i| {
            let types = [
                "user_message",
                "agent_response",
                "mcp_call",
                "shell",
                "native_tool",
            ];
            make_event(types[i % types.len()], 100 + i % 500, None)
        })
        .collect();
    write_jsonl(dir.path(), &events);

    let start = Instant::now();
    let radar = ContextRadar::load(dir.path(), 200_000);
    let elapsed = start.elapsed();

    assert_eq!(radar.events.len(), 10_000);
    let b = radar.budget_breakdown();
    assert!(b.tracked_total > 0);
    assert!(
        elapsed.as_millis() < 500,
        "10k events took {elapsed:?} — should be <500ms"
    );
}

#[test]
fn perf_radar_load_50k_events() {
    let dir = tempfile::tempdir().unwrap();
    let events: Vec<RadarEvent> = (0..50_000)
        .map(|i| make_event("agent_response", 200 + i % 300, None))
        .collect();
    write_jsonl(dir.path(), &events);

    let start = Instant::now();
    let radar = ContextRadar::load(dir.path(), 200_000);
    let elapsed = start.elapsed();

    assert_eq!(radar.events.len(), 50_000);
    assert!(
        elapsed.as_millis() < 2000,
        "50k events took {elapsed:?} — should be <2s"
    );
}

// ---------------------------------------------------------------------------
// Budget breakdown correctness: mixed event types
// ---------------------------------------------------------------------------

#[test]
fn breakdown_mixed_event_types() {
    let mut radar = ContextRadar::new(200_000);
    radar.events = vec![
        make_event("user_message", 100, None),
        make_event("compaction", 0, None),
        make_event("compaction", 0, None),
        make_event("user_message", 500, None),
        make_event("agent_response", 2000, None),
        make_event("mcp_call", 300, Some("ctx_read")),
        make_event("mcp_call", 150, Some("other_tool")),
        make_event("shell", 400, None),
        make_event("native_tool", 250, None),
        make_event("thinking", 1000, None),
    ];

    let b = radar.budget_breakdown();
    assert_eq!(
        b.user_message_tokens, 500,
        "current window: only after last compaction"
    );
    assert_eq!(b.agent_response_tokens, 2000);
    assert_eq!(b.lean_ctx_tool_tokens, 300);
    assert_eq!(b.other_mcp_tokens, 150);
    assert_eq!(b.shell_tokens, 400);
    assert_eq!(b.native_read_tokens, 250);
    assert_eq!(b.thinking_tokens, 1000);
    assert_eq!(b.compaction_count, 2);
    assert_eq!(b.tracked_total, 500 + 2000 + 300 + 150 + 400 + 250);
    assert_eq!(b.available, 200_000 - b.tracked_total);
    assert_eq!(
        b.session_user_tokens, 600,
        "session total includes pre-compaction"
    );
}

#[test]
fn breakdown_lean_ctx_detection_by_detail() {
    let mut radar = ContextRadar::new(200_000);
    radar.events.push(RadarEvent {
        ts: 1000,
        event_type: "mcp_call".to_string(),
        tokens: 500,
        tool_name: Some("some_tool".to_string()),
        detail: Some("lean-ctx server".to_string()),
        content: None,
        model: None,
        conversation_id: None,
    });
    let b = radar.budget_breakdown();
    assert_eq!(
        b.lean_ctx_tool_tokens, 500,
        "detail containing 'lean-ctx' → lean_ctx bucket"
    );
    assert_eq!(b.other_mcp_tokens, 0);
}

#[test]
fn breakdown_lean_ctx_detection_by_tool_prefix() {
    let mut radar = ContextRadar::new(200_000);
    radar.events.push(RadarEvent {
        ts: 1000,
        event_type: "mcp_call".to_string(),
        tokens: 300,
        tool_name: Some("ctx_search".to_string()),
        detail: None,
        content: None,
        model: None,
        conversation_id: None,
    });
    let b = radar.budget_breakdown();
    assert_eq!(
        b.lean_ctx_tool_tokens, 300,
        "tool_name ctx_* → lean_ctx bucket"
    );
}

// ---------------------------------------------------------------------------
// format_display output sanity
// ---------------------------------------------------------------------------

#[test]
fn format_display_includes_all_categories() {
    let mut radar = ContextRadar::new(200_000);
    radar.events = vec![
        make_event("user_message", 1000, None),
        make_event("agent_response", 5000, None),
        make_event("shell", 200, None),
    ];
    let display = radar.format_display();
    assert!(display.contains("CONTEXT RADAR"));
    assert!(display.contains("User Messages"));
    assert!(display.contains("Agent Responses"));
    assert!(display.contains("Shell Output"));
    assert!(display.contains("TRACKED"));
    assert!(display.contains("Available"));
}

// ---------------------------------------------------------------------------
// Default window sizes for all supported IDEs
// ---------------------------------------------------------------------------

#[test]
fn default_window_all_ides() {
    // If a detected model file exists on the system, default_window_for_client
    // returns that model's window for all clients regardless of the client name.
    if lean_ctx::hook_handlers::load_detected_model().is_some() {
        let w = default_window_for_client("cursor");
        assert!(
            (128_000..=2_000_000).contains(&w),
            "window {w} out of range"
        );
        return;
    }
    assert_eq!(default_window_for_client("cursor"), 200_000);
    assert_eq!(default_window_for_client("claude-code"), 200_000);
    assert_eq!(default_window_for_client("claude"), 200_000);
    assert_eq!(default_window_for_client("codex"), 200_000);
    assert_eq!(default_window_for_client("gemini"), 1_000_000);
    assert_eq!(default_window_for_client("windsurf"), 128_000);
    assert_eq!(default_window_for_client("copilot"), 128_000);
    assert_eq!(default_window_for_client("zed"), 128_000);
    assert_eq!(default_window_for_client("unknown"), 200_000);
}

// ---------------------------------------------------------------------------
// Proxy introspection: all three providers
// ---------------------------------------------------------------------------

#[test]
fn introspect_anthropic_large_request() {
    use lean_ctx::proxy::introspect::{Provider, analyze_request};

    let system = "a]".repeat(10_000);
    let user_text = "b".repeat(20_000);
    let assistant_text = "c".repeat(8_000);
    let tools: Vec<serde_json::Value> = (0..60)
        .map(|i| {
            serde_json::json!({
                "name": format!("tool_{i}"),
                "description": format!("This is tool number {i} with a medium-length description for testing."),
                "input_schema": { "type": "object", "properties": { "arg": { "type": "string" } } }
            })
        })
        .collect();

    let body = serde_json::json!({
        "model": "claude-sonnet-4-20250514",
        "system": system,
        "messages": [
            {"role": "user", "content": user_text},
            {"role": "assistant", "content": assistant_text}
        ],
        "tools": tools,
    });

    let start = Instant::now();
    let b = analyze_request(&body, Provider::Anthropic);
    let elapsed = start.elapsed();

    assert!(
        b.system_prompt_tokens >= 2000,
        "system={}",
        b.system_prompt_tokens
    );
    assert!(
        b.user_message_tokens >= 4000,
        "user={}",
        b.user_message_tokens
    );
    assert!(
        b.assistant_message_tokens >= 1500,
        "assistant={}",
        b.assistant_message_tokens
    );
    assert_eq!(b.tool_definition_count, 60);
    assert!(b.tool_definition_tokens > 0);
    assert!(b.total_input_tokens > 7000);
    assert!(elapsed.as_millis() < 50, "introspect took {elapsed:?}");
}

#[test]
fn introspect_openai_with_tool_results() {
    use lean_ctx::proxy::introspect::{Provider, analyze_request};

    let body = serde_json::json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "You are an assistant that uses tools."},
            {"role": "user", "content": "Read the file src/main.rs"},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"src/main.rs\"}"}}
            ]},
            {"role": "tool", "content": "fn main() { println!(\"Hello, world!\"); }", "tool_call_id": "call_1"},
            {"role": "assistant", "content": "The file contains a simple Hello World program."}
        ]
    });

    let b = analyze_request(&body, Provider::OpenAi);
    assert!(b.system_prompt_tokens > 0);
    assert!(b.user_message_tokens > 0);
    assert!(b.tool_result_tokens > 0);
    assert!(b.assistant_message_tokens > 0);
    assert_eq!(b.message_count, 5);
}

#[test]
fn introspect_gemini_with_function_response() {
    use lean_ctx::proxy::introspect::{Provider, analyze_request};

    let body = serde_json::json!({
        "systemInstruction": {
            "parts": [{"text": "You are a coding assistant with deep knowledge of Rust programming language."}]
        },
        "contents": [
            {"role": "user", "parts": [{"text": "Search for functions that handle authentication in the codebase."}]},
            {"role": "model", "parts": [{"functionCall": {"name": "search", "args": {"query": "fn auth"}}}]},
            {"role": "user", "parts": [{"functionResponse": {"name": "search", "response": {"results": "fn authenticate() {} fn authorize() {}"}}}]},
            {"role": "model", "parts": [{"text": "I found two authentication-related functions in the codebase: authenticate() and authorize()."}]}
        ],
        "tools": [{"functionDeclarations": [
            {"name": "search", "description": "Search the codebase", "parameters": {"type": "object"}},
            {"name": "read", "description": "Read a file", "parameters": {"type": "object"}}
        ]}]
    });

    let b = analyze_request(&body, Provider::Gemini);
    assert!(
        b.system_prompt_tokens > 0,
        "system={}",
        b.system_prompt_tokens
    );
    assert!(b.user_message_tokens > 0, "user={}", b.user_message_tokens);
    assert!(
        b.tool_result_tokens > 0,
        "tool_result={}",
        b.tool_result_tokens
    );
    assert!(
        b.assistant_message_tokens > 0,
        "assistant={}",
        b.assistant_message_tokens
    );
    assert_eq!(b.tool_definition_count, 2);
    assert_eq!(b.message_count, 4);
}

// ---------------------------------------------------------------------------
// IntrospectState thread safety
// ---------------------------------------------------------------------------

#[test]
fn introspect_state_concurrent_recording() {
    use lean_ctx::proxy::introspect::{IntrospectState, Provider, analyze_request};
    use std::sync::Arc;

    let state = Arc::new(IntrospectState::default());
    let mut handles = vec![];

    for i in 0..10 {
        let s = Arc::clone(&state);
        handles.push(std::thread::spawn(move || {
            let body = serde_json::json!({
                "model": format!("model-{i}"),
                "system": format!("System prompt number {i} for testing concurrent access."),
                "messages": [{"role": "user", "content": format!("Message {i}")}]
            });
            let b = analyze_request(&body, Provider::Anthropic);
            s.record(b);
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        state
            .total_requests
            .load(std::sync::atomic::Ordering::Relaxed),
        10,
    );
    assert!(
        state
            .total_system_prompt_tokens
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0,
    );
    assert!(state.last_breakdown.lock().unwrap().is_some());
}

// ---------------------------------------------------------------------------
// End-to-end: JSONL write → load → breakdown pipeline
// ---------------------------------------------------------------------------

#[test]
fn e2e_jsonl_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let radar_path = dir.path().join("context_radar.jsonl");

    let events = vec![
        make_event("user_message", 999, None),
        make_event("compaction", 0, None),
        make_event("user_message", 100, None),
        make_event("mcp_call", 50, Some("ctx_read")),
        make_event("mcp_call", 75, Some("oplane")),
        make_event("shell", 200, None),
        make_event("agent_response", 1500, None),
    ];

    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&radar_path)
            .unwrap();
        for ev in &events {
            let line = serde_json::to_string(ev).unwrap();
            writeln!(f, "{line}").unwrap();
        }
    }

    let radar = ContextRadar::load(dir.path(), 128_000);
    assert_eq!(radar.events.len(), 7);

    let b = radar.budget_breakdown();
    assert_eq!(
        b.user_message_tokens, 100,
        "current window after compaction"
    );
    assert_eq!(b.lean_ctx_tool_tokens, 50);
    assert_eq!(b.other_mcp_tokens, 75);
    assert_eq!(b.shell_tokens, 200);
    assert_eq!(b.agent_response_tokens, 1500);
    assert_eq!(b.compaction_count, 1);
    let event_total = 100 + 50 + 75 + 200 + 1500;
    assert_eq!(
        b.tracked_total,
        event_total + b.system_prompt_tokens,
        "tracked_total = current window events + rules tokens"
    );
    assert_eq!(b.window_size, 128_000);
    assert_eq!(
        b.session_user_tokens, 1099,
        "session includes pre-compaction"
    );
}

// ---------------------------------------------------------------------------
// Performance: budget_breakdown with many events
// ---------------------------------------------------------------------------

#[test]
fn perf_budget_breakdown_100k_events() {
    let mut radar = ContextRadar::new(1_000_000);
    radar.events = (0..100_000)
        .map(|i| {
            let types = [
                "user_message",
                "agent_response",
                "mcp_call",
                "shell",
                "native_tool",
                "thinking",
            ];
            RadarEvent {
                ts: 1700000000 + i as u64,
                event_type: types[i % types.len()].to_string(),
                tokens: 50 + i % 500,
                tool_name: if i % 3 == 0 {
                    Some("ctx_read".to_string())
                } else {
                    None
                },
                detail: None,
                content: None,
                model: None,
                conversation_id: None,
            }
        })
        .collect();

    let start = Instant::now();
    let b = radar.budget_breakdown();
    let elapsed = start.elapsed();

    assert!(b.tracked_total > 0);
    assert!(
        elapsed.as_millis() < 50,
        "budget_breakdown on 100k events took {elapsed:?}"
    );
}
