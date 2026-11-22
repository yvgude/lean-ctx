// SPDX-License-Identifier: Apache-2.0
use super::{CURRENT_VERSION, https_agent};

/// A GitHub token for the updater's API calls (#2037): `GITHUB_TOKEN`,
/// `GH_TOKEN` or `LEAN_CTX_GITHUB_TOKEN`, first non-empty wins. Without one,
/// GitHub allows 60 requests per hour per IP address, shared by everything
/// behind the same address.
pub(super) fn github_token() -> Option<String> {
    ["GITHUB_TOKEN", "GH_TOKEN", "LEAN_CTX_GITHUB_TOKEN"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

/// The message for an exhausted GitHub API quota, with its reset time when
/// GitHub sent one (`x-ratelimit-reset`, Unix seconds).
pub(super) fn rate_limit_message(reset: Option<i64>, authenticated: bool, now: i64) -> String {
    let when = reset
        .and_then(|seconds| lean_ctx_time::UtcDateTime::from_timestamp(seconds, 0))
        .map(|at| {
            let minutes = ((at.timestamp() - now).max(0) + 59) / 60;
            format!(
                "; it resets at {} UTC (in {minutes} min)",
                at.format("%H:%M")
            )
        })
        .unwrap_or_default();
    if authenticated {
        format!("GitHub API rate limit reached for your token{when}.")
    } else {
        format!(
            "GitHub API rate limit reached (60 requests per hour per IP address without a token){when}. \
             Set GITHUB_TOKEN or GH_TOKEN to a GitHub token (no scopes needed) to raise it to 5000."
        )
    }
}

/// GET a JSON document from the GitHub REST API. Sends the token from
/// [`github_token`] when set — only to api.github.com, and without following
/// redirects so it never reaches another host — and turns an exhausted quota
/// into an actionable message instead of a bare "http status: 403".
pub(crate) fn github_api_json(url: &str) -> Result<serde_json::Value, String> {
    if !url.starts_with("https://api.github.com/") {
        return Err(format!("refusing non-GitHub API URL: {url}"));
    }
    let token = github_token();
    let mut request = https_agent()
        .get(url)
        .config()
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .header("Accept", "application/vnd.github.v3+json");
    if let Some(token) = &token {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    let response = request.call().map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let remaining = header("x-ratelimit-remaining");
    let reset = header("x-ratelimit-reset").and_then(|value| value.parse::<i64>().ok());
    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    match status {
        200 => serde_json::from_str(&body).map_err(|e| e.to_string()),
        403 | 429 if remaining.as_deref() == Some("0") || body.contains("rate limit") => Err(
            rate_limit_message(reset, token.is_some(), lean_ctx_time::UtcDateTime::now().timestamp()),
        ),
        401 if token.is_some() => Err(
            "GitHub rejected the token from GITHUB_TOKEN/GH_TOKEN (401 Bad credentials); fix or unset it."
                .to_string(),
        ),
        404 => Err("not found on GitHub (http status: 404)".to_string()),
        other => Err(format!("GitHub API returned http status: {other}")),
    }
}
