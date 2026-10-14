//! Risk classification for `config set` governance (#852).
//!
//! Most config keys are routine (cache sizes, density, theme) and are written
//! without friction. A small set, however, changes lean-ctx's *security posture*
//! (containment, secret redaction) or *network routing / data egress* (upstream
//! redirects). Flipping one of those silently can weaken the user's machine or
//! leak credentials to a provider, so `config set` shows a before→after review
//! and requires confirmation (or `--yes`) before applying — mirroring the
//! existing `yolo` / `secure` confirmation pattern.
//!
//! This is a deterministic, local-only lookup: no telemetry, no heuristics.

/// A consequential config key and the one-line note explaining what changing it
/// does. Returned by [`classify`]; `None` ⇒ a routine key, written directly.
pub struct ConfigRisk {
    /// Human-readable consequence of changing this key, shown in the review.
    pub note: &'static str,
}

/// Classifies a fully-qualified config key (dot-path, e.g. `secret_detection.enabled`).
///
/// Returns a [`ConfigRisk`] for keys whose change is security- or
/// egress-relevant; `None` for everything else.
#[must_use]
pub fn classify(key: &str) -> Option<ConfigRisk> {
    let note = match key {
        "path_jail" => {
            "Path jail confines agent file access to your projects. Disabling it lets tools read and write any path on this machine."
        }
        "path_jail_scope" => {
            "Jail scope decides what tools may reach: \"home\" admits every project below your home directory (protected dot dirs and ~/Library excepted), \"project\" only the active project."
        }
        "shell_security" => {
            "Shell gating blocks dangerous commands via an allowlist. Lowering it (warn/off) lets the agent run any command."
        }
        "sandbox_level" => {
            "Sandbox level governs how strictly tool execution is contained. Lowering it reduces isolation."
        }
        "secret_detection.enabled" => {
            "Secret detection masks API keys and .env values before they reach the LLM. Disabling it can leak credentials to the provider."
        }
        "secret_detection.redact" => {
            "Secret redaction masks detected secrets. Disabling it sends them verbatim to the provider."
        }
        "boundary_policy" => {
            "Boundary policy controls what context is allowed to leave this machine. Relaxing it widens data egress."
        }
        "proxy_require_token" => {
            "Proxy token policy controls whether provider API keys can authenticate local proxy requests without the lean-ctx Bearer token."
        }
        "proxy_loopback_open" => {
            "Disables ALL proxy authentication on loopback binds. Any local process can access the proxy without a token."
        }
        "proxy.anthropic_upstream"
        | "proxy.openai_upstream"
        | "proxy.chatgpt_upstream"
        | "proxy.gemini_upstream" => {
            "Redirects provider traffic to a custom upstream — every request and API key for this provider will flow through it."
        }
        _ => return None,
    };
    Some(ConfigRisk { note })
}

/// True if changing `key` is consequential enough to require a review.
#[must_use]
pub fn is_consequential(key: &str) -> bool {
    classify(key).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_keys_are_consequential() {
        for key in [
            "path_jail",
            "path_jail_scope",
            "shell_security",
            "sandbox_level",
            "secret_detection.enabled",
            "secret_detection.redact",
            "boundary_policy",
            "proxy_require_token",
            "proxy_loopback_open",
            "proxy.openai_upstream",
            "proxy.chatgpt_upstream",
            "proxy.anthropic_upstream",
            "proxy.gemini_upstream",
        ] {
            assert!(is_consequential(key), "{key} should be consequential");
            assert!(
                !classify(key).unwrap().note.is_empty(),
                "`classify(key).unwrap().note` is empty"
            );
        }
    }

    #[test]
    fn routine_keys_are_not_consequential() {
        for key in [
            "theme",
            "max_ram_percent",
            "compression_level",
            "proxy.port",
            "proxy.effort",
            "bm25_max_cache_mb",
        ] {
            assert!(!is_consequential(key), "{key} should be routine");
        }
    }
}
