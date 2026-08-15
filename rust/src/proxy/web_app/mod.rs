#[allow(dead_code)]
pub(crate) mod conversation_tracker;
pub(crate) mod dashboard;
#[allow(dead_code)]
pub(crate) mod normalize;
#[cfg(test)]
mod proof_tests;

/// Recognized web-app AI providers (domain-detected, not path-detected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAppProvider {
    ClaudeWeb,
    ChatGptWeb,
    GeminiWeb,
}

/// A web-app request normalized to the canonical messages format that
/// the existing compression pipeline understands.
#[derive(Debug, Clone)]
pub struct NormalizedRequest {
    pub provider: WebAppProvider,
    pub messages: Vec<serde_json::Value>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub conversation_id: Option<String>,
    pub parent_message_id: Option<String>,
}

pub(crate) fn detect_web_provider(host: &str, path: &str) -> Option<WebAppProvider> {
    let _ = path;
    let host = host
        .trim()
        .trim_end_matches('.')
        .split(':')
        .next()
        .unwrap_or_default();

    if host.eq_ignore_ascii_case("claude.ai") {
        Some(WebAppProvider::ClaudeWeb)
    } else if host.eq_ignore_ascii_case("chat.openai.com")
        || host.eq_ignore_ascii_case("chatgpt.com")
    {
        Some(WebAppProvider::ChatGptWeb)
    } else if host.eq_ignore_ascii_case("gemini.google.com") {
        Some(WebAppProvider::GeminiWeb)
    } else {
        None
    }
}
