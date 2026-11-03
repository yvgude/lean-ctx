// SPDX-License-Identifier: Apache-2.0
//! Cross-shape translation Anthropic ↔ OpenAI (enterprise#16, feature `shape-xlat`).
//!
//! Lets a Claude client (`POST /v1/messages`) transparently use an
//! OpenAI-shape upstream (Azure AI Foundry, vLLM, Ollama, Groq…): the request
//! is rewritten Messages→Chat-Completions before it leaves, and the response —
//! streaming or not — is rewritten back so the caller's Anthropic SDK never
//! notices. Pure enabler for the router (`[proxy.routing]` cross-shape
//! targets), deliberately not a headline feature (scope gate `12` §6).
//!
//! Mapping summary:
//!
//! | Anthropic                              | OpenAI                                  |
//! |----------------------------------------|-----------------------------------------|
//! | `system` (string/blocks)               | leading `role:"system"` message         |
//! | `tool_use` block (assistant)           | `tool_calls[]` entry                    |
//! | `tool_result` block (user)             | `role:"tool"` message                   |
//! | `image` block (base64)                 | `image_url` part (data URL)             |
//! | `tools[].input_schema`                 | `tools[].function.parameters`           |
//! | `tool_choice {auto,any,tool}`          | `"auto"`, `"required"`, function pick   |
//! | `stop_sequences`                       | `stop`                                  |
//! | `metadata.user_id`                     | `user`                                  |
//! | `cache_control`                        | stripped (OpenAI caches implicitly)     |
//! | response `content[]` / SSE events      | `choices[0].message` / `delta` chunks   |
//! | `usage.prompt_tokens(_details.cached)` | `input_tokens` / `cache_read_…`         |
//!
//! Streaming is a stateful SSE-to-SSE rewrite ([`StreamXlat`]): OpenAI
//! `chat.completion.chunk` deltas become `message_start` /
//! `content_block_start|delta|stop` / `message_delta` / `message_stop` events.
//! Usage metering does NOT read the translated stream — the forward path scans
//! the raw upstream bytes with the OpenAI scanner before translation.

use serde_json::{Value, json};

// ─── Request: Anthropic Messages → OpenAI Chat Completions ──────────────────

/// Translates a parsed Anthropic Messages request into an OpenAI Chat
/// Completions body. `None` = not translatable (caller must fail open and
/// forward natively).
#[must_use]
pub fn messages_to_chat(anthropic: &Value) -> Option<Value> {
    let model = anthropic.get("model")?.as_str()?;
    let src_messages = anthropic.get("messages")?.as_array()?;

    let mut messages: Vec<Value> = Vec::with_capacity(src_messages.len() + 1);
    if let Some(system) = anthropic.get("system")
        && let Some(text) = system_text(system)
    {
        messages.push(json!({"role": "system", "content": text}));
    }
    for msg in src_messages {
        translate_message(msg, &mut messages)?;
    }

    let mut out = json!({"model": model, "messages": messages});
    let o = out.as_object_mut().expect("constructed as object");

    if let Some(v) = anthropic.get("max_tokens").and_then(Value::as_u64) {
        o.insert("max_tokens".into(), json!(v));
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = anthropic.get(key).and_then(Value::as_f64) {
            o.insert(key.into(), json!(v));
        }
    }
    if let Some(stops) = anthropic.get("stop_sequences").and_then(Value::as_array)
        && !stops.is_empty()
    {
        o.insert("stop".into(), Value::Array(stops.clone()));
    }
    if let Some(user) = anthropic
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
    {
        o.insert("user".into(), json!(user));
    }
    if let Some(tools) = anthropic.get("tools").and_then(Value::as_array) {
        let translated: Vec<Value> = tools.iter().filter_map(translate_tool).collect();
        if !translated.is_empty() {
            o.insert("tools".into(), Value::Array(translated));
        }
    }
    if let Some(choice) = anthropic.get("tool_choice")
        && let Some(mapped) = translate_tool_choice(choice)
    {
        o.insert("tool_choice".into(), mapped);
    }
    if anthropic.get("stream").and_then(Value::as_bool) == Some(true) {
        o.insert("stream".into(), json!(true));
        // The final chunk must carry usage — metering and the translated
        // message_delta both need it.
        o.insert("stream_options".into(), json!({"include_usage": true}));
    }
    Some(out)
}

/// Anthropic `system` is a plain string or an array of text blocks.
fn system_text(system: &Value) -> Option<String> {
    if let Some(s) = system.as_str() {
        return Some(s.to_string());
    }
    let parts: Vec<&str> = system
        .as_array()?
        .iter()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Translates one Anthropic message into 1..n OpenAI messages, appended to
/// `out`. `tool_result` blocks become individual `role:"tool"` messages (they
/// answer the assistant's `tool_calls` from the previous turn and must come
/// first); remaining user content follows as one user message.
fn translate_message(msg: &Value, out: &mut Vec<Value>) -> Option<()> {
    let role = msg.get("role")?.as_str()?;
    let content = msg.get("content")?;

    if let Some(text) = content.as_str() {
        out.push(json!({"role": role, "content": text}));
        return Some(());
    }
    let blocks = content.as_array()?;
    if role == "assistant" {
        translate_assistant_blocks(blocks, out)
    } else {
        translate_user_blocks(blocks, out);
        Some(())
    }
}

/// Assistant turn: text blocks join into `content`, `tool_use` blocks become
/// `tool_calls`; thinking blocks have no OpenAI equivalent and are dropped.
fn translate_assistant_blocks(blocks: &[Value], out: &mut Vec<Value>) -> Option<()> {
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                let args = b.get("input").cloned().unwrap_or_else(|| json!({}));
                tool_calls.push(json!({
                    "id": b.get("id").and_then(Value::as_str).unwrap_or_default(),
                    "type": "function",
                    "function": {
                        "name": b.get("name").and_then(Value::as_str).unwrap_or_default(),
                        "arguments": serde_json::to_string(&args).ok()?,
                    }
                }));
            }
            _ => {}
        }
    }
    let mut m = json!({"role": "assistant"});
    let mo = m.as_object_mut().expect("object");
    mo.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !tool_calls.is_empty() {
        mo.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    out.push(m);
    Some(())
}

/// User turn: `tool_result` blocks become `role:"tool"` messages (first — they
/// answer the previous assistant `tool_calls`), text/image parts follow as one
/// user message.
fn translate_user_blocks(blocks: &[Value], out: &mut Vec<Value>) {
    let mut parts: Vec<Value> = Vec::new();
    let mut plain_text_only = true;
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("tool_result") => {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": b.get("tool_use_id").and_then(Value::as_str).unwrap_or_default(),
                    "content": tool_result_text(b),
                }));
            }
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    parts.push(json!({"type": "text", "text": t}));
                }
            }
            Some("image") => {
                plain_text_only = false;
                if let (Some(mt), Some(data)) = (
                    b.pointer("/source/media_type").and_then(Value::as_str),
                    b.pointer("/source/data").and_then(Value::as_str),
                ) {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{mt};base64,{data}")}
                    }));
                }
            }
            _ => {}
        }
    }
    if !parts.is_empty() {
        let content = if plain_text_only {
            // Collapse to a plain string — maximum upstream compat.
            let joined: Vec<&str> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect();
            json!(joined.join("\n"))
        } else {
            Value::Array(parts)
        };
        out.push(json!({"role": "user", "content": content}));
    }
}

/// Flattens a `tool_result`'s content (string or text blocks) to plain text.
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn translate_tool(tool: &Value) -> Option<Value> {
    let name = tool.get("name")?.as_str()?;
    let mut function = json!({
        "name": name,
        "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"})),
    });
    if let Some(desc) = tool.get("description").and_then(Value::as_str) {
        function["description"] = json!(desc);
    }
    Some(json!({"type": "function", "function": function}))
}

fn translate_tool_choice(choice: &Value) -> Option<Value> {
    match choice.get("type").and_then(Value::as_str)? {
        "auto" => Some(json!("auto")),
        "any" => Some(json!("required")),
        "none" => Some(json!("none")),
        "tool" => {
            let name = choice.get("name")?.as_str()?;
            Some(json!({"type": "function", "function": {"name": name}}))
        }
        _ => None,
    }
}

// ─── Response (non-streaming): chat.completion → Anthropic message ──────────

/// Translates a full OpenAI `chat.completion` body into an Anthropic message.
/// `None` = body not recognizable (caller forwards it unchanged).
#[must_use]
pub fn chat_to_messages(openai: &Value) -> Option<Value> {
    let choice = openai.get("choices")?.as_array()?.first()?;
    let message = choice.get("message")?;

    let mut content: Vec<Value> = Vec::new();
    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        content.push(json!({"type": "text", "text": text}));
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let f = call.get("function")?;
            let args: Value = f
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
                "name": f.get("name").and_then(Value::as_str).unwrap_or_default(),
                "input": args,
            }));
        }
    }

    let finish = choice.get("finish_reason").and_then(Value::as_str);
    let id = openai.get("id").and_then(Value::as_str).unwrap_or("xlat");
    Some(json!({
        "id": format!("msg_{id}"),
        "type": "message",
        "role": "assistant",
        "model": openai.get("model").and_then(Value::as_str).unwrap_or_default(),
        "content": content,
        "stop_reason": stop_reason(finish),
        "stop_sequence": Value::Null,
        "usage": usage_to_anthropic(openai.get("usage")),
    }))
}

/// Maps an OpenAI error envelope (`{"error": {...}}`) to the Anthropic one
/// (`{"type":"error","error":{...}}`) so the caller's SDK renders the message.
#[must_use]
pub fn error_to_anthropic(openai: &Value) -> Option<Value> {
    let err = openai.get("error")?;
    let message = err.get("message").and_then(Value::as_str).unwrap_or("");
    let kind = match err.get("type").and_then(Value::as_str) {
        Some("insufficient_quota" | "rate_limit_error" | "requests" | "tokens") => {
            "rate_limit_error"
        }
        Some("invalid_request_error") => "invalid_request_error",
        Some("authentication_error") => "authentication_error",
        _ => "api_error",
    };
    Some(json!({
        "type": "error",
        "error": {"type": kind, "message": message},
    }))
}

fn stop_reason(finish: Option<&str>) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("tool_calls" | "function_call") => "tool_use",
        // stop / content_filter / unknown all end the turn.
        _ => "end_turn",
    }
}

fn usage_to_anthropic(usage: Option<&Value>) -> Value {
    let prompt = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .and_then(|u| u.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        // Anthropic reports input EXCLUDING cache reads; OpenAI's prompt_tokens
        // includes them.
        "input_tokens": prompt.saturating_sub(cached),
        "output_tokens": completion,
        "cache_read_input_tokens": cached,
        "cache_creation_input_tokens": 0,
    })
}

// ─── Response (streaming): chat.completion.chunk SSE → Anthropic SSE ────────

/// Which content block is currently open on the translated (Anthropic) side.
#[derive(Debug, PartialEq)]
enum OpenBlock {
    Text,
    /// OpenAI `tool_calls[].index` this block translates.
    Tool(u64),
}

/// Stateful OpenAI→Anthropic SSE translator. Feed raw upstream bytes, get
/// translated Anthropic event bytes; call [`StreamXlat::finish`] at stream end
/// to flush the closing events if the upstream never sent `[DONE]`.
#[derive(Default)]
pub struct StreamXlat {
    line_buf: Vec<u8>,
    started: bool,
    finished: bool,
    block_index: u64,
    open: Option<OpenBlock>,
    finish_reason: Option<String>,
    usage: Option<Value>,
}

/// Bound for a buffered partial SSE line (matches the usage scanner's guard).
const MAX_LINE_BYTES: usize = 1 << 20;

impl StreamXlat {
    /// Consumes one upstream chunk, returns the translated bytes (possibly empty).
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for byte in chunk {
            if *byte == b'\n' {
                let line = std::mem::take(&mut self.line_buf);
                self.process_line(&line, &mut out);
            } else if self.line_buf.len() < MAX_LINE_BYTES {
                self.line_buf.push(*byte);
            }
        }
        out
    }

    /// Flushes the closing events. Idempotent.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        self.emit_closing(&mut out);
        out
    }

    fn process_line(&mut self, line: &[u8], out: &mut Vec<u8>) {
        let line = std::str::from_utf8(line).unwrap_or("").trim();
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            return; // event:/comment/empty lines carry nothing we need
        };
        if data == "[DONE]" {
            self.emit_closing(out);
            return;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return;
        };

        // Usage-only final chunk (stream_options.include_usage).
        if let Some(usage) = chunk.get("usage")
            && !usage.is_null()
        {
            self.usage = Some(usage_to_anthropic(Some(usage)));
        }

        if !self.started {
            self.emit_message_start(&chunk, out);
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_string());
        }
        let Some(delta) = choice.get("delta") else {
            return;
        };

        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            if self.open != Some(OpenBlock::Text) {
                self.close_block(out);
                emit_event(
                    out,
                    "content_block_start",
                    &json!({
                        "type": "content_block_start",
                        "index": self.block_index,
                        "content_block": {"type": "text", "text": ""},
                    }),
                );
                self.open = Some(OpenBlock::Text);
            }
            emit_event(
                out,
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": self.block_index,
                    "delta": {"type": "text_delta", "text": text},
                }),
            );
        }

        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                self.process_tool_delta(call, out);
            }
        }
    }

    fn process_tool_delta(&mut self, call: &Value, out: &mut Vec<u8>) {
        let idx = call.get("index").and_then(Value::as_u64).unwrap_or(0);
        let name = call.pointer("/function/name").and_then(Value::as_str);

        // A named entry starts a new tool call; bare-arguments entries continue
        // the block already open for that index.
        let continues = self.open == Some(OpenBlock::Tool(idx)) && name.is_none();
        if !continues {
            self.close_block(out);
            emit_event(
                out,
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": self.block_index,
                    "content_block": {
                        "type": "tool_use",
                        "id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
                        "name": name.unwrap_or_default(),
                        "input": {},
                    },
                }),
            );
            self.open = Some(OpenBlock::Tool(idx));
        }
        if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str)
            && !args.is_empty()
        {
            emit_event(
                out,
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": self.block_index,
                    "delta": {"type": "input_json_delta", "partial_json": args},
                }),
            );
        }
    }

    fn emit_message_start(&mut self, chunk: &Value, out: &mut Vec<u8>) {
        self.started = true;
        let id = chunk.get("id").and_then(Value::as_str).unwrap_or("xlat");
        let model = chunk.get("model").and_then(Value::as_str).unwrap_or("");
        emit_event(
            out,
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": format!("msg_{id}"),
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    // Real numbers arrive with the final usage chunk and are
                    // delivered via message_delta (SDKs merge cumulatively).
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                },
            }),
        );
    }

    fn close_block(&mut self, out: &mut Vec<u8>) {
        if self.open.take().is_some() {
            emit_event(
                out,
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": self.block_index}),
            );
            self.block_index += 1;
        }
    }

    fn emit_closing(&mut self, out: &mut Vec<u8>) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.started {
            // Upstream produced nothing usable; still emit a valid envelope.
            self.emit_message_start(&json!({}), out);
        }
        self.close_block(out);
        let usage = self
            .usage
            .take()
            .unwrap_or_else(|| json!({"output_tokens": 0}));
        emit_event(
            out,
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": stop_reason(self.finish_reason.as_deref()),
                    "stop_sequence": Value::Null,
                },
                "usage": usage,
            }),
        );
        emit_event(out, "message_stop", &json!({"type": "message_stop"}));
    }
}

fn emit_event(out: &mut Vec<u8>, event: &str, data: &Value) {
    out.extend_from_slice(b"event: ");
    out.extend_from_slice(event.as_bytes());
    out.extend_from_slice(b"\ndata: ");
    out.extend_from_slice(data.to_string().as_bytes());
    out.extend_from_slice(b"\n\n");
}

/// Wraps an (already usage-teed) OpenAI SSE byte stream into the translated
/// Anthropic SSE stream. Chunks that translate to nothing are skipped; on
/// upstream end the closing events are flushed.
pub fn to_anthropic_stream<S, B, E>(
    inner: S,
) -> impl futures::Stream<Item = Result<Vec<u8>, E>> + Send + 'static
where
    S: futures::Stream<Item = Result<B, E>> + Send + Unpin + 'static,
    B: AsRef<[u8]> + Send + 'static,
    E: Send + 'static,
{
    use futures::StreamExt;
    futures::stream::unfold(
        (inner, StreamXlat::default(), false),
        |(mut inner, mut xlat, ended)| async move {
            if ended {
                return None;
            }
            loop {
                match inner.next().await {
                    Some(Ok(chunk)) => {
                        let translated = xlat.feed(chunk.as_ref());
                        if translated.is_empty() {
                            continue; // nothing translatable in this chunk yet
                        }
                        return Some((Ok(translated), (inner, xlat, false)));
                    }
                    Some(Err(e)) => return Some((Err(e), (inner, xlat, false))),
                    None => {
                        let tail = xlat.finish();
                        if tail.is_empty() {
                            return None;
                        }
                        return Some((Ok(tail), (inner, xlat, true)));
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Request direction ────────────────────────────────────────────────

    fn full_anthropic_request() -> Value {
        json!({
            "model": "gpt-4o-mini",
            "max_tokens": 1024,
            "temperature": 0.5,
            "stop_sequences": ["END"],
            "metadata": {"user_id": "u-42"},
            "system": [
                {"type": "text", "text": "You are helpful.", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [
                {"role": "user", "content": "What's the weather in Zurich?"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                     "input": {"city": "Zurich"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1",
                     "content": [{"type": "text", "text": "18°C, sunny"}]},
                    {"type": "text", "text": "And tomorrow?"}
                ]}
            ],
            "tools": [
                {"name": "get_weather", "description": "Weather lookup",
                 "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}},
                 "cache_control": {"type": "ephemeral"}}
            ],
            "tool_choice": {"type": "auto"},
            "stream": true
        })
    }

    #[test]
    fn request_maps_system_tools_and_history() {
        let out = messages_to_chat(&full_anthropic_request()).expect("translates");
        let messages = out["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "What's the weather in Zurich?");

        // Assistant turn: text + tool_use → content + tool_calls.
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "Let me check.");
        let call = &messages[2]["tool_calls"][0];
        assert_eq!(call["id"], "toolu_1");
        assert_eq!(call["function"]["name"], "get_weather");
        let args: Value = serde_json::from_str(call["function"]["arguments"].as_str().unwrap())
            .expect("arguments are a JSON string");
        assert_eq!(args["city"], "Zurich");

        // tool_result → role:"tool" message BEFORE the follow-up user text.
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "toolu_1");
        assert_eq!(messages[3]["content"], "18°C, sunny");
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"], "And tomorrow?");

        // Tools + params.
        assert_eq!(out["tools"][0]["type"], "function");
        assert_eq!(out["tools"][0]["function"]["name"], "get_weather");
        assert!(out["tools"][0]["function"]["parameters"]["properties"]["city"].is_object());
        assert_eq!(out["tool_choice"], "auto");
        assert_eq!(out["max_tokens"], 1024);
        assert_eq!(out["stop"][0], "END");
        assert_eq!(out["user"], "u-42");
        assert_eq!(out["stream"], true);
        assert_eq!(out["stream_options"]["include_usage"], true);
        // cache_control never crosses shapes.
        assert!(out.to_string().find("cache_control").is_none());
    }

    #[test]
    fn request_maps_images_and_forced_tool_choice() {
        let body = json!({
            "model": "gpt-4o", "max_tokens": 100,
            "tool_choice": {"type": "tool", "name": "extract"},
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "Describe this"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
            ]}]
        });
        let out = messages_to_chat(&body).unwrap();
        let content = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
        assert_eq!(out["tool_choice"]["function"]["name"], "extract");
    }

    #[test]
    fn untranslatable_bodies_return_none() {
        assert!(messages_to_chat(&json!({"model": "m"})).is_none());
        assert!(messages_to_chat(&json!({"messages": []})).is_none());
    }

    // ── Response direction (non-streaming) ───────────────────────────────

    #[test]
    fn response_maps_text_tools_and_cached_usage() {
        let openai = json!({
            "id": "chatcmpl-9x", "object": "chat.completion", "model": "gpt-4o-mini",
            "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": "Checking.",
                "tool_calls": [{"id": "call_7", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Zurich\"}"}}]
            }}],
            "usage": {"prompt_tokens": 120, "completion_tokens": 30,
                      "prompt_tokens_details": {"cached_tokens": 100}}
        });
        let msg = chat_to_messages(&openai).expect("translates");
        assert_eq!(msg["type"], "message");
        assert_eq!(msg["id"], "msg_chatcmpl-9x");
        assert_eq!(msg["model"], "gpt-4o-mini");
        assert_eq!(msg["stop_reason"], "tool_use");
        assert_eq!(msg["content"][0]["type"], "text");
        assert_eq!(msg["content"][0]["text"], "Checking.");
        assert_eq!(msg["content"][1]["type"], "tool_use");
        assert_eq!(msg["content"][1]["id"], "call_7");
        assert_eq!(msg["content"][1]["input"]["city"], "Zurich");
        // OpenAI prompt_tokens INCLUDES cached; Anthropic input_tokens excludes.
        assert_eq!(msg["usage"]["input_tokens"], 20);
        assert_eq!(msg["usage"]["output_tokens"], 30);
        assert_eq!(msg["usage"]["cache_read_input_tokens"], 100);
    }

    #[test]
    fn error_envelope_translates_to_anthropic_shape() {
        let openai = json!({"error": {"message": "quota exceeded",
            "type": "insufficient_quota", "code": "insufficient_quota"}});
        let anthropic = error_to_anthropic(&openai).unwrap();
        assert_eq!(anthropic["type"], "error");
        assert_eq!(anthropic["error"]["type"], "rate_limit_error");
        assert_eq!(anthropic["error"]["message"], "quota exceeded");
        assert!(error_to_anthropic(&json!({"ok": true})).is_none());
    }

    // ── Streaming direction ──────────────────────────────────────────────

    /// Collects `(event, data)` pairs from translated SSE bytes.
    fn parse_events(bytes: &[u8]) -> Vec<(String, Value)> {
        let text = std::str::from_utf8(bytes).unwrap();
        let mut events = Vec::new();
        let mut current_event = String::new();
        for line in text.lines() {
            if let Some(e) = line.strip_prefix("event: ") {
                current_event = e.to_string();
            } else if let Some(d) = line.strip_prefix("data: ") {
                events.push((current_event.clone(), serde_json::from_str(d).unwrap()));
            }
        }
        events
    }

    #[test]
    fn stream_translates_text_then_tool_call() {
        let mut xlat = StreamXlat::default();
        let mut out = Vec::new();
        for line in [
            r#"data: {"id":"c1","model":"gpt-4o-mini","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}"#,
            r#"data: {"id":"c1","choices":[{"index":0,"delta":{"content":"lo"}}]}"#,
            r#"data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}"#,
            r#"data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":\"ZRH\"}"}}]}}]}"#,
            r#"data: {"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"data: {"id":"c1","choices":[],"usage":{"prompt_tokens":50,"completion_tokens":12}}"#,
            "data: [DONE]",
        ] {
            out.extend(xlat.feed(line.as_bytes()));
            out.extend(xlat.feed(b"\n\n"));
        }

        let events = parse_events(&out);
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start", // text
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "content_block_start", // tool_use
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );

        assert_eq!(events[0].1["message"]["model"], "gpt-4o-mini");
        assert_eq!(events[2].1["delta"]["text"], "Hel");
        assert_eq!(events[3].1["delta"]["text"], "lo");
        let tool_start = &events[5].1;
        assert_eq!(tool_start["content_block"]["type"], "tool_use");
        assert_eq!(tool_start["content_block"]["id"], "call_1");
        assert_eq!(tool_start["content_block"]["name"], "get_weather");
        assert_eq!(tool_start["index"], 1);
        assert_eq!(events[6].1["delta"]["partial_json"], "{\"city\":\"ZRH\"}");
        let delta = &events[8].1;
        assert_eq!(delta["delta"]["stop_reason"], "tool_use");
        assert_eq!(delta["usage"]["input_tokens"], 50);
        assert_eq!(delta["usage"]["output_tokens"], 12);
    }

    #[test]
    fn stream_survives_chunk_splits_mid_line() {
        let full = concat!(
            r#"data: {"id":"c2","model":"m","choices":[{"index":0,"delta":{"content":"split works"}}]}"#,
            "\n\ndata: [DONE]\n\n"
        );
        let mut xlat = StreamXlat::default();
        let mut out = Vec::new();
        // Feed in pathological 7-byte slices.
        for chunk in full.as_bytes().chunks(7) {
            out.extend(xlat.feed(chunk));
        }
        let events = parse_events(&out);
        assert_eq!(events.first().unwrap().0, "message_start");
        assert!(
            events
                .iter()
                .any(|(e, d)| e == "content_block_delta" && d["delta"]["text"] == "split works")
        );
        assert_eq!(events.last().unwrap().0, "message_stop");
    }

    #[test]
    fn stream_without_done_is_closed_by_finish() {
        let mut xlat = StreamXlat::default();
        let mut out = xlat.feed(
            b"data: {\"id\":\"c3\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n",
        );
        out.extend(xlat.finish());
        out.extend(xlat.finish()); // idempotent
        let events = parse_events(&out);
        assert_eq!(events.last().unwrap().0, "message_stop");
        let delta = events.iter().find(|(e, _)| e == "message_delta").unwrap();
        assert_eq!(delta.1["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn stream_adapter_translates_and_flushes() {
        let chunks: Vec<Result<Vec<u8>, std::convert::Infallible>> = vec![
            Ok(br#"data: {"id":"c4","model":"m","choices":[{"index":0,"delta":{"content":"ok"}}]}"#.to_vec()),
            Ok(b"\n\n".to_vec()),
        ];
        let inner = futures::stream::iter(chunks);
        let translated = to_anthropic_stream(Box::pin(inner));
        let collected: Vec<_> =
            futures::executor::block_on(futures::StreamExt::collect::<Vec<_>>(translated));
        let bytes: Vec<u8> = collected.into_iter().flat_map(Result::unwrap).collect();
        let events = parse_events(&bytes);
        assert_eq!(events.first().unwrap().0, "message_start");
        assert_eq!(events.last().unwrap().0, "message_stop");
    }
}
