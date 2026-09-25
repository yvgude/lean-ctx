//! `lean-ctx value`: every number the value surface shows, re-derived from the
//! two tamper-evident chains instead of the display counters.
//!
//! * Tokens come from the savings ledger (SHA-256 chain, `session_id`
//!   committed since canonical v7).
//! * Security events come from the audit trail (SHA-256 chain, Ed25519
//!   signed; kind, count and session committed in the hashed `action`).
//!
//! Both chains are re-walked from genesis first. A broken chain makes the
//! report *tampered*: the numbers are still printed, but flagged as not proof,
//! and the CLI exits non-zero.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::core::audit_trail::{self, AuditEntry};
use crate::core::savings_ledger::{self, SavingsEvent};
use crate::core::security_events::{SecurityCounts, parse_action};
use crate::core::wrapped::format_tokens;

use super::format::{Style, security_phrases};
use super::snapshot::ValueSnapshot;

#[derive(Debug, Clone, Serialize)]
pub struct ChainCheck {
    pub name: &'static str,
    pub path: Option<String>,
    pub entries: usize,
    pub intact: bool,
    pub first_invalid_at: Option<usize>,
}

/// First and last `entry_hash` of the chain entries a claim rests on —
/// anyone can look them up in the JSONL file.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Evidence {
    pub entries: u64,
    pub first_hash: Option<String>,
    pub last_hash: Option<String>,
}

impl Evidence {
    fn push(&mut self, hash: &str) {
        self.entries += 1;
        if self.first_hash.is_none() {
            self.first_hash = Some(hash.to_string());
        }
        self.last_hash = Some(hash.to_string());
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Proof {
    /// `None` = lifetime (every session).
    pub session_id: Option<String>,
    pub ledger: ChainCheck,
    pub audit: ChainCheck,
    /// ✓ measured: counterfactual input tokens of the attributed tool results.
    pub tokens_baseline: u64,
    /// ✓ measured: tokens kept out of the model's context.
    pub tokens_saved: u64,
    /// ✓ measured: tokens the agent spent re-reading (netted out).
    pub tokens_bounced: u64,
    pub ledger_evidence: Evidence,
    /// ✓ measured: security events committed to the audit trail.
    pub security: SecurityCounts,
    pub audit_evidence: Evidence,
    /// The live display counters, for comparison only — not proof.
    pub live: Option<ValueSnapshot>,
    /// UTC days (`YYYY-MM-DD`) with at least one attributed ledger event —
    /// the evidence behind streak milestones.
    #[serde(skip)]
    pub active_days: BTreeSet<String>,
}

impl Proof {
    pub fn tampered(&self) -> bool {
        !self.ledger.intact || !self.audit.intact
    }

    pub fn net_saved(&self) -> u64 {
        self.tokens_saved.saturating_sub(self.tokens_bounced)
    }

    /// ≈ derived: share of the attributed input kept out of context.
    pub fn saved_pct(&self) -> Option<f64> {
        (self.tokens_baseline > 0)
            .then(|| self.net_saved() as f64 * 100.0 / self.tokens_baseline as f64)
    }
}

fn in_session(entry_session: Option<&str>, wanted: Option<&str>) -> bool {
    wanted.is_none_or(|id| entry_session == Some(id))
}

/// Builds the proof for `session` (or lifetime) from the chains at the given
/// paths. A missing file is an empty, intact chain.
pub fn build_from(
    ledger_path: Option<&Path>,
    trail_path: Option<&Path>,
    session: Option<&str>,
    live: Option<ValueSnapshot>,
) -> Proof {
    let ledger_verify = ledger_path.map_or_else(savings_ledger::VerifyResult::empty, |p| {
        savings_ledger::store::verify(p)
    });
    let events: Vec<SavingsEvent> = ledger_path
        .map(savings_ledger::store::load)
        .unwrap_or_default();

    let mut proof = Proof {
        session_id: session.map(str::to_string),
        ledger: ChainCheck {
            name: "savings ledger",
            path: ledger_path.map(|p| p.display().to_string()),
            entries: ledger_verify.total.max(events.len()),
            intact: ledger_verify.valid,
            first_invalid_at: ledger_verify.first_invalid_at,
        },
        audit: ChainCheck {
            name: "audit trail",
            path: trail_path.map(|p| p.display().to_string()),
            entries: 0,
            intact: true,
            first_invalid_at: None,
        },
        tokens_baseline: 0,
        tokens_saved: 0,
        tokens_bounced: 0,
        ledger_evidence: Evidence::default(),
        security: SecurityCounts::default(),
        audit_evidence: Evidence::default(),
        live,
        active_days: BTreeSet::new(),
    };

    for ev in &events {
        if !in_session(ev.session_id.as_deref(), session) {
            continue;
        }
        if ev.saved_tokens == 0 && ev.bounce_adjustment == 0 {
            continue;
        }
        proof.tokens_baseline = proof.tokens_baseline.saturating_add(ev.baseline_tokens);
        proof.tokens_saved = proof.tokens_saved.saturating_add(ev.saved_tokens);
        proof.tokens_bounced = proof.tokens_bounced.saturating_add(ev.bounce_adjustment);
        proof.ledger_evidence.push(&ev.entry_hash);
        if let Some(day) = ev.ts.get(..10) {
            proof.active_days.insert(day.to_string());
        }
    }

    if let Some(path) = trail_path {
        let verify = audit_trail::verify_chain_at(path);
        let entries: Vec<AuditEntry> = audit_trail::load_all_at(path);
        proof.audit.entries = verify.total_entries.max(entries.len());
        proof.audit.intact = verify.valid;
        proof.audit.first_invalid_at = verify.first_invalid_at;
        for entry in &entries {
            let Some((kind, n, entry_session)) = entry.action.as_deref().and_then(parse_action)
            else {
                continue;
            };
            if !in_session(entry_session, session) {
                continue;
            }
            proof.security.add(kind, n);
            proof.audit_evidence.push(&entry.entry_hash);
        }
    }
    proof
}

/// The proof over the local chains. `session = None` and `lifetime = false`
/// picks the most recently active session (from `value/current.json`).
pub fn build(session: Option<&str>, lifetime: bool) -> Proof {
    let live = if lifetime {
        None
    } else {
        super::snapshot::load(session)
    };
    let session = if lifetime {
        None
    } else {
        session
            .map(str::to_string)
            .or_else(|| live.as_ref().map(|s| s.session_id.clone()))
            .filter(|id| !id.is_empty())
    };
    let ledger = savings_ledger::store::default_path();
    let trail = audit_trail::default_trail_path();
    build_from(
        ledger.as_deref(),
        trail.as_deref(),
        session.as_deref(),
        live,
    )
}

/// ✓ measured security events since `cutoff` (lifetime when `None`), from the
/// local audit trail. `None` when the trail fails verification, so a tampered
/// chain never reaches a report or share card.
pub fn verified_security_since(cutoff: Option<DateTime<Utc>>) -> Option<SecurityCounts> {
    security_since_at(&audit_trail::default_trail_path()?, cutoff)
}

/// ✓ measured lifetime security events together with the audit-trail entry
/// hashes they rest on. `None` when there is no trail or it fails verification.
pub fn verified_security_evidence() -> Option<(SecurityCounts, Evidence)> {
    security_evidence_at(&audit_trail::default_trail_path()?)
}

fn security_evidence_at(path: &Path) -> Option<(SecurityCounts, Evidence)> {
    let proof = build_from(None, Some(path), None, None);
    proof
        .audit
        .intact
        .then_some((proof.security, proof.audit_evidence))
}

fn security_since_at(path: &Path, cutoff: Option<DateTime<Utc>>) -> Option<SecurityCounts> {
    if !audit_trail::verify_chain_at(path).valid {
        return None;
    }
    let mut counts = SecurityCounts::default();
    for entry in audit_trail::load_all_at(path) {
        if let Some(cutoff) = cutoff {
            let Ok(ts) = DateTime::parse_from_rfc3339(&entry.timestamp) else {
                continue;
            };
            if ts.with_timezone(&Utc) < cutoff {
                continue;
            }
        }
        if let Some((kind, n, _)) = entry.action.as_deref().and_then(parse_action) {
            counts.add(kind, n);
        }
    }
    Some(counts)
}

fn short(hash: Option<&String>) -> String {
    hash.map_or_else(|| "—".to_string(), |h| h.chars().take(12).collect())
}

struct Marks {
    measured: String,
    derived: String,
    ok: String,
    bad: String,
}

impl Marks {
    fn new(style: Style) -> Self {
        let paint = |code: &str, text: &str| {
            if style.color {
                format!("\x1b[{code}m{text}\x1b[0m")
            } else {
                text.to_string()
            }
        };
        let (check, approx, cross) = if style.unicode {
            ("✓", "≈", "✗")
        } else {
            ("OK", "~", "FAIL")
        };
        Self {
            measured: paint("32", &format!("{check} measured")),
            derived: paint("2", &format!("{approx} derived ")),
            ok: paint("32", check),
            bad: paint("31", cross),
        }
    }
}

fn chain_line(check: &ChainCheck, marks: &Marks) -> String {
    if check.intact {
        format!(
            "{} {} intact ({} entries)",
            marks.ok, check.name, check.entries
        )
    } else {
        format!(
            "{} {} TAMPERED at entry {}",
            marks.bad,
            check.name,
            check.first_invalid_at.unwrap_or(0)
        )
    }
}

/// Human report. Every value line carries its label and source.
pub fn render(proof: &Proof, style: Style) -> String {
    let marks = Marks::new(style);
    let mut out = String::new();
    let scope = proof
        .session_id
        .as_deref()
        .map_or_else(|| "all sessions".to_string(), |id| format!("session {id}"));
    out.push_str(&format!("lean-ctx value · {scope}\n\n"));

    if proof.ledger_evidence.entries == 0 && proof.security.is_empty() {
        out.push_str("  nothing recorded for this scope yet\n");
    }
    if proof.ledger_evidence.entries > 0 {
        out.push_str(&format!(
            "  {}  {} tokens kept out of context   savings ledger, {} events\n",
            marks.measured,
            format_tokens(proof.net_saved()),
            proof.ledger_evidence.entries
        ));
        if proof.tokens_bounced > 0 {
            out.push_str(&format!(
                "                 (net of {} tokens re-read by the agent)\n",
                format_tokens(proof.tokens_bounced)
            ));
        }
        if let Some(pct) = proof.saved_pct() {
            out.push_str(&format!(
                "  {}  {pct:.0}% of {} tool-input tokens   net saved ÷ baseline\n",
                marks.derived,
                format_tokens(proof.tokens_baseline)
            ));
        }
    }
    for phrase in security_phrases(&proof.security) {
        out.push_str(&format!("  {}  {phrase}   audit trail\n", marks.measured));
    }

    out.push('\n');
    if proof.ledger_evidence.entries > 0 {
        out.push_str(&format!(
            "  evidence  ledger {} … {}\n",
            short(proof.ledger_evidence.first_hash.as_ref()),
            short(proof.ledger_evidence.last_hash.as_ref())
        ));
    }
    if proof.audit_evidence.entries > 0 {
        out.push_str(&format!(
            "  evidence  audit  {} … {}\n",
            short(proof.audit_evidence.first_hash.as_ref()),
            short(proof.audit_evidence.last_hash.as_ref())
        ));
    }
    out.push_str(&format!(
        "  chains    {}\n",
        chain_line(&proof.ledger, &marks)
    ));
    out.push_str(&format!(
        "            {}\n",
        chain_line(&proof.audit, &marks)
    ));

    if let Some(line) = proof.live.as_ref().and_then(|snap| {
        super::format::one_line(
            snap,
            Style {
                color: false,
                ..style
            },
        )
    }) {
        out.push_str(&format!(
            "\n  live      {line}\n            (display counters; the chains above are the proof)\n"
        ));
    }
    if proof.tampered() {
        out.push_str(
            "\n  A chain failed verification: the numbers above are NOT proof.\n\
             \x20 Inspect the file at the reported entry; `lean-ctx savings verify` shows details.\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::audit_trail::{AuditEntryData, AuditEventType};

    fn ledger_event(saved: u64, session: Option<&str>) -> SavingsEvent {
        let mut json = serde_json::json!({
            "ts": "2026-09-25T10:00:00+00:00",
            "tool": "ctx_read",
            "mechanism": "compression",
            "model_id": "m",
            "tokenizer": "o200k_base",
            "baseline_tokens": saved + 100,
            "actual_tokens": 100,
            "saved_tokens": saved,
            "bounce_adjustment": 0,
            "unit_price_per_m_usd": 3.0,
            "saved_usd": 0.0,
            "repo_hash": "r",
            "agent_id": "a",
            "prev_hash": "",
            "entry_hash": "",
            "version": "3.10.3",
        });
        if let Some(id) = session {
            json["session_id"] = id.into();
        }
        serde_json::from_value(json).unwrap()
    }

    fn security(path: &Path, action: &str) {
        audit_trail::record_at(
            path,
            AuditEntryData {
                agent_id: "a".into(),
                tool: "ctx_shell".into(),
                action: Some(action.into()),
                input_hash: String::new(),
                output_tokens: 0,
                role: "coder".into(),
                event_type: AuditEventType::SecretDetected,
            },
        );
    }

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let trail = dir.path().join("trail.jsonl");
        savings_ledger::store::append(&ledger, ledger_event(1_000, Some("s1"))).unwrap();
        savings_ledger::store::append(&ledger, ledger_event(500, Some("s2"))).unwrap();
        savings_ledger::store::append(&ledger, ledger_event(250, None)).unwrap();
        security(&trail, "secret_redacted:2|session=s1");
        security(&trail, "shell_blocked:1|session=s2");
        security(&trail, "tool_call");
        (dir, ledger, trail)
    }

    #[test]
    fn session_scope_counts_only_that_session() {
        let (_dir, ledger, trail) = fixture();
        let proof = build_from(Some(&ledger), Some(&trail), Some("s1"), None);
        assert!(!proof.tampered());
        assert_eq!(proof.tokens_saved, 1_000);
        assert_eq!(proof.ledger_evidence.entries, 1);
        assert_eq!(proof.security.secrets_redacted, 2);
        assert_eq!(proof.security.shell_blocked, 0);
        assert_eq!(proof.audit_evidence.entries, 1);
        assert_eq!(proof.ledger.entries, 3);
        assert_eq!(proof.audit.entries, 3);
    }

    #[test]
    fn lifetime_scope_counts_everything() {
        let (_dir, ledger, trail) = fixture();
        let proof = build_from(Some(&ledger), Some(&trail), None, None);
        assert_eq!(proof.tokens_saved, 1_750);
        assert_eq!(proof.security.total(), 3);
        let text = render(&proof, Style::PLAIN);
        assert!(text.contains("✓ measured"), "{text}");
        assert!(text.contains("2 secrets kept out of context"), "{text}");
        assert!(text.contains("savings ledger intact (3 entries)"), "{text}");
    }

    #[test]
    fn tampered_ledger_is_reported() {
        let (_dir, ledger, trail) = fixture();
        let raw = std::fs::read_to_string(&ledger).unwrap();
        std::fs::write(
            &ledger,
            raw.replacen("\"saved_tokens\":1000", "\"saved_tokens\":9000", 1),
        )
        .unwrap();
        let proof = build_from(Some(&ledger), Some(&trail), Some("s1"), None);
        assert!(proof.tampered());
        assert!(!proof.ledger.intact);
        let text = render(&proof, Style::PLAIN);
        assert!(text.contains("TAMPERED"), "{text}");
        assert!(text.contains("NOT proof"), "{text}");
    }

    #[test]
    fn tampered_trail_is_reported() {
        let (_dir, ledger, trail) = fixture();
        let raw = std::fs::read_to_string(&trail).unwrap();
        std::fs::write(
            &trail,
            raw.replacen("secret_redacted:2", "secret_redacted:9", 1),
        )
        .unwrap();
        let proof = build_from(Some(&ledger), Some(&trail), None, None);
        assert!(proof.tampered());
        assert!(!proof.audit.intact);
    }

    #[test]
    fn missing_chains_are_empty_and_intact() {
        let dir = tempfile::tempdir().unwrap();
        let proof = build_from(
            Some(&dir.path().join("none.jsonl")),
            Some(&dir.path().join("none2.jsonl")),
            Some("x"),
            None,
        );
        assert!(!proof.tampered());
        assert!(render(&proof, Style::PLAIN).contains("nothing recorded"));
    }

    #[test]
    fn security_since_filters_by_time_and_refuses_a_tampered_trail() {
        let (_dir, _ledger, trail) = fixture();
        let all = security_since_at(&trail, None).unwrap();
        assert_eq!(all.total(), 3);
        let future = Utc::now() + chrono::Duration::days(1);
        assert!(security_since_at(&trail, Some(future)).unwrap().is_empty());
        let past = Utc::now() - chrono::Duration::days(7);
        assert_eq!(security_since_at(&trail, Some(past)).unwrap(), all);

        let raw = std::fs::read_to_string(&trail).unwrap();
        std::fs::write(
            &trail,
            raw.replacen("secret_redacted:2", "secret_redacted:9", 1),
        )
        .unwrap();
        assert_eq!(security_since_at(&trail, None), None);
    }

    #[test]
    fn security_evidence_carries_the_hashes_and_refuses_a_tampered_trail() {
        let (_dir, _ledger, trail) = fixture();
        let (counts, evidence) = security_evidence_at(&trail).unwrap();
        assert_eq!(counts.total(), 3);
        assert_eq!(evidence.entries, 2);
        assert!(evidence.first_hash.is_some() && evidence.last_hash.is_some());

        let raw = std::fs::read_to_string(&trail).unwrap();
        std::fs::write(
            &trail,
            raw.replacen("shell_blocked:1", "shell_blocked:5", 1),
        )
        .unwrap();
        assert!(security_evidence_at(&trail).is_none());
    }

    #[test]
    fn json_has_labels_tooling_needs() {
        let (_dir, ledger, trail) = fixture();
        let proof = build_from(Some(&ledger), Some(&trail), Some("s2"), None);
        let json = serde_json::to_value(&proof).unwrap();
        assert_eq!(json["tokens_saved"], 500);
        assert_eq!(json["security"]["shell_blocked"], 1);
        assert_eq!(json["ledger"]["intact"], true);
        assert!(json["ledger_evidence"]["last_hash"].is_string());
    }
}
