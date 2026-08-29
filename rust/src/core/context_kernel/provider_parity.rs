//! Provider detection and provider-neutral usage normalization.

use serde_json::Value;

use super::token_envelope::{ProviderKind, TokenEnvelope};

const SUPPORTED: [ProviderKind; 8] = [
    ProviderKind::OpenAi,
    ProviderKind::Anthropic,
    ProviderKind::Gemini,
    ProviderKind::OpenRouter,
    ProviderKind::OrcaRouter,
    ProviderKind::Bedrock,
    ProviderKind::Azure,
    ProviderKind::Local,
];

/// Detects a provider from its API base URL.
pub fn detect_provider(base_url: &str) -> ProviderKind {
    let url = base_url.to_ascii_lowercase();
    if url.contains("api.openai.com") {
        ProviderKind::OpenAi
    } else if url.contains("api.anthropic.com") {
        ProviderKind::Anthropic
    } else if url.contains("generativelanguage.googleapis.com")
        || url.contains("aiplatform.googleapis.com")
    {
        ProviderKind::Gemini
    } else if url.contains("bedrock-runtime") && url.contains("amazonaws.com") {
        ProviderKind::Bedrock
    } else if url.contains("openai.azure.com")
        || url.contains("services.ai.azure.com")
        || url.contains("cognitiveservices.azure.com")
    {
        ProviderKind::Azure
    } else if url.contains("openrouter.ai") {
        ProviderKind::OpenRouter
    } else if url.contains("orcarouter.ai") {
        ProviderKind::OrcaRouter
    } else if url.contains("localhost") || url.contains("127.0.0.1") || url.contains("0.0.0.0") {
        ProviderKind::Local
    } else {
        ProviderKind::Unknown
    }
}

fn token(usage: &Value, path: &[&str]) -> usize {
    path.iter()
        .try_fold(usage, |value, key| value.get(key))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

/// Creates a canonical token envelope from provider-specific usage JSON.
pub fn envelope_from_usage(provider: ProviderKind, model: &str, usage: &Value) -> TokenEnvelope {
    let (input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens) =
        match provider {
            ProviderKind::Anthropic => (
                token(usage, &["input_tokens"]),
                token(usage, &["output_tokens"]),
                token(usage, &["cache_read_input_tokens"]),
                token(usage, &["cache_creation_input_tokens"]),
                0,
            ),
            ProviderKind::Gemini => (
                token(usage, &["promptTokenCount"]),
                token(usage, &["candidatesTokenCount"]),
                token(usage, &["cachedContentTokenCount"]),
                0,
                0,
            ),
            ProviderKind::Bedrock => (
                token(usage, &["inputTokens"]),
                token(usage, &["outputTokens"]),
                0,
                0,
                0,
            ),
            ProviderKind::OpenAi
            | ProviderKind::Azure
            | ProviderKind::OpenRouter
            | ProviderKind::OrcaRouter
            | ProviderKind::Local
            | ProviderKind::Unknown => (
                token(usage, &["prompt_tokens"]),
                token(usage, &["completion_tokens"]),
                token(usage, &["prompt_tokens_details", "cached_tokens"]),
                0,
                token(usage, &["completion_tokens_details", "reasoning_tokens"]),
            ),
        };

    TokenEnvelope {
        model: model.to_owned(),
        provider,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        cost_usd: None,
        tokens_saved: 0,
        is_retry: false,
    }
}

/// Returns the stable human-readable name for a provider.
pub const fn provider_display_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OpenAi => "OpenAI",
        ProviderKind::Anthropic => "Anthropic",
        ProviderKind::Gemini => "Gemini",
        ProviderKind::Bedrock => "Bedrock",
        ProviderKind::Azure => "Azure",
        ProviderKind::OpenRouter => "OpenRouter",
        ProviderKind::OrcaRouter => "OrcaRouter",
        ProviderKind::Local => "Local",
        ProviderKind::Unknown => "Unknown",
    }
}

/// Returns every provider with a supported canonical usage mapping.
pub const fn all_supported() -> &'static [ProviderKind] {
    &SUPPORTED
}

#[cfg(test)]
pub mod tests {
    use serde_json::json;

    use super::{detect_provider, envelope_from_usage};
    use crate::core::context_kernel::token_envelope::ProviderKind;

    macro_rules! detect_test {
        ($name:ident, $url:expr, $kind:expr) => {
            #[test]
            fn $name() {
                assert_eq!(detect_provider($url), $kind);
            }
        };
    }

    detect_test!(
        detect_openai,
        "https://api.openai.com/v1",
        ProviderKind::OpenAi
    );
    detect_test!(
        detect_anthropic,
        "https://api.anthropic.com",
        ProviderKind::Anthropic
    );
    detect_test!(
        detect_bedrock,
        "https://bedrock-runtime.us-east-1.amazonaws.com",
        ProviderKind::Bedrock
    );
    detect_test!(
        detect_azure_foundry,
        "https://westus.services.ai.azure.com",
        ProviderKind::Azure
    );
    detect_test!(
        detect_azure_classic,
        "https://tenant.openai.azure.com",
        ProviderKind::Azure
    );
    detect_test!(
        detect_gemini,
        "https://generativelanguage.googleapis.com",
        ProviderKind::Gemini
    );
    detect_test!(
        detect_localhost,
        "http://localhost:11434",
        ProviderKind::Local
    );
    detect_test!(
        detect_orcarouter,
        "https://api.orcarouter.ai/v1",
        ProviderKind::OrcaRouter
    );
    detect_test!(detect_unknown, "https://example.com", ProviderKind::Unknown);

    #[test]
    fn envelope_openai() {
        let value = json!({"prompt_tokens": 100, "completion_tokens": 50});
        let envelope = envelope_from_usage(ProviderKind::OpenAi, "gpt", &value);
        assert_eq!((envelope.input_tokens, envelope.output_tokens), (100, 50));
    }

    #[test]
    fn envelope_anthropic() {
        let value = json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_read_input_tokens": 20
        });
        let envelope = envelope_from_usage(ProviderKind::Anthropic, "claude", &value);
        assert_eq!(envelope.input_tokens, 100);
        assert_eq!(envelope.output_tokens, 50);
        assert_eq!(envelope.cache_read_tokens, 20);
    }

    #[test]
    fn envelope_empty_safe() {
        let envelope = envelope_from_usage(ProviderKind::Unknown, "unknown", &json!({}));
        assert_eq!(envelope.input_tokens, 0);
        assert_eq!(envelope.output_tokens, 0);
        assert_eq!(envelope.cache_read_tokens, 0);
        assert_eq!(envelope.cache_write_tokens, 0);
        assert_eq!(envelope.reasoning_tokens, 0);
    }
}
