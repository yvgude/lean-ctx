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
//! - OpenAI Responses: the `response.completed` event nests `response.usage`.
//! - OpenAI Chat Completions: the final chunk carries `usage` (needs
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
    /// Maps the proxy's `provider_label` (`"Anthropic"`, `"OpenAI"`/`"ChatGPT"`, else Gemini).
    pub fn from_label(label: &str) -> Self {
        match label {
            "Anthropic" | "Bedrock" => Self::Anthropic,
            "OpenAI" | "ChatGPT" => Self::OpenAi,
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
    /// Input tokens written to the prompt cache (Anthropic + OpenRouter models
    /// with explicit cache-write pricing; 0 elsewhere).
    pub cache_write_tokens: u64,
    /// Reasoning/thinking subset of `output_tokens` (display only).
    pub reasoning_tokens: u64,
    /// USD the provider *actually charged* for this turn, when the response
    /// reports it. OpenRouter: `usage.cost` in credits (≡ USD); for BYOK
    /// requests the separate `cost_details.upstream_inference_cost` is added
    /// when it differs from `cost` (#746 — non-BYOK mirrors cost there).
    /// The measured figure beats any price-table estimate wherever both exist.
    /// `None` for providers that report tokens only (Anthropic/OpenAI/Gemini).
    pub provider_cost_usd: Option<f64>,
    /// Output-savings experiment arm for this turn (#895 Track B), or `None` when
    /// no holdout is active. Stamped from the request, not parsed from the
    /// response — it identifies whether this turn was output-shaped.
    pub cohort: Option<super::holdout::Arm>,
    /// Request-side gateway context (enterprise#11/#17/#18): identity tags,
    /// compression savings and baseline inputs, stamped from the request before
    /// it left for the upstream. `None` outside the forward path (e.g. tests
    /// that only parse response bodies).
    pub wire: Option<Box<WireContext>>,
}

/// Request-side context the forward path knows and the response scanner does
/// not: who sent the request (gateway identity, enterprise#11), what the proxy
/// saved on the wire, and the counterfactual-baseline inputs (enterprise#18).
/// Travels inside [`RealUsage`] so `usage_meter::record` stays the single
/// choke-point through which every measured turn flows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WireContext {
    /// Provider label (`Anthropic|OpenAI|ChatGPT|Gemini`) of the serving route.
    pub provider: String,
    /// Person tag from the gateway key (enterprise#11).
    pub person: Option<String>,
    /// Team tag from the gateway key.
    pub team: Option<String>,
    /// Project: `x-leanctx-project` header wins over the key's default project.
    pub project: Option<String>,
    /// Estimated tokens this request saved through wire compression
    /// (bytes/4 heuristic, same basis as the proxy stats).
    pub saved_tokens: u64,
    /// Estimated request tokens BEFORE lean-ctx compression (bytes/4) — the
    /// SEE-attribution input of the avoided-cost baseline (enterprise#18).
    pub uncompressed_input_tokens: u64,
    /// True when the serving upstream is a local/loopback endpoint — billed via
    /// the transparent `local_shadow_rate`, never 0 (enterprise#15/#18).
    pub is_local: bool,
    /// Originally requested model when the router downgraded/aliased it
    /// (enterprise#13); `None` for passthrough.
    pub routed_from: Option<String>,
    /// Provider-counted input tokens of the ORIGINAL (uncompressed) request,
    /// from the free `count_tokens` probe (#701). `None` when metering is off
    /// or no probe was spawned; an empty slot at read time (probe failed or
    /// still in flight) degrades the row to the local estimate.
    pub counterfactual: Option<super::counterfactual::CounterfactualSlot>,
    /// Complete, validated OCLA lineage for managed HTTP requests. `None` keeps
    /// legacy, malformed and non-HTTP traffic explicitly unmanaged.
    pub lineage: Option<crate::core::ocla::OclaRequestContext>,
}

impl WireContext {
    #[must_use]
    pub fn ocla_request_context(&self) -> Option<&crate::core::ocla::OclaRequestContext> {
        self.lineage.as_ref()
    }
}

impl RealUsage {
    /// Projects measured proxy usage into the payload-free canonical OCLA record.
    /// Missing lineage is unmanaged; invalid data and arithmetic overflow fail
    /// closed for OCLA without disturbing the existing usage pipeline.
    pub fn to_ocla_usage_record(
        &self,
    ) -> crate::core::ocla::OclaResult<Option<crate::core::ocla::UsageRecord>> {
        use crate::core::ocla::{OclaError, UsageRecord};

        let Some(context) = self
            .wire
            .as_deref()
            .and_then(WireContext::ocla_request_context)
            .cloned()
        else {
            return Ok(None);
        };
        context.validate()?;
        if self.model.trim().is_empty() {
            return Err(OclaError::InvalidRequest("model is required".into()));
        }
        let input_tokens = self
            .input_tokens
            .checked_add(self.cache_read_tokens)
            .and_then(|value| value.checked_add(self.cache_write_tokens))
            .ok_or_else(|| OclaError::InvalidRequest("input token total overflow".into()))?;
        let provider_billed_tokens = input_tokens
            .checked_add(self.output_tokens)
            .ok_or_else(|| OclaError::InvalidRequest("billed token total overflow".into()))?;

        Ok(Some(UsageRecord {
            context,
            model: self.model.clone(),
            input_tokens,
            output_tokens: self.output_tokens,
            provider_billed_tokens,
        }))
    }

    /// True once any model, token or measured-cost field has been observed —
    /// the gate for recording. Avoids emitting empty rows for streams that
    /// never reported usage.
    fn is_meaningful(&self) -> bool {
        !self.model.is_empty()
            || self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_tokens > 0
            || self.cache_write_tokens > 0
            || self.provider_cost_usd.is_some()
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
    /// Request-side gateway context (enterprise#11/#18), stamped at finalize.
    wire: Option<Box<WireContext>>,
    /// Billed USD from a gateway response header (#1189), stamped at finalize
    /// unless the body already reported the charge.
    header_cost: Option<f64>,
    buf: Vec<u8>,
    usage: RealUsage,
}

impl Scanner {
    pub fn new(provider: Provider, url_model: Option<String>) -> Self {
        Self {
            provider,
            url_model,
            cohort: None,
            wire: None,
            header_cost: None,
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

    /// Attaches the request-side gateway context (identity tags, wire savings,
    /// baseline inputs — enterprise#11/#17/#18) stamped onto the usage record.
    #[must_use]
    pub fn with_wire_context(mut self, wire: Option<Box<WireContext>>) -> Self {
        self.wire = wire;
        self
    }

    /// Attaches a billed USD figure reported by the upstream via a response
    /// header (LiteLLM `x-litellm-response-cost`, or the operator-configured
    /// `[proxy] cost_response_header`, #1189). Applied at finalize only when
    /// the body did not already carry a measured cost — a body figure
    /// (OpenRouter `usage.cost`) is the bill itself and always wins.
    #[must_use]
    pub fn with_header_cost(mut self, cost: Option<f64>) -> Self {
        self.header_cost = cost.filter(|c| c.is_finite() && *c >= 0.0);
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
    pub fn finalize(mut self) -> Option<RealUsage> {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.scan_line(&line);
        }
        // Gateway header cost (#1189): measured, but the body figure is the
        // bill itself (OpenRouter usage.cost) and keeps priority when present.
        if self.usage.provider_cost_usd.is_none() {
            self.usage.provider_cost_usd = self.header_cost;
        }
        if self.usage.is_meaningful() {
            self.usage.cohort = self.cohort;
            self.usage.wire = self.wire;
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

/// OpenAI Chat Completions + Responses. `response.completed` nests the payload
/// under `response`; chat chunks and non-streaming bodies are top-level. Both
/// `usage` dialects are accepted (Responses: `input_tokens`/`output_tokens`;
/// Chat: `prompt_tokens`/`completion_tokens`). `cached_tokens` is the cache-read
/// portion of the reported input; OpenAI bills no separate cache write, but
/// OpenRouter reports one (`prompt_tokens_details.cache_write_tokens`) for
/// models with explicit cache-write pricing.
///
/// OpenRouter usage accounting additionally carries the money actually charged:
/// `usage.cost` (credits ≡ USD) and, for BYOK requests, the upstream provider's
/// own bill under `cost_details.upstream_inference_cost`. Their sum is this
/// turn's real price — measured, not table-derived.
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

    // Measured cost (OpenRouter dialect). Parsed before the token guard so a
    // cost-bearing usage object is never lost, and `0` is preserved — a
    // `:free` model's real price IS zero, not "unknown".
    if let Some(cost) = usage.get("cost").and_then(Value::as_f64) {
        let upstream = usage
            .get("cost_details")
            .and_then(|d| d.get("upstream_inference_cost"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        // #746: non-BYOK responses may mirror `cost` in upstream_inference_cost;
        // summing both would double-count. BYOK responses split the total:
        // `cost` = OpenRouter fee (small), `upstream` = provider bill (large,
        // always different from cost). Only add when genuinely distinct.
        let byok_upstream = if upstream > 0.0 && upstream != cost {
            upstream
        } else {
            0.0
        };
        u.provider_cost_usd = Some(cost + byok_upstream);
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
    let input_details = usage
        .get("input_tokens_details")
        .or_else(|| usage.get("prompt_tokens_details"));
    let cached = input_details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = input_details
        .and_then(|d| d.get("cache_write_tokens"))
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
    // OpenRouter counts cache writes inside prompt_tokens (unlike Anthropic's
    // separate bucket) — subtract both cached reads and writes so the three
    // buckets stay disjoint and are never double-priced.
    u.input_tokens = total_input
        .saturating_sub(cached)
        .saturating_sub(cache_write);
    u.cache_read_tokens = cached;
    u.cache_write_tokens = cache_write;
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
    fn scanner_stamps_wire_context_onto_usage() {
        // enterprise#11/#17: the request-side context must survive scanning and
        // arrive on the finalized record that usage_meter/store consume.
        let wire = Box::new(WireContext {
            provider: "Anthropic".into(),
            person: Some("yves".into()),
            team: None,
            project: Some("billing".into()),
            saved_tokens: 42,
            uncompressed_input_tokens: 500,
            is_local: false,
            routed_from: None,
            counterfactual: None,
            lineage: None,
        });
        let mut s = Scanner::new(Provider::Anthropic, None).with_wire_context(Some(wire.clone()));
        s.feed_body(
            br#"{"model":"claude-sonnet-4-5","usage":{"input_tokens":24,"output_tokens":18}}"#,
        );
        let u = s.finalize().expect("usage");
        assert_eq!(u.wire, Some(wire));
    }

    fn managed_context() -> crate::core::ocla::OclaRequestContext {
        crate::core::ocla::OclaRequestContext {
            request_id: "req-1".into(),
            session_id: "session-1".into(),
            agent_id: "agent-1".into(),
            content_ref: "blake3:abc".into(),
            tenant_id: None,
        }
    }

    #[test]
    fn ocla_projection_includes_cache_tokens_once() {
        let usage = RealUsage {
            model: "gpt-5".into(),
            input_tokens: 100,
            output_tokens: 40,
            cache_read_tokens: 20,
            cache_write_tokens: 5,
            wire: Some(Box::new(WireContext {
                lineage: Some(managed_context()),
                ..Default::default()
            })),
            ..Default::default()
        };
        let record = usage
            .to_ocla_usage_record()
            .expect("valid projection")
            .expect("managed record");
        assert_eq!(record.input_tokens, 125);
        assert_eq!(record.output_tokens, 40);
        assert_eq!(record.provider_billed_tokens, 165);
        assert_eq!(record.context, managed_context());
    }

    #[test]
    fn ocla_projection_keeps_missing_lineage_unmanaged() {
        let usage = RealUsage {
            model: "gpt-5".into(),
            input_tokens: 1,
            ..Default::default()
        };
        assert_eq!(usage.to_ocla_usage_record().unwrap(), None);
    }

    #[test]
    fn ocla_projection_fails_closed_on_overflow_or_invalid_context() {
        let overflow = RealUsage {
            model: "gpt-5".into(),
            input_tokens: u64::MAX,
            cache_read_tokens: 1,
            wire: Some(Box::new(WireContext {
                lineage: Some(managed_context()),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(overflow.to_ocla_usage_record().is_err());

        let mut invalid = managed_context();
        invalid.request_id.clear();
        let invalid = RealUsage {
            model: "gpt-5".into(),
            wire: Some(Box::new(WireContext {
                lineage: Some(invalid),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(invalid.to_ocla_usage_record().is_err());
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
        assert_eq!(u.provider_cost_usd, None, "OpenAI reports no usage.cost");
    }

    #[test]
    fn openrouter_cost_and_cache_writes_are_measured() {
        // #1179: OpenRouter usage accounting — the final streamed chunk carries
        // the billed USD (`cost`) and the cache-write bucket inside
        // prompt_tokens_details. All three token buckets must stay disjoint.
        let u = feed_lines(
            Provider::OpenAi,
            None,
            &[
                r#"data: {"choices":[{"delta":{"content":"hi"}}],"model":"deepseek/deepseek-v4-flash-20260423"}"#,
                r#"data: {"choices":[],"model":"deepseek/deepseek-v4-flash-20260423","usage":{"prompt_tokens":700,"prompt_tokens_details":{"cached_tokens":150,"cache_write_tokens":50},"completion_tokens":40,"cost":0.0123,"cost_details":{"upstream_inference_cost":null},"total_tokens":740}}"#,
                "data: [DONE]",
            ],
        )
        .expect("usage");
        assert_eq!(u.input_tokens, 500, "700 - 150 cached - 50 cache-write");
        assert_eq!(u.cache_read_tokens, 150);
        assert_eq!(u.cache_write_tokens, 50);
        assert_eq!(u.output_tokens, 40);
        let cost = u.provider_cost_usd.expect("measured cost");
        assert!((cost - 0.0123).abs() < 1e-12);
    }

    #[test]
    fn openrouter_byok_adds_upstream_inference_cost() {
        let mut s = Scanner::new(Provider::OpenAi, None);
        s.feed_body(
            br#"{"model":"anthropic/claude-sonnet-5","usage":{"prompt_tokens":100,"completion_tokens":10,"cost":0.05,"cost_details":{"upstream_inference_cost":0.95}}}"#,
        );
        let u = s.finalize().expect("usage");
        let cost = u.provider_cost_usd.expect("measured cost");
        assert!(
            (cost - 1.0).abs() < 1e-12,
            "OpenRouter fee + BYOK upstream bill"
        );
    }

    /// #746: non-BYOK responses mirror `cost` in `upstream_inference_cost`.
    /// The gateway must not sum both — that would double-count.
    #[test]
    fn non_byok_upstream_equal_to_cost_is_not_doubled() {
        let mut s = Scanner::new(Provider::OpenAi, None);
        s.feed_body(
            br#"{"model":"deepseek/deepseek-v4-flash","usage":{"prompt_tokens":50,"completion_tokens":5,"cost":0.000001568,"cost_details":{"upstream_inference_cost":0.000001568}}}"#,
        );
        let u = s.finalize().expect("usage");
        let cost = u.provider_cost_usd.expect("measured cost");
        assert!(
            (cost - 0.000001568).abs() < 1e-15,
            "#746: must book cost once, not 2x; got {cost}"
        );
    }

    #[test]
    fn gateway_header_cost_fills_when_body_has_none() {
        // #1189: a LiteLLM-style gateway reports the bill in a header; tokens
        // come from a normal OpenAI-shaped body without `usage.cost`.
        let mut s = Scanner::new(Provider::OpenAi, None).with_header_cost(Some(0.0042));
        s.feed_body(
            br#"{"model":"azure-gpt-4o","usage":{"prompt_tokens":100,"completion_tokens":10}}"#,
        );
        let u = s.finalize().expect("usage");
        assert_eq!(u.provider_cost_usd, Some(0.0042), "header is measured");
    }

    #[test]
    fn body_reported_cost_beats_the_header_figure() {
        // OpenRouter behind another gateway: `usage.cost` is the bill itself.
        let mut s = Scanner::new(Provider::OpenAi, None).with_header_cost(Some(9.99));
        s.feed_body(
            br#"{"model":"deepseek/deepseek-v4-flash","usage":{"prompt_tokens":100,"completion_tokens":10,"cost":0.05}}"#,
        );
        let u = s.finalize().expect("usage");
        assert_eq!(u.provider_cost_usd, Some(0.05), "body wins over header");
    }

    #[test]
    fn openrouter_free_model_reports_zero_cost_as_measured() {
        let mut s = Scanner::new(Provider::OpenAi, None);
        s.feed_body(
            br#"{"model":"poolside/laguna-xs-2.1:free","usage":{"prompt_tokens":80,"completion_tokens":20,"cost":0}}"#,
        );
        let u = s.finalize().expect("usage");
        assert_eq!(u.provider_cost_usd, Some(0.0), "free is a price, not a gap");
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
