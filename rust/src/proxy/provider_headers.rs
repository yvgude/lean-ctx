// SPDX-License-Identifier: Apache-2.0

//! Pure provider header policy shared by Local and Via Edge.

// Public because the same source is compiled by the sibling operators crate.
#![allow(unreachable_pub)]

pub const ALLOWED_REQUEST_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "api-key",
    "content-type",
    "accept",
    "user-agent",
    "originator",
    "anthropic-version",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
    "openai-organization",
    "openai-project",
    "openai-beta",
    "chatgpt-account-id",
    "x-openai-fedramp",
    "x-openai-internal-codex-residency",
    "x-openai-internal-codex-responses-lite",
    "x-openai-product-sku",
    "oai-product-sku",
    "x-oai-attestation",
    "x-client-request-id",
    "x-codex-beta-features",
    "x-codex-installation-id",
    "x-codex-parent-thread-id",
    "x-openai-subagent",
    "x-codex-turn-state",
    "x-codex-turn-metadata",
    "x-codex-window-id",
    "x-openai-memgen-request",
    "x-responsesapi-include-timing-metrics",
    "mcp-session-id",
    "last-event-id",
    "cache-control",
    "x-goog-api-key",
    "x-goog-api-client",
    "x-xai-token-auth",
    "x-models-etag",
    "x-grok-client-version",
    "x-grok-client-identifier",
    "x-grok-client-mode",
    "x-grok-client-surface",
    "x-grok-model-override",
    "x-grok-agent-id",
    "x-grok-session-id",
    "x-grok-turn-id",
    "x-grok-conv-id",
    "x-grok-req-id",
    "x-grok-deployment-id",
    "x-grok-user-id",
    "x-grok-context-window",
    "x-grok-max-completion-tokens",
    "x-grok-doom-loop-check",
    "x-grok-managed-gateway",
    "x-command-code-version",
    "x-cli-environment",
    "x-oauth-token",
    "x-oauth-provider",
    "x-project-slug",
    "x-taste-learning",
    "x-taste-usage",
    "x-oss-primary-provider",
    "x-system-prompt-breakdown",
    "x-cmd-zdr",
    "x-session-id",
];

pub const FORWARDED_HEADERS: &[&str] = &[
    "content-type",
    "content-encoding",
    "mcp-session-id",
    "x-request-id",
    "x-oai-request-id",
    "cf-ray",
    "x-openai-authorization-error",
    "x-error-json",
    "openai-organization",
    "openai-model",
    "openai-processing-ms",
    "openai-version",
    "x-models-etag",
    "x-reasoning-included",
    "anthropic-ratelimit-requests-limit",
    "anthropic-ratelimit-requests-remaining",
    "anthropic-ratelimit-tokens-limit",
    "anthropic-ratelimit-tokens-remaining",
    "retry-after",
    "x-ratelimit-limit-requests",
    "x-ratelimit-remaining-requests",
    "x-ratelimit-limit-tokens",
    "x-ratelimit-remaining-tokens",
    "cache-control",
];

const ANTHROPIC_REQUEST_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "content-type",
    "accept",
    "user-agent",
    "originator",
    "anthropic-version",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
    "cache-control",
];

const ANTHROPIC_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "content-encoding",
    "x-request-id",
    "cf-ray",
    "anthropic-ratelimit-requests-limit",
    "anthropic-ratelimit-requests-remaining",
    "anthropic-ratelimit-tokens-limit",
    "anthropic-ratelimit-tokens-remaining",
    "retry-after",
    "cache-control",
];

pub fn is_allowed_request_header(name: &str) -> bool {
    ALLOWED_REQUEST_HEADERS.contains(&name)
}

pub fn is_forwarded_response_header(name: &str) -> bool {
    FORWARDED_HEADERS.contains(&name)
        || name.starts_with("x-codex-")
        || name.starts_with("x-ratelimit-")
}

pub fn is_anthropic_request_header(name: &str) -> bool {
    ANTHROPIC_REQUEST_HEADERS.contains(&name)
}

pub fn is_anthropic_response_header(name: &str) -> bool {
    ANTHROPIC_RESPONSE_HEADERS.contains(&name) || name.starts_with("x-ratelimit-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_policy_forwards_auth_but_not_cookies_or_hop_headers() {
        for allowed in ["authorization", "x-api-key", "anthropic-version"] {
            assert!(is_anthropic_request_header(allowed));
        }
        for rejected in ["cookie", "host", "content-length", "proxy-authorization"] {
            assert!(!is_anthropic_request_header(rejected));
        }
        assert!(is_anthropic_response_header("content-type"));
        assert!(!is_anthropic_response_header("set-cookie"));
    }
}
