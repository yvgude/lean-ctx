// SPDX-License-Identifier: Apache-2.0
//! Optional licensed semantic detectors over a caller-admitted text. Detection
//! remains private; the public host bounds the input, validates every span the
//! runtime returns, applies redactions itself and owns all wording. Nothing a
//! runtime returns can rewrite, add or echo content.

use super::semantic_coverage::Declared;
use super::{InstallError, Result};

/// Largest text sent per invocation. Larger objects are reported as partially
/// inspected by the caller, never as clean.
pub(crate) const MAX_SOURCE_BYTES: usize = 65_536;
pub(crate) const MAX_FINDINGS: usize = 32;
const MAX_RESPONSE_BYTES: usize = 16_384;
/// Milliseconds one inspection may take, waiting for the worker included.
const ANSWER_DEADLINE_MS: u64 = 2000;
/// How long "the package declares the capability but its model set is not
/// installed" is remembered before the package is authenticated again: a set
/// can be installed later without the settings changing.
const SET_RECHECK: std::time::Duration = std::time::Duration::from_mins(1);

/// Why a text was not inspected. Either way the caller reports the semantic
/// check as unavailable, never as clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotInspected {
    /// The worker is starting and has not loaded its models yet. Nothing
    /// waits for that: a short-lived process normally ends in this state.
    Warming,
    /// Refused, failed, timed out, busy, or not usable with these settings.
    Unavailable,
}

impl NotInspected {
    /// The reason on the detector signal's coverage. The decision keeps the
    /// one code `semantic.unavailable` for both.
    pub(crate) const fn coverage_reason(self) -> &'static str {
        match self {
            Self::Warming => "semantic.warming",
            Self::Unavailable => "semantic.unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticCategory {
    Secret,
    Pii,
    PromptInjection,
}

impl SemanticCategory {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Pii => "pii",
            Self::PromptInjection => "prompt_injection",
        }
    }
}

/// One validated span in the inspected text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticFinding {
    pub(crate) category: SemanticCategory,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) confidence_milli: u16,
}

/// A validated response: spans lie inside `bytes_inspected`, on character
/// boundaries, sorted and non-overlapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SemanticReport {
    pub(crate) bytes_inspected: usize,
    pub(crate) findings: Vec<SemanticFinding>,
}

/// The categories a semantic answer speaks for. A category outside the set
/// was not examined: no finding for it means nothing, never "clean".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CategorySet(u8);

impl CategorySet {
    /// Contract 1.0.0 has no coverage statement: an answer speaks for all.
    pub(crate) const ALL: Self = Self(0b111);
    pub(crate) const NONE: Self = Self(0);
    pub(crate) const EVERY: [SemanticCategory; 3] = [
        SemanticCategory::Secret,
        SemanticCategory::Pii,
        SemanticCategory::PromptInjection,
    ];

    pub(crate) const fn with(self, category: SemanticCategory) -> Self {
        Self(self.0 | (1 << category as u8))
    }

    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub(crate) const fn contains(self, category: SemanticCategory) -> bool {
        self.0 & (1 << category as u8) != 0
    }

    /// In contract order.
    #[cfg(test)]
    pub(crate) fn names(self) -> Vec<&'static str> {
        Self::EVERY
            .into_iter()
            .filter(|category| self.contains(*category))
            .map(SemanticCategory::as_str)
            .collect()
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema_version: u32,
    strategy: String,
    source_sha256: String,
    bytes_inspected: usize,
    /// Contract 1.1.0 only: the requested categories that were examined.
    #[serde(default)]
    categories: Option<Vec<SemanticCategory>>,
    findings: Vec<SemanticFinding>,
}

/// The licensed detector, when the runtime is installed, enabled, accepted and
/// declares the capability; `None` otherwise (the gateway then runs built-ins
/// only and reports nothing about semantics).
///
/// The runtime settings are taken from the configuration snapshot the caller
/// resolves its admission policy from (they are user-global and never merged
/// from a project file), and are returned so that every inspection of that
/// admission uses exactly them: nothing here reads the configuration again.
pub(crate) fn bound(
    config: &crate::core::config::Config,
) -> Option<std::sync::Arc<crate::core::config::IntelligenceRuntimeConfig>> {
    let runtime = &config.intelligence_runtime;
    if !(runtime.enabled && runtime.accept_proprietary && super::bootstrap::permits_config(runtime))
    {
        // A runtime that is switched off keeps no worker in this process.
        super::semantic_worker::retire();
        return None;
    }
    attached(runtime).then(|| std::sync::Arc::new(runtime.clone()))
}

/// Whether the configured runtime offers semantic detectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Offer {
    /// The authenticated package's installed model set examines at least
    /// one category. Which ones is reported by `runtime status` from the
    /// installation, and by every answer for itself.
    Offered,
    /// The authenticated package does not declare it: nothing to attach.
    NotOffered,
    /// The configured package could not be authenticated at this moment.
    Unverified,
}

/// A runtime that does not declare the capability is never asked: the stage
/// is attached only for a package whose authenticated description offers it.
fn attached(runtime: &crate::core::config::IntelligenceRuntimeConfig) -> bool {
    #[cfg(test)]
    if test_seam::invoke_override().is_some() {
        return true;
    }
    offer(runtime) == Offer::Offered
}

/// What the package these settings select offers. The package is
/// authenticated once per process and settings: what one manifest digest
/// declares cannot change, so a definite answer is remembered for the process
/// lifetime. "Declared, but the model set is not installed" can change and is
/// remembered for [`SET_RECHECK`] only. No runtime process is started.
pub(crate) fn offer(runtime: &crate::core::config::IntelligenceRuntimeConfig) -> Offer {
    match declared(runtime) {
        None => Offer::Unverified,
        Some(declared) if declared.categories() == CategorySet::NONE => Offer::NotOffered,
        Some(_) => Offer::Offered,
    }
}

/// What the installed model set of these settings declares: its categories
/// and, per model, the languages and document types it covers. `None` when
/// the package cannot be authenticated at this moment; an empty declaration
/// when nothing is offered. Remembered like [`offer`].
pub(crate) fn declared(
    runtime: &crate::core::config::IntelligenceRuntimeConfig,
) -> Option<Declared> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, PoisonError};
    use std::time::Instant;
    // An empty set: nothing is offered. With an instant: until then only.
    type Known = HashMap<[u8; 32], (Declared, Option<Instant>)>;
    static KNOWN: OnceLock<Mutex<Known>> = OnceLock::new();

    let settings = serde_json::to_vec(runtime).ok()?;
    let key = *blake3::hash(&settings).as_bytes();
    let known = KNOWN.get_or_init(Mutex::default);
    let now = Instant::now();
    let remembered = known
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .filter(|(_, until)| !until.is_some_and(|until| now >= until))
        .map(|(declared, _)| declared.clone());
    if remembered.is_some() {
        return remembered;
    }
    let (declared, definite) = declares(runtime)?;
    let until = (!definite).then(|| now + SET_RECHECK);
    known
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, (declared.clone(), until));
    Some(declared)
}

/// Authenticate the selected package and read which categories its installed
/// model set examines, and whether that answer is final for this manifest;
/// `None` when the package or the set's descriptor cannot be authenticated
/// (not installed, replaced, wrong trust). The capability is offered only
/// with a model set: the warm worker is the one product path, and it serves
/// nothing without one. A package that declares the capability while its set
/// is not installed offers nothing for now.
fn declares(runtime: &crate::core::config::IntelligenceRuntimeConfig) -> Option<(Declared, bool)> {
    let trust = super::trust_key(&runtime.trust_key_hex).ok()?;
    let package = super::install::verified_active(
        std::path::Path::new(&runtime.root),
        &runtime.manifest_sha256,
        &trust,
    )
    .ok()?;
    if !super::health::supports_semantic_detectors(&package) {
        return Some((Declared::default(), true));
    }
    let Some(set) = super::install::semantic_set(&package) else {
        use super::install::SemanticState;
        return match super::install::semantic_state(&package) {
            // A companion directory that is not the signed set.
            SemanticState::Unverified => None,
            // The set can still be installed under this manifest.
            SemanticState::SetNotPresent => Some((Declared::default(), false)),
            // An earlier format that declares the capability has no set
            // and so nothing the worker could serve.
            SemanticState::Offered { .. } | SemanticState::NotOffered => {
                Some((Declared::default(), true))
            }
        };
    };
    declared_by(&set).map(|declared| (declared, true))
}

/// What the set's descriptor declares: the categories its model outputs map
/// to and the coverage each model states. The file is read from the
/// installed set and counts only with the digest the signed manifest states.
pub(super) fn declared_by(set: &super::install::SemanticSet) -> Option<Declared> {
    let bytes = super::read_regular(
        &set.directory.join(super::manifest::SEMANTIC_DESCRIPTOR),
        super::manifest::MAX_SEMANTIC_DESCRIPTOR,
    )
    .ok()?;
    if super::sha256(&bytes) != set.descriptor_sha256 {
        return None;
    }
    Declared::parse(&bytes).filter(|declared| declared.categories() != CategorySet::NONE)
}

/// Inspect one text with the runtime `config` names — the settings bound by
/// [`bound`], not the configuration that is current at this moment. `invoke`
/// still refuses when the live configuration has moved away from them.
///
/// The answer comes with the categories it speaks for: all three under
/// contract 1.0.0, the ones the runtime names under 1.1.0.
pub(crate) fn inspect_bound(
    config: &crate::core::config::IntelligenceRuntimeConfig,
    text: &str,
) -> std::result::Result<(SemanticReport, CategorySet), NotInspected> {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    if text.len() > MAX_SOURCE_BYTES || text.contains('\0') {
        return Err(NotInspected::Unavailable);
    }
    // One budget for the whole text, continuations included.
    let deadline = Instant::now() + Duration::from_millis(ANSWER_DEADLINE_MS);
    let deadline_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| u64::try_from(now.as_millis()).ok())
        .and_then(|now| now.checked_add(ANSWER_DEADLINE_MS))
        .ok_or(NotInspected::Unavailable)?;
    let ask = |piece: &str| -> std::result::Result<(SemanticReport, CategorySet), NotInspected> {
        #[cfg(test)]
        if let Some(stand_in) = test_seam::invoke_override() {
            let raw = stand_in(config, piece).map_err(|_| NotInspected::Unavailable)?;
            return validate_answer(&raw, piece, false).map_err(|_| NotInspected::Unavailable);
        }
        let (raw, names_categories) = invoke(config, piece, deadline, deadline_unix_ms)?;
        validate_answer(&raw, piece, names_categories).map_err(|_| NotInspected::Unavailable)
    };
    let (mut report, examined) = ask(text)?;
    // A runtime that stops at the findings bound reports where it stopped.
    // While the budget allows, the rest is asked for, with some text before
    // that point as context; only findings from that point on are taken.
    while report.bytes_inspected < text.len()
        && report.findings.len() >= MAX_FINDINGS
        && deadline.saturating_duration_since(Instant::now())
            >= Duration::from_millis(CONTINUE_MIN_MS)
    {
        let resume = report.bytes_inspected;
        let from = continuation_start(text, resume);
        let Ok((next, next_examined)) = ask(&text[from..]) else {
            break;
        };
        let reached = from + next.bytes_inspected;
        if next_examined != examined || reached <= resume {
            break;
        }
        report.findings.extend(
            next.findings
                .into_iter()
                .filter(|finding| from + finding.start >= resume)
                .map(|finding| SemanticFinding {
                    start: from + finding.start,
                    end: from + finding.end,
                    ..finding
                }),
        );
        report.bytes_inspected = reached;
    }
    Ok((report, examined))
}

/// Context, in bytes, handed again to a continuation.
const CONTINUATION_CONTEXT_BYTES: usize = 512;
/// A continuation is not started with less budget than this left.
const CONTINUE_MIN_MS: u64 = 300;

/// Where a continuation of `text` at `resume` begins: up to
/// [`CONTINUATION_CONTEXT_BYTES`] earlier, after a whitespace so that no word
/// is cut; `resume` itself when there is none.
pub(crate) fn continuation_start(text: &str, resume: usize) -> usize {
    let mut from = resume.saturating_sub(CONTINUATION_CONTEXT_BYTES);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    text[from..resume]
        .find(char::is_whitespace)
        .map_or(resume, |at| {
            let space = from + at;
            space + text[space..].chars().next().map_or(0, char::len_utf8)
        })
}

/// Test seam: stands in for the private runtime process, and shows which
/// runtime settings an inspection was made with.
#[cfg(test)]
pub(crate) mod test_seam {
    use super::Result;
    use crate::core::config::IntelligenceRuntimeConfig;
    use std::cell::Cell;

    pub(crate) type Invoke = fn(&IntelligenceRuntimeConfig, &str) -> Result<String>;

    thread_local! {
        static INVOKE: Cell<Option<Invoke>> = const { Cell::new(None) };
    }

    pub(super) fn invoke_override() -> Option<Invoke> {
        INVOKE.with(Cell::get)
    }

    /// Replace the runtime invocation on this thread until the guard drops.
    pub(crate) fn stand_in(invoke: Invoke) -> impl Drop {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                INVOKE.with(|slot| slot.set(None));
            }
        }
        INVOKE.with(|slot| slot.set(Some(invoke)));
        Reset
    }

    /// A valid runtime answer without findings for `text`.
    pub(crate) fn clean_answer(text: &str) -> String {
        serde_json::json!({
            "schema_version": 1,
            "strategy": "semantic_v1",
            "source_sha256": super::super::sha256(text.as_bytes()),
            "bytes_inspected": text.len(),
            "findings": [],
        })
        .to_string()
    }
}

/// An answer of contract 1.0.0.
#[cfg(test)]
pub(crate) fn validate(raw: &str, text: &str) -> Result<SemanticReport> {
    validate_answer(raw, text, false).map(|(report, _)| report)
}

/// `names_categories` is contract 1.1.0: the answer must name the examined
/// categories (at least one, each once, in contract order) and may carry
/// findings for those only. Under 1.0.0 the field must be absent.
fn validate_answer(
    raw: &str,
    text: &str,
    names_categories: bool,
) -> Result<(SemanticReport, CategorySet)> {
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err(InstallError::Exchange);
    }
    let wire: Wire = serde_json::from_str(raw).map_err(|_| InstallError::Exchange)?;
    let examined = match (&wire.categories, names_categories) {
        (None, false) => CategorySet::ALL,
        (Some(named), true)
            if !named.is_empty() && named.windows(2).all(|pair| pair[0] < pair[1]) =>
        {
            named
                .iter()
                .fold(CategorySet::NONE, |set, category| set.with(*category))
        }
        _ => return Err(InstallError::Exchange),
    };
    let bytes = wire.bytes_inspected;
    let valid = wire.schema_version == 1
        && wire.findings.iter().all(|f| examined.contains(f.category))
        && wire.strategy == "semantic_v1"
        && wire.source_sha256 == super::sha256(text.as_bytes())
        && bytes <= text.len()
        && text.is_char_boundary(bytes)
        && wire.findings.len() <= MAX_FINDINGS
        && wire.findings.iter().all(|f| {
            f.start < f.end
                && f.end <= bytes
                && text.is_char_boundary(f.start)
                && text.is_char_boundary(f.end)
                && f.confidence_milli <= 1000
        })
        && wire
            .findings
            .windows(2)
            .all(|pair| pair[0].end <= pair[1].start);
    if !valid {
        return Err(InstallError::Exchange);
    }
    Ok((
        SemanticReport {
            bytes_inspected: bytes,
            findings: wire.findings,
        },
        examined,
    ))
}

/// The raw answer, and whether it was asked for under contract 1.1.0.
///
/// The warm worker of this process is the only way a packaged model set is
/// asked; there is no one-process-per-text start beside it. The deadline
/// covers everything here, the wait for another caller's request included.
/// A worker that is not warm yet is started in the background and this text
/// is reported as not inspected: nothing waits for a model to load.
fn invoke(
    config: &crate::core::config::IntelligenceRuntimeConfig,
    text: &str,
    deadline: std::time::Instant,
    deadline_unix_ms: u64,
) -> std::result::Result<(String, bool), NotInspected> {
    // Settings that are no longer the live ones neither reach the worker of
    // the live ones nor replace it.
    match selection(config) {
        Some(true) => {}
        // The store is held at this moment. When this process's own worker
        // start holds it (the worker is warming), the true statement about
        // this text is "the detector was still starting", not "unavailable".
        None if super::semantic_worker::state(config).0 == "warming" => {
            return Err(NotInspected::Warming);
        }
        Some(false) | None => return Err(NotInspected::Unavailable),
    }
    let input = request_input(text, declared(config).as_ref()).to_string();
    let answer = super::semantic_worker::request(config, &input, deadline, deadline_unix_ms)?;
    // As before: an answer counts only while the selection it was asked
    // under is still the live one.
    if !selected(config) {
        return Err(NotInspected::Unavailable);
    }
    Ok(answer)
}

/// The detection request for `text`. A set that declares its coverage is
/// told that this host applies the declarations (the caller has already
/// checked that the text lies inside them); its worker refuses a request
/// without that, so an older host never takes its answers for full
/// coverage. A set without declarations gets the request it always got.
fn request_input(text: &str, declared: Option<&Declared>) -> serde_json::Value {
    let mut input = serde_json::json!({
        "schema_version": 1,
        "categories": ["secret", "pii", "prompt_injection"],
        "text": text,
        "max_findings": MAX_FINDINGS,
    });
    if declared.is_some_and(Declared::declares_coverage) {
        input["coverage_acknowledged"] = true.into();
    }
    input
}

/// Whether `config` is the live global runtime configuration and names the
/// active package. When the active package has moved under unchanged
/// settings, the worker of the earlier package is stopped.
fn selected(config: &crate::core::config::IntelligenceRuntimeConfig) -> bool {
    selection(config) == Some(true)
}

/// [`selected`], with the third answer kept apart: `None` when the store was
/// held and nothing was learned about the active package.
fn selection(config: &crate::core::config::IntelligenceRuntimeConfig) -> Option<bool> {
    if crate::core::config::Config::load_global().intelligence_runtime != *config {
        return Some(false);
    }
    match standing(std::path::Path::new(&config.root), &config.manifest_sha256) {
        Standing::Active => Some(true),
        Standing::Unknown => None,
        Standing::Moved => {
            super::semantic_worker::retire();
            Some(false)
        }
    }
}

/// What the store says about the package the settings name.
#[derive(Debug, PartialEq, Eq)]
enum Standing {
    /// Read, and it is the active package.
    Active,
    /// Read, and another package is active, or the store does not hold up.
    Moved,
    /// The store is held at this moment: by an installation, or by this
    /// process's own worker start, which authenticates the package under the
    /// same lock. Nothing was learned, so the answer asked under it does not
    /// count and the worker stays; stopping it here would end every start
    /// that an admission happens to overlap.
    Unknown,
}

fn standing(root: &std::path::Path, manifest_sha256: &str) -> Standing {
    match super::install::active_manifest(root) {
        Ok(active) if active == manifest_sha256 => Standing::Active,
        // Held by someone else. Whether the selection MOVED can still be read
        // from the atomically replaced selection file; that is all an answer
        // of a running worker needs. A reading that does not name this
        // manifest, or none, stays unknown: only a locked read retires.
        Err(InstallError::Busy) => match super::install::active_manifest_unlocked(root) {
            Ok(active) if active == manifest_sha256 => Standing::Active,
            _ => Standing::Unknown,
        },
        Ok(_) | Err(_) => Standing::Moved,
    }
}

/// The state of this process's worker for these settings (`cold`, `warming`,
/// `warm`, `failed`) and the contract versions a warm one named. Nothing is
/// started. A worker belongs to one process: a command-line status call
/// reports its own, which is cold, not the worker of a running server.
pub(crate) fn worker_state(
    config: &crate::core::config::IntelligenceRuntimeConfig,
) -> (&'static str, Vec<String>) {
    super::semantic_worker::state(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(text: &str, findings: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "strategy": "semantic_v1",
            "source_sha256": super::super::sha256(text.as_bytes()),
            "bytes_inspected": text.len(),
            "findings": findings,
        })
    }

    #[test]
    fn valid_spans_are_accepted() {
        let text = "keep this; drop that\n";
        let raw = response(
            text,
            &serde_json::json!([{"category":"prompt_injection","start":11,"end":20,"confidence_milli":900}]),
        );
        let report = validate(&raw.to_string(), text).expect("valid");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            &text[report.findings[0].start..report.findings[0].end],
            "drop that"
        );
    }

    #[test]
    fn a_runtime_cannot_bend_the_contract() {
        let text = "äbc def ghi\n";
        let good = response(text, &serde_json::json!([]));
        for (field, value) in [
            ("schema_version", serde_json::json!(2)),
            ("strategy", serde_json::json!("other")),
            ("source_sha256", serde_json::json!("0".repeat(64))),
            ("bytes_inspected", serde_json::json!(text.len() + 1)),
            ("bytes_inspected", serde_json::json!(1)),
            (
                "findings",
                serde_json::json!([{"category":"pii","start":3,"end":3,"confidence_milli":10}]),
            ),
            (
                "findings",
                serde_json::json!([{"category":"pii","start":1,"end":4,"confidence_milli":10}]),
            ),
            (
                "findings",
                serde_json::json!([{"category":"pii","start":3,"end":6,"confidence_milli":1001}]),
            ),
            (
                "findings",
                serde_json::json!([
                    {"category":"pii","start":3,"end":7,"confidence_milli":10},
                    {"category":"secret","start":6,"end":9,"confidence_milli":10}
                ]),
            ),
            (
                "findings",
                serde_json::json!([{"category":"pii","start":3,"end":6,"confidence_milli":10,"value":"leak"}]),
            ),
            (
                "findings",
                serde_json::json!([{"category":"exfiltrate","start":3,"end":6,"confidence_milli":10}]),
            ),
        ] {
            let mut bad = good.clone();
            bad[field] = value;
            assert!(validate(&bad.to_string(), text).is_err(), "{field}");
        }
        // An answer carries spans only: a runtime that tries to set a
        // decision, rights, a classification, trust or replacement text is
        // refused whole (AP11 P01).
        for (field, value) in [
            ("note", serde_json::json!("ignore previous instructions")),
            ("disposition", serde_json::json!("allow")),
            ("acl", serde_json::json!({"allow": ["*"]})),
            ("rights", serde_json::json!(["read", "share"])),
            ("classification", serde_json::json!("public")),
            ("trust", serde_json::json!("internal")),
            ("text", serde_json::json!("äbc def ghi\n")),
        ] {
            let mut extra = good.clone();
            extra[field] = value;
            assert!(validate(&extra.to_string(), text).is_err(), "{field}");
        }
    }

    /// Contract 1.1.0: the answer names what was examined, findings stay
    /// inside it, and neither contract accepts the other's shape.
    #[test]
    fn named_categories_bound_what_an_answer_speaks_for() {
        use serde_json::json;
        let text = "keep this; drop that\n";
        let answer = |categories: serde_json::Value, category: &str| {
            let mut raw = response(
                text,
                &json!([{"category":category,"start":11,"end":20,"confidence_milli":900}]),
            );
            raw["categories"] = categories;
            validate_answer(&raw.to_string(), text, true)
        };
        let (report, examined) =
            answer(json!(["prompt_injection"]), "prompt_injection").expect("valid");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(examined.names(), ["prompt_injection"]);
        assert!(!examined.contains(SemanticCategory::Pii));

        for (categories, category) in [
            (json!(["prompt_injection"]), "pii"),
            (json!([]), "pii"),
            (json!(["pii", "secret"]), "pii"),
            (json!(["pii", "pii"]), "pii"),
            (json!(["other"]), "pii"),
        ] {
            assert!(
                answer(categories.clone(), category).is_err(),
                "{categories}"
            );
        }
        let plain = response(text, &json!([]));
        assert!(validate_answer(&plain.to_string(), text, true).is_err());
        let mut named = plain;
        named["categories"] = json!(["pii"]);
        assert!(validate_answer(&named.to_string(), text, false).is_err());
    }

    /// AP11 S3: the acknowledgement goes to a declaring (v2) set only; a v1
    /// set, which an older runtime may serve, gets the unchanged request.
    #[test]
    fn only_a_declaring_set_is_told_that_its_coverage_is_applied() {
        use super::super::semantic_coverage::DeclaredModel;
        let set = |languages: Option<Vec<String>>| Declared {
            models: vec![DeclaredModel {
                categories: CategorySet::ALL,
                languages,
                document_types: None,
            }],
        };
        let acknowledged = |declared: Option<&Declared>| {
            request_input("text\n", declared)
                .get("coverage_acknowledged")
                .cloned()
        };
        assert_eq!(
            acknowledged(Some(&set(Some(vec!["de".to_owned()])))),
            Some(serde_json::json!(true))
        );
        assert_eq!(acknowledged(Some(&set(None))), None);
        assert_eq!(acknowledged(None), None);
    }

    #[test]
    fn oversized_input_is_never_sent() {
        let runtime = crate::core::config::IntelligenceRuntimeConfig::default();
        assert!(inspect_bound(&runtime, &"x".repeat(MAX_SOURCE_BYTES + 1)).is_err());
        assert!(inspect_bound(&runtime, "a\0b").is_err());
    }

    /// An enabled, permitted runtime whose package does not verifiably
    /// declare the capability is not attached, so no admission asks it and
    /// none reports an unavailable semantic check. The same settings attach
    /// once a runtime answers (the stand-in), which shows the permission
    /// check is not what kept the stage away.
    #[test]
    fn a_runtime_that_does_not_offer_the_capability_is_not_attached() {
        let directory = tempfile::tempdir().expect("runtime root");
        let root = std::fs::canonicalize(directory.path()).expect("canonical runtime root");
        let config = crate::core::config::Config {
            intelligence_runtime: crate::core::config::IntelligenceRuntimeConfig {
                enabled: true,
                accept_proprietary: true,
                staging: true,
                root: root.to_str().expect("utf-8 runtime root").to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(bound(&config).is_none());
        assert_eq!(offer(&config.intelligence_runtime), Offer::Unverified);

        let _runtime = test_seam::stand_in(|_, text| Ok(test_seam::clean_answer(text)));
        assert!(bound(&config).is_some());
    }

    /// A runtime that stops at the findings bound is asked again for the
    /// rest: a text with more personal data than one answer may carry is
    /// still inspected whole, every mention once, ascending and disjoint.
    #[test]
    fn the_rest_after_the_findings_bound_is_inspected_too() {
        fn capped(
            _: &crate::core::config::IntelligenceRuntimeConfig,
            text: &str,
        ) -> Result<String> {
            let mentions: Vec<usize> = text.match_indices("Anna").map(|(at, _)| at).collect();
            let inspected = mentions.get(MAX_FINDINGS).copied().unwrap_or(text.len());
            let findings: Vec<_> = mentions
                .iter()
                .take(MAX_FINDINGS)
                .map(|at| serde_json::json!({"category": "pii", "start": at, "end": at + 4, "confidence_milli": 900}))
                .collect();
            let mut answer = response(text, &serde_json::json!(findings));
            answer["bytes_inspected"] = inspected.into();
            Ok(answer.to_string())
        }
        let text = "Anna Meier unterschrieb.\n".repeat(80);
        let _runtime = test_seam::stand_in(capped);
        let (report, _) = inspect_bound(&Default::default(), &text).expect("inspected");
        assert_eq!(report.bytes_inspected, text.len());
        assert_eq!(report.findings.len(), 80);
        assert!(
            report
                .findings
                .windows(2)
                .all(|pair| pair[0].end <= pair[1].start)
        );
        assert!(
            report
                .findings
                .iter()
                .all(|f| &text[f.start..f.end] == "Anna")
        );
    }

    /// A worker start authenticates its package under the store lock. An
    /// admission that overlaps it learns nothing about the selection and must
    /// not take that for a moved package, which stops the starting worker.
    #[cfg(unix)]
    #[test]
    fn a_held_store_is_not_taken_for_a_moved_package() {
        // The store asks where the production root is, which is under the
        // data directory: without an isolated one the reading is an error.
        let _data = crate::core::data_dir::isolated_data_dir();
        // The store opens only a directory private to its owner.
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("runtime root");
        let root = std::fs::canonicalize(directory.path()).expect("canonical runtime root");
        assert_eq!(
            super::super::install::active_manifest(&root).expect("a fresh store is read"),
            "none"
        );
        assert_eq!(standing(&root, "none"), Standing::Active);
        assert_eq!(standing(&root, "another"), Standing::Moved);

        let held = std::fs::File::open(root.join("install.lock")).expect("store lock");
        // The readings above just released this lock; a child another test
        // forked in between shares that released handle until it execs, so
        // the lock can read as held for a moment. Taken the way the store
        // takes it — with its bounded wait — not with a single try.
        super::super::install::lock_exclusive_bounded(&held).expect("hold the store");
        // Held: the selection file still says whether the selection moved.
        // The named manifest is active; another one is not known to have
        // replaced it, and nothing is retired on an unlocked reading.
        assert_eq!(standing(&root, "none"), Standing::Active);
        assert_eq!(standing(&root, "another"), Standing::Unknown);
        drop(held);
        assert_eq!(standing(&root, "none"), Standing::Active);
    }

    /// The supervisor of the warm worker, driven through an in-memory worker
    /// that speaks the line protocol: the test reads what the supervisor
    /// writes and decides what, and when, the worker answers.
    mod worker {
        use super::super::NotInspected;
        use crate::core::intelligence_runtime::semantic_worker::{
            Event, Launcher, Link, Phase, Started, Supervisor, Timing,
        };
        use std::collections::VecDeque;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        const KEY: [u8; 32] = [1; 32];
        const OTHER: [u8; 32] = [2; 32];
        const LONG: Duration = Duration::from_secs(10);
        const HOUR: Duration = Duration::from_hours(1);

        /// The test's end of one started worker.
        struct Peer {
            events: SyncSender<Event>,
            requests: Receiver<Vec<u8>>,
            stopped: Arc<AtomicBool>,
        }

        struct Wire {
            requests: Sender<Vec<u8>>,
            stopped: Arc<AtomicBool>,
        }

        impl Link for Wire {
            fn send(&mut self, line: Vec<u8>) -> bool {
                self.requests.send(line).is_ok()
            }

            fn stop(&mut self) {
                self.stopped.store(true, Ordering::SeqCst);
            }
        }

        fn child(version: &'static str) -> (Option<Started>, Peer) {
            let (events, incoming) = sync_channel(4);
            let (requests, sent) = channel();
            let stopped = Arc::new(AtomicBool::new(false));
            let link = Wire {
                requests,
                stopped: Arc::clone(&stopped),
            };
            let started = Started {
                link: Box::new(link),
                events: incoming,
                version,
                takes_lease: false,
                lease: None,
            };
            let peer = Peer {
                events,
                requests: sent,
                stopped,
            };
            (Some(started), peer)
        }

        /// Hands out the prepared starts in order; `None` is a failed start.
        struct Children {
            next: Mutex<VecDeque<Option<Started>>>,
            launched: AtomicUsize,
            /// When set, the first start waits here, as a model load would.
            gate: Mutex<Option<Receiver<()>>>,
        }

        impl Launcher for Arc<Children> {
            fn launch(
                &self,
                _: &crate::core::config::IntelligenceRuntimeConfig,
            ) -> Option<Started> {
                self.launched.fetch_add(1, Ordering::SeqCst);
                let gate = self.gate.lock().expect("gate").take();
                if let Some(gate) = gate {
                    let _ = gate.recv_timeout(LONG);
                }
                self.next.lock().expect("starts").pop_front().flatten()
            }
        }

        fn supervised(
            starts: Vec<Option<Started>>,
            gate: Option<Receiver<()>>,
            timing: Timing,
        ) -> (Arc<Supervisor>, Arc<Children>) {
            let children = Arc::new(Children {
                next: Mutex::new(starts.into()),
                launched: AtomicUsize::new(0),
                gate: Mutex::new(gate),
            });
            let supervisor = Supervisor::with(Box::new(Arc::clone(&children)), timing);
            (supervisor, children)
        }

        fn timing(idle: Duration, drain: Duration, backoff: Duration) -> Timing {
            Timing {
                idle,
                drain,
                backoff: [backoff; 3],
            }
        }

        fn ask(
            supervisor: &Arc<Supervisor>,
            key: [u8; 32],
            wait: Duration,
        ) -> Result<(String, bool), NotInspected> {
            let config = crate::core::config::IntelligenceRuntimeConfig::default();
            supervisor.request(&config, key, "{}", Instant::now() + wait, 0)
        }

        fn asking(
            supervisor: &Arc<Supervisor>,
            wait: Duration,
        ) -> std::thread::JoinHandle<Result<(String, bool), NotInspected>> {
            let supervisor = Arc::clone(supervisor);
            std::thread::spawn(move || ask(&supervisor, KEY, wait))
        }

        fn line(value: &serde_json::Value) -> Event {
            Event::Line(format!("{value}\n").into_bytes())
        }

        fn ready(versions: &[&str]) -> Event {
            line(&serde_json::json!({
                "schema_version": 1, "status": "ready", "versions": versions
            }))
        }

        fn output(answer: &str) -> Event {
            line(&serde_json::json!({"status": "ok", "output": answer}))
        }

        fn eventually(what: &str, reached: impl Fn() -> bool) {
            let end = Instant::now() + LONG;
            while !reached() {
                assert!(Instant::now() < end, "never: {what}");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        fn in_phase(supervisor: &Arc<Supervisor>, key: [u8; 32], phase: Phase) {
            eventually(phase.as_str(), || supervisor.phase(key).0 == phase);
        }

        /// First use starts the worker and is told it is warming.
        fn warm(supervisor: &Arc<Supervisor>, key: [u8; 32], peer: &Peer) {
            assert_eq!(ask(supervisor, key, LONG), Err(NotInspected::Warming));
            peer.events.send(ready(&["1.1.0"])).expect("worker output");
            in_phase(supervisor, key, Phase::Warm);
        }

        /// The first inspection starts the worker and returns while the
        /// start is still held up; once the worker is ready the next one is
        /// answered by it, under the negotiated contract.
        #[test]
        fn a_cold_start_is_reported_as_warming_and_never_waited_for() {
            let (open, gate) = channel();
            let (started, peer) = child("1.1.0");
            let (supervisor, children) =
                supervised(vec![started], Some(gate), timing(HOUR, LONG, HOUR));
            assert_eq!(supervisor.phase(KEY).0, Phase::Cold);

            let began = Instant::now();
            assert_eq!(ask(&supervisor, KEY, LONG), Err(NotInspected::Warming));
            assert_eq!(ask(&supervisor, KEY, LONG), Err(NotInspected::Warming));
            assert!(
                began.elapsed() < LONG / 2,
                "an inspection waited for the worker to start"
            );
            assert_eq!(supervisor.phase(KEY).0, Phase::Warming);

            open.send(()).expect("start released");
            peer.events.send(ready(&["1.1.0"])).expect("worker output");
            in_phase(&supervisor, KEY, Phase::Warm);
            assert_eq!(supervisor.phase(KEY).1, ["1.1.0"]);

            let inspection = asking(&supervisor, LONG);
            let request = peer.requests.recv_timeout(LONG).expect("request line");
            assert_eq!(request.last(), Some(&b'\n'));
            let request: serde_json::Value =
                serde_json::from_slice(&request).expect("request object");
            assert_eq!(request["version"], "1.1.0");
            assert_eq!(request["input"], "{}");
            peer.events.send(output("answer")).expect("worker output");
            assert_eq!(
                inspection.join().expect("inspection"),
                Ok(("answer".to_owned(), true))
            );
            assert_eq!(children.launched.load(Ordering::SeqCst), 1);
        }

        /// B04: a worker whose licence expired refuses with `entitlement`.
        /// That is never an answer: the text is not inspected (the gateway
        /// keeps its built-in floor), the worker is kept, and once the
        /// licence is renewed it answers again.
        #[test]
        fn an_entitlement_refusal_is_not_inspected_never_a_clean_answer() {
            let (started, peer) = child("1.1.0");
            let (supervisor, children) = supervised(vec![started], None, timing(HOUR, LONG, HOUR));
            warm(&supervisor, KEY, &peer);

            let expired = asking(&supervisor, LONG);
            peer.requests.recv_timeout(LONG).expect("request line");
            let refusal = serde_json::json!({"status": "refused", "reason": "entitlement"});
            peer.events.send(line(&refusal)).expect("worker output");
            assert_eq!(
                expired.join().expect("inspection"),
                Err(NotInspected::Unavailable)
            );
            assert_eq!(supervisor.phase(KEY).0, Phase::Warm, "the worker is kept");

            let renewed = asking(&supervisor, LONG);
            peer.requests.recv_timeout(LONG).expect("request line");
            peer.events.send(output("answer")).expect("worker output");
            assert_eq!(
                renewed.join().expect("inspection"),
                Ok(("answer".to_owned(), true))
            );
            assert_eq!(children.launched.load(Ordering::SeqCst), 1);
        }

        /// The protocol has no request id. After a request gave up, nothing
        /// is written until its late answer has been read and dropped, so
        /// the next request is answered by its own line.
        #[test]
        fn a_late_answer_is_never_taken_for_the_next_request() {
            let (started, peer) = child("1.1.0");
            let (supervisor, _) = supervised(vec![started], None, timing(HOUR, HOUR, HOUR));
            warm(&supervisor, KEY, &peer);

            assert_eq!(
                ask(&supervisor, KEY, Duration::from_millis(50)),
                Err(NotInspected::Unavailable)
            );
            peer.requests.recv_timeout(LONG).expect("first request");
            assert_eq!(supervisor.phase(KEY).0, Phase::Warm, "the worker is kept");

            let next = asking(&supervisor, LONG);
            assert!(
                peer.requests
                    .recv_timeout(Duration::from_millis(200))
                    .is_err(),
                "a request was written while an answer was outstanding"
            );
            peer.events.send(output("late")).expect("worker output");
            peer.requests.recv_timeout(LONG).expect("second request");
            peer.events.send(output("own")).expect("worker output");
            assert_eq!(
                next.join().expect("inspection"),
                Ok(("own".to_owned(), true))
            );
        }

        /// A late answer that does not come within the drain bound ends the
        /// worker; it is not asked again while an answer could still arrive.
        #[test]
        fn an_answer_that_never_comes_stops_the_worker() {
            let (started, peer) = child("1.1.0");
            let drain = Duration::from_millis(50);
            let (supervisor, children) = supervised(vec![started], None, timing(HOUR, drain, HOUR));
            warm(&supervisor, KEY, &peer);

            assert_eq!(
                ask(&supervisor, KEY, Duration::from_millis(50)),
                Err(NotInspected::Unavailable)
            );
            assert_eq!(ask(&supervisor, KEY, LONG), Err(NotInspected::Unavailable));
            assert_eq!(supervisor.phase(KEY).0, Phase::Failed);
            assert!(peer.stopped.load(Ordering::SeqCst));
            assert_eq!(
                peer.requests.try_iter().count(),
                1,
                "only the first request was written"
            );
            assert_eq!(children.launched.load(Ordering::SeqCst), 1, "backoff holds");
        }

        /// One caller holds the worker. A second one waits no longer than
        /// its own deadline, writes nothing and is not inspected.
        #[test]
        fn a_caller_never_waits_past_its_deadline_for_a_busy_worker() {
            let (started, peer) = child("1.1.0");
            let (supervisor, _) = supervised(vec![started], None, timing(HOUR, HOUR, HOUR));
            warm(&supervisor, KEY, &peer);

            let holder = asking(&supervisor, LONG);
            peer.requests.recv_timeout(LONG).expect("first request");
            let (done, finished) = channel();
            let second = Arc::clone(&supervisor);
            std::thread::spawn(move || {
                let _ = done.send(ask(&second, KEY, Duration::from_millis(100)));
            });
            assert_eq!(
                finished
                    .recv_timeout(LONG / 2)
                    .expect("a waiting caller outlived its deadline"),
                Err(NotInspected::Unavailable)
            );
            assert!(
                peer.requests.try_recv().is_err(),
                "nothing else was written"
            );
            peer.events.send(output("held")).expect("worker output");
            assert_eq!(
                holder.join().expect("inspection"),
                Ok(("held".to_owned(), true))
            );
        }

        /// Three restarts after the first failed start, then unavailable for
        /// good; and no restart before its wait has passed.
        #[test]
        fn restarts_are_bounded_and_wait_their_turn() {
            let zero = Duration::ZERO;
            let (supervisor, children) = supervised(Vec::new(), None, timing(HOUR, LONG, zero));
            for _ in 0..4 {
                assert_eq!(ask(&supervisor, KEY, LONG), Err(NotInspected::Warming));
                in_phase(&supervisor, KEY, Phase::Failed);
            }
            for _ in 0..3 {
                assert_eq!(ask(&supervisor, KEY, LONG), Err(NotInspected::Unavailable));
            }
            assert_eq!(supervisor.phase(KEY).0, Phase::Failed);
            assert_eq!(children.launched.load(Ordering::SeqCst), 4);

            let (waiting, children) = supervised(Vec::new(), None, timing(HOUR, LONG, HOUR));
            assert_eq!(ask(&waiting, KEY, LONG), Err(NotInspected::Warming));
            in_phase(&waiting, KEY, Phase::Failed);
            assert_eq!(ask(&waiting, KEY, LONG), Err(NotInspected::Unavailable));
            assert_eq!(children.launched.load(Ordering::SeqCst), 1);
        }

        /// A ready line that does not name the negotiated contract is not a
        /// worker for this package.
        #[test]
        fn a_worker_without_the_negotiated_contract_fails() {
            let (started, peer) = child("1.0.0");
            let (supervisor, _) = supervised(vec![started], None, timing(HOUR, LONG, HOUR));
            assert_eq!(ask(&supervisor, KEY, LONG), Err(NotInspected::Warming));
            peer.events.send(ready(&["1.1.0"])).expect("worker output");
            in_phase(&supervisor, KEY, Phase::Failed);
            assert!(peer.stopped.load(Ordering::SeqCst));
            assert!(supervisor.phase(KEY).1.is_empty());
        }

        /// Other runtime settings (another package, another root) never use
        /// the running worker: it is stopped and a new one is started. So
        /// does retiring it.
        #[test]
        fn changed_settings_stop_the_worker_and_start_another() {
            let (first, earlier) = child("1.1.0");
            let (second, later) = child("1.1.0");
            let (supervisor, children) =
                supervised(vec![first, second], None, timing(HOUR, LONG, HOUR));
            warm(&supervisor, KEY, &earlier);

            warm(&supervisor, OTHER, &later);
            assert!(earlier.stopped.load(Ordering::SeqCst));
            assert_eq!(children.launched.load(Ordering::SeqCst), 2);
            assert_eq!(supervisor.phase(KEY).0, Phase::Cold);
            assert!(earlier.requests.try_recv().is_err());

            supervisor.retire();
            assert!(later.stopped.load(Ordering::SeqCst));
            assert_eq!(supervisor.phase(OTHER).0, Phase::Cold);
        }

        /// A worker that ends after its idle time is cold again and is
        /// simply started on the next use; one that ends earlier has failed.
        #[test]
        fn an_idle_exit_is_cold_and_an_early_exit_is_a_failure() {
            let (first, idle) = child("1.1.0");
            let (second, _next) = child("1.1.0");
            let (supervisor, children) = supervised(
                vec![first, second],
                None,
                timing(Duration::ZERO, LONG, HOUR),
            );
            warm(&supervisor, KEY, &idle);
            idle.events.send(Event::Closed).expect("worker output");
            in_phase(&supervisor, KEY, Phase::Cold);
            assert_eq!(ask(&supervisor, KEY, LONG), Err(NotInspected::Warming));
            eventually("second start", || {
                children.launched.load(Ordering::SeqCst) == 2
            });

            let (only, early) = child("1.1.0");
            let (supervisor, _) = supervised(vec![only], None, timing(HOUR, LONG, HOUR));
            warm(&supervisor, KEY, &early);
            early.events.send(Event::Closed).expect("worker output");
            in_phase(&supervisor, KEY, Phase::Failed);
            assert_eq!(ask(&supervisor, KEY, LONG), Err(NotInspected::Unavailable));
        }
    }
}
