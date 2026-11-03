// SPDX-License-Identifier: Apache-2.0
use axum::http::request::Parts;

const INTENT_MESSAGE_CAP: usize = 2000;

#[derive(Clone, Debug)]
pub(super) struct ProxyIntentClassification {
    pub(super) _decision: lean_ctx::core::ocla::types::IntentDecision,
}

pub(super) fn classify_and_store_proxy_intent(
    parts: &mut Parts,
    parsed: Option<&serde_json::Value>,
    lineage: Option<&lean_ctx::core::ocla::types::OclaRequestContext>,
    body_bytes: &[u8],
) -> Option<ProxyIntentClassification> {
    let model = parsed
        .and_then(|value| value.get("model"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| super::usage::gemini_model_from_path(parts.uri.path()))
        .unwrap_or_else(|| "unknown".to_string());
    let message = parsed.and_then(proxy_intent_message);
    let candidate = match message.as_deref() {
        Some(message) => format!("model={model}; message={message}"),
        None => format!("model={model}"),
    };
    let context = lineage.cloned().unwrap_or_else(|| {
        let content_ref = format!("blake3:{}", blake3::hash(body_bytes).to_hex());
        lean_ctx::core::ocla::types::OclaRequestContext {
            request_id: format!("proxy-intent:{content_ref}"),
            session_id: "proxy".to_string(),
            agent_id: "proxy".to_string(),
            content_ref,
            tenant_id: None,
            trace_id: "tr-unit".into(),
            task_id: None,
            parent_task_id: None,
        }
    });
    let request = lean_ctx::core::ocla::types::IntentRequest {
        context,
        candidate_intents: vec![candidate],
    };
    let decision = match lean_ctx::core::ocla::OclaRegistry::global()
        .intent_classifier
        .classify_intent(request)
    {
        Ok(decision) => decision,
        Err(error) => {
            tracing::warn!("lean-ctx proxy intent classifier unavailable: {error:?}");
            return None;
        }
    };
    let classification = ProxyIntentClassification {
        _decision: decision,
    };
    parts.extensions.insert(classification.clone());
    Some(classification)
}

fn proxy_intent_message(parsed: &serde_json::Value) -> Option<String> {
    let items = parsed.get("messages").or_else(|| parsed.get("input"))?;
    if let Some(text) = items.as_str() {
        return bounded_intent_text(text);
    }
    let last_user = items
        .as_array()?
        .iter()
        .rev()
        .find(|item| item.get("role").and_then(serde_json::Value::as_str) == Some("user"))?;
    let content = last_user.get("content")?;
    if let Some(text) = content.as_str() {
        return bounded_intent_text(text);
    }
    let mut message = String::new();
    for part in content.as_array()? {
        if matches!(
            part.get("type").and_then(serde_json::Value::as_str),
            Some("text" | "input_text")
        ) && let Some(text) = part.get("text").and_then(serde_json::Value::as_str)
        {
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(text);
            if message.len() >= INTENT_MESSAGE_CAP {
                break;
            }
        }
    }
    bounded_intent_text(&message)
}

fn bounded_intent_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut end = trimmed.len().min(INTENT_MESSAGE_CAP);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    Some(trimmed[..end].to_string())
}
