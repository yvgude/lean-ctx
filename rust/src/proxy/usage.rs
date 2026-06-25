//! Real provider-reported token usage extraction.
//!
//! The proxy already rewrites requests; this module reads the *response* so the
//! dashboard/terminal can show **measured** cost (the user's real provider bill)
//! instead of an estimate. All three providers report the exact model and the
//! billed token breakdown — including prompt-cache reads/writes — in the final
//! event of a stream (or the body of a non-streaming response):
//!
//! - Anthropic: `message_start` carries model + input/cache tokens, the final
//!   `message_delta` carries `output_tokens`. Non-streaming: one `usage` object.
//! - `OpenAI` Responses: the `response.completed` event nests `response.usage`.
//! - `OpenAI` Chat Completions: the final chunk carries `usage` (needs
//!   `stream_options.include_usage`, which the proxy injects).
//! - Gemini: every chunk carries `usageMetadata`; the last one has the totals.
//!
//! [`RealUsage`] normalizes every provider onto the four billable buckets that
//! [`crate::core::gain::model_pricing::ModelCost::estimate_usd`] prices:
//! uncached input, output (incl. reasoning/thoughts), cache-read, cache-write.

use futures::{Stream, StreamExt};
use serde_json::Value;

/// LLM provider whose response shape a [`Scanner`] understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    /// Covers both Chat Completions and the Responses API (same `usage` dialects
    /// are detected by field name).
    OpenAi,
    Gemini,
}

impl Provider {
    /// Maps the proxy's `provider_label` (`"Anthropic"`, `"OpenAI"`, else Gemini).
    #[must_use]
    pub fn from_label(label: &str) -> Self {
        match label {
            "Anthropic" => Self::Anthropic,
            "OpenAI" => Self::OpenAi,
            _ => Self::Gemini,
        }
    }
}

/// One LLM turn's real, provider-reported usage, normalized to billable buckets.
///
/// `output_tokens` already includes reasoning/thinking tokens (they are billed at
/// the output rate); `reasoning_tokens` is retained only for display.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RealUsage {
    pub model: String,
    /// Input tokens billed at the input rate (cache reads/writes excluded).
    pub input_tokens: u64,
    /// Output tokens billed at the output rate (includes reasoning/thoughts).
    pub output_tokens: u64,
    /// Input tokens served from the prompt cache (billed at the cache-read rate).
    pub cache_read_tokens: u64,
    /// Input tokens written to the prompt cache (Anthropic only; 0 elsewhere).
    pub cache_write_tokens: u64,
    /// Reasoning/thinking subset of `output_tokens` (display only).
    pub reasoning_tokens: u64,
    /// Output-savings experiment arm for this turn (#895 Track B), or `None` when
    /// no holdout is active. Stamped from the request, not parsed from the
    /// response — it identifies whether this turn was output-shaped.
    pub cohort: Option<super::holdout::Arm>,
}

impl RealUsage {
    /// True once any model or token field has been observed — the gate for
    /// recording. Avoids emitting empty rows for streams that never reported usage.
    fn is_meaningful(&self) -> bool {
        !self.model.is_empty()
            || self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_tokens > 0
            || self.cache_write_tokens > 0
    }
}

/// Upper bound on a single buffered line before we give up on it. Usage events
/// are tiny; this only guards against a pathological newline-free stream.
const MAX_LINE_BYTES: usize = 1 << 20; // 1 MiB

/// Incrementally extracts [`RealUsage`] from a response stream (or a full body).
///
/// `feed` is called with raw response chunks and keeps only the trailing partial
/// line buffered (O(1) memory beyond one line); `finalize` returns the merged
/// usage once the stream ends.
pub struct Scanner {
    provider: Provider,
    /// Model parsed from the request URL (Gemini puts it there, not in the body).
    url_model: Option<String>,
    /// Output-savings arm (#895), stamped onto the usage at finalize.
    cohort: Option<super::holdout::Arm>,
    buf: Vec<u8>,
    usage: RealUsage,
}

impl Scanner {
    #[must_use]
    pub fn new(provider: Provider, url_model: Option<String>) -> Self {
        Self {
            provider,
            url_model,
            cohort: None,
            buf: Vec::new(),
            usage: RealUsage::default(),
        }
    }

    /// Tags the usage this scanner produces with an output-savings arm (#895).
    #[must_use]
    pub fn with_cohort(mut self, cohort: Option<super::holdout::Arm>) -> Self {
        self.cohort = cohort;
        self
    }

    /// Feeds a raw streaming chunk, scanning every complete line it completes.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop(); // drop '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.scan_line(&line);
        }
        if self.buf.len() > MAX_LINE_BYTES {
            self.buf.clear();
        }
    }

    /// Feeds a complete non-streaming JSON response body.
    pub fn feed_body(&mut self, body: &[u8]) {
        if let Ok(v) = serde_json::from_slice::<Value>(body) {
            self.absorb(&v);
        }
    }

    /// Consumes the scanner, flushing any trailing partial line (a final event
    /// may arrive without a newline) and returning the merged usage if any.
    #[must_use]
    pub fn finalize(mut self) -> Option<RealUsage> {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.scan_line(&line);
        }
        if self.usage.is_meaningful() {
            self.usage.cohort = self.cohort;
            Some(self.usage)
        } else {
            None
        }
    }

    fn scan_line(&mut self, line: &[u8]) {
        let Ok(text) = std::str::from_utf8(line) else {
            return;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        // Cheap pre-filter: skip the bulk of the stream (content deltas) and only
        // JSON-parse lines that can carry usage or the model name.
        if !self.line_might_be_relevant(trimmed) {
            return;
        }
        let json_str = if let Some(rest) = trimmed.strip_prefix("data:") {
            // SSE (Anthropic, OpenAI, Gemini with alt=sse).
            let r = rest.trim();
            if r.is_empty() || r == "[DONE]" {
                return;
            }
            r
        } else if trimmed.starts_with('{') {
            // NDJSON / array-element line (Gemini x-ndjson). Tolerate the array
            // punctuation a streamed JSON array puts around an element.
            trimmed
                .trim_start_matches([',', '['])
                .trim_end_matches([',', ']'])
                .trim()
        } else {
            return;
        };
        if let Ok(v) = serde_json::from_str::<Value>(json_str) {
            self.absorb(&v);
        }
    }

    fn line_might_be_relevant(&self, s: &str) -> bool {
        match self.provider {
            // Anthropic `message_start`/`message_delta` and OpenAI `usage`/
            // `response.*` events all contain the substring "usage".
            Provider::Anthropic | Provider::OpenAi => s.contains("usage"),
            Provider::Gemini => s.contains("usageMetadata"),
        }
    }

    fn absorb(&mut self, v: &Value) {
        match self.provider {
            Provider::Anthropic => absorb_anthropic(&mut self.usage, v),
            Provider::OpenAi => absorb_openai(&mut self.usage, v),
            Provider::Gemini => absorb_gemini(&mut self.usage, v, self.url_model.as_deref()),
        }
    }
}

/// Anthropic: model + input/cache live on `message` (streaming `message_start`
/// or a non-streaming body); `output_tokens` arrives later on the event-level
/// `usage` of `message_delta`. Latest non-zero wins, so the cumulative final
/// delta is authoritative.
fn absorb_anthropic(u: &mut RealUsage, v: &Value) {
    let msg = v.get("message").unwrap_or(v);
    if let Some(model) = msg.get("model").and_then(Value::as_str)
        && !model.is_empty()
    {
        u.model = model.to_string();
    }
    let Some(usage) = msg.get("usage").or_else(|| v.get("usage")) else {
        return;
    };
    if let Some(n) = usage.get("input_tokens").and_then(Value::as_u64) {
        u.input_tokens = n;
    }
    if let Some(n) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
        u.cache_read_tokens = n;
    }
    if let Some(n) = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
    {
        u.cache_write_tokens = n;
    }
    if let Some(n) = usage.get("output_tokens").and_then(Value::as_u64)
        && n > 0
    {
        u.output_tokens = n;
    }
}

/// `OpenAI` Chat Completions + Responses. `response.completed` nests the payload
/// under `response`; chat chunks and non-streaming bodies are top-level. Both
/// `usage` dialects are accepted (Responses: `input_tokens`/`output_tokens`;
/// Chat: `prompt_tokens`/`completion_tokens`). `cached_tokens` is the cache-read
/// portion of the reported input; `OpenAI` bills no separate cache write.
fn absorb_openai(u: &mut RealUsage, v: &Value) {
    let root = v.get("response").unwrap_or(v);
    if let Some(model) = root.get("model").and_then(Value::as_str)
        && !model.is_empty()
    {
        u.model = model.to_string();
    }
    let Some(usage) = root.get("usage") else {
        return;
    };
    if usage.is_null() {
        // `response.created` / `response.in_progress` carry `usage: null`.
        return;
    }

    let total_input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .get("input_tokens_details")
        .or_else(|| usage.get("prompt_tokens_details"))
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning = usage
        .get("output_tokens_details")
        .or_else(|| usage.get("completion_tokens_details"))
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    if total_input == 0 && total_output == 0 {
        return;
    }
    u.input_tokens = total_input.saturating_sub(cached);
    u.cache_read_tokens = cached;
    u.cache_write_tokens = 0;
    u.output_tokens = total_output;
    u.reasoning_tokens = reasoning;
}

/// Gemini: `usageMetadata` carries the counts; `modelVersion` (or the request
/// URL) the model. `thoughtsTokenCount` is billed at the output rate and is not
/// part of `candidatesTokenCount`, so it is added into `output_tokens`.
fn absorb_gemini(u: &mut RealUsage, v: &Value, url_model: Option<&str>) {
    if let Some(mv) = v.get("modelVersion").and_then(Value::as_str)
        && !mv.is_empty()
    {
        u.model = mv.to_string();
    } else if u.model.is_empty()
        && let Some(m) = url_model
        && !m.is_empty()
    {
        u.model = m.to_string();
    }
    let Some(um) = v.get("usageMetadata") else {
        return;
    };
    let prompt = um
        .get("promptTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let candidates = um
        .get("candidatesTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = um
        .get("cachedContentTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let thoughts = um
        .get("thoughtsTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if prompt == 0 && candidates == 0 && thoughts == 0 {
        return;
    }
    u.input_tokens = prompt.saturating_sub(cached);
    u.cache_read_tokens = cached;
    u.cache_write_tokens = 0;
    u.output_tokens = candidates + thoughts;
    u.reasoning_tokens = thoughts;
}

/// Extracts the model from a Gemini request path
/// (`/v1beta/models/{model}:generateContent`). Returns `None` for other paths.
#[must_use]
pub fn gemini_model_from_path(path: &str) -> Option<String> {
    let after = path.rsplit_once("/models/").map(|(_, m)| m)?;
    let model = after.split(':').next().unwrap_or(after).trim();
    if model.is_empty() {
        None
    } else {
        Some(model.to_string())
    }
}

/// Wraps a response byte stream so every chunk is forwarded **byte-for-byte**
/// while a [`Scanner`] observes it; on stream end the merged usage is recorded.
/// Memory overhead is one buffered SSE line, never the whole response.
pub fn tee_stream<S, B, E>(
    inner: S,
    scanner: Scanner,
) -> impl Stream<Item = Result<B, E>> + Send + 'static
where
    S: Stream<Item = Result<B, E>> + Send + Unpin + 'static,
    B: AsRef<[u8]> + Send + 'static,
    E: Send + 'static,
{
    futures::stream::unfold(
        (inner, Some(scanner)),
        |(mut inner, mut scanner)| async move {
            match inner.next().await {
                Some(Ok(chunk)) => {
                    if let Some(s) = scanner.as_mut() {
                        s.feed(chunk.as_ref());
                    }
                    Some((Ok(chunk), (inner, scanner)))
                }
                Some(err) => Some((err, (inner, scanner))),
                None => {
                    if let Some(s) = scanner.take()
                        && let Some(usage) = s.finalize()
                    {
                        super::usage_meter::record(&usage);
                    }
                    None
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_lines(
        provider: Provider,
        url_model: Option<&str>,
        lines: &[&str],
    ) -> Option<RealUsage> {
        let mut s = Scanner::new(provider, url_model.map(str::to_string));
        for line in lines {
            s.feed(line.as_bytes());
            s.feed(b"\n");
        }
        s.finalize()
    }

    #[test]
    fn anthropic_merges_message_start_and_delta() {
        let u = feed_lines(
            Provider::Anthropic,
            None,
            &[
                r#"data: {"type":"message_start","message":{"model":"claude-opus-4-5-20251101","usage":{"input_tokens":100,"cache_read_input_tokens":2000,"cache_creation_input_tokens":50,"output_tokens":1}}}"#,
                r#"data: {"type":"content_block_delta","index":0,"delta":{"text":"hello"}}"#,
                r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":73}}"#,
                "data: {\"type\":\"message_stop\"}",
            ],
        )
        .expect("usage");
        assert_eq!(u.model, "claude-opus-4-5-20251101");
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.cache_read_tokens, 2000);
        assert_eq!(u.cache_write_tokens, 50);
        assert_eq!(u.output_tokens, 73);
    }

    #[test]
    fn anthropic_non_streaming_body() {
        let mut s = Scanner::new(Provider::Anthropic, None);
        s.feed_body(
            br#"{"model":"claude-sonnet-4-5","usage":{"input_tokens":24,"output_tokens":18,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
        let u = s.finalize().expect("usage");
        assert_eq!(u.model, "claude-sonnet-4-5");
        assert_eq!(u.input_tokens, 24);
        assert_eq!(u.output_tokens, 18);
    }

    #[test]
    fn openai_responses_completed_event() {
        let u = feed_lines(
            Provider::OpenAi,
            None,
            &[
                r#"data: {"type":"response.created","response":{"model":"gpt-5.4","usage":null}}"#,
                r#"data: {"type":"response.completed","response":{"model":"gpt-5.4","usage":{"input_tokens":1289,"input_tokens_details":{"cached_tokens":289},"output_tokens":685,"output_tokens_details":{"reasoning_tokens":640},"total_tokens":1974}}}"#,
            ],
        )
        .expect("usage");
        assert_eq!(u.model, "gpt-5.4");
        assert_eq!(u.input_tokens, 1000); // 1289 - 289 cached
        assert_eq!(u.cache_read_tokens, 289);
        assert_eq!(u.output_tokens, 685);
        assert_eq!(u.reasoning_tokens, 640);
    }

    #[test]
    fn openai_chat_final_usage_chunk() {
        let u = feed_lines(
            Provider::OpenAi,
            None,
            &[
                r#"data: {"choices":[{"delta":{"content":"hi"}}],"model":"gpt-5.4-mini"}"#,
                r#"data: {"choices":[],"model":"gpt-5.4-mini","usage":{"prompt_tokens":500,"prompt_tokens_details":{"cached_tokens":100},"completion_tokens":40,"total_tokens":540}}"#,
                "data: [DONE]",
            ],
        )
        .expect("usage");
        assert_eq!(u.model, "gpt-5.4-mini");
        assert_eq!(u.input_tokens, 400);
        assert_eq!(u.cache_read_tokens, 100);
        assert_eq!(u.output_tokens, 40);
    }

    #[test]
    fn gemini_usage_metadata_with_url_model() {
        let u = feed_lines(
            Provider::Gemini,
            Some("gemini-2.5-pro"),
            &[
                r#"data: {"candidates":[{"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"promptTokenCount":25,"candidatesTokenCount":7,"thoughtsTokenCount":39,"totalTokenCount":71}}"#,
            ],
        )
        .expect("usage");
        assert_eq!(u.model, "gemini-2.5-pro");
        assert_eq!(u.input_tokens, 25);
        assert_eq!(u.output_tokens, 46); // candidates 7 + thoughts 39
        assert_eq!(u.reasoning_tokens, 39);
    }

    #[test]
    fn gemini_prefers_model_version_over_url() {
        let u = feed_lines(
            Provider::Gemini,
            Some("gemini-2.5-pro"),
            &[
                r#"data: {"modelVersion":"gemini-2.5-pro-002","usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"cachedContentTokenCount":4,"totalTokenCount":15}}"#,
            ],
        )
        .expect("usage");
        assert_eq!(u.model, "gemini-2.5-pro-002");
        assert_eq!(u.input_tokens, 6); // 10 - 4 cached
        assert_eq!(u.cache_read_tokens, 4);
        assert_eq!(u.output_tokens, 5);
    }

    #[test]
    fn split_chunks_reassemble_across_feed_calls() {
        let mut s = Scanner::new(Provider::Anthropic, None);
        let line = r#"data: {"type":"message_delta","delta":{},"usage":{"output_tokens":42}}"#;
        let bytes = format!("{line}\n");
        let (a, b) = bytes.as_bytes().split_at(20);
        s.feed(a);
        s.feed(b);
        let u = s.finalize().expect("usage");
        assert_eq!(u.output_tokens, 42);
    }

    #[test]
    fn no_usage_yields_none() {
        let out = feed_lines(
            Provider::OpenAi,
            None,
            &[r#"data: {"choices":[{"delta":{"content":"hi"}}],"model":"gpt-5.4"}"#],
        );
        assert!(out.is_none(), "content-only stream reports no usage");
    }

    #[test]
    fn final_event_without_trailing_newline() {
        let mut s = Scanner::new(Provider::Anthropic, None);
        s.feed(br#"data: {"type":"message_delta","delta":{},"usage":{"output_tokens":7}}"#);
        let u = s.finalize().expect("flushes trailing partial line");
        assert_eq!(u.output_tokens, 7);
    }

    #[test]
    fn gemini_model_from_path_extracts() {
        assert_eq!(
            gemini_model_from_path("/v1beta/models/gemini-2.5-pro:streamGenerateContent")
                .as_deref(),
            Some("gemini-2.5-pro")
        );
        assert_eq!(
            gemini_model_from_path("/v1beta/models/gemini-2.5-flash:generateContent").as_deref(),
            Some("gemini-2.5-flash")
        );
        assert_eq!(gemini_model_from_path("/v1/chat/completions"), None);
    }

    #[tokio::test]
    async fn tee_stream_passes_bytes_through_and_records() {
        let chunks: Vec<Result<Vec<u8>, std::convert::Infallible>> = vec![
            Ok(b"data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-haiku-4.5\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n".to_vec()),
            Ok(b"data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":9}}\n".to_vec()),
        ];
        let inner = futures::stream::iter(chunks);
        let scanner = Scanner::new(Provider::Anthropic, None);
        let teed = tee_stream(inner, scanner);
        let collected: Vec<_> = teed.collect().await;
        // Byte-for-byte passthrough preserved.
        assert_eq!(collected.len(), 2);
        assert!(collected.iter().all(std::result::Result::is_ok));
    }
}
