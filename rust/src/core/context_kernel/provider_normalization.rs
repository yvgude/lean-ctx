//! Provider-specific usage normalization for execution receipts.
//!
//! Provider APIs do not agree on field names, and a response can be a stream of
//! partial usage observations rather than one JSON object.  This module keeps
//! the provider differences at the boundary and deliberately represents absent
//! observations as `None`.  The receipt builder can then decide how to project
//! those observations onto the existing wire contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider-neutral usage for one logical model invocation.
///
/// Token and cost fields are optional because an API may omit a field or omit
/// usage entirely.  An absent field is not equivalent to a provider reporting
/// zero.  `provider` is the normalized route label supplied by the caller and
/// `model` is empty when the response did not identify a serving model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_model_calls: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    /// Provider-reported charge, represented as USD micros when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

impl NormalizedUsage {
    /// Creates a complete observation without introducing floating-point cost.
    #[must_use]
    pub fn complete(
        provider: impl Into<String>,
        model: impl Into<String>,
        fresh_input_tokens: u64,
        cached_input_tokens: u64,
        output_tokens: u64,
        reasoning_tokens: u64,
    ) -> Self {
        Self {
            fresh_input_tokens: Some(fresh_input_tokens),
            cached_input_tokens: Some(cached_input_tokens),
            output_tokens: Some(output_tokens),
            reasoning_tokens: Some(reasoning_tokens),
            total_model_calls: Some(1),
            retries: Some(0),
            provider: provider.into(),
            model: model.into(),
            ..Self::default()
        }
    }

    /// Returns whether at least one usage fact was observed.
    #[must_use]
    pub fn has_observation(&self) -> bool {
        self.fresh_input_tokens.is_some()
            || self.cached_input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.reasoning_tokens.is_some()
            || self.provider_cost_micros.is_some()
    }

    /// Adds independent observations while preserving unknown fields.
    ///
    /// If any contributing observation is missing a field, the aggregate for
    /// that field remains unknown.  This prevents a partial provider response
    /// from being made to look complete by aggregation.
    pub fn merge_additive(&mut self, other: &Self) {
        if !self.has_observation()
            && self.provider.is_empty()
            && self.model.is_empty()
            && self.requested_model.is_none()
        {
            *self = other.clone();
            return;
        }
        merge_optional_sum(&mut self.fresh_input_tokens, other.fresh_input_tokens);
        merge_optional_sum(&mut self.cached_input_tokens, other.cached_input_tokens);
        merge_optional_sum(&mut self.output_tokens, other.output_tokens);
        merge_optional_sum(&mut self.reasoning_tokens, other.reasoning_tokens);
        merge_optional_sum(&mut self.total_model_calls, other.total_model_calls);
        merge_optional_sum(&mut self.retries, other.retries);
        merge_optional_sum(&mut self.provider_cost_micros, other.provider_cost_micros);
        merge_optional_sum(&mut self.latency_ms, other.latency_ms);

        if self.provider.is_empty() {
            self.provider.clone_from(&other.provider);
        } else if !other.provider.is_empty() && self.provider != other.provider {
            self.provider = String::from("mixed");
        }
        if self.model.is_empty() {
            self.model.clone_from(&other.model);
        } else if !other.model.is_empty() && self.model != other.model {
            self.model = String::from("mixed");
        }
        if self.requested_model.is_none() {
            self.requested_model.clone_from(&other.requested_model);
        }
    }

    /// Applies the newest observation from a streaming response.
    ///
    /// Streaming usage objects are commonly cumulative.  The newest known
    /// value therefore replaces an earlier value instead of being added to it.
    pub fn replace_known_from(&mut self, other: &Self) {
        replace_if_some(&mut self.fresh_input_tokens, other.fresh_input_tokens);
        replace_if_some(&mut self.cached_input_tokens, other.cached_input_tokens);
        replace_if_some(&mut self.output_tokens, other.output_tokens);
        replace_if_some(&mut self.reasoning_tokens, other.reasoning_tokens);
        replace_if_some(&mut self.total_model_calls, other.total_model_calls);
        replace_if_some(&mut self.retries, other.retries);
        replace_if_some(&mut self.provider_cost_micros, other.provider_cost_micros);
        replace_if_some(&mut self.latency_ms, other.latency_ms);
        if !other.provider.is_empty() {
            self.provider.clone_from(&other.provider);
        }
        if !other.model.is_empty() {
            self.model.clone_from(&other.model);
        }
        if other.requested_model.is_some() {
            self.requested_model.clone_from(&other.requested_model);
        }
    }
}

fn merge_optional_sum<T>(current: &mut Option<T>, incoming: Option<T>)
where
    T: Copy + std::ops::Add<Output = T>,
{
    *current = match (*current, incoming) {
        (Some(left), Some(right)) => Some(left + right),
        _ => None,
    };
}

fn replace_if_some<T: Copy>(current: &mut Option<T>, incoming: Option<T>) {
    if incoming.is_some() {
        *current = incoming;
    }
}

fn provider_base(provider: &str, requested_model: Option<&str>) -> NormalizedUsage {
    NormalizedUsage {
        provider: canonical_provider(provider),
        requested_model: requested_model
            .filter(|model| !model.trim().is_empty())
            .map(str::to_owned),
        ..NormalizedUsage::default()
    }
}

fn finish(mut usage: NormalizedUsage, retries: u32, observed: bool) -> NormalizedUsage {
    if observed {
        usage.total_model_calls = Some(1);
        usage.retries = Some(retries);
    }
    usage
}

fn canonical_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai" | "chatgpt" | "azure" | "azure_openai" | "openai_responses" => "openai".to_owned(),
        "anthropic" | "claude" | "bedrock" => "anthropic".to_owned(),
        "gemini" | "google" | "google_ai" => "gemini".to_owned(),
        "openrouter" => "openrouter".to_owned(),
        "orcarouter" => "orcarouter".to_owned(),
        other => other.to_owned(),
    }
}

fn usage_value<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| value.get(*name))
}

fn optional_u64(value: &Value, names: &[&str]) -> Option<u64> {
    usage_value(value, names).and_then(Value::as_u64)
}

fn optional_model(value: &Value) -> Option<String> {
    value
        .get("model")
        .or_else(|| value.get("modelVersion"))
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
}

fn openai_root(value: &Value) -> &Value {
    value.get("response").unwrap_or(value)
}

fn provider_usage(value: &Value) -> Option<&Value> {
    value.get("usage").filter(|usage| !usage.is_null())
}

/// Normalizes an OpenAI Chat Completions, Responses, or OpenRouter object.
#[must_use]
pub fn normalize_openai(
    value: &Value,
    requested_model: Option<&str>,
    retries: u32,
) -> NormalizedUsage {
    let root = openai_root(value);
    let mut result = provider_base("openai", requested_model);
    result.model = optional_model(root).unwrap_or_default();
    let Some(usage) = provider_usage(root) else {
        return result;
    };

    let input_details = usage
        .get("input_tokens_details")
        .or_else(|| usage.get("prompt_tokens_details"));
    let cached = input_details
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64);
    let cache_write = input_details
        .and_then(|details| details.get("cache_write_tokens"))
        .and_then(Value::as_u64);
    let total_input = optional_u64(usage, &["input_tokens", "prompt_tokens"]);
    result.fresh_input_tokens = total_input.map(|total| {
        total
            .saturating_sub(cached.unwrap_or(0))
            .saturating_sub(cache_write.unwrap_or(0))
    });
    result.cached_input_tokens = cached;
    result.output_tokens = optional_u64(usage, &["output_tokens", "completion_tokens"]);
    result.reasoning_tokens = usage
        .get("output_tokens_details")
        .or_else(|| usage.get("completion_tokens_details"))
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64);
    result.provider_cost_micros = openrouter_cost_micros(usage);
    let observed = total_input.is_some()
        || result.output_tokens.is_some()
        || result.provider_cost_micros.is_some();
    finish(result, retries, observed)
}

/// Normalizes an Anthropic message, message stream event, or non-streaming body.
#[must_use]
pub fn normalize_anthropic(
    value: &Value,
    requested_model: Option<&str>,
    retries: u32,
) -> NormalizedUsage {
    let message = value.get("message").unwrap_or(value);
    let mut result = provider_base("anthropic", requested_model);
    result.model = optional_model(message).unwrap_or_default();
    let usage = message
        .get("usage")
        .or_else(|| value.get("usage"))
        .filter(|usage| !usage.is_null());
    let Some(usage) = usage else {
        return result;
    };

    result.fresh_input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
    result.cached_input_tokens = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
    // Anthropic calls this a cache creation count; it is not part of the
    // receipt's read-cache bucket, but it is retained as an input observation.
    if let Some(cache_write) = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
    {
        result.fresh_input_tokens = result
            .fresh_input_tokens
            .map(|fresh| fresh.saturating_add(cache_write));
    }
    result.output_tokens = usage.get("output_tokens").and_then(Value::as_u64);
    let observed = usage.get("input_tokens").and_then(Value::as_u64).is_some()
        || usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .is_some()
        || usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .is_some()
        || usage.get("output_tokens").and_then(Value::as_u64).is_some();
    finish(result, retries, observed)
}

/// Normalizes a Gemini `usageMetadata` object or stream chunk.
#[must_use]
pub fn normalize_gemini(
    value: &Value,
    requested_model: Option<&str>,
    retries: u32,
) -> NormalizedUsage {
    let mut result = provider_base("gemini", requested_model);
    result.model = optional_model(value).unwrap_or_default();
    let Some(metadata) = value.get("usageMetadata") else {
        return result;
    };
    let prompt = metadata.get("promptTokenCount").and_then(Value::as_u64);
    let cached = metadata
        .get("cachedContentTokenCount")
        .and_then(Value::as_u64);
    let candidates = metadata.get("candidatesTokenCount").and_then(Value::as_u64);
    let thoughts = metadata.get("thoughtsTokenCount").and_then(Value::as_u64);
    result.fresh_input_tokens = prompt.map(|total| total.saturating_sub(cached.unwrap_or(0)));
    result.cached_input_tokens = cached;
    result.output_tokens = match (candidates, thoughts) {
        (Some(candidates), Some(thoughts)) => Some(candidates.saturating_add(thoughts)),
        (Some(candidates), None) => Some(candidates),
        (None, Some(thoughts)) => Some(thoughts),
        (None, None) => None,
    };
    result.reasoning_tokens = thoughts;
    finish(
        result,
        retries,
        prompt.is_some() || candidates.is_some() || thoughts.is_some(),
    )
}

/// Normalizes a provider response while retaining the provider-specific model.
#[must_use]
pub fn normalize_provider(
    provider: &str,
    value: &Value,
    requested_model: Option<&str>,
    retries: u32,
) -> NormalizedUsage {
    match canonical_provider(provider).as_str() {
        "openai" => normalize_openai(value, requested_model, retries),
        "openrouter" => {
            let mut usage = normalize_openai(value, requested_model, retries);
            usage.provider = String::from("openrouter");
            usage
        }
        "orcarouter" => {
            let mut usage = normalize_openai(value, requested_model, retries);
            usage.provider = String::from("orcarouter");
            usage
        }
        "anthropic" => normalize_anthropic(value, requested_model, retries),
        "gemini" => normalize_gemini(value, requested_model, retries),
        _ => {
            let mut result = provider_base(provider, requested_model);
            result.model = optional_model(value).unwrap_or_default();
            result
        }
    }
}

/// Normalizes newline-delimited streaming events.
///
/// Each provider's usage event is cumulative in the common streaming APIs, so
/// the newest known field replaces the previous field. Invalid or keep-alive
/// lines are ignored; a missing usage event remains an all-`None` result.
#[must_use]
pub fn normalize_stream<I, S>(
    provider: &str,
    chunks: I,
    requested_model: Option<&str>,
    retries: u32,
) -> NormalizedUsage
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut result = provider_base(provider, requested_model);
    for chunk in chunks {
        let line = chunk.as_ref().trim();
        let line = line.strip_prefix("data:").map_or(line, str::trim);
        if line.is_empty() || line == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let observation = normalize_provider(provider, &value, requested_model, retries);
        result.replace_known_from(&observation);
    }
    result
}

/// Alias with an explicit name for callers that already have JSON lines.
#[must_use]
pub fn normalize_json_lines<I, S>(
    provider: &str,
    lines: I,
    requested_model: Option<&str>,
    retries: u32,
) -> NormalizedUsage
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    normalize_stream(provider, lines, requested_model, retries)
}

/// Returns a normalized response only when a usage fact was observed.
#[must_use]
pub fn try_normalize_provider(
    provider: &str,
    value: &Value,
    requested_model: Option<&str>,
    retries: u32,
) -> Option<NormalizedUsage> {
    let usage = normalize_provider(provider, value, requested_model, retries);
    usage.has_observation().then_some(usage)
}

fn openrouter_cost_micros(usage: &Value) -> Option<u64> {
    let cost = usage.get("cost").and_then(decimal_value_to_micros);
    let upstream = usage
        .get("cost_details")
        .and_then(|details| details.get("upstream_inference_cost"))
        .and_then(decimal_value_to_micros);
    match (cost, upstream) {
        (Some(cost), Some(upstream)) if upstream != cost => cost.checked_add(upstream),
        (Some(cost), _) => Some(cost),
        (None, Some(upstream)) => Some(upstream),
        (None, None) => None,
    }
}

/// Converts a JSON decimal USD value into micros without using floating point.
///
/// Values smaller than one micro are truncated, and negative values are
/// rejected.  Scientific notation is accepted because `serde_json::Number`
/// may render large or small values that way.
#[must_use]
pub fn decimal_value_to_micros(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => decimal_to_micros(&number.to_string()),
        Value::String(value) => decimal_to_micros(value),
        _ => None,
    }
}

/// Converts a decimal string in USD into integer micros.
#[must_use]
pub fn decimal_to_micros(input: &str) -> Option<u64> {
    let input = input.trim();
    if input.is_empty() || input.starts_with('-') || input.starts_with('+') {
        return None;
    }
    let (mantissa, exponent) = match input.find(['e', 'E']) {
        Some(index) => {
            let exponent = input.get(index + 1..)?.parse::<i32>().ok()?;
            (input.get(..index)?, exponent)
        }
        None => (input, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole.chars().all(|ch| ch.is_ascii_digit())
        || !fraction.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    let digits = format!("{whole}{fraction}");
    let significant = digits.trim_start_matches('0');
    let base = if significant.is_empty() {
        0
    } else {
        significant.parse::<u64>().ok()?
    };
    let decimal_places = i32::try_from(fraction.len()).ok()?;
    let shift = 6i32.checked_sub(decimal_places)?.checked_add(exponent)?;
    if shift >= 0 {
        base.checked_mul(pow10(u32::try_from(shift).ok()?))
    } else {
        Some(base / pow10(u32::try_from(-shift).ok()?))
    }
}

fn pow10(power: u32) -> u64 {
    (0..power).fold(1u64, |value, _| value.saturating_mul(10))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        NormalizedUsage, decimal_to_micros, normalize_anthropic, normalize_gemini,
        normalize_openai, normalize_provider, normalize_stream,
    };

    #[test]
    fn openai_missing_fields_stay_unknown() {
        let usage = normalize_openai(
            &json!({"model": "gpt-test", "usage": {"prompt_tokens": 20}}),
            Some("requested"),
            0,
        );
        assert_eq!(usage.fresh_input_tokens, Some(20));
        assert_eq!(usage.cached_input_tokens, None);
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.requested_model.as_deref(), Some("requested"));
    }

    #[test]
    fn provider_shapes_are_normalized() {
        let anthropic = normalize_anthropic(
            &json!({
                "message": {"model": "claude-test", "usage": {
                    "input_tokens": 100,
                    "cache_read_input_tokens": 20,
                    "output_tokens": 30
                }}
            }),
            None,
            1,
        );
        assert_eq!(anthropic.fresh_input_tokens, Some(100));
        assert_eq!(anthropic.cached_input_tokens, Some(20));
        assert_eq!(anthropic.output_tokens, Some(30));
        assert_eq!(anthropic.retries, Some(1));

        let gemini = normalize_gemini(
            &json!({
                "modelVersion": "gemini-test",
                "usageMetadata": {
                    "promptTokenCount": 100,
                    "cachedContentTokenCount": 10,
                    "candidatesTokenCount": 20,
                    "thoughtsTokenCount": 5
                }
            }),
            None,
            0,
        );
        assert_eq!(gemini.fresh_input_tokens, Some(90));
        assert_eq!(gemini.output_tokens, Some(25));
        assert_eq!(gemini.reasoning_tokens, Some(5));
    }

    #[test]
    fn orcarouter_uses_openai_shape_and_keeps_provider() {
        let usage = normalize_provider(
            "orcarouter",
            &json!({
                "model": "openai/gpt-oss-120b",
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 20,
                    "cost": 0.00000392
                }
            }),
            None,
            0,
        );
        assert_eq!(usage.provider, "orcarouter");
        assert_eq!(usage.fresh_input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(20));
    }

    #[test]
    fn streaming_uses_latest_cumulative_usage() {
        let usage = normalize_stream(
            "openai",
            [
                r#"data: {"model":"gpt","usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
                r#"data: {"model":"gpt","usage":{"prompt_tokens":12,"completion_tokens":4}}"#,
                "data: [DONE]",
            ],
            None,
            0,
        );
        assert_eq!(usage.fresh_input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.total_model_calls, Some(1));
    }

    #[test]
    fn decimal_costs_use_micros() {
        assert_eq!(decimal_to_micros("1.234567"), Some(1_234_567));
        assert_eq!(decimal_to_micros("0.0000009"), Some(0));
        assert_eq!(decimal_to_micros("1e-3"), Some(1_000));
        assert_eq!(decimal_to_micros("-1"), None);
    }

    #[test]
    fn additive_merge_keeps_unknown_unknown() {
        let mut left = NormalizedUsage::complete("openai", "gpt", 1, 2, 3, 4);
        let right = NormalizedUsage {
            output_tokens: None,
            ..NormalizedUsage::complete("openai", "gpt", 1, 2, 3, 4)
        };
        left.merge_additive(&right);
        assert_eq!(left.fresh_input_tokens, Some(2));
        assert_eq!(left.output_tokens, None);
    }

    #[test]
    fn additive_merge_copies_empty_accumulator_once() {
        let mut accumulated = NormalizedUsage::default();
        let observation = NormalizedUsage::complete("openai", "gpt", 1, 2, 3, 4);
        accumulated.merge_additive(&observation);
        assert_eq!(accumulated.fresh_input_tokens, Some(1));
        assert_eq!(accumulated.cached_input_tokens, Some(2));
        assert_eq!(accumulated.output_tokens, Some(3));
        assert_eq!(accumulated.reasoning_tokens, Some(4));
        assert_eq!(accumulated.total_model_calls, Some(1));
    }
}
