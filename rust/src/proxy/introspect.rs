use serde::Serialize;
use serde_json::Value;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Anthropic,
    OpenAi,
    Gemini,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestBreakdown {
    pub provider: Provider,
    pub model: String,
    pub system_prompt_tokens: usize,
    pub user_message_tokens: usize,
    pub assistant_message_tokens: usize,
    pub tool_definition_tokens: usize,
    pub tool_definition_count: usize,
    pub tool_result_tokens: usize,
    pub image_count: usize,
    pub total_input_tokens: usize,
    pub message_count: usize,
    #[serde(default)]
    pub rules_tokens: usize,
    #[serde(default)]
    pub skills_tokens: usize,
    #[serde(default)]
    pub mcp_config_tokens: usize,
    #[serde(default)]
    pub subagent_tokens: usize,
    #[serde(default)]
    pub summarized_conversation_tokens: usize,
    #[serde(default)]
    pub conversation_tokens: usize,
}

#[must_use]
pub fn analyze_request(body: &Value, provider: Provider) -> RequestBreakdown {
    match provider {
        Provider::Anthropic => analyze_anthropic(body),
        Provider::OpenAi => analyze_openai(body),
        Provider::Gemini => analyze_gemini(body),
    }
}

/// IDE clients (Cursor, Copilot) often send routing IDs like "model-0", "model-4"
/// instead of real model names. We keep track of the last real model name per provider
/// and fall back to it when we see a generic routing ID.
fn normalize_model(raw: &str, provider: Provider) -> String {
    use std::sync::Mutex;
    static LAST_REAL: Mutex<[Option<String>; 3]> = Mutex::new([None, None, None]);

    let is_routing_id = raw.starts_with("model-") || raw == "unknown" || raw.is_empty();

    let idx = match provider {
        Provider::Anthropic => 0,
        Provider::OpenAi => 1,
        Provider::Gemini => 2,
    };

    if is_routing_id {
        if let Ok(guard) = LAST_REAL.lock()
            && let Some(ref real) = guard[idx]
        {
            return real.clone();
        }
        return raw.to_string();
    }

    if let Ok(mut guard) = LAST_REAL.lock() {
        guard[idx] = Some(raw.to_string());
    }
    raw.to_string()
}

fn analyze_anthropic(body: &Value) -> RequestBreakdown {
    let raw_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown");
    let model = normalize_model(raw_model, Provider::Anthropic);

    let mut system_prompt_tokens = 0;
    let mut rules_tokens = 0;
    let mut skills_tokens = 0;
    let mut mcp_config_tokens = 0;

    match body.get("system") {
        Some(Value::String(s)) => {
            let sp = classify_system_prompt(s);
            system_prompt_tokens = sp.base;
            rules_tokens = sp.rules;
            skills_tokens = sp.skills;
            mcp_config_tokens = sp.mcp;
        }
        Some(Value::Array(arr)) => {
            for block in arr {
                let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let sp = classify_system_prompt(text);
                system_prompt_tokens += sp.base;
                rules_tokens += sp.rules;
                skills_tokens += sp.skills;
                mcp_config_tokens += sp.mcp;
            }
        }
        _ => {}
    }

    let tool_definition_tokens = body
        .get("tools")
        .and_then(|t| t.as_array())
        .map_or(0, |arr| json_chars(arr) / 4);

    let tool_definition_count = body
        .get("tools")
        .and_then(|t| t.as_array())
        .map_or(0, Vec::len);

    let mut user_message_tokens = 0;
    let mut assistant_message_tokens = 0;
    let mut tool_result_tokens = 0;
    let mut image_count = 0;
    let mut message_count = 0;
    let mut subagent_tokens = 0;
    let mut summarized_conversation_tokens = 0;

    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        message_count = messages.len();
        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let content_tokens = estimate_content_tokens(msg.get("content"));
            let has_images = count_images(msg.get("content"));
            image_count += has_images;

            match role {
                "user" => {
                    if has_tool_results(msg.get("content")) {
                        tool_result_tokens += content_tokens;
                    } else if is_summary_message(msg.get("content")) {
                        summarized_conversation_tokens += content_tokens;
                    } else if is_subagent_message(msg.get("content")) {
                        subagent_tokens += content_tokens;
                    } else {
                        user_message_tokens += content_tokens;
                    }
                }
                "assistant" => assistant_message_tokens += content_tokens,
                _ => user_message_tokens += content_tokens,
            }
        }
    }

    let conversation_tokens = user_message_tokens + assistant_message_tokens;

    let total_input_tokens = system_prompt_tokens
        + rules_tokens
        + skills_tokens
        + mcp_config_tokens
        + user_message_tokens
        + assistant_message_tokens
        + tool_definition_tokens
        + tool_result_tokens
        + subagent_tokens
        + summarized_conversation_tokens;

    RequestBreakdown {
        provider: Provider::Anthropic,
        model,
        system_prompt_tokens,
        user_message_tokens,
        assistant_message_tokens,
        tool_definition_tokens,
        tool_definition_count,
        tool_result_tokens,
        image_count,
        total_input_tokens,
        message_count,
        rules_tokens,
        skills_tokens,
        mcp_config_tokens,
        subagent_tokens,
        summarized_conversation_tokens,
        conversation_tokens,
    }
}

fn analyze_openai(body: &Value) -> RequestBreakdown {
    // The Responses API (`/v1/responses`) carries its turns in `input` instead
    // of `messages` and its system prompt in `instructions`. Detect that shape
    // and analyze it separately so introspection stays accurate for opencode and
    // the OpenAI Agents SDK rather than reporting an empty breakdown.
    if body.get("messages").is_none()
        && (body.get("input").is_some() || body.get("instructions").is_some())
    {
        return analyze_openai_responses(body);
    }

    let raw_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown");
    let model = normalize_model(raw_model, Provider::OpenAi);

    let mut system_prompt_tokens = 0;
    let mut rules_tokens = 0;
    let mut skills_tokens = 0;
    let mut mcp_config_tokens = 0;
    let mut user_message_tokens = 0;
    let mut assistant_message_tokens = 0;
    let mut tool_result_tokens = 0;
    let mut image_count = 0;
    let mut message_count = 0;
    let mut subagent_tokens = 0;
    let mut summarized_conversation_tokens = 0;

    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        message_count = messages.len();
        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let content_tokens = estimate_content_tokens(msg.get("content"));
            image_count += count_images(msg.get("content"));

            match role {
                "system" | "developer" => {
                    let text = extract_text_content(msg.get("content"));
                    let sp = classify_system_prompt(&text);
                    system_prompt_tokens += sp.base;
                    rules_tokens += sp.rules;
                    skills_tokens += sp.skills;
                    mcp_config_tokens += sp.mcp;
                }
                "assistant" => assistant_message_tokens += content_tokens,
                "tool" => tool_result_tokens += content_tokens,
                _ => {
                    if is_summary_message(msg.get("content")) {
                        summarized_conversation_tokens += content_tokens;
                    } else if is_subagent_message(msg.get("content")) {
                        subagent_tokens += content_tokens;
                    } else {
                        user_message_tokens += content_tokens;
                    }
                }
            }
        }
    }

    let tool_definition_tokens = body
        .get("tools")
        .and_then(|t| t.as_array())
        .map_or(0, |arr| json_chars(arr) / 4);

    let tool_definition_count = body
        .get("tools")
        .and_then(|t| t.as_array())
        .map_or(0, Vec::len);

    let conversation_tokens = user_message_tokens + assistant_message_tokens;

    let total_input_tokens = system_prompt_tokens
        + rules_tokens
        + skills_tokens
        + mcp_config_tokens
        + user_message_tokens
        + assistant_message_tokens
        + tool_definition_tokens
        + tool_result_tokens
        + subagent_tokens
        + summarized_conversation_tokens;

    RequestBreakdown {
        provider: Provider::OpenAi,
        model,
        system_prompt_tokens,
        user_message_tokens,
        assistant_message_tokens,
        tool_definition_tokens,
        tool_definition_count,
        tool_result_tokens,
        image_count,
        total_input_tokens,
        message_count,
        rules_tokens,
        skills_tokens,
        mcp_config_tokens,
        subagent_tokens,
        summarized_conversation_tokens,
        conversation_tokens,
    }
}

/// Analyze an `OpenAI` **Responses API** request (`/v1/responses`).
///
/// Shape differs from Chat Completions: the system prompt lives in
/// `instructions`, and conversation turns live in `input` — either a bare string
/// (single user turn) or an array of typed items (`message`, `function_call`,
/// `function_call_output`, `reasoning`, …). We map those onto the same
/// [`RequestBreakdown`] buckets the other providers use.
fn analyze_openai_responses(body: &Value) -> RequestBreakdown {
    let raw_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown");
    let model = normalize_model(raw_model, Provider::OpenAi);

    let mut system_prompt_tokens = 0;
    let mut rules_tokens = 0;
    let mut skills_tokens = 0;
    let mut mcp_config_tokens = 0;
    let mut user_message_tokens = 0;
    let mut assistant_message_tokens = 0;
    let mut tool_result_tokens = 0;
    let mut image_count = 0;
    let mut message_count = 0;
    let mut subagent_tokens = 0;
    let mut summarized_conversation_tokens = 0;

    if let Some(instructions) = body.get("instructions").and_then(|i| i.as_str()) {
        let sp = classify_system_prompt(instructions);
        system_prompt_tokens += sp.base;
        rules_tokens += sp.rules;
        skills_tokens += sp.skills;
        mcp_config_tokens += sp.mcp;
    }

    match body.get("input") {
        Some(Value::String(s)) => {
            message_count = 1;
            user_message_tokens += chars_to_tokens(s.len());
        }
        Some(Value::Array(items)) => {
            message_count = items.len();
            for item in items {
                // Items default to "message" when no explicit `type` is present.
                let item_type = item
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("message");
                match item_type {
                    "function_call_output" => {
                        tool_result_tokens += estimate_content_tokens(item.get("output"));
                    }
                    "function_call" | "custom_tool_call" | "reasoning" => {
                        // The model's own tool invocations / reasoning.
                        assistant_message_tokens += json_chars(std::slice::from_ref(item)) / 4;
                    }
                    _ => {
                        let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                        let content = item.get("content");
                        let content_tokens = estimate_content_tokens(content);
                        image_count += count_images(content);
                        match role {
                            "system" | "developer" => {
                                let text = extract_text_content(content);
                                let sp = classify_system_prompt(&text);
                                system_prompt_tokens += sp.base;
                                rules_tokens += sp.rules;
                                skills_tokens += sp.skills;
                                mcp_config_tokens += sp.mcp;
                            }
                            "assistant" => assistant_message_tokens += content_tokens,
                            _ => {
                                if is_summary_message(content) {
                                    summarized_conversation_tokens += content_tokens;
                                } else if is_subagent_message(content) {
                                    subagent_tokens += content_tokens;
                                } else {
                                    user_message_tokens += content_tokens;
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }

    let tool_definition_tokens = body
        .get("tools")
        .and_then(|t| t.as_array())
        .map_or(0, |arr| json_chars(arr) / 4);

    let tool_definition_count = body
        .get("tools")
        .and_then(|t| t.as_array())
        .map_or(0, Vec::len);

    let conversation_tokens = user_message_tokens + assistant_message_tokens;

    let total_input_tokens = system_prompt_tokens
        + rules_tokens
        + skills_tokens
        + mcp_config_tokens
        + user_message_tokens
        + assistant_message_tokens
        + tool_definition_tokens
        + tool_result_tokens
        + subagent_tokens
        + summarized_conversation_tokens;

    RequestBreakdown {
        provider: Provider::OpenAi,
        model,
        system_prompt_tokens,
        user_message_tokens,
        assistant_message_tokens,
        tool_definition_tokens,
        tool_definition_count,
        tool_result_tokens,
        image_count,
        total_input_tokens,
        message_count,
        rules_tokens,
        skills_tokens,
        mcp_config_tokens,
        subagent_tokens,
        summarized_conversation_tokens,
        conversation_tokens,
    }
}

fn analyze_gemini(body: &Value) -> RequestBreakdown {
    let model = "gemini".to_string();

    let system_prompt_tokens = body
        .get("systemInstruction")
        .and_then(|si| si.get("parts"))
        .and_then(|p| p.as_array())
        .map_or(0, |parts| {
            parts
                .iter()
                .map(|p| p.get("text").and_then(|t| t.as_str()).map_or(0, str::len))
                .sum::<usize>()
                / 4
        });

    let mut user_message_tokens = 0;
    let mut assistant_message_tokens = 0;
    let mut tool_result_tokens = 0;
    let mut message_count = 0;

    if let Some(contents) = body.get("contents").and_then(|c| c.as_array()) {
        message_count = contents.len();
        for content in contents {
            let role = content
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("user");
            let parts_tokens = content
                .get("parts")
                .and_then(|p| p.as_array())
                .map_or(0, |parts| {
                    parts
                        .iter()
                        .map(|p| {
                            if p.get("functionResponse").is_some() {
                                json_chars(std::slice::from_ref(p)) / 4
                            } else {
                                p.get("text")
                                    .and_then(|t| t.as_str())
                                    .map_or(0, |s| chars_to_tokens(s.len()))
                            }
                        })
                        .sum::<usize>()
                });

            let has_fn_response = content
                .get("parts")
                .and_then(|p| p.as_array())
                .is_some_and(|parts| parts.iter().any(|p| p.get("functionResponse").is_some()));

            if has_fn_response {
                tool_result_tokens += parts_tokens;
            } else {
                match role {
                    "model" => assistant_message_tokens += parts_tokens,
                    _ => user_message_tokens += parts_tokens,
                }
            }
        }
    }

    let tool_definition_tokens = body
        .get("tools")
        .and_then(|t| t.as_array())
        .map_or(0, |arr| json_chars(arr) / 4);

    let tool_definition_count = body
        .get("tools")
        .and_then(|t| t.as_array())
        .map_or(0, |arr| {
            arr.iter()
                .filter_map(|t| t.get("functionDeclarations").and_then(|f| f.as_array()))
                .map(Vec::len)
                .sum()
        });

    let total_input_tokens = system_prompt_tokens
        + user_message_tokens
        + assistant_message_tokens
        + tool_definition_tokens
        + tool_result_tokens;

    let conversation_tokens = user_message_tokens + assistant_message_tokens;

    RequestBreakdown {
        provider: Provider::Gemini,
        model,
        system_prompt_tokens,
        user_message_tokens,
        assistant_message_tokens,
        tool_definition_tokens,
        tool_definition_count,
        tool_result_tokens,
        image_count: 0,
        total_input_tokens,
        message_count,
        rules_tokens: 0,
        skills_tokens: 0,
        mcp_config_tokens: 0,
        subagent_tokens: 0,
        summarized_conversation_tokens: 0,
        conversation_tokens,
    }
}

fn chars_to_tokens(chars: usize) -> usize {
    chars / 4
}

fn json_chars(arr: &[Value]) -> usize {
    arr.iter().map(|v| v.to_string().len()).sum()
}

fn estimate_content_tokens(content: Option<&Value>) -> usize {
    match content {
        Some(Value::String(s)) => chars_to_tokens(s.len()),
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|block| {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    chars_to_tokens(text.len())
                } else {
                    block.to_string().len() / 4
                }
            })
            .sum(),
        Some(v) => v.to_string().len() / 4,
        None => 0,
    }
}

fn count_images(content: Option<&Value>) -> usize {
    match content {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter(|block| {
                matches!(
                    block.get("type").and_then(|t| t.as_str()),
                    // "image"/"image_url": Chat Completions; "input_image": Responses API.
                    Some("image" | "image_url" | "input_image")
                )
            })
            .count(),
        _ => 0,
    }
}

struct SystemPromptParts {
    base: usize,
    rules: usize,
    skills: usize,
    mcp: usize,
}

fn classify_system_prompt(text: &str) -> SystemPromptParts {
    let mut rules = 0usize;
    let mut skills = 0usize;
    let mut mcp = 0usize;
    let mut base = 0usize;

    let rule_markers = [
        "<always_applied_workspace_rule",
        "<user_rule",
        ".cursorrules",
        "AGENTS.md",
        ".mdc",
        "workspace_rule",
        "cursor_rules",
        "CLAUDE.md",
        "<rules>",
    ];
    let skill_markers = [
        "<agent_skill",
        "<available_skills",
        "SKILL.md",
        "skills-cursor",
        "agent_skills",
    ];
    let mcp_markers = [
        "<mcp_file_system",
        "mcp_server",
        "MCP server",
        "CallMcpTool",
        "FetchMcpResource",
        "<mcp_file_system_server",
    ];

    for line in text.lines() {
        let tok = chars_to_tokens(line.len() + 1);
        let l = line.trim();

        if rule_markers.iter().any(|m| l.contains(m)) {
            rules += tok;
        } else if skill_markers.iter().any(|m| l.contains(m)) {
            skills += tok;
        } else if mcp_markers.iter().any(|m| l.contains(m)) {
            mcp += tok;
        } else {
            base += tok;
        }
    }

    SystemPromptParts {
        base,
        rules,
        skills,
        mcp,
    }
}

fn is_summary_message(content: Option<&Value>) -> bool {
    let text = extract_text_content(content);
    text.contains("[Previous conversation summary]")
        || text.contains("conversation summary")
        || text.contains("Here is a summary of the conversation")
        || text.contains("summarized conversation")
}

fn is_subagent_message(content: Option<&Value>) -> bool {
    let text = extract_text_content(content);
    text.contains("subagent")
        || text.contains("background agent")
        || text.contains("<task>")
        || text.contains("system_notification")
}

fn extract_text_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn has_tool_results(content: Option<&Value>) -> bool {
    match content {
        Some(Value::Array(arr)) => arr
            .iter()
            .any(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result")),
        _ => false,
    }
}

pub struct IntrospectState {
    pub last_breakdown: Mutex<Option<RequestBreakdown>>,
    pub total_system_prompt_tokens: AtomicU64,
    pub total_requests: AtomicU64,
    last_persist_epoch: AtomicU64,
}

impl Default for IntrospectState {
    fn default() -> Self {
        Self {
            last_breakdown: Mutex::new(None),
            total_system_prompt_tokens: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            last_persist_epoch: AtomicU64::new(0),
        }
    }
}

impl IntrospectState {
    pub fn record(&self, breakdown: RequestBreakdown) {
        self.total_system_prompt_tokens.fetch_add(
            (breakdown.system_prompt_tokens
                + breakdown.rules_tokens
                + breakdown.skills_tokens
                + breakdown.mcp_config_tokens) as u64,
            Ordering::Relaxed,
        );
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut last) = self.last_breakdown.lock() {
            *last = Some(breakdown);
        }
        self.maybe_persist();
    }

    fn maybe_persist(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let prev = self.last_persist_epoch.load(Ordering::Relaxed);
        if now <= prev {
            return;
        }
        if self
            .last_persist_epoch
            .compare_exchange(prev, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        self.persist(now);
    }

    fn persist(&self, ts: u64) {
        let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
            return;
        };
        let breakdown_val = self
            .last_breakdown
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|b| serde_json::to_value(b).ok()))
            .flatten();
        let payload = serde_json::json!({
            "ts": ts,
            "proxy_active": true,
            "last_breakdown": breakdown_val,
            "cumulative": {
                "total_requests": self.total_requests.load(Ordering::Relaxed),
                "total_system_prompt_tokens": self.total_system_prompt_tokens.load(Ordering::Relaxed),
            }
        });

        let target = data_dir.join("proxy-introspect.json");
        let tmp = data_dir.join("proxy-introspect.json.tmp");
        if let Ok(json) = serde_json::to_string_pretty(&payload)
            && std::fs::write(&tmp, &json).is_ok()
        {
            let _ = std::fs::rename(&tmp, &target);
        }
    }
}

/// Load persisted proxy introspection data from disk.
/// Returns `None` if the file doesn't exist or is stale (> `max_age_secs`).
pub fn load_persisted(max_age_secs: u64) -> Option<serde_json::Value> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let path = data_dir.join("proxy-introspect.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;

    let ts = val
        .get("ts")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.saturating_sub(ts) > max_age_secs {
        return None;
    }
    Some(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_basic() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are a helpful assistant.",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"}
            ],
            "tools": [{"name": "read", "description": "Read a file", "input_schema": {}}]
        });
        let b = analyze_request(&body, Provider::Anthropic);
        assert_eq!(b.provider, Provider::Anthropic);
        assert!(b.system_prompt_tokens > 0);
        assert_eq!(b.message_count, 2);
        assert!(b.user_message_tokens > 0);
        assert!(b.assistant_message_tokens > 0);
        assert_eq!(b.tool_definition_count, 1);
        assert!(b.tool_definition_tokens > 0);
    }

    #[test]
    fn openai_system_message() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "System prompt here"},
                {"role": "user", "content": "Hello"},
                {"role": "tool", "content": "tool result data", "tool_call_id": "x"}
            ]
        });
        let b = analyze_request(&body, Provider::OpenAi);
        assert!(b.system_prompt_tokens > 0);
        assert!(b.user_message_tokens > 0);
        assert!(b.tool_result_tokens > 0);
        assert_eq!(b.message_count, 3);
    }

    #[test]
    fn openai_responses_api_shape() {
        // `/v1/responses`: system prompt in `instructions`, turns in `input`.
        let body = serde_json::json!({
            "model": "gpt-5",
            "instructions": "You are a careful coding assistant.",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "List the files"},
                    {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
                ]},
                {"type": "function_call", "call_id": "c1", "name": "ls", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": "a.rs\nb.rs\nc.rs"}
            ],
            "tools": [{"type": "function", "name": "ls", "parameters": {}}]
        });
        let b = analyze_request(&body, Provider::OpenAi);
        assert_eq!(b.provider, Provider::OpenAi);
        assert!(b.system_prompt_tokens > 0, "instructions → system prompt");
        assert!(b.user_message_tokens > 0, "user input_text counted");
        assert!(b.assistant_message_tokens > 0, "function_call → assistant");
        assert!(
            b.tool_result_tokens > 0,
            "function_call_output → tool result"
        );
        assert_eq!(b.tool_definition_count, 1);
        assert!(b.tool_definition_tokens > 0);
        assert_eq!(b.image_count, 1, "input_image counted");
        assert_eq!(b.message_count, 3);
    }

    #[test]
    fn openai_responses_string_input() {
        let body = serde_json::json!({"model": "gpt-5", "input": "just a question"});
        let b = analyze_request(&body, Provider::OpenAi);
        assert_eq!(b.provider, Provider::OpenAi);
        assert!(b.user_message_tokens > 0);
        assert_eq!(b.message_count, 1);
    }

    #[test]
    fn gemini_system_instruction() {
        let body = serde_json::json!({
            "systemInstruction": {
                "parts": [{"text": "Be concise and helpful to the user at all times."}]
            },
            "contents": [
                {"role": "user", "parts": [{"text": "What is the meaning of life and everything?"}]},
                {"role": "model", "parts": [{"text": "The answer is 42 according to Douglas Adams."}]}
            ]
        });
        let b = analyze_request(&body, Provider::Gemini);
        assert!(b.system_prompt_tokens > 0);
        assert!(b.user_message_tokens > 0);
        assert!(b.assistant_message_tokens > 0);
        assert_eq!(b.message_count, 2);
    }
}
