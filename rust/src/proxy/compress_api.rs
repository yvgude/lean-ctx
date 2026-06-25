//! `POST /v1/compress` — deterministic messages-in / messages-out compression.
//!
//! Drop-in parity with library-style `compress(messages, model)` gateways: the
//! caller sends a chat-style `messages` array, the proxy rewrites every text
//! payload through the same deterministic funnel used on the wire
//! ([`super::compress::compress_tool_result`]), and returns the rewritten
//! messages plus a structured token-savings summary.
//!
//! ## Contract
//! Request:  `{ "messages": [ … ], "model": "…"? }`
//! Response: `{ "messages": [ … ], "stats": { … } }`
//!
//! Both `OpenAI` (`content: "string"`) and Anthropic (`content: [ {type:"text"…},
//! {type:"tool_result"…} ]`) message shapes are accepted. Only text payloads are
//! compressed; images, `tool_use` blocks, ids and every other field pass through
//! untouched. lean-ctx's own `ctx_*` tool results are left verbatim (#479).
//!
//! ## Determinism (#498)
//! Output is a pure function of `(messages, model)`. Compression runs footer-free
//! — savings are reported in `stats`, never injected into message bodies — so the
//! result stays byte-stable for provider prompt caching.

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::protocol::strip_trailing_savings_footer;
use crate::core::tokens::count_tokens;

use super::compress::compress_tool_result;

/// Default tokenizer behind [`count_tokens`]; surfaced so SDK clients can label
/// the savings figures correctly.
const TOKENIZER: &str = "o200k_base";

#[derive(Debug, Deserialize)]
pub struct CompressRequest {
    pub messages: Vec<Value>,
    /// Optional, echoed into `stats.model`. Routing/pricing hint for SDK clients;
    /// the deterministic funnel itself is model-agnostic.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CompressStats {
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub saved_tokens: usize,
    /// Percentage saved over the compressible text payloads, one decimal place.
    pub saved_pct: f64,
    pub tokenizer: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CompressResponse {
    pub messages: Vec<Value>,
    pub stats: CompressStats,
}

#[derive(Default)]
struct Totals {
    original: usize,
    compressed: usize,
}

/// Axum handler. Malformed bodies are rejected by the `Json` extractor (400).
pub async fn handler(Json(req): Json<CompressRequest>) -> impl IntoResponse {
    (StatusCode::OK, Json(compress_messages(req)))
}

/// Pure, deterministic core: rewrites every text payload in `messages` and
/// reports aggregate token savings. Same input → same output bytes (#498).
#[must_use]
pub fn compress_messages(req: CompressRequest) -> CompressResponse {
    let mut messages = req.messages;
    let mut totals = Totals::default();
    for msg in &mut messages {
        compress_message(msg, &mut totals);
    }

    let saved = totals.original.saturating_sub(totals.compressed);
    let saved_pct = if totals.original > 0 {
        ((saved as f64 / totals.original as f64) * 1000.0).round() / 10.0
    } else {
        0.0
    };

    CompressResponse {
        messages,
        stats: CompressStats {
            original_tokens: totals.original,
            compressed_tokens: totals.compressed,
            saved_tokens: saved,
            saved_pct,
            tokenizer: TOKENIZER,
            model: req.model,
        },
    }
}

fn compress_message(msg: &mut Value, totals: &mut Totals) {
    // OpenAI `tool`/`function` messages carry the tool name; pass it to the funnel
    // so it can honour the #479 pass-through for lean-ctx's own `ctx_*` results.
    let name = msg.get("name").and_then(Value::as_str).map(str::to_string);
    if let Some(content) = msg.get_mut("content") {
        compress_content(content, name.as_deref(), totals);
    }
}

fn compress_content(content: &mut Value, name: Option<&str>, totals: &mut Totals) {
    match content {
        Value::String(s) => squeeze_in_place(s, name, totals),
        Value::Array(blocks) => {
            for block in blocks.iter_mut() {
                compress_block(block, name, totals);
            }
        }
        _ => {}
    }
}

fn compress_block(block: &mut Value, name: Option<&str>, totals: &mut Totals) {
    let Some(obj) = block.as_object_mut() else {
        return;
    };
    match obj.get("type").and_then(Value::as_str) {
        // OpenAI + Anthropic text parts.
        Some("text") => {
            if let Some(Value::String(s)) = obj.get_mut("text") {
                squeeze_in_place(s, name, totals);
            }
        }
        // Anthropic tool_result: nested string or array of content blocks — the
        // single biggest compressible payload in an agent transcript.
        Some("tool_result") => {
            if let Some(inner) = obj.get_mut("content") {
                compress_content(inner, name, totals);
            }
        }
        // image, tool_use, input_audio, document, … pass through untouched.
        _ => {}
    }
}

fn squeeze_in_place(s: &mut String, name: Option<&str>, totals: &mut Totals) {
    let before = count_tokens(s);
    let compressed = compress_tool_result(s, name);
    let clean = strip_trailing_savings_footer(&compressed);
    let after = count_tokens(clean);
    totals.original += before;
    totals.compressed += after;
    if clean != s {
        *s = clean.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A prose blob well over the funnel's 600-char floor: eight identical
    /// paragraphs the prose squeeze deduplicates down to one.
    fn dedupable_prose() -> String {
        let para = "Rust is a multi-paradigm systems programming language that \
                    emphasizes performance, type safety, and fearless concurrency, \
                    achieving memory safety without a garbage collector at runtime.";
        format!("{}\n", [para; 8].join("\n\n"))
    }

    fn run(messages: Vec<Value>, model: Option<&str>) -> CompressResponse {
        compress_messages(CompressRequest {
            messages,
            model: model.map(str::to_string),
        })
    }

    #[test]
    fn string_content_is_compressed_and_stats_reported() {
        let resp = run(
            vec![json!({"role": "user", "content": dedupable_prose()})],
            Some("claude-sonnet-4"),
        );
        let out = resp.messages[0]["content"].as_str().unwrap();
        assert_eq!(
            out.matches("fearless concurrency").count(),
            1,
            "duplicate paragraphs must be deduped"
        );
        assert!(resp.stats.saved_tokens > 0, "stats must reflect savings");
        assert!(resp.stats.compressed_tokens < resp.stats.original_tokens);
        assert_eq!(resp.stats.tokenizer, "o200k_base");
        assert_eq!(resp.stats.model.as_deref(), Some("claude-sonnet-4"));
    }

    #[test]
    fn message_bodies_stay_footer_free() {
        let resp = run(
            vec![json!({"role": "user", "content": dedupable_prose()})],
            None,
        );
        let out = resp.messages[0]["content"].as_str().unwrap();
        assert!(!out.contains('\u{2500}'), "no box-drawing footer in body");
        assert!(!out.contains("[lean-ctx:"), "no verbatim footer in body");
        assert!(resp.stats.model.is_none());
    }

    #[test]
    fn output_is_deterministic() {
        let msgs = vec![
            json!({"role": "system", "content": "You are a helpful assistant."}),
            json!({"role": "user", "content": dedupable_prose()}),
        ];
        let a = serde_json::to_string(&run(msgs.clone(), Some("gpt-4o"))).unwrap();
        let b = serde_json::to_string(&run(msgs, Some("gpt-4o"))).unwrap();
        assert_eq!(a, b, "same input must yield byte-identical output");
    }

    #[test]
    fn short_content_is_untouched() {
        let resp = run(vec![json!({"role": "user", "content": "hi there"})], None);
        assert_eq!(resp.messages[0]["content"], "hi there");
        assert_eq!(resp.stats.saved_tokens, 0);
        assert_eq!(resp.stats.saved_pct, 0.0);
    }

    #[test]
    fn anthropic_blocks_text_compressed_image_passthrough() {
        let resp = run(
            vec![json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": dedupable_prose()},
                    {"type": "image", "source": {"type": "base64", "data": "AAAA"}},
                ],
            })],
            None,
        );
        let blocks = resp.messages[0]["content"].as_array().unwrap();
        assert_eq!(
            blocks[0]["text"]
                .as_str()
                .unwrap()
                .matches("fearless concurrency")
                .count(),
            1
        );
        // Image block is preserved verbatim.
        assert_eq!(blocks[1]["source"]["data"], "AAAA");
    }

    #[test]
    fn anthropic_tool_result_block_is_compressed() {
        let resp = run(
            vec![json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_123",
                    "content": dedupable_prose(),
                }],
            })],
            None,
        );
        let block = &resp.messages[0]["content"][0];
        assert_eq!(block["tool_use_id"], "toolu_123", "ids preserved");
        assert_eq!(
            block["content"]
                .as_str()
                .unwrap()
                .matches("fearless concurrency")
                .count(),
            1
        );
        assert!(resp.stats.saved_tokens > 0);
    }

    #[test]
    fn lean_ctx_tool_output_passes_through_verbatim() {
        // A ctx_* result is already compressed at the tool boundary (#479).
        let prose = dedupable_prose();
        let resp = run(
            vec![json!({"role": "tool", "name": "ctx_read", "content": prose.clone()})],
            None,
        );
        assert_eq!(resp.messages[0]["content"].as_str().unwrap(), prose);
        assert_eq!(
            resp.stats.saved_tokens, 0,
            "ctx_* output is not re-compressed"
        );
    }

    #[test]
    fn non_string_content_is_ignored() {
        // A malformed/absent content field must not panic.
        let resp = run(vec![json!({"role": "assistant", "tool_calls": []})], None);
        assert_eq!(resp.stats.original_tokens, 0);
        assert_eq!(resp.messages.len(), 1);
    }

    /// #498 regression: a full, mixed-shape conversation must serialise to
    /// byte-identical output across repeated calls. Provider prompt caching keys
    /// on the exact bytes, so any non-determinism (ordering, footer leakage,
    /// counter/timestamp) would silently destroy the cache discount.
    #[test]
    fn determinism_regression_full_conversation_498() {
        let conversation = || {
            vec![
                json!({"role": "system", "content": "You are a helpful assistant."}),
                json!({"role": "user", "content": dedupable_prose()}),
                json!({
                    "role": "user",
                    "content": [
                        {"type": "text", "text": dedupable_prose()},
                        {"type": "image", "source": {"type": "base64", "data": "AAAA"}},
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": dedupable_prose()},
                    ],
                }),
                json!({"role": "tool", "name": "ctx_read", "content": dedupable_prose()}),
            ]
        };

        let baseline =
            serde_json::to_string(&run(conversation(), Some("claude-sonnet-4"))).unwrap();
        for _ in 0..4 {
            let again =
                serde_json::to_string(&run(conversation(), Some("claude-sonnet-4"))).unwrap();
            assert_eq!(again, baseline, "/v1/compress output must be byte-stable");
        }

        // The byte-stable bodies must also be footer-free (savings live in stats).
        assert!(!baseline.contains("[lean-ctx:"));
        assert!(!baseline.contains('\u{2500}'));
    }

    /// Daemon-free, o200k_base benchmark over a real on-disk corpus. Prints a
    /// JSON report (ratio + latency) and is `#[ignore]`d so it stays out of CI.
    /// Reproduce: `cargo test -p lean-ctx --lib \
    /// proxy::compress_api::tests::bench_real_corpus_o200k -- --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark; run explicitly with --ignored --nocapture"]
    fn bench_real_corpus_o200k() {
        use std::path::Path;
        use std::time::Instant;

        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/reference");
        let mut messages = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&corpus) {
            let mut paths: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
                .collect();
            paths.sort();
            for path in paths {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    messages.push(json!({"role": "user", "content": text}));
                }
            }
        }
        assert!(!messages.is_empty(), "no corpus files found at {corpus:?}");

        let files = messages.len();
        let started = Instant::now();
        let resp = run(messages, Some("gpt-4o"));
        let latency_ms = started.elapsed().as_secs_f64() * 1000.0;

        let report = json!({
            "corpus": corpus.to_string_lossy(),
            "files": files,
            "tokenizer": resp.stats.tokenizer,
            "original_tokens": resp.stats.original_tokens,
            "compressed_tokens": resp.stats.compressed_tokens,
            "tokens_saved": resp.stats.saved_tokens,
            "saved_pct": resp.stats.saved_pct,
            "latency_ms": (latency_ms * 100.0).round() / 100.0,
        });
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    }
}
