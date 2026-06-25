//! Deterministic reflection over the gotcha trace (ACE Reflector analog).
//!
//! ACE (Agentic Context Engineering) turns raw execution traces into curated,
//! reusable skillbook entries. lean-ctx now records a structured trace of real
//! shell outcomes — the gotcha `error_log` plus correlated `gotchas`, populated
//! by [`record_shell_outcome`](super::record_shell_outcome). This module
//! distills that trace into [`ReflectionInsight`]s with deterministic,
//! rule-based passes (no LLM), which the session [`Playbook`] folds in as
//! Strategy / Pitfall deltas at checkpoint time.
//!
//! [`Playbook`]: crate::core::session::Playbook

use std::collections::{BTreeMap, BTreeSet};

use super::GotchaStore;
use crate::core::session::EntryKind;

/// Distinct sessions an unresolved error must span to count as a recurring
/// pitfall worth surfacing — one-off failures are noise.
const PITFALL_MIN_SESSIONS: usize = 2;
/// Occurrences a correlated fix needs before it is a proven, reusable strategy.
const STRATEGY_MIN_OCCURRENCES: u32 = 2;
/// Bound on emitted insights; the playbook caps anyway, so keep reflection cheap.
const MAX_INSIGHTS: usize = 12;

/// A distilled, reusable insight derived from the gotcha trace.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectionInsight {
    pub kind: EntryKind,
    pub content: String,
    pub confidence: f32,
}

/// Distill the gotcha trace into reusable insights. Pure and deterministic:
/// an identical store always yields byte-identical, identically-ordered output
/// (no timestamps, no map iteration order — `BTree*` keep it stable).
#[must_use]
pub fn reflect(store: &GotchaStore) -> Vec<ReflectionInsight> {
    let mut insights: Vec<ReflectionInsight> = Vec::new();

    // Pass 1 — proven strategies: a gotcha whose fix recurred is reusable.
    for g in &store.gotchas {
        if g.occurrences >= STRATEGY_MIN_OCCURRENCES && !g.resolution.trim().is_empty() {
            let trigger = crate::core::sanitize::neutralize_metadata(&g.trigger);
            let resolution = crate::core::sanitize::neutralize_metadata(&g.resolution);
            insights.push(ReflectionInsight {
                kind: EntryKind::Strategy,
                content: format!(
                    "When `{}`: {}",
                    short(&trigger, 80),
                    short(&resolution, 100)
                ),
                confidence: g.confidence,
            });
        }
    }

    // Pass 2 — recurring unresolved pitfalls: error signatures seen across
    // multiple sessions with no recorded fix anywhere in the log.
    let fixed: BTreeSet<&str> = store
        .error_log
        .iter()
        .flat_map(|l| l.fixes.iter().map(|f| f.error_signature.as_str()))
        .collect();

    let mut sig_sessions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for log in &store.error_log {
        for e in &log.errors {
            sig_sessions
                .entry(e.signature.as_str())
                .or_default()
                .insert(log.session_id.as_str());
        }
    }
    for (sig, sessions) in &sig_sessions {
        if sessions.len() >= PITFALL_MIN_SESSIONS && !fixed.contains(*sig) {
            let clean = crate::core::sanitize::neutralize_metadata(sig);
            insights.push(ReflectionInsight {
                kind: EntryKind::Pitfall,
                content: format!(
                    "Recurring unresolved error across {} sessions: {}",
                    sessions.len(),
                    short(&clean, 120)
                ),
                // Reach raises salience but stays below a proven fix's confidence.
                confidence: (0.5 + 0.1 * sessions.len() as f32).min(0.85),
            });
        }
    }

    // Deterministic order: strongest first, ties broken by content.
    insights.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.content.cmp(&b.content))
    });
    insights.truncate(MAX_INSIGHTS);
    insights
}

/// Fold reflection insights into a session playbook as deltas. Near-duplicates
/// confirm the existing entry (the playbook's grow-and-refine invariant), so
/// repeated checkpoints reinforce rather than bloat. Returns `(added, confirmed)`.
pub fn fold_into_playbook(
    insights: &[ReflectionInsight],
    playbook: &mut crate::core::session::Playbook,
    turn: u32,
) -> (usize, usize) {
    use crate::core::session::DeltaOutcome;

    let (mut added, mut confirmed) = (0usize, 0usize);
    for insight in insights {
        match playbook.add_delta(insight.kind, &insight.content, turn) {
            DeltaOutcome::Added(_) => added += 1,
            DeltaOutcome::Confirmed(_) => confirmed += 1,
        }
    }
    (added, confirmed)
}

/// Human-readable **Learning Ledger** — what lean-ctx has actually learned from
/// real shell outcomes: proven error→fix strategies, recurring pitfalls, and the
/// counts behind them. Honest counts only — no fabricated token-savings figures,
/// since "repeat errors avoided" has no measured per-incident token cost.
#[must_use]
pub fn format_ledger(store: &GotchaStore) -> String {
    let insights = reflect(store);
    let s = &store.stats;

    let mut out = String::from("LEARNING LEDGER — what lean-ctx learned from real runs\n");
    out.push_str(&format!(
        "  Errors observed:        {}\n",
        s.total_errors_detected
    ));
    out.push_str(&format!(
        "  Fixes correlated:       {}\n",
        s.total_fixes_correlated
    ));
    out.push_str(&format!(
        "  Repeat errors avoided:  {}\n",
        s.total_prevented
    ));
    out.push_str(&format!(
        "  Promoted to knowledge:  {}\n",
        s.gotchas_promoted
    ));
    out.push_str(&format!(
        "  Active gotchas:         {}\n",
        store.gotchas.len()
    ));

    let strategies: Vec<&ReflectionInsight> = insights
        .iter()
        .filter(|i| i.kind == EntryKind::Strategy)
        .collect();
    let pitfalls: Vec<&ReflectionInsight> = insights
        .iter()
        .filter(|i| i.kind == EntryKind::Pitfall)
        .collect();

    if insights.is_empty() {
        out.push_str(
            "\nNo distilled insights yet — run builds/tests through lean-ctx and they accrue here.\n",
        );
        return out;
    }

    if !strategies.is_empty() {
        out.push_str(&format!("\nProven strategies ({}):\n", strategies.len()));
        for i in &strategies {
            out.push_str(&format!(
                "  • {} ({:.0}%)\n",
                i.content,
                i.confidence * 100.0
            ));
        }
    }
    if !pitfalls.is_empty() {
        out.push_str(&format!("\nRecurring pitfalls ({}):\n", pitfalls.len()));
        for i in &pitfalls {
            out.push_str(&format!(
                "  ! {} ({:.0}%)\n",
                i.content,
                i.confidence * 100.0
            ));
        }
    }
    out
}

/// Char-boundary-safe truncation with an ellipsis (keeps multibyte signatures
/// from panicking on a byte slice).
fn short(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gotcha_tracker::{
        ErrorEntry, FixEntry, Gotcha, GotchaCategory, GotchaSeverity, GotchaSource, SessionErrorLog,
    };
    use crate::core::session::Playbook;
    use chrono::Utc;

    fn gotcha(trigger: &str, resolution: &str, occurrences: u32) -> Gotcha {
        let mut g = Gotcha::new(
            GotchaCategory::Build,
            GotchaSeverity::Critical,
            trigger,
            resolution,
            GotchaSource::AutoDetected {
                command: "cargo build".into(),
                exit_code: 1,
            },
            "s1",
        );
        g.occurrences = occurrences;
        g
    }

    fn error_log(session: &str, sig: &str) -> SessionErrorLog {
        SessionErrorLog {
            session_id: session.to_string(),
            timestamp: Utc::now(),
            errors: vec![ErrorEntry {
                signature: sig.to_string(),
                command: "cargo test".into(),
                timestamp: Utc::now(),
            }],
            fixes: Vec::new(),
        }
    }

    #[test]
    fn reflects_proven_fix_as_strategy() {
        let mut store = GotchaStore::new("h");
        store
            .gotchas
            .push(gotcha("error E0507", "use clone() on the field", 3));
        let insights = reflect(&store);
        assert!(
            insights
                .iter()
                .any(|i| i.kind == EntryKind::Strategy && i.content.contains("use clone()")),
            "a recurring fix must surface as a Strategy: {insights:?}"
        );
    }

    #[test]
    fn skips_one_off_fix() {
        let mut store = GotchaStore::new("h");
        store.gotchas.push(gotcha("error E0507", "use clone()", 1));
        assert!(
            reflect(&store).is_empty(),
            "a single occurrence is not yet a proven strategy"
        );
    }

    #[test]
    fn reflects_recurring_unresolved_error_as_pitfall() {
        let mut store = GotchaStore::new("h");
        store.error_log.push(error_log("s1", "flaky link error"));
        store.error_log.push(error_log("s2", "flaky link error"));
        let insights = reflect(&store);
        assert!(
            insights
                .iter()
                .any(|i| i.kind == EntryKind::Pitfall && i.content.contains("flaky link error")),
            "an unresolved error across sessions must surface as a Pitfall: {insights:?}"
        );
    }

    #[test]
    fn resolved_error_is_not_a_pitfall() {
        let mut store = GotchaStore::new("h");
        store.error_log.push(error_log("s1", "fixed error"));
        let mut s2 = error_log("s2", "fixed error");
        s2.fixes.push(FixEntry {
            error_signature: "fixed error".into(),
            resolution: "added the missing import".into(),
            files_changed: vec!["src/lib.rs".into()],
            timestamp: Utc::now(),
        });
        store.error_log.push(s2);
        assert!(
            !reflect(&store).iter().any(|i| i.kind == EntryKind::Pitfall),
            "an error with a recorded fix must not be flagged unresolved"
        );
    }

    #[test]
    fn single_session_error_is_not_a_pitfall() {
        let mut store = GotchaStore::new("h");
        store.error_log.push(error_log("s1", "one-off error"));
        assert!(!reflect(&store).iter().any(|i| i.kind == EntryKind::Pitfall));
    }

    #[test]
    fn output_is_deterministic() {
        let mut store = GotchaStore::new("h");
        store.gotchas.push(gotcha("err A", "fix A", 4));
        store.gotchas.push(gotcha("err B", "fix B", 2));
        store.error_log.push(error_log("s1", "recurring C"));
        store.error_log.push(error_log("s2", "recurring C"));
        assert_eq!(reflect(&store), reflect(&store));
    }

    #[test]
    fn ledger_reports_counts_and_insights() {
        let mut store = GotchaStore::new("h");
        store.stats.total_errors_detected = 5;
        store.stats.total_fixes_correlated = 2;
        store.stats.total_prevented = 1;
        store
            .gotchas
            .push(gotcha("error E0507", "use clone() on the field", 3));

        let ledger = format_ledger(&store);
        assert!(ledger.contains("LEARNING LEDGER"));
        assert!(ledger.contains("Errors observed:        5"));
        assert!(ledger.contains("Repeat errors avoided:  1"));
        assert!(ledger.contains("Proven strategies (1)"));
        assert!(ledger.contains("use clone()"));
    }

    #[test]
    fn ledger_handles_empty_store() {
        let store = GotchaStore::new("h");
        let ledger = format_ledger(&store);
        assert!(ledger.contains("LEARNING LEDGER"));
        assert!(ledger.contains("No distilled insights yet"));
    }

    #[test]
    fn fold_into_playbook_confirms_on_repeat() {
        let insights = vec![ReflectionInsight {
            kind: EntryKind::Strategy,
            content: "When `cargo build` fails with E0507: clone the field".into(),
            confidence: 0.9,
        }];
        let mut pb = Playbook::default();
        assert_eq!(fold_into_playbook(&insights, &mut pb, 1), (1, 0));
        assert_eq!(
            fold_into_playbook(&insights, &mut pb, 2),
            (0, 1),
            "a second fold confirms, never duplicates"
        );
    }
}
