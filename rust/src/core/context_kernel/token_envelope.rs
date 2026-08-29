//! Provider-neutral token usage representation.

use serde::{Deserialize, Serialize};

/// Provider responsible for serving a model request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderKind {
    /// OpenAI API.
    OpenAi,
    /// Anthropic API.
    Anthropic,
    /// Google Gemini API.
    Gemini,
    /// OpenRouter gateway.
    OpenRouter,
    /// OrcaRouter gateway.
    OrcaRouter,
    /// Amazon Bedrock.
    Bedrock,
    /// Azure OpenAI Service.
    Azure,
    /// Locally hosted model.
    Local,
    /// Unknown or unavailable provider.
    #[default]
    Unknown,
}

/// Provider-neutral token usage envelope.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenEnvelope {
    /// Canonical model identifier.
    pub model: String,
    /// Provider that served the request.
    pub provider: ProviderKind,
    /// Total input tokens (prompt + context).
    pub input_tokens: usize,
    /// Total output tokens (completion).
    pub output_tokens: usize,
    /// Cache read tokens (provider-specific).
    pub cache_read_tokens: usize,
    /// Cache write tokens.
    pub cache_write_tokens: usize,
    /// Reasoning/thinking tokens (if supported).
    pub reasoning_tokens: usize,
    /// Estimated cost in USD (if available).
    pub cost_usd: Option<f64>,
    /// Tokens saved by compression.
    pub tokens_saved: usize,
    /// Whether this was a retry.
    pub is_retry: bool,
}

/// Converts proxy request usage into a canonical token envelope.
#[must_use]
pub(crate) fn from_proxy_data(data: &super::proxy_bridge::ProxyRequestData) -> TokenEnvelope {
    TokenEnvelope {
        model: data.model.clone().unwrap_or_default(),
        provider: data
            .provider
            .as_deref()
            .map_or(ProviderKind::Unknown, parse_provider),
        input_tokens: data.input_tokens,
        output_tokens: data.output_tokens,
        reasoning_tokens: data.reasoning_tokens,
        tokens_saved: data.tokens_saved,
        is_retry: data.is_retry,
        ..TokenEnvelope::default()
    }
}

/// Converts MCP call usage into a canonical token envelope.
#[must_use]
pub(crate) fn from_mcp_call(data: &super::mcp_bridge::McpCallData) -> TokenEnvelope {
    TokenEnvelope {
        provider: ProviderKind::Unknown,
        input_tokens: data.input_tokens,
        output_tokens: data.output_tokens,
        is_retry: data.is_retry,
        ..TokenEnvelope::default()
    }
}

/// Parses a provider label into its canonical provider kind.
#[must_use]
pub(crate) fn parse_provider(label: &str) -> ProviderKind {
    match label.trim().to_ascii_lowercase().as_str() {
        "openai" => ProviderKind::OpenAi,
        "anthropic" => ProviderKind::Anthropic,
        "gemini" | "google" => ProviderKind::Gemini,
        "openrouter" => ProviderKind::OpenRouter,
        "orcarouter" => ProviderKind::OrcaRouter,
        "bedrock" => ProviderKind::Bedrock,
        "azure" | "azure_openai" => ProviderKind::Azure,
        "local" => ProviderKind::Local,
        _ => ProviderKind::Unknown,
    }
}

impl TokenEnvelope {
    /// Returns input, output, and reasoning tokens combined.
    #[must_use]
    pub fn total_tokens(&self) -> usize {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.reasoning_tokens)
    }

    /// Returns total tokens excluding tokens served from the provider cache.
    #[must_use]
    pub fn effective_tokens(&self) -> usize {
        self.total_tokens().saturating_sub(self.cache_read_tokens)
    }

    /// Returns the fraction of original input eliminated by compression.
    #[must_use]
    pub fn compression_ratio(&self) -> f64 {
        let original_input = self.input_tokens.saturating_add(self.tokens_saved);
        if original_input == 0 {
            0.0
        } else {
            self.tokens_saved as f64 / original_input as f64
        }
    }

    /// Returns whether any input tokens were served from a provider cache.
    #[must_use]
    pub const fn is_cached(&self) -> bool {
        self.cache_read_tokens > 0
    }

    /// Aggregates envelopes into a single canonical usage summary.
    #[must_use]
    pub fn merge(envelopes: &[Self]) -> Self {
        let Some(first) = envelopes.first() else {
            return Self::default();
        };

        let same_model = envelopes
            .iter()
            .all(|envelope| envelope.model == first.model);
        let same_provider = envelopes
            .iter()
            .all(|envelope| envelope.provider == first.provider);
        let sum = |field: fn(&Self) -> usize| {
            envelopes.iter().fold(0usize, |total, envelope| {
                total.saturating_add(field(envelope))
            })
        };

        Self {
            model: if same_model {
                first.model.clone()
            } else {
                String::new()
            },
            provider: if same_provider {
                first.provider
            } else {
                ProviderKind::Unknown
            },
            input_tokens: sum(|envelope| envelope.input_tokens),
            output_tokens: sum(|envelope| envelope.output_tokens),
            cache_read_tokens: sum(|envelope| envelope.cache_read_tokens),
            cache_write_tokens: sum(|envelope| envelope.cache_write_tokens),
            reasoning_tokens: sum(|envelope| envelope.reasoning_tokens),
            cost_usd: envelopes
                .iter()
                .filter_map(|envelope| envelope.cost_usd)
                .reduce(|total, cost| total + cost),
            tokens_saved: sum(|envelope| envelope.tokens_saved),
            is_retry: envelopes.iter().any(|envelope| envelope.is_retry),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderKind, TokenEnvelope, from_mcp_call, from_proxy_data, parse_provider};
    use crate::core::context_kernel::mcp_bridge::McpCallData;
    use crate::core::context_kernel::proxy_bridge::ProxyRequestData;

    #[test]
    fn from_proxy_openai() {
        let envelope = from_proxy_data(&ProxyRequestData {
            provider: Some("OpenAI".to_owned()),
            model: Some("gpt-5".to_owned()),
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            tokens_saved: 30,
            is_retry: true,
            ..ProxyRequestData::default()
        });

        assert_eq!(envelope.provider, ProviderKind::OpenAi);
        assert_eq!(envelope.model, "gpt-5");
        assert_eq!(envelope.total_tokens(), 125);
        assert_eq!(envelope.tokens_saved, 30);
        assert!(envelope.is_retry);
    }

    #[test]
    fn from_proxy_anthropic() {
        let envelope = from_proxy_data(&ProxyRequestData {
            provider: Some("Anthropic".to_owned()),
            ..ProxyRequestData::default()
        });

        assert_eq!(envelope.provider, ProviderKind::Anthropic);
    }

    #[test]
    fn from_mcp_call_maps_usage() {
        let envelope = from_mcp_call(&McpCallData {
            input_tokens: 80,
            output_tokens: 12,
            is_retry: true,
            ..McpCallData::default()
        });

        assert_eq!(envelope.provider, ProviderKind::Unknown);
        assert_eq!(envelope.input_tokens, 80);
        assert_eq!(envelope.output_tokens, 12);
        assert!(envelope.is_retry);
    }

    #[test]
    fn total_tokens_sum() {
        let envelope = TokenEnvelope {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 7,
            ..TokenEnvelope::default()
        };

        assert_eq!(envelope.total_tokens(), 127);
    }

    #[test]
    fn effective_excludes_cache() {
        let envelope = TokenEnvelope {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 7,
            cache_read_tokens: 40,
            ..TokenEnvelope::default()
        };

        assert_eq!(envelope.effective_tokens(), 87);
        assert!(envelope.is_cached());
    }

    #[test]
    fn compression_ratio_correct() {
        let envelope = TokenEnvelope {
            input_tokens: 1_000,
            tokens_saved: 300,
            ..TokenEnvelope::default()
        };

        assert!((envelope.compression_ratio() - 0.230_769).abs() < 0.000_001);
    }

    #[test]
    fn merge_aggregates() {
        let envelopes = (1..=3)
            .map(|multiplier| TokenEnvelope {
                model: "gpt-5".to_owned(),
                provider: ProviderKind::OpenAi,
                input_tokens: 10 * multiplier,
                output_tokens: 2 * multiplier,
                cache_read_tokens: multiplier,
                cache_write_tokens: multiplier,
                reasoning_tokens: multiplier,
                cost_usd: Some(0.01 * multiplier as f64),
                tokens_saved: 3 * multiplier,
                is_retry: multiplier == 3,
            })
            .collect::<Vec<_>>();

        let merged = TokenEnvelope::merge(&envelopes);
        assert_eq!(merged.model, "gpt-5");
        assert_eq!(merged.provider, ProviderKind::OpenAi);
        assert_eq!(merged.input_tokens, 60);
        assert_eq!(merged.output_tokens, 12);
        assert_eq!(merged.cache_read_tokens, 6);
        assert_eq!(merged.cache_write_tokens, 6);
        assert_eq!(merged.reasoning_tokens, 6);
        assert!((merged.cost_usd.unwrap_or_default() - 0.06).abs() < f64::EPSILON);
        assert_eq!(merged.tokens_saved, 18);
        assert!(merged.is_retry);
    }

    #[test]
    fn parse_case_insensitive() {
        for label in ["openai", "OPENAI", "OpenAI"] {
            assert_eq!(parse_provider(label), ProviderKind::OpenAi);
        }
    }

    #[test]
    fn parse_provider_aliases_and_unknown() {
        assert_eq!(parse_provider("google"), ProviderKind::Gemini);
        assert_eq!(parse_provider("openrouter"), ProviderKind::OpenRouter);
        assert_eq!(parse_provider("orcarouter"), ProviderKind::OrcaRouter);
        assert_eq!(parse_provider("local"), ProviderKind::Local);
        assert_eq!(parse_provider("other"), ProviderKind::Unknown);
    }
}
