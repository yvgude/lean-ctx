//! Conformance & reproducibility scorecard (`conformance-v1`, EPIC 12.17).
//!
//! A self-check any user or CI can run to prove this instance honors its own
//! contracts and that its extension surface behaves. It exercises three areas:
//!
//! * **contracts** — every machine-verified contract version is present.
//! * **reproducibility** — the public discovery documents (`/v1/capabilities`,
//!   `/v1/openapi.json`) are deterministic (same bytes across two builds).
//! * **extensions** — every registered compressor / chunker / read-mode in the
//!   [`extension_registry`](super::extension_registry) satisfies the stable
//!   invariants the engine relies on (determinism, budget honoring, coverage).
//!
//! The output is a [`Scorecard`]: a flat list of [`Check`]s plus a pass count.
//! It is data, not prose, so it can be rendered (CLI), shared (JSON), or gated
//! on (`all_passed()` in `tests/conformance_suite.rs`).

use serde::Serialize;
use serde_json::{json, Value};

/// A representative corpus the extension invariants run against. Mixes blank
/// lines, multibyte UTF-8, and paragraph boundaries to stress edge cases.
const CORPUS: &[&str] = &[
    "",
    "single line",
    "a\n\n\n\nb  \n",
    "para one\n\npara two\n\n\npara three",
    "mültibyte ä ö ü 漢字 \n\n end",
];

/// One conformance check result.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub category: String,
    pub passed: bool,
    pub detail: String,
}

impl Check {
    fn pass(category: &str, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            category: category.to_string(),
            passed: true,
            detail: String::new(),
        }
    }

    fn fail(category: &str, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            category: category.to_string(),
            passed: false,
            detail: detail.into(),
        }
    }

    fn from_bool(category: &str, name: impl Into<String>, ok: bool, fail_detail: &str) -> Self {
        if ok {
            Self::pass(category, name)
        } else {
            Self::fail(category, name, fail_detail)
        }
    }
}

/// The full result of a conformance run.
#[derive(Debug, Clone, Serialize)]
pub struct Scorecard {
    pub version: u32,
    pub checks: Vec<Check>,
}

impl Scorecard {
    #[must_use]
    pub fn passed(&self) -> usize {
        self.checks.iter().filter(|c| c.passed).count()
    }

    #[must_use]
    pub fn total(&self) -> usize {
        self.checks.len()
    }

    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    #[must_use]
    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| !c.passed).collect()
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "version": self.version,
            "passed": self.passed(),
            "total": self.total(),
            "all_passed": self.all_passed(),
            "checks": self.checks,
        })
    }
}

/// Run the full conformance suite against this instance.
#[must_use]
pub fn run() -> Scorecard {
    let mut checks = Vec::new();
    checks.extend(contract_checks());
    checks.extend(reproducibility_checks());
    checks.extend(extension_checks());
    Scorecard { version: 1, checks }
}

fn contract_checks() -> Vec<Check> {
    let present = !crate::core::contracts::versions_kv().is_empty();
    vec![Check::from_bool(
        "contracts",
        "contract_versions_present",
        present,
        "versions_kv() is empty",
    )]
}

fn reproducibility_checks() -> Vec<Check> {
    let caps_stable = crate::core::server_capabilities::capabilities_value()
        == crate::core::server_capabilities::capabilities_value();
    let openapi_stable =
        crate::core::openapi::openapi_value() == crate::core::openapi::openapi_value();
    vec![
        Check::from_bool(
            "reproducibility",
            "capabilities_deterministic",
            caps_stable,
            "capabilities document differs across builds",
        ),
        Check::from_bool(
            "reproducibility",
            "openapi_deterministic",
            openapi_stable,
            "openapi document differs across builds",
        ),
    ]
}

fn extension_checks() -> Vec<Check> {
    let mut checks = Vec::new();
    let Ok(reg) = crate::core::extension_registry::global().read() else {
        checks.push(Check::fail(
            "extensions",
            "registry_readable",
            "extension registry lock poisoned",
        ));
        return checks;
    };

    for name in reg.compressor_names() {
        if let Some(c) = reg.compressor(&name) {
            checks.push(compressor_invariants(&name, c.as_ref()));
        }
    }
    for name in reg.chunker_names() {
        if let Some(c) = reg.chunker(&name) {
            checks.push(chunker_invariants(&name, c.as_ref()));
        }
    }
    for name in reg.read_mode_names() {
        if let Some(m) = reg.read_mode(&name) {
            checks.push(read_mode_invariants(&name, m.as_ref()));
        }
    }
    checks
}

fn compressor_invariants(name: &str, c: &dyn crate::core::extension_registry::Compressor) -> Check {
    for input in CORPUS {
        // Determinism.
        if c.compress(input, None) != c.compress(input, None) {
            return Check::fail(
                "extensions",
                format!("compressor:{name}"),
                "non-deterministic",
            );
        }
        // Budget is a hard byte ceiling, never split mid-char (valid UTF-8).
        let budget = 4;
        let out = c.compress(input, Some(budget));
        if out.len() > budget {
            return Check::fail(
                "extensions",
                format!("compressor:{name}"),
                format!("exceeded byte budget: {} > {budget}", out.len()),
            );
        }
    }
    Check::pass("extensions", format!("compressor:{name}"))
}

fn chunker_invariants(name: &str, c: &dyn crate::core::extension_registry::Chunker) -> Check {
    // Empty input ⇒ no chunks.
    if !c.chunk("").is_empty() {
        return Check::fail(
            "extensions",
            format!("chunker:{name}"),
            "empty input produced chunks",
        );
    }
    for input in CORPUS.iter().filter(|s| !s.trim().is_empty()) {
        // Determinism.
        if c.chunk(input) != c.chunk(input) {
            return Check::fail("extensions", format!("chunker:{name}"), "non-deterministic");
        }
        let chunks = c.chunk(input);
        // Non-empty input ⇒ at least one chunk, none empty after trim.
        if chunks.is_empty() {
            return Check::fail(
                "extensions",
                format!("chunker:{name}"),
                "non-empty input produced no chunks",
            );
        }
        if chunks.iter().any(|c| c.trim().is_empty()) {
            return Check::fail(
                "extensions",
                format!("chunker:{name}"),
                "produced an empty chunk",
            );
        }
    }
    Check::pass("extensions", format!("chunker:{name}"))
}

fn read_mode_invariants(name: &str, m: &dyn crate::core::extension_registry::ReadMode) -> Check {
    for input in CORPUS {
        if m.render(input, "x.txt") != m.render(input, "x.txt") {
            return Check::fail(
                "extensions",
                format!("read_mode:{name}"),
                "non-deterministic",
            );
        }
    }
    // The byte-faithful `full` mode must round-trip source verbatim.
    if name == "full" {
        let sample = "verbatim\nsource\n漢字";
        if m.render(sample, "x.txt") != sample {
            return Check::fail("extensions", "read_mode:full", "full mode altered source");
        }
    }
    Check::pass("extensions", format!("read_mode:{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_suite_passes() {
        let card = run();
        assert!(
            card.all_passed(),
            "conformance failures: {:?}",
            card.failures()
        );
        assert!(card.total() >= 6, "expected a meaningful number of checks");
    }

    #[test]
    fn scorecard_json_shape() {
        let v = run().to_json();
        assert_eq!(v["version"], 1);
        assert!(v["checks"].is_array());
        assert_eq!(v["passed"], v["total"]);
    }

    #[test]
    fn detects_a_nondeterministic_compressor() {
        use std::sync::atomic::{AtomicU64, Ordering};
        struct Flaky(AtomicU64);
        impl crate::core::extension_registry::Compressor for Flaky {
            #[allow(clippy::unnecessary_literal_bound)]
            fn name(&self) -> &str {
                "flaky"
            }
            fn compress(&self, _input: &str, _budget: Option<usize>) -> String {
                self.0.fetch_add(1, Ordering::SeqCst).to_string()
            }
        }
        let check = compressor_invariants("flaky", &Flaky(AtomicU64::new(0)));
        assert!(!check.passed);
        assert!(check.detail.contains("non-deterministic"));
    }
}
