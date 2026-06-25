//! Runtime safeguards for addon tool output (#866).
//!
//! An addon's tool result is **untrusted content** that flows straight into the
//! model context — both an exfiltration surface (the addon could echo back a
//! secret it read) and a prompt-injection surface. Before the gateway hands a
//! downstream result to the model ([`crate::core::gateway::proxy`]), it runs the
//! output through the same redaction the shell layer uses (single source of
//! truth: [`crate::core::redaction`] + [`crate::core::secret_detection`]) and
//! records an audit line tagging the bytes as untrusted, attributed to the
//! originating server.

/// Redact secrets from a downstream addon's tool output and emit an audit trace
/// marking it untrusted. Returns the scrubbed text the model will see.
#[must_use]
pub fn scrub_output(server: &str, text: &str) -> String {
    let masked = crate::core::redaction::redact_text(text);
    let (redacted, matches) = crate::core::secret_detection::scan_and_redact_from_config(&masked);

    if !matches.is_empty() {
        let mut names: Vec<&str> = matches.iter().map(|m| m.pattern_name).collect();
        names.sort_unstable();
        names.dedup();
        tracing::warn!(
            "[ADDON OUTPUT REDACTION] {} secret(s) redacted from untrusted server `{server}` output: {}",
            matches.len(),
            names.join(", ")
        );
    }
    tracing::debug!(
        "[ADDON UNTRUSTED OUTPUT] server=`{server}` bytes={} — entered model context as untrusted content",
        redacted.len()
    );
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_clean_output_through() {
        let out = scrub_output("demo", "hello world, nothing secret here");
        assert_eq!(out, "hello world, nothing secret here");
    }

    #[test]
    fn redacts_a_secret_in_addon_output() {
        // A GitHub token the addon tried to echo back must not reach the model.
        let leaked = "token=ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        let out = scrub_output("evil", leaked);
        assert!(!out.contains("ghp_0123456789abcdefghijklmnopqrstuvwxyzAB"));
        assert!(out.contains("REDACTED"));
    }
}
