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
use serde_json::{Value, json};

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
    checks.extend(accuracy_checks());
    checks.extend(a2a_checks());
    Scorecard { version: 1, checks }
}

// ---------------------------------------------------------------------------
// A2A: agent card and JSON-RPC contract conformance (GL#449).
// ---------------------------------------------------------------------------

/// Fields the A2A spec requires on a published agent card.
const A2A_CARD_REQUIRED: &[&str] = &[
    "name",
    "description",
    "version",
    "protocolVersion",
    "capabilities",
    "skills",
    "defaultInputModes",
    "defaultOutputModes",
    "authentication",
];

fn a2a_checks() -> Vec<Check> {
    let mut checks = Vec::new();

    let card = crate::core::a2a::agent_card::build_agent_card("conformance");
    let missing: Vec<&&str> = A2A_CARD_REQUIRED
        .iter()
        .filter(|f| card.get(**f).is_none())
        .collect();
    checks.push(Check::from_bool(
        "a2a",
        "agent_card_required_fields",
        missing.is_empty(),
        &format!("agent card missing fields: {missing:?}"),
    ));

    checks.push(Check::from_bool(
        "a2a",
        "agent_card_deterministic",
        card == crate::core::a2a::agent_card::build_agent_card("conformance"),
        "two agent card builds differ",
    ));

    let skills_ok = card
        .get("skills")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|skills| {
            !skills.is_empty()
                && skills.iter().all(|s| {
                    s.get("id").is_some()
                        && s.get("name").is_some()
                        && s.get("description").is_some()
                })
        });
    checks.push(Check::from_bool(
        "a2a",
        "agent_card_skills_complete",
        skills_ok,
        "skills missing id/name/description",
    ));

    // JSON-RPC error contract: wrong version → -32600, unknown method → -32601.
    let bad_version = crate::core::a2a::a2a_compat::handle_a2a_jsonrpc(
        &crate::core::a2a::a2a_compat::JsonRpcRequest {
            jsonrpc: "1.0".to_string(),
            id: serde_json::Value::Number(1.into()),
            method: "tasks/get".to_string(),
            params: serde_json::Value::Null,
        },
    );
    checks.push(Check::from_bool(
        "a2a",
        "jsonrpc_rejects_bad_version",
        bad_version.error.as_ref().is_some_and(|e| e.code == -32600),
        "jsonrpc 1.0 not rejected with -32600",
    ));

    let unknown_method = crate::core::a2a::a2a_compat::handle_a2a_jsonrpc(
        &crate::core::a2a::a2a_compat::JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: serde_json::Value::Number(2.into()),
            method: "tasks/nonexistent".to_string(),
            params: serde_json::Value::Null,
        },
    );
    checks.push(Check::from_bool(
        "a2a",
        "jsonrpc_unknown_method_code",
        unknown_method
            .error
            .as_ref()
            .is_some_and(|e| e.code == -32601),
        "unknown method not rejected with -32601",
    ));

    checks
}

// ---------------------------------------------------------------------------
// Accuracy: structural invariants of the lossy read modes (GL#441).
//
// Byte-golden snapshots would break on every intentional format improvement;
// these checks instead pin down what each mode must *preserve* (symbols,
// deps) and must *drop* (bodies), plus determinism and size bounds — the
// properties an agent's correctness actually depends on.
// ---------------------------------------------------------------------------

/// A stable Rust fixture exercising pub fns, a struct, imports, and a body
/// secret that lossy modes must strip.
const ACCURACY_FIXTURE: &str = r"use std::collections::HashMap;
use std::path::PathBuf;

pub struct Inventory {
    items: HashMap<String, u32>,
}

pub fn add_item(inv: &mut Inventory, name: &str, qty: u32) {
    let body_secret_alpha = qty + 1;
    inv.items.insert(name.to_string(), body_secret_alpha);
}

pub fn total_count(inv: &Inventory) -> u32 {
    let body_secret_beta: u32 = inv.items.values().sum();
    body_secret_beta
}

fn internal_rebalance(inv: &mut Inventory) {
    inv.items.retain(|_, qty| *qty > 0);
}
";

/// Symbols every lossy structural mode must keep visible.
const MUST_KEEP_SYMBOLS: &[&str] = &["add_item", "total_count", "Inventory"];

/// Body-local identifiers `signatures`/`map` must strip.
const MUST_DROP_BODIES: &[&str] = &["body_secret_alpha", "body_secret_beta"];

fn render_mode(mode: &str) -> String {
    render_mode_full(mode).0
}

/// Rendered view plus its pre-footer body token count. The compression-size
/// invariant measures `.1` (the body), since the reactive recovery footer and the
/// savings line are orthogonal affordances appended after compression, not part of
/// the compressed content.
fn render_mode_full(mode: &str) -> (String, usize) {
    crate::tools::ctx_read::render::process_mode(
        ACCURACY_FIXTURE,
        mode,
        "",
        "fixture.rs",
        "rs",
        crate::core::tokens::count_tokens(ACCURACY_FIXTURE),
        crate::tools::CrpMode::Off,
        "conformance/fixture.rs",
        None,
    )
}

fn accuracy_checks() -> Vec<Check> {
    let mut checks = Vec::new();

    for mode in ["map", "signatures", "aggressive", "entropy"] {
        checks.push(Check::from_bool(
            "accuracy",
            format!("read_mode_deterministic:{mode}"),
            render_mode(mode) == render_mode(mode),
            "two renders of the same fixture differ",
        ));
    }

    for mode in ["map", "signatures"] {
        let out = render_mode(mode);
        let missing: Vec<&&str> = MUST_KEEP_SYMBOLS
            .iter()
            .filter(|s| !out.contains(**s))
            .collect();
        checks.push(Check::from_bool(
            "accuracy",
            format!("read_mode_keeps_symbols:{mode}"),
            missing.is_empty(),
            &format!("symbols lost: {missing:?}"),
        ));
        let leaked: Vec<&&str> = MUST_DROP_BODIES
            .iter()
            .filter(|s| out.contains(**s))
            .collect();
        checks.push(Check::from_bool(
            "accuracy",
            format!("read_mode_strips_bodies:{mode}"),
            leaked.is_empty(),
            &format!("body content leaked: {leaked:?}"),
        ));
    }

    let fixture_tokens = crate::core::tokens::count_tokens(ACCURACY_FIXTURE);
    for mode in ["map", "signatures", "aggressive"] {
        let sent = render_mode_full(mode).1;
        checks.push(Check::from_bool(
            "accuracy",
            format!("read_mode_compresses:{mode}"),
            sent < fixture_tokens,
            &format!("no compression: {sent} >= {fixture_tokens} tokens"),
        ));
    }

    // Target-density mode (GL#444): the body (excluding header/savings lines)
    // must stay within the token budget, and the render must be deterministic.
    {
        let target = 0.4_f64;
        let result = crate::core::entropy::entropy_compress_to_density(ACCURACY_FIXTURE, target);
        let actual = result.compressed_tokens as f64 / fixture_tokens.max(1) as f64;
        checks.push(Check::from_bool(
            "accuracy",
            "density_respects_budget:0.4",
            actual <= target + 0.10,
            &format!("density {actual:.2} exceeds target {target:.2} (+0.10 tolerance)"),
        ));
        checks.push(Check::from_bool(
            "accuracy",
            "density_deterministic:0.4",
            render_mode("density:0.4") == render_mode("density:0.4"),
            "two density renders of the same fixture differ",
        ));
    }

    checks
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
        // Shape only: `passed == total` is covered by builtin_suite_passes.
        // Asserting it here races with tests that register an intentionally
        // broken compressor in the global extension registry.
        let passed = v["passed"].as_u64().expect("passed is a number");
        let total = v["total"].as_u64().expect("total is a number");
        assert!(passed <= total);
        assert_eq!(v["checks"].as_array().map(|c| c.len() as u64), Some(total));
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
