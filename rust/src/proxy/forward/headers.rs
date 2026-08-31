//! Request/response header allowlists for the loopback proxy.

/// Request headers forwarded verbatim to the upstream provider. Anything not
/// listed here is stripped before the request leaves the loopback proxy.
///
/// `openai-project` (and `openai-organization`) must be forwarded: OpenCode and
/// the OpenAI SDK send the project scope via this header for project-scoped API
/// keys when calling the Responses API (`/responses`). Dropping it makes OpenAI
/// reject the request with `Missing scopes: api.responses.write` (#366).
pub(crate) const ALLOWED_REQUEST_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    // Azure OpenAI / AI Foundry credential header (universal providers, #7).
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
    // Grok CLI → cli-chat-proxy.grok.com (subscription rail). Enumerated like
    // Codex/OpenAI above — no prefix wildcards. Missing `x-grok-client-version`
    // makes upstream return 426 Upgrade Required with version "(none)".
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
    // Command Code (`cmd`) CLI headers. Without `x-command-code-version`
    // the upstream returns 403 `upgrade_required` ("CLI is out of date")
    // even for current clients — the version gate runs before body validation.
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

pub(crate) fn is_allowed_request_header(name: &str) -> bool {
    ALLOWED_REQUEST_HEADERS.contains(&name)
        || crate::proxy::bedrock::is_bedrock_request_header(name)
}

pub(crate) fn should_forward_request_header(name: &str, preserve_content_encoding: bool) -> bool {
    is_allowed_request_header(name)
        || (preserve_content_encoding && name.eq_ignore_ascii_case("content-encoding"))
}

pub(crate) const FORWARDED_HEADERS: &[&str] = &[
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
    // #1638: `anthropic-ratelimit-*` is covered by the prefix rule in
    // `is_forwarded_response_header` rather than enumerated here. Enumeration
    // is what broke: the four legacy `requests-`/`tokens-` names were listed,
    // and when Anthropic introduced the `unified-*` family the allowlist
    // silently kept dropping them.
    //
    // `request-id` and `x-should-retry` are client-side state too — the first
    // is what a user quotes to Anthropic support, the second tells the SDK
    // whether a failure is worth retrying.
    "request-id",
    "x-should-retry",
    "retry-after",
    "x-ratelimit-limit-requests",
    "x-ratelimit-remaining-requests",
    "x-ratelimit-limit-tokens",
    "x-ratelimit-remaining-tokens",
    "cache-control",
];

pub(crate) fn is_forwarded_response_header(name: &str) -> bool {
    FORWARDED_HEADERS.contains(&name)
        || name.starts_with("x-codex-")
        || name.starts_with("x-ratelimit-")
        // #1638: the whole `anthropic-ratelimit-*` family, by prefix.
        //
        // Claude Code derives its plan-usage windows *only* from
        // `anthropic-ratelimit-unified-5h-utilization` and its siblings. With
        // them dropped, `rate_limits` vanishes from the statusline payload —
        // and, less visibly but far worse, the near-limit warning machinery
        // reads the same state, so a user behind the proxy is never told they
        // are approaching a limit.
        //
        // A prefix rather than eighteen more literals: these headers carry
        // client-side state, and the family grows upstream on Anthropic's
        // schedule, not ours. Enumerating it means going quiet again the next
        // time it grows.
        || name.starts_with("anthropic-ratelimit-")
        || crate::proxy::bedrock::is_bedrock_response_header(name)
}
