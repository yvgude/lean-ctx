//! Cosign (sigstore) output compression.
//!
//! `cosign verify` prints a human-readable verdict (`Verification for X --`
//! plus the checks performed) followed by a large JSON signature payload. We
//! keep the verdict/checks and any error, dropping the trailing JSON blob.

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("cosign: ok".to_string());
    }

    let mut kept: Vec<String> = Vec::new();
    for raw in trimmed.lines() {
        let line = strip_ansi(raw);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // The JSON signature payload starts with '[' or '{' — stop there.
        if line.starts_with('[') || line.starts_with('{') {
            break;
        }
        kept.push(line.to_string());
    }

    if kept.is_empty() {
        return Some("cosign: ok".to_string());
    }
    Some(kept.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERIFY: &str = "\nVerification for myimage:latest --\nThe following checks were performed on each of these signatures:\n  - The cosign claims were validated\n  - The signatures were verified against the specified public key\n[{\"critical\":{\"identity\":{\"docker-reference\":\"myimage\"},\"image\":{\"docker-manifest-digest\":\"sha256:deadbeef\"}}}]\n";

    #[test]
    fn keeps_verdict_drops_json() {
        let r = compress("cosign verify myimage", VERIFY).unwrap();
        assert!(r.contains("Verification for myimage:latest"), "{r}");
        assert!(r.contains("claims were validated"), "{r}");
        assert!(
            !r.contains("docker-manifest-digest"),
            "drops json payload: {r}"
        );
        assert!(!r.contains("sha256:deadbeef"), "{r}");
    }

    #[test]
    fn keeps_errors() {
        let out = "Error: no matching signatures:\nfailed to verify signature\n";
        let r = compress("cosign verify x", out).unwrap();
        assert!(r.contains("no matching signatures"), "{r}");
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("cosign verify x", "").unwrap(), "cosign: ok");
    }
}
