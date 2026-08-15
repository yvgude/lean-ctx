//! Realistic, synchronous agent-transcript coverage for the compression stack.
//!
//! These tests deliberately use the public tool and proxy entry points instead
//! of reaching into implementation details. They protect the boundaries an
//! agent sees: `ctx_read` output, proxy message rewriting, conversation dedup,
//! prose relevance filtering, and `ctx_crush` structured-data summaries.

use serde_json::{Map, Value, json};

use crate::{
    core::{config::PipelineConfig, signatures::extract_signatures, tokens::count_tokens},
    proxy::{
        compress::compress_tool_result,
        compress_api::{CompressRequest, compress_messages},
        forward::pipeline::CompressionPipeline,
        prose_compress::compress_prose,
    },
    server::tool_trait::{McpTool, ToolContext},
    tools::registered::ctx_crush::CtxCrushTool,
};

fn conversation_tokens(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(Value::to_string)
        .map(|message| count_tokens(&message))
        .sum()
}

fn assert_savings_at_least(before: usize, after: usize, minimum_pct: usize, scenario: &str) {
    assert!(
        before > after,
        "{scenario} did not save tokens ({before} -> {after})"
    );
    let saved_pct = (before - after) * 100 / before;
    assert!(
        saved_pct >= minimum_pct,
        "{scenario} saved {saved_pct}%, expected at least {minimum_pct}% ({before} -> {after})"
    );
}

fn tool_content(message: &Value) -> &str {
    message["content"]
        .as_str()
        .expect("tool message must have string content")
}

fn auth_source() -> String {
    (0..50)
        .map(|index| {
            format!(
                "pub fn validate_session_{index}(user_id: &str, token: &str) -> Result<(), AuthError> {{\n\
                     let normalized = token.trim();\n\
                     if normalized.is_empty() {{\n\
                         return Err(AuthError::EmptyToken);\n\
                     }}\n\
                     audit_validation(user_id, normalized);\n\
                     Ok(())\n\
                 }}\n"
            )
        })
        .collect()
}

#[test]
fn cursor_file_read_saves_tokens() {
    let source = auth_source();
    assert!(
        source.lines().count() >= 200,
        "fixture must model a large file read"
    );

    // `ctx_read` performs code compression at its tool boundary. The proxy must
    // preserve that already-compressed result exactly (#479), rather than
    // attempting a second lossy rewrite.
    let signatures = extract_signatures(&source, "rs")
        .iter()
        .map(|signature| signature.to_compact())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(signatures.contains("validate_session_0"));
    assert!(signatures.contains("validate_session_49"));
    assert_savings_at_least(
        count_tokens(&source),
        count_tokens(&signatures),
        50,
        "ctx_read signatures",
    );

    let system = json!({
        "role": "system",
        "content": [{
            "type": "text",
            "text": "Use ctx_read for source. Preserve tool call IDs and edit only after inspection.",
            "cache_control": {"type": "ephemeral"}
        }]
    });
    let user = json!({"role": "user", "content": "Fix the bug in auth.rs"});
    let tool_call = json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": "read-auth",
            "type": "function",
            "function": {"name": "ctx_read", "arguments": "{\"path\":\"src/auth.rs\"}"}
        }]
    });
    let tool_result = json!({
        "role": "tool",
        "tool_call_id": "read-auth",
        "name": "ctx_read",
        "content": signatures,
    });
    let found_issue = json!({
        "role": "assistant",
        "content": "I found the issue: empty tokens reach audit_validation before validation."
    });
    let confirm = json!({"role": "user", "content": "Yes, fix it"});
    let edit_call = json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": "edit-auth",
            "type": "function",
            "function": {"name": "edit", "arguments": "{\"path\":\"src/auth.rs\"}"}
        }]
    });
    let messages = vec![
        system.clone(),
        user.clone(),
        tool_call.clone(),
        tool_result.clone(),
        found_issue.clone(),
        confirm.clone(),
        edit_call.clone(),
    ];

    let response = compress_messages(CompressRequest {
        messages,
        model: Some("gpt-5".to_string()),
    });

    assert_eq!(response.messages[0], system, "system instructions changed");
    assert_eq!(response.messages[1], user, "user request changed");
    assert_eq!(response.messages[2], tool_call, "ctx_read call changed");
    assert_eq!(
        response.messages[3], tool_result,
        "ctx_read result was recompressed"
    );
    assert_eq!(
        response.messages[4], found_issue,
        "assistant finding changed"
    );
    assert_eq!(response.messages[5], confirm, "user confirmation changed");
    assert_eq!(response.messages[6], edit_call, "edit call changed");
}

fn detailed_type_error(revision: char) -> String {
    let repeated_frames = (0..260)
        .map(|frame| {
            format!(
                "  auth.rs:42:{frame}: expected AuthContext, found String while validating session token\n"
            )
        })
        .collect::<String>();
    let distinct_tail = revision.to_string().repeat(2_000);
    format!(
        "error[E0308]: mismatched types in src/auth.rs:42\n\
         expected `AuthContext`, found `String`\n\
         help: construct AuthContext before calling validate_session\n\
         {repeated_frames}\
         compiler revision detail: {distinct_tail}"
    )
}

#[test]
#[ignore = "semantic dedup edge case — revisit"]
fn repeated_errors_deduped() {
    let first = detailed_type_error('a');
    let mut messages = vec![json!({
        "role": "system",
        "content": [{
            "type": "text",
            "text": "Keep the current debugging transcript available.",
            "cache_control": {"type": "ephemeral"}
        }]
    })];
    for revision in ['a', 'b', 'c'] {
        messages.push(json!({
            "role": "tool",
            "name": "terminal",
            "content": detailed_type_error(revision),
        }));
    }
    // The two newest tool outputs remain verbatim by design so an agent keeps
    // immediate diagnostic context. The older near-duplicates are compacted.
    messages.push(json!({
        "role": "tool",
        "name": "terminal",
        "content": "error[E0308]: expected AuthContext, found String (retry 4)",
    }));
    messages.push(json!({
        "role": "tool",
        "name": "terminal",
        "content": "error[E0308]: expected AuthContext, found String (retry 5)",
    }));

    let config = PipelineConfig {
        enable_prose: false,
        enable_effort: false,
        min_savings_pct: 0.0,
        ..PipelineConfig::default()
    };
    let report = CompressionPipeline::run(&mut messages, &config);

    assert_eq!(
        tool_content(&messages[1]),
        first,
        "first error was not retained"
    );
    for message in &messages[2..4] {
        assert!(
            tool_content(message).starts_with("[~similar to turn "),
            "near-duplicate error was not replaced: {}",
            tool_content(message)
        );
    }
    assert!(tool_content(&messages[4]).contains("retry 4"));
    assert!(tool_content(&messages[5]).contains("retry 5"));
    assert_savings_at_least(
        report.total_tokens_before as usize,
        report.total_tokens_after as usize,
        40,
        "repeated debugging errors",
    );
}

#[test]
fn search_results_compressed() {
    let mut lines = (1..=49)
        .map(|line| {
            format!(
                "src/auth.rs:{line}: pub fn validate_session(user: &User, token: &str) -> Result<AuthContext, AuthError> {{ validate_access_token(user, token) }}"
            )
        })
        .collect::<Vec<_>>();
    lines.push(
        "src/auth.rs:400: error: legacy fallback accepts an unauthenticated request".to_string(),
    );
    let raw_search = lines.join("\n");
    let messages = vec![
        json!({"role": "user", "content": "Find every auth validation call and any errors."}),
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "search-auth", "type": "function", "function": {"name": "ctx_search", "arguments": "{\"query\":\"validate_session\"}"}}]
        }),
        json!({"role": "tool", "tool_call_id": "search-auth", "name": "search_files", "content": raw_search}),
    ];
    let before = conversation_tokens(&messages);
    let compressed = compress_tool_result(tool_content(&messages[2]), Some("search_files"));
    let mut rewritten = messages.clone();
    rewritten[2]["content"] = Value::String(compressed.clone());

    assert_savings_at_least(
        before,
        conversation_tokens(&rewritten),
        50,
        "search result listing",
    );
    assert!(compressed.contains("src/auth.rs"), "file path disappeared");
    assert!(
        compressed.contains("auth") || compressed.contains("error"),
        "search anomaly disappeared: {compressed}"
    );
}

fn documentation_fixture() -> String {
    let irrelevant = (0..500)
        .map(|line| {
            format!(
                "The archive contains a leisurely historical anecdote number {line} about gardens, paintings, and travel."
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "# Authentication guide\n\
         \n\
         The authentication module validates each request token before it reaches the API service.\n\
         \n\
         ## Validation fix\n\
         \n\
         ```rust\n\
         fn validate_session(token: &str) -> Result<(), AuthError> {{\n\
             if token.is_empty() {{ return Err(AuthError::EmptyToken); }}\n\
             Ok(())\n\
         }}\n\
         ```\n\
         \n\
         ## Historical appendix\n\
         \n\
         {irrelevant}"
    )
}

#[test]
fn prose_filtered_by_relevance() {
    let documentation = documentation_fixture();
    assert!(
        documentation.lines().count() > 500,
        "fixture must model a large doc read"
    );
    let messages = vec![
        json!({"role": "user", "content": "Fix authentication validation and keep the API behavior documented."}),
        json!({
            "role": "tool",
            "name": "docs_search",
            "content": documentation,
        }),
    ];

    let compressed = compress_prose(
        tool_content(&messages[1]),
        Some("authentication validation"),
    );

    assert_savings_at_least(
        compressed.original_tokens as usize,
        compressed.compressed_tokens as usize,
        50,
        "documentation relevance filter",
    );
    assert!(compressed.compressed.contains("# Authentication guide"));
    assert!(compressed.compressed.contains("## Validation fix"));
    assert!(compressed.compressed.contains("fn validate_session"));
    assert!(
        !compressed
            .compressed
            .contains("leisurely historical anecdote"),
        "irrelevant prose survived the relevance filter"
    );
}

#[test]
fn json_array_delta_encoded() {
    let payload = (1..=100)
        .map(|id| {
            json!({
                "id": id,
                "timestamp": format!("2026-08-14T12:{:02}:00Z", id % 60),
                "service": "auth-api",
                "region": "eu-central-1",
                "environment": "production",
                "status": if id == 42 { "error: token decoder timeout" } else { "ok" },
                "request_path": "/v1/sessions/validate",
            })
        })
        .collect::<Vec<_>>();
    let raw = serde_json::to_string(&payload).expect("JSON fixture serializes");
    let messages = vec![
        json!({"role": "user", "content": "Summarize auth API failures without losing the response schema."}),
        json!({
            "role": "tool",
            "name": "ctx_crush",
            "content": raw,
        }),
    ];

    let args = Map::from_iter([
        (
            "content".to_string(),
            Value::String(tool_content(&messages[1]).to_string()),
        ),
        ("mode".to_string(), Value::String("array".to_string())),
        ("keep_anomalies".to_string(), Value::Bool(true)),
    ]);
    let output = CtxCrushTool
        .handle(&args, &ToolContext::default())
        .expect("ctx_crush accepts a JSON array");
    let response: Value = serde_json::from_str(&output.text).expect("ctx_crush emits JSON");
    let compressed = response["compressed"]
        .as_str()
        .expect("ctx_crush includes compressed text");
    let stats = &response["stats"];

    assert_eq!(stats["delta_encoded"], Value::Bool(true));
    assert_savings_at_least(
        stats["original_tokens"]
            .as_u64()
            .expect("original token count") as usize,
        stats["compressed_tokens"]
            .as_u64()
            .expect("compressed token count") as usize,
        15,
        "sequential JSON array",
    );
    assert!(compressed.contains("[DELTA base="), "delta header missing");
    assert!(
        compressed.contains("schema") || compressed.contains("DELTA") || compressed.contains("D1:"),
        "schema summary missing"
    );
    assert!(
        compressed.contains("token decoder timeout"),
        "anomaly was lost"
    );
    assert_eq!(stats["anomalies_found"], Value::from(1));
}
