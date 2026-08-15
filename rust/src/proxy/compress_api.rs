//! `POST /v1/compress` — deterministic messages-in / messages-out compression.
//!
//! Drop-in parity with library-style `compress(messages, model)` gateways: the
//! caller sends a chat-style `messages` array, the proxy rewrites every text
//! payload through the same deterministic funnel used on the wire
//! ([`super::compress::compress_tool_result_gateway`]), and returns the
//! rewritten messages plus a structured token-savings summary. A lossy rewrite
//! embeds a `hash=<24hex>` retrieval marker (#702) that LiteLLM's headroom
//! guardrail resolves through `GET /v1/retrieve/{hash}` — the CCR agentic loop
//! (BerriAI/litellm#31681) works against lean-ctx unchanged.
//!
//! ## Contract
//! Request:  `{ "messages": [ … ], "model": "…"? }`
//! Response: `{ "messages": [ … ], "stats": { … } }`
//!
//! Both OpenAI (`content: "string"`) and Anthropic (`content: [ {type:"text"…},
//! {type:"tool_result"…} ]`) message shapes are accepted. Only text payloads are
//! compressed; images, `tool_use` blocks, ids and every other field pass through
//! untouched. lean-ctx's own `ctx_*` tool results are left verbatim (#479).
//!
//! ## Gateway compatibility (#700)
//! The response also carries `tokens_before` / `tokens_after` /
//! `compression_ratio` at the top level — the field names LiteLLM's
//! prompt-compression guardrail reads for its per-request savings log. That
//! makes lean-ctx a drop-in `api_base` for `guardrail: headroom` deployments:
//! LiteLLM only requires `messages` in the reply and treats the token fields
//! as optional telemetry.
//!
//! ## Determinism (#498)
//! Output is a pure function of `(messages, model)`. Compression runs footer-free
//! — savings are reported in `stats`, never injected into message bodies — so the
//! result stays byte-stable for provider prompt caching.

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{adaptive_policy::select_policy, live_zone};
use crate::core::protocol::strip_trailing_savings_footer;
use crate::core::tokens::{TokenizerFamily, count_tokens_for, detect_tokenizer};

use super::compress::compress_tool_result_gateway_with_policy;

#[derive(Debug, Deserialize)]
pub struct CompressRequest {
    pub messages: Vec<Value>,
    /// Optional model name, echoed into `stats.model` and used to select the
    /// tokenizer family for compression and token accounting.
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
    pub tokenizer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CompressResponse {
    pub messages: Vec<Value>,
    pub stats: CompressStats,
    /// LiteLLM-guardrail telemetry aliases (#700): duplicates of
    /// `stats.original_tokens` / `stats.compressed_tokens` under the field
    /// names the LiteLLM headroom guardrail logs (`tokens_before` →
    /// `tokens_after`, ratio `after/before`).
    pub tokens_before: usize,
    pub tokens_after: usize,
    /// `tokens_after / tokens_before`, rounded to 2 decimals; `1.0` when the
    /// input had no compressible text.
    pub compression_ratio: f64,
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
pub fn compress_messages(req: CompressRequest) -> CompressResponse {
    let family = req
        .model
        .as_deref()
        .map(detect_tokenizer)
        .unwrap_or_default();
    let mut messages = req.messages;
    let mut totals = Totals::default();
    let last_user_content = messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|message| message.get("content").and_then(Value::as_str));
    let task_class = super::pre_optimize::classify_task(last_user_content);

    let policy = super::adaptive_policy::best_policy_for(task_class);
    let live_split = live_zone::detect_live_zone(&messages);
    for msg in &mut messages[live_split.boundary_turn..] {
        compress_message(msg, &mut totals, family, policy);
    }
    let _live_stats = live_zone::compress_live_only(&mut messages, family, select_policy("chat"));

    let saved = totals.original.saturating_sub(totals.compressed);
    let saved_pct = if totals.original > 0 {
        ((saved as f64 / totals.original as f64) * 1000.0).round() / 10.0
    } else {
        0.0
    };
    let compression_ratio = if totals.original > 0 {
        ((totals.compressed as f64 / totals.original as f64) * 100.0).round() / 100.0
    } else {
        1.0
    };

    CompressResponse {
        messages,
        stats: CompressStats {
            original_tokens: totals.original,
            compressed_tokens: totals.compressed,
            saved_tokens: saved,
            saved_pct,
            tokenizer: family.to_string(),
            model: req.model,
        },
        tokens_before: totals.original,
        tokens_after: totals.compressed,
        compression_ratio,
    }
}

fn compress_message(
    msg: &mut Value,
    totals: &mut Totals,
    family: TokenizerFamily,
    policy: super::adaptive_policy::CompressionPolicy,
) {
    // OpenAI `tool`/`function` messages carry the tool name; pass it to the funnel
    // so it can honour the #479 pass-through for lean-ctx's own `ctx_*` results.
    let name = msg.get("name").and_then(Value::as_str).map(str::to_string);
    if let Some(content) = msg.get_mut("content") {
        compress_content(content, name.as_deref(), totals, family, policy);
    }
}

fn compress_content(
    content: &mut Value,
    name: Option<&str>,
    totals: &mut Totals,
    family: TokenizerFamily,
    policy: super::adaptive_policy::CompressionPolicy,
) {
    match content {
        Value::String(s) => squeeze_in_place(s, name, totals, family, policy),
        Value::Array(blocks) => {
            for block in blocks.iter_mut() {
                compress_block(block, name, totals, family, policy);
            }
        }
        _ => {}
    }
}

fn compress_block(
    block: &mut Value,
    name: Option<&str>,
    totals: &mut Totals,
    family: TokenizerFamily,
    policy: super::adaptive_policy::CompressionPolicy,
) {
    let Some(obj) = block.as_object_mut() else {
        return;
    };
    match obj.get("type").and_then(Value::as_str) {
        // OpenAI + Anthropic text parts.
        Some("text") => {
            if let Some(Value::String(s)) = obj.get_mut("text") {
                squeeze_in_place(s, name, totals, family, policy);
            }
        }
        // Anthropic tool_result: nested string or array of content blocks — the
        // single biggest compressible payload in an agent transcript.
        Some("tool_result") => {
            if let Some(inner) = obj.get_mut("content") {
                compress_content(inner, name, totals, family, policy);
            }
        }
        // image, tool_use, input_audio, document, … pass through untouched.
        _ => {}
    }
}

fn squeeze_in_place(
    s: &mut String,
    name: Option<&str>,
    totals: &mut Totals,
    family: TokenizerFamily,
    policy: super::adaptive_policy::CompressionPolicy,
) {
    let before = count_tokens_for(s, family);
    // Gateway audience (#702): a lossy rewrite carries the `hash=<24hex>`
    // retrieval marker LiteLLM's CCR loop scans for; the savings footer is
    // stripped inside the gateway funnel (stats carry the numbers instead).
    let compressed = compress_tool_result_gateway_with_policy(s, name, family, policy);
    let clean = strip_trailing_savings_footer(&compressed);
    let after = count_tokens_for(clean, family);
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
        let _lock = crate::core::data_dir::test_env_lock();
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
        assert_eq!(resp.stats.tokenizer, "cl100k_base");
        assert_eq!(resp.stats.model.as_deref(), Some("claude-sonnet-4"));
    }

    #[test]
    fn model_selects_tokenizer_family_for_gateway_compression() {
        let resp = run(
            vec![json!({"role": "user", "content": dedupable_prose()})],
            Some("claude-sonnet-4"),
        );

        assert_eq!(resp.stats.model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(resp.stats.tokenizer, "cl100k_base");
    }

    #[test]
    fn litellm_guardrail_fields_present_and_consistent() {
        let _lock = crate::core::data_dir::test_env_lock();
        // LiteLLM's headroom guardrail logs `tokens_before`/`tokens_after`/
        // `compression_ratio` from the /v1/compress reply (#700). They must
        // exist at the top level and agree with `stats`.
        let resp = run(
            vec![json!({"role": "user", "content": dedupable_prose()})],
            None,
        );
        assert_eq!(resp.tokens_before, resp.stats.original_tokens);
        assert_eq!(resp.tokens_after, resp.stats.compressed_tokens);
        assert!(resp.compression_ratio > 0.0 && resp.compression_ratio < 1.0);

        let wire = serde_json::to_value(&resp).unwrap();
        assert!(wire["tokens_before"].is_u64());
        assert!(wire["tokens_after"].is_u64());
        assert!(wire["compression_ratio"].is_f64());

        // No compressible text → ratio pins to 1.0, not 0/0.
        let empty = run(vec![json!({"role": "user", "content": "hi"})], None);
        assert_eq!(empty.compression_ratio, 1.0);
    }

    #[test]
    fn message_bodies_stay_footer_free() {
        let _lock = crate::core::data_dir::test_env_lock();
        let resp = run(
            vec![json!({"role": "user", "content": dedupable_prose()})],
            None,
        );
        let out = resp.messages[0]["content"].as_str().unwrap();
        assert!(!out.contains('\u{2500}'), "no box-drawing footer in body");
        assert!(!out.contains("[lean-ctx:"), "no savings footer in body");
        assert!(resp.stats.model.is_none());
    }

    /// #702: a lossy rewrite through the gateway contract must advertise its
    /// retrieval hash in LiteLLM's regex-locked `hash=<24hex>` form, and the
    /// hash must resolve back to the verbatim original — the wire half of the
    /// guardrail's CCR agentic loop.
    #[test]
    fn lossy_rewrite_carries_litellm_retrieval_marker() {
        let _lock = crate::core::data_dir::test_env_lock();
        let original = dedupable_prose();
        let resp = run(
            vec![json!({"role": "user", "content": original.clone()})],
            None,
        );
        let out = resp.messages[0]["content"].as_str().unwrap();

        let litellm_regex = regex::Regex::new(r"hash=([a-f0-9]{24})").unwrap();
        let hash = litellm_regex
            .captures(out)
            .unwrap_or_else(|| panic!("lossy body must carry the hash= marker: {out}"))
            .get(1)
            .unwrap()
            .as_str();
        let recovered = super::super::ccr::retrieve_litellm(hash)
            .expect("marker hash must resolve via /v1/retrieve");
        assert!(
            recovered.contains("fearless concurrency"),
            "retrieve returns the verbatim pre-compression original"
        );
        assert_eq!(
            recovered.matches("fearless concurrency").count(),
            8,
            "all deduped paragraphs are recoverable"
        );
    }

    #[test]
    fn output_is_deterministic() {
        let _lock = crate::core::data_dir::test_env_lock();
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
        let _lock = crate::core::data_dir::test_env_lock();
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
        let _lock = crate::core::data_dir::test_env_lock();
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
        let _lock = crate::core::data_dir::test_env_lock();
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
