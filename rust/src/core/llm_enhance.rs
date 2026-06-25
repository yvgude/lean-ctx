//! Optional LLM enhancement layer.
//!
//! Deterministic by default — LLM calls are opt-in and always fall back to
//! the deterministic pipeline on failure, timeout, or when disabled.
//!
//! Supported backends: Ollama (local), `OpenRouter`, Claude (Anthropic).

use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROMPT_CHARS: usize = 2000;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    pub backend: LlmBackend,
    pub model: String,
    pub timeout_secs: u64,
    pub base_url: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: LlmBackend::Ollama,
            model: "qwen2.5-coder:1.5b".to_string(),
            timeout_secs: 10,
            base_url: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum LlmBackend {
    #[default]
    Ollama,
    OpenRouter,
    Anthropic,
}

impl LlmConfig {
    fn effective_base_url(&self) -> String {
        if let Some(ref url) = self.base_url {
            return url.clone();
        }
        match self.backend {
            LlmBackend::Ollama => "http://localhost:11434".to_string(),
            LlmBackend::OpenRouter => "https://openrouter.ai/api".to_string(),
            LlmBackend::Anthropic => "https://api.anthropic.com".to_string(),
        }
    }

    fn api_key(&self) -> Option<String> {
        match self.backend {
            LlmBackend::Ollama => None,
            LlmBackend::OpenRouter => std::env::var("OPENROUTER_API_KEY").ok(),
            LlmBackend::Anthropic => std::env::var("ANTHROPIC_API_KEY").ok(),
        }
    }

    fn timeout(&self) -> Duration {
        if self.timeout_secs > 0 {
            Duration::from_secs(self.timeout_secs)
        } else {
            DEFAULT_TIMEOUT
        }
    }
}

/// Expand a search query using LLM. Falls back to the original query on failure.
#[must_use]
pub fn expand_query(query: &str) -> String {
    let cfg = crate::core::config::Config::load().llm;
    if !cfg.enabled {
        return query.to_string();
    }

    let prompt = format!(
        "Expand this code search query with 2-3 related terms. \
         Return ONLY the expanded query, no explanation.\n\
         Query: {query}"
    );

    match call_llm(&cfg, &prompt) {
        Ok(expanded) => {
            let cleaned = expanded.trim().to_string();
            if cleaned.is_empty() || cleaned.len() > query.len() * 5 {
                query.to_string()
            } else {
                cleaned
            }
        }
        Err(_) => query.to_string(),
    }
}

/// Generate a human-readable explanation for a knowledge contradiction.
/// Falls back to a simple diff-style description.
#[must_use]
pub fn explain_contradiction(fact_a: &str, fact_b: &str) -> String {
    let cfg = crate::core::config::Config::load().llm;
    if !cfg.enabled {
        return deterministic_contradiction(fact_a, fact_b);
    }

    let prompt = format!(
        "These two facts contradict. Explain the conflict in one sentence:\n\
         A: {fact_a}\nB: {fact_b}"
    );

    match call_llm(&cfg, &prompt) {
        Ok(explanation) => explanation.trim().to_string(),
        Err(_) => deterministic_contradiction(fact_a, fact_b),
    }
}

fn deterministic_contradiction(a: &str, b: &str) -> String {
    format!("Conflict: \"{a}\" vs \"{b}\"")
}

/// Refine a synthesized observation summary with an LLM (#802). Opt-in via
/// `llm.enabled`; the deterministic input is always a valid result and is returned
/// unchanged when LLM is disabled, errors, times out, or yields something unusable.
/// The model is constrained to the supplied notes (no invention). Because the
/// result is stored once and recalled byte-stably, enabling this does not break
/// hot-path read determinism (#498) — only the one-time stored value differs.
#[must_use]
pub fn enhance_observation(entity: &str, deterministic: &str) -> String {
    let cfg = crate::core::config::Config::load().llm;
    if !cfg.enabled {
        return deterministic.to_string();
    }

    let prompt = format!(
        "Summarize what is known about `{entity}` in ONE concise, factual sentence. \
         Use ONLY these notes; do not invent. Return ONLY the sentence.\n\
         Notes: {deterministic}"
    );

    match call_llm(&cfg, &prompt) {
        Ok(text) => {
            let cleaned = text.trim();
            // Reject empty or runaway output; the deterministic digest is the floor.
            if cleaned.is_empty() || cleaned.len() > deterministic.len() * 4 {
                deterministic.to_string()
            } else {
                format!("{entity} — {cleaned}")
            }
        }
        Err(_) => deterministic.to_string(),
    }
}

/// Low-level LLM call. Supports Ollama, `OpenRouter`, and Anthropic.
fn call_llm(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    let truncated = if prompt.len() > MAX_PROMPT_CHARS {
        &prompt[..prompt.floor_char_boundary(MAX_PROMPT_CHARS)]
    } else {
        prompt
    };

    match cfg.backend {
        LlmBackend::Ollama => call_ollama(cfg, truncated),
        LlmBackend::OpenRouter => call_openai_compatible(cfg, truncated),
        LlmBackend::Anthropic => call_anthropic(cfg, truncated),
    }
}

fn make_agent(cfg: &LlmConfig) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(cfg.timeout()))
            .build(),
    )
}

fn call_ollama(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    let url = format!("{}/api/generate", cfg.effective_base_url());
    let body = serde_json::json!({
        "model": cfg.model,
        "prompt": prompt,
        "stream": false,
        "options": { "num_predict": 100 }
    });

    let agent = make_agent(cfg);
    let payload = serde_json::to_vec(&body).map_err(|e| format!("json: {e}"))?;
    let resp = agent
        .post(&url)
        .header("Content-Type", "application/json")
        .send(payload.as_slice())
        .map_err(|e| format!("ollama: {e}"))?;

    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    json.get("response")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "no response field".to_string())
}

fn call_openai_compatible(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    let key = cfg.api_key().ok_or("OPENROUTER_API_KEY not set")?;
    let url = format!("{}/v1/chat/completions", cfg.effective_base_url());
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 100
    });

    let agent = make_agent(cfg);
    let payload = serde_json::to_vec(&body).map_err(|e| format!("json: {e}"))?;
    let resp = agent
        .post(&url)
        .header("Authorization", &format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .send(payload.as_slice())
        .map_err(|e| format!("openrouter: {e}"))?;

    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    json.pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "no content in response".to_string())
}

fn call_anthropic(cfg: &LlmConfig, prompt: &str) -> Result<String, String> {
    let key = cfg.api_key().ok_or("ANTHROPIC_API_KEY not set")?;
    let url = format!("{}/v1/messages", cfg.effective_base_url());
    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": 100,
        "messages": [{"role": "user", "content": prompt}]
    });

    let agent = make_agent(cfg);
    let payload = serde_json::to_vec(&body).map_err(|e| format!("json: {e}"))?;
    let resp = agent
        .post(&url)
        .header("x-api-key", &key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .send(payload.as_slice())
        .map_err(|e| format!("anthropic: {e}"))?;

    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    json.pointer("/content/0/text")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "no text in response".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_disabled() {
        let cfg = LlmConfig::default();
        assert!(!cfg.enabled);
        assert!(matches!(cfg.backend, LlmBackend::Ollama));
    }

    #[test]
    fn expand_query_passthrough_when_disabled() {
        let result = expand_query("test query");
        assert_eq!(result, "test query");
    }

    #[test]
    fn deterministic_contradiction_format() {
        let result = deterministic_contradiction("A is true", "A is false");
        assert!(result.contains("Conflict"));
        assert!(result.contains("A is true"));
    }

    #[test]
    fn effective_base_url_defaults() {
        let cfg = LlmConfig::default();
        assert!(cfg.effective_base_url().contains("11434"));

        let cfg = LlmConfig {
            backend: LlmBackend::OpenRouter,
            ..Default::default()
        };
        assert!(cfg.effective_base_url().contains("openrouter"));
    }
}
