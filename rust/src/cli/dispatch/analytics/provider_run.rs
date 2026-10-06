#[allow(dead_code, unused_imports)]
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_mins(5);

/// Configuration for one matched direct-provider/proxy-provider run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRunConfig {
    pub run_id: String,
    pub task_description: String,
    pub commit_sha: String,
    pub repo_path: PathBuf,
    pub provider: String,
    pub model: String,
    pub prompt_template: String,
    pub target_files: Vec<PathBuf>,
    pub budget_tokens: u64,
    pub temperature: f32,
}

/// The two arms in a matched provider run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmType {
    Baseline,
    Treatment,
}

impl ArmType {
    fn label(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Treatment => "treatment",
        }
    }
}

/// Measurements and response captured for one arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmResult {
    pub arm_type: ArmType,
    /// Provider that executed this arm.  Kept with the measurements so
    /// downstream receipts remain self-describing when persisted alone.
    pub provider: String,
    /// Model requested for this arm.
    pub model: String,
    /// Source revision evaluated by this arm.
    pub commit_sha: String,
    pub input_tokens: u64,
    pub cached_tokens: u64,
    pub output_tokens: u64,
    pub cost_micros: u64,
    pub latency_ms: u64,
    pub output_content: String,
    pub proxy_observed: bool,
    pub measurement_method: String,
}

/// Matched-arm comparison metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonSummary {
    pub token_savings_ratio: f64,
    pub cost_savings_ratio: f64,
    pub quality_preserved: Option<bool>,
}

/// Complete matched provider run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRunResult {
    pub run_id: String,
    pub baseline: ArmResult,
    pub treatment: ArmResult,
    pub comparison: ComparisonSummary,
}

/// Execute the same rendered task once directly and once through the proxy.
pub async fn execute_provider_run(
    config: ProviderRunConfig,
) -> Result<ProviderRunResult, anyhow::Error> {
    validate_config(&config)?;
    let (prompt, context) = load_task_material(&config)?;

    let baseline_config = config.clone();
    let baseline_prompt = prompt.clone();
    let baseline = tokio::task::spawn_blocking(move || {
        execute_arm(&baseline_config, ArmType::Baseline, &baseline_prompt)
    })
    .await
    .context("baseline provider task failed to join")??;

    let treatment_config = config.clone();
    let treatment_prompt = prompt.clone();
    let treatment = tokio::task::spawn_blocking(move || {
        execute_arm(&treatment_config, ArmType::Treatment, &treatment_prompt)
    })
    .await
    .context("treatment provider task failed to join")??;

    let comparison = compare_arms(&baseline, &treatment);
    let result = ProviderRunResult {
        run_id: config.run_id.clone(),
        baseline,
        treatment,
        comparison,
    };

    save_artifacts(&config, &prompt, &context, &result)?;
    Ok(result)
}

fn validate_config(config: &ProviderRunConfig) -> anyhow::Result<()> {
    if config.run_id.trim().is_empty() {
        bail!("provider run id must not be empty");
    }
    if config.provider.trim().is_empty() {
        bail!("provider must not be empty");
    }
    if config.model.trim().is_empty() {
        bail!("model must not be empty");
    }
    if config.budget_tokens == 0 {
        bail!("budget_tokens must be greater than zero");
    }
    if !config.temperature.is_finite() || config.temperature < 0.0 {
        bail!("temperature must be finite and non-negative");
    }
    Ok(())
}

fn load_task_material(config: &ProviderRunConfig) -> anyhow::Result<(String, String)> {
    let template_path = resolve_repo_path(&config.repo_path, Path::new(&config.prompt_template));
    let template = fs::read_to_string(&template_path)
        .with_context(|| format!("failed to read prompt template {}", template_path.display()))?;

    let mut context_parts = Vec::with_capacity(config.target_files.len());
    for target in &config.target_files {
        let path = resolve_repo_path(&config.repo_path, target);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read target file {}", path.display()))?;
        let display_path = path
            .strip_prefix(&config.repo_path)
            .unwrap_or(&path)
            .display();
        context_parts.push(format!("===== {display_path} =====\n{content}"));
    }
    let context = context_parts.join("\n\n");
    let target_files = config
        .target_files
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let repo_path = config.repo_path.display().to_string();

    let prompt = render_prompt(
        &template,
        &[
            ("{run_id}", config.run_id.as_str()),
            ("{task_description}", config.task_description.as_str()),
            ("{commit_sha}", config.commit_sha.as_str()),
            ("{repo_path}", &repo_path),
            ("{provider}", config.provider.as_str()),
            ("{model}", config.model.as_str()),
            ("{target_files}", &target_files),
            ("{context}", &context),
        ],
    );

    Ok((prompt, context))
}

fn resolve_repo_path(repo_path: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_path.join(path)
    }
}

fn render_prompt(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut rendered = template.to_owned();
    for (placeholder, value) in replacements {
        rendered = rendered.replace(placeholder, value);
    }
    if !template.contains("{context}") {
        if let Some((_, context)) = replacements.iter().find(|(key, _)| *key == "{context}") {
            rendered.push_str("\n\n## Review context\n\n");
            rendered.push_str(context);
        }
    }
    rendered
}

fn execute_arm(
    config: &ProviderRunConfig,
    arm_type: ArmType,
    prompt: &str,
) -> anyhow::Result<ArmResult> {
    let provider = config.provider.trim().to_ascii_lowercase();
    let base_url = if arm_type == ArmType::Treatment {
        proxy_base_url()
    } else {
        direct_base_url(&provider)
    };
    let url = request_url(&base_url, &provider, &config.model);
    let payload = request_payload(config, &provider, prompt);
    let payload = serde_json::to_vec(&payload).context("failed to encode provider request")?;
    let api_key = provider_api_key(&provider);
    let started = Instant::now();

    let response = send_provider_request(
        &url,
        &provider,
        &api_key,
        &config.run_id,
        arm_type,
        &payload,
    )
    .with_context(|| format!("{arm_type:?} provider request failed"))?;
    let latency_ms = elapsed_ms(started);
    let headers = response.headers;
    let response_body = response.body;
    let body: Value = serde_json::from_str(&response_body)
        .with_context(|| format!("{arm_type:?} provider returned invalid JSON: {response_body}"))?;

    let output_content = extract_output_content(&body, &provider)
        .ok_or_else(|| anyhow!("{arm_type:?} provider response contained no text content"))?;
    let usage = usage_metrics(&body, &headers);
    let estimated_input = estimate_tokens(prompt);
    let estimated_output = estimate_tokens(&output_content);
    let input_tokens = usage.input_tokens.unwrap_or(estimated_input);
    let output_tokens = usage.output_tokens.unwrap_or(estimated_output);
    let input_method = if usage.input_tokens.is_some() {
        "provider_reported"
    } else if input_tokens > 0 {
        "estimated"
    } else {
        "unavailable"
    };
    let output_method = if usage.output_tokens.is_some() {
        "provider_reported"
    } else if output_tokens > 0 {
        "estimated"
    } else {
        "unavailable"
    };
    let cached_method = if usage.cached_tokens.is_some() {
        "provider_reported"
    } else {
        "unavailable"
    };
    let reported_cost = cost_from_headers(&headers);
    let cost_micros = reported_cost.or_else(|| {
        if input_tokens == 0 && output_tokens == 0 {
            None
        } else {
            estimated_cost_micros(&config.model, input_tokens, output_tokens)
        }
    });
    let cost_method = if reported_cost.is_some() {
        "provider_reported"
    } else if cost_micros.is_some() {
        "estimated"
    } else {
        "unavailable"
    };
    let measurement_method = format!(
        "input:{input_method},cached:{cached_method},output:{output_method},cost:{cost_method}"
    );

    Ok(ArmResult {
        arm_type,
        provider,
        model: config.model.clone(),
        commit_sha: config.commit_sha.clone(),
        input_tokens,
        cached_tokens: usage.cached_tokens.unwrap_or(0),
        output_tokens,
        cost_micros: cost_micros.unwrap_or(0),
        latency_ms,
        output_content,
        proxy_observed: arm_type == ArmType::Treatment,
        measurement_method,
    })
}

struct ProviderResponse {
    body: String,
    headers: Vec<(String, String)>,
}

fn send_provider_request(
    url: &str,
    provider: &str,
    api_key: &str,
    run_id: &str,
    arm_type: ArmType,
    payload: &[u8],
) -> anyhow::Result<ProviderResponse> {
    let agent = crate::core::http_client::ureq_agent(
        ureq::config::Config::builder()
            .tls_config(crate::core::http_client::platform_tls_config())
            .timeout_global(Some(request_timeout()))
            .build(),
    );
    let mut request = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("x-leanctx-provider-run-id", run_id)
        .header("x-leanctx-provider-run-arm", arm_type.label());
    if provider == "anthropic" {
        if !api_key.is_empty() {
            request = request.header("x-api-key", api_key);
        }
        request = request.header("anthropic-version", "2023-06-01");
    } else if !api_key.is_empty() {
        request = request.header("Authorization", &format!("Bearer {api_key}"));
    }

    let response = request
        .send(payload)
        .map_err(|error| anyhow!("HTTP request failed: {error}"))?;
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            Some((
                name.as_str().to_ascii_lowercase(),
                value.to_str().ok()?.to_owned(),
            ))
        })
        .collect();
    let body = response
        .into_body()
        .read_to_string()
        .map_err(|error| anyhow!("failed to read provider response: {error}"))?;
    Ok(ProviderResponse { body, headers })
}

fn request_payload(config: &ProviderRunConfig, provider: &str, prompt: &str) -> Value {
    if provider == "anthropic" {
        serde_json::json!({
            "model": config.model,
            "max_tokens": config.budget_tokens,
            "temperature": config.temperature,
            "messages": [{"role": "user", "content": prompt}],
        })
    } else if matches!(provider, "gemini" | "google") {
        serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": prompt}]}],
            "generationConfig": {
                "maxOutputTokens": config.budget_tokens,
                "temperature": config.temperature,
            },
        })
    } else {
        serde_json::json!({
            "model": config.model,
            "max_tokens": config.budget_tokens,
            "temperature": config.temperature,
            "messages": [{"role": "user", "content": prompt}],
            "stream": false,
        })
    }
}

fn extract_output_content(body: &Value, provider: &str) -> Option<String> {
    if provider == "anthropic" {
        if let Some(content) = body.get("content").and_then(Value::as_array) {
            let text = content
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    if matches!(provider, "gemini" | "google") {
        if let Some(text) = body
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .and_then(|candidate| candidate.get("content"))
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|text| !text.is_empty())
        {
            return Some(text);
        }
    }
    body.get("output_text")
        .and_then(Value::as_str)
        .or_else(|| body.get("content").and_then(Value::as_str))
        .or_else(|| {
            body.get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            body.get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("text"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
        .filter(|text| !text.is_empty())
}

#[derive(Default)]
struct UsageMetrics {
    input_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

fn usage_metrics(body: &Value, headers: &[(String, String)]) -> UsageMetrics {
    let usage = body.get("usage");
    let input_tokens = usage.and_then(|usage| {
        first_json_u64(
            usage,
            &["input_tokens", "prompt_tokens", "promptTokenCount"],
        )
    });
    let cached_tokens = usage.and_then(|usage| {
        first_json_u64(
            usage,
            &[
                "cache_read_input_tokens",
                "cache_read_tokens",
                "cached_tokens",
                "cachedContentTokenCount",
            ],
        )
    });
    let output_tokens = usage.and_then(|usage| {
        first_json_u64(
            usage,
            &["output_tokens", "completion_tokens", "candidatesTokenCount"],
        )
    });

    UsageMetrics {
        input_tokens: input_tokens.or_else(|| {
            header_u64(
                headers,
                &[
                    "x-leanctx-input-tokens",
                    "x-leanctx-compressed-input-tokens",
                    "x-provider-input-tokens",
                ],
            )
        }),
        cached_tokens: cached_tokens.or_else(|| {
            header_u64(
                headers,
                &["x-leanctx-cached-tokens", "x-provider-cached-tokens"],
            )
        }),
        output_tokens: output_tokens.or_else(|| {
            header_u64(
                headers,
                &["x-leanctx-output-tokens", "x-provider-output-tokens"],
            )
        }),
    }
}

fn first_json_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(json_u64))
        .or_else(|| {
            value
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(json_u64)
        })
}

fn json_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0)
            .map(|number| number.round() as u64)
    })
}

fn header_u64(headers: &[(String, String)], names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| {
        headers
            .iter()
            .find(|(header, _)| header == name)
            .and_then(|(_, value)| value.trim().parse::<u64>().ok())
    })
}

fn cost_from_headers(headers: &[(String, String)]) -> Option<u64> {
    headers.iter().find_map(|(name, value)| {
        if !name.contains("cost") {
            return None;
        }
        let value = value.trim().trim_start_matches('$');
        if name.contains("micro") {
            return value.parse::<u64>().ok();
        }
        let dollars = value.parse::<f64>().ok()?;
        if !dollars.is_finite() || dollars < 0.0 {
            return None;
        }
        Some((dollars * 1_000_000.0).round() as u64)
    })
}

fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    text.len().div_ceil(4) as u64
}

fn estimated_cost_micros(model: &str, input_tokens: u64, output_tokens: u64) -> Option<u64> {
    let usd = crate::core::benchmark_study::llm_client::cost_for_tokens(
        model,
        input_tokens,
        output_tokens,
    );
    if usd.is_finite() && usd >= 0.0 {
        Some((usd * 1_000_000.0).round() as u64)
    } else {
        None
    }
}

fn compare_arms(baseline: &ArmResult, treatment: &ArmResult) -> ComparisonSummary {
    ComparisonSummary {
        token_savings_ratio: savings_ratio(baseline.input_tokens, treatment.input_tokens),
        cost_savings_ratio: savings_ratio(baseline.cost_micros, treatment.cost_micros),
        quality_preserved: None,
    }
}

fn savings_ratio(baseline: u64, treatment: u64) -> f64 {
    if baseline == 0 {
        0.0
    } else {
        (baseline as f64 - treatment as f64) / baseline as f64
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn request_timeout() -> Duration {
    std::env::var("LEAN_CTX_PROVIDER_RUN_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .filter(|timeout| !timeout.is_zero())
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT)
}

fn direct_base_url(provider: &str) -> String {
    let env_name = format!(
        "LEAN_CTX_PROVIDER_RUN_{}_BASE_URL",
        provider.to_ascii_uppercase().replace('-', "_")
    );
    std::env::var(&env_name)
        .or_else(|_| std::env::var("LEAN_CTX_PROVIDER_RUN_BASE_URL"))
        .or_else(|_| std::env::var(format!("{}_BASE_URL", provider.to_ascii_uppercase())))
        .unwrap_or_else(|_| match provider {
            "anthropic" => "https://api.anthropic.com".to_owned(),
            "gemini" | "google" => "https://generativelanguage.googleapis.com".to_owned(),
            _ => "https://api.openai.com".to_owned(),
        })
}

fn proxy_base_url() -> String {
    std::env::var("LEAN_CTX_PROVIDER_RUN_PROXY_URL")
        .or_else(|_| std::env::var("LEAN_CTX_PROXY_URL"))
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", crate::proxy_setup::default_port()))
}

fn provider_api_key(provider: &str) -> String {
    let env_name = format!(
        "{}_API_KEY",
        provider.to_ascii_uppercase().replace('-', "_")
    );
    if let Ok(key) = std::env::var(&env_name) {
        return key;
    }
    if provider == "anthropic" {
        std::env::var("ANTHROPIC_API_KEY").unwrap_or_default()
    } else {
        std::env::var("OPENAI_API_KEY").unwrap_or_default()
    }
}

fn request_url(base_url: &str, provider: &str, model: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if matches!(provider, "gemini" | "google") {
        format!("{base}/v1beta/models/{model}:generateContent")
    } else if provider == "anthropic" {
        format!("{base}/v1/messages")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

fn save_artifacts(
    config: &ProviderRunConfig,
    prompt: &str,
    context: &str,
    result: &ProviderRunResult,
) -> anyhow::Result<()> {
    let run_dir = poc_run_directory(config);
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("failed to create POC run directory {}", run_dir.display()))?;
    write_json(&run_dir.join("config.json"), config)?;
    write_json(&run_dir.join("baseline.json"), &result.baseline)?;
    write_json(&run_dir.join("treatment.json"), &result.treatment)?;
    write_json(&run_dir.join("comparison.json"), &result.comparison)?;
    write_json(&run_dir.join("result.json"), result)?;
    fs::write(run_dir.join("prompt.txt"), prompt)
        .with_context(|| format!("failed to write {}", run_dir.join("prompt.txt").display()))?;
    fs::write(run_dir.join("context.txt"), context)
        .with_context(|| format!("failed to write {}", run_dir.join("context.txt").display()))?;
    fs::write(
        run_dir.join("baseline.output.txt"),
        &result.baseline.output_content,
    )
    .with_context(|| format!("failed to write baseline output in {}", run_dir.display()))?;
    fs::write(
        run_dir.join("treatment.output.txt"),
        &result.treatment.output_content,
    )
    .with_context(|| format!("failed to write treatment output in {}", run_dir.display()))?;
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(value).context("failed to serialize run artifact")?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn poc_run_directory(config: &ProviderRunConfig) -> PathBuf {
    let root = std::env::var_os("LEAN_CTX_POC_RUN_DIR")
        .or_else(|| std::env::var_os("LEAN_CTX_POC_DIR"))
        .or_else(|| std::env::var_os("LEAN_CTX_RUN_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| config.repo_path.join(".lean-ctx").join("poc").join("runs"));
    root.join(safe_path_component(&config.run_id))
}

fn safe_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "run".to_owned()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_ratio_reports_savings_and_regressions() {
        assert!((savings_ratio(100, 25) - 0.75).abs() < f64::EPSILON);
        assert!((savings_ratio(100, 125) + 0.25).abs() < f64::EPSILON);
        assert_eq!(savings_ratio(0, 0), 0.0);
    }

    #[test]
    fn cost_headers_accept_usd_and_micros() {
        let headers = vec![
            ("x-litellm-response-cost".to_owned(), "0.00042".to_owned()),
            ("x-provider-cost-micros".to_owned(), "7".to_owned()),
        ];
        assert_eq!(cost_from_headers(&headers), Some(420));
        assert_eq!(
            cost_from_headers(&[("x-provider-cost-micros".to_owned(), "7".to_owned())]),
            Some(7)
        );
    }

    #[test]
    fn prompt_context_is_added_when_template_has_no_context_slot() {
        let rendered = render_prompt("Review this task", &[("{context}", "file body")]);
        assert!(rendered.contains("Review context"));
        assert!(rendered.contains("file body"));
    }

    #[test]
    fn output_parsers_cover_provider_shapes() {
        let anthropic = serde_json::json!({
            "content": [{"type": "text", "text": "hello"}]
        });
        let openai = serde_json::json!({
            "choices": [{"message": {"content": "world"}}]
        });
        assert_eq!(
            extract_output_content(&anthropic, "anthropic").as_deref(),
            Some("hello")
        );
        assert_eq!(
            extract_output_content(&openai, "openai").as_deref(),
            Some("world")
        );
    }
}
