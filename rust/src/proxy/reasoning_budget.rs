//! Provider-specific reasoning budgets for trivially simple requests.

use crate::core::config::ReasoningBudgetConfig;
use serde_json::{Map, Value, json};

/// Applies the default reasoning budget to a simple, low-complexity request.
///
/// This convenience entry point uses the built-in configuration defaults. The
/// forwarding path uses [`apply_reasoning_budget_with_config`] so deployments
/// can opt out or select a different Anthropic thinking budget.
pub fn apply_reasoning_budget(body: &mut Value, task_class: &str, complexity: &str) {
    apply_reasoning_budget_with_config(
        body,
        task_class,
        complexity,
        &ReasoningBudgetConfig::default(),
    );
}

/// Applies a configured reasoning budget to a simple, low-complexity request.
///
/// Explicit provider settings are preserved: the proxy only supplies a value
/// when the client did not already select one.
pub fn apply_reasoning_budget_with_config(
    body: &mut Value,
    task_class: &str,
    complexity: &str,
    config: &ReasoningBudgetConfig,
) {
    if !config.enabled || task_class != "simple" || complexity != "low" {
        return;
    }

    let Some(request) = body.as_object_mut() else {
        return;
    };

    if is_anthropic_request(request) {
        apply_anthropic_budget(request, config.simple_task_budget);
    } else if is_openai_request(request) {
        request
            .entry("reasoning_effort")
            .or_insert_with(|| Value::String("low".into()));
    }
}

fn is_anthropic_request(request: &Map<String, Value>) -> bool {
    if is_openai_request(request) {
        return false;
    }

    request
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|model| model.to_ascii_lowercase().contains("claude"))
        || request.contains_key("anthropic_version")
        || (request.contains_key("max_tokens") && !request.contains_key("max_completion_tokens"))
}

fn is_openai_request(request: &Map<String, Value>) -> bool {
    request.contains_key("max_completion_tokens")
        || request
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| {
                let model = model.to_ascii_lowercase();
                model.starts_with("gpt-") || model.starts_with('o')
            })
}

fn apply_anthropic_budget(request: &mut Map<String, Value>, budget_tokens: u32) {
    if request
        .get("max_tokens")
        .and_then(Value::as_u64)
        .is_some_and(|max_tokens| max_tokens <= u64::from(budget_tokens))
    {
        return;
    }

    let thinking = request
        .entry("thinking")
        .or_insert_with(|| json!({"type": "enabled"}));
    let Some(thinking) = thinking.as_object_mut() else {
        return;
    };

    thinking
        .entry("type")
        .or_insert_with(|| Value::String("enabled".into()));
    thinking
        .entry("budget_tokens")
        .or_insert_with(|| Value::from(budget_tokens));
}

#[cfg(test)]
mod tests {
    use super::apply_reasoning_budget;
    use serde_json::json;

    #[test]
    fn simple_low_anthropic_task_gets_reduced_thinking_budget() {
        let mut body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 4096,
            "messages": []
        });

        apply_reasoning_budget(&mut body, "simple", "low");

        assert_eq!(body["thinking"]["budget_tokens"], json!(1024));
    }

    #[test]
    fn simple_low_openai_task_gets_low_reasoning_effort() {
        let mut body = json!({"model": "gpt-5.4", "max_tokens": 4096, "messages": []});

        apply_reasoning_budget(&mut body, "simple", "low");

        assert_eq!(body["reasoning_effort"], json!("low"));
    }

    #[test]
    fn non_simple_task_keeps_provider_request_unchanged() {
        let mut body = json!({"model": "claude-sonnet-4", "max_tokens": 4096});

        apply_reasoning_budget(&mut body, "coding", "low");

        assert!(body.get("thinking").is_none());
    }
}
