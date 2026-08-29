//! Proxy-to-kernel provider and usage conversion.

use crate::core::context_kernel::provider_parity;
use crate::core::context_kernel::token_envelope::{ProviderKind, TokenEnvelope};

/// Maps a proxy provider label to its kernel provider kind.
pub fn provider_kind_from_label(label: &str) -> ProviderKind {
    match label {
        "Anthropic" => ProviderKind::Anthropic,
        "OpenAI" | "ChatGPT" => ProviderKind::OpenAi,
        "Gemini" => ProviderKind::Gemini,
        "Bedrock" => ProviderKind::Bedrock,
        "Azure" => ProviderKind::Azure,
        _ if label.to_ascii_lowercase().contains("openrouter") => ProviderKind::OpenRouter,
        _ if label.to_ascii_lowercase().contains("orcarouter") => ProviderKind::OrcaRouter,
        _ => ProviderKind::Unknown,
    }
}

/// Converts proxy-reported usage into a provider-neutral kernel envelope.
pub fn real_usage_to_envelope(usage: &super::usage::RealUsage, label: &str) -> TokenEnvelope {
    TokenEnvelope {
        model: usage.model.clone(),
        provider: provider_kind_from_label(label),
        input_tokens: usage.input_tokens as usize,
        output_tokens: usage.output_tokens as usize,
        cache_read_tokens: usage.cache_read_tokens as usize,
        cache_write_tokens: usage.cache_write_tokens as usize,
        reasoning_tokens: usage.reasoning_tokens as usize,
        cost_usd: usage.provider_cost_usd,
        tokens_saved: 0,
        is_retry: false,
    }
}

/// Detects a provider from a base URL and returns its canonical display label.
pub fn label_from_base_url(base_url: &str) -> &'static str {
    provider_parity::provider_display_name(provider_parity::detect_provider(base_url))
}

#[cfg(test)]
mod tests {
    use super::{label_from_base_url, provider_kind_from_label, real_usage_to_envelope};
    use crate::core::context_kernel::token_envelope::ProviderKind;
    use crate::proxy::usage::RealUsage;

    #[test]
    fn kind_from_anthropic_label() {
        assert_eq!(
            provider_kind_from_label("Anthropic"),
            ProviderKind::Anthropic
        );
    }

    #[test]
    fn kind_from_openai_label() {
        assert_eq!(provider_kind_from_label("OpenAI"), ProviderKind::OpenAi);
    }

    #[test]
    fn kind_from_chatgpt_label() {
        assert_eq!(provider_kind_from_label("ChatGPT"), ProviderKind::OpenAi);
    }

    #[test]
    fn kind_from_bedrock_label() {
        assert_eq!(provider_kind_from_label("Bedrock"), ProviderKind::Bedrock);
    }

    #[test]
    fn kind_from_azure_label() {
        assert_eq!(provider_kind_from_label("Azure"), ProviderKind::Azure);
    }

    #[test]
    fn kind_from_gemini_label() {
        assert_eq!(provider_kind_from_label("Gemini"), ProviderKind::Gemini);
    }

    #[test]
    fn kind_from_unknown() {
        assert_eq!(provider_kind_from_label("Other"), ProviderKind::Unknown);
    }

    #[test]
    fn kind_from_orcarouter_label() {
        assert_eq!(
            provider_kind_from_label("OrcaRouter"),
            ProviderKind::OrcaRouter
        );
        assert_eq!(
            provider_kind_from_label("orcarouter"),
            ProviderKind::OrcaRouter
        );
    }

    #[test]
    fn real_usage_full_convert() {
        let usage = RealUsage {
            model: "model-1".to_owned(),
            input_tokens: 11,
            output_tokens: 12,
            cache_read_tokens: 13,
            cache_write_tokens: 14,
            reasoning_tokens: 15,
            provider_cost_usd: Some(0.25),
            ..RealUsage::default()
        };

        let envelope = real_usage_to_envelope(&usage, "Anthropic");

        assert_eq!(envelope.model, "model-1");
        assert_eq!(envelope.provider, ProviderKind::Anthropic);
        assert_eq!(envelope.input_tokens, 11);
        assert_eq!(envelope.output_tokens, 12);
        assert_eq!(envelope.cache_read_tokens, 13);
        assert_eq!(envelope.cache_write_tokens, 14);
        assert_eq!(envelope.reasoning_tokens, 15);
        assert_eq!(envelope.cost_usd, Some(0.25));
    }

    #[test]
    fn label_from_url_openai() {
        assert_eq!(label_from_base_url("https://api.openai.com/v1"), "OpenAI");
    }
}
