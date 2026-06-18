//! linkerd (`linkerd check`) output compression.
//!
//! `linkerd check` prints a `√`/`×` line per check, grouped under section
//! headers, ending with a `Status check results are …` line. Passing checks are
//! noise once you know the total; failures (with their indented hint detail) and
//! the final verdict are what matter. We keep failures + a pass/fail tally.

use crate::core::compressor::strip_ansi;

pub fn compress(command: &str, output: &str) -> Option<String> {
    let sub = command
        .trim()
        .strip_prefix("linkerd")
        .map_or("", str::trim_start)
        .split_whitespace()
        .next()
        .unwrap_or("");
    if sub != "check" {
        return None;
    }
    Some(compress_check(output))
}

fn compress_check(output: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut in_failure = false;

    for raw in output.lines() {
        let line = strip_ansi(raw);
        let t = line.trim_end();
        let probe = t.trim();
        if probe.is_empty() {
            in_failure = false;
            continue;
        }
        // section underlines like "-----".
        if probe.chars().all(|c| c == '-') {
            continue;
        }
        if probe.starts_with('√') || probe.starts_with('✓') {
            passed += 1;
            in_failure = false;
            continue;
        }
        if probe.starts_with('×') || probe.starts_with('✗') {
            failed += 1;
            in_failure = true;
            kept.push(probe.to_string());
            continue;
        }
        // indented hint/detail lines belong to the preceding failed check.
        if in_failure && (t.starts_with(' ') || t.starts_with('\t')) {
            kept.push(probe.to_string());
            continue;
        }
        let pl = probe.to_ascii_lowercase();
        if pl.starts_with("status check results") {
            kept.push(probe.to_string());
            in_failure = false;
        }
    }

    let tally = format!("linkerd check: {passed} passed, {failed} failed");
    if kept.is_empty() {
        return tally;
    }
    format!("{tally}\n{}", kept.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECK: &str = "kubernetes-api\n--------------\n√ can initialize the client\n√ can query the Kubernetes API\n\nlinkerd-existence\n-----------------\n√ 'linkerd-config' config map exists\n× control plane pods are ready\n    some pods are not ready: linkerd-destination-abc\n    see https://linkerd.io/2/checks/#l5d-api-control-ready for hints\n\nStatus check results are ×\n";

    #[test]
    fn keeps_failures_and_verdict_drops_passing() {
        let r = compress("linkerd check", CHECK).unwrap();
        assert!(r.contains("× control plane pods are ready"), "{r}");
        assert!(r.contains("some pods are not ready"), "keeps hint: {r}");
        assert!(r.contains("Status check results are ×"), "{r}");
        assert!(r.contains("3 passed, 1 failed"), "tally: {r}");
        assert!(
            !r.contains("can initialize the client"),
            "drops passing: {r}"
        );
    }

    #[test]
    fn non_check_subcommand_passes_through() {
        assert!(compress("linkerd viz stat deploy", "some table").is_none());
    }
}
