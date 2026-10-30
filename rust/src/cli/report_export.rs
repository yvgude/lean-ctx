// SPDX-License-Identifier: Apache-2.0

//! `lean-ctx cockpit --export` without a terminal: the offline Cockpit report.
//!
//! One self-contained HTML page in the Cockpit's style. Every number comes
//! from the code that already proves it: the chain-verified totals of
//! `lean-ctx value` (savings ledger and audit trail, both re-walked from
//! genesis) and the signed ROI batch of `lean-ctx roi` (the estimated dollars
//! and the top tools and models). Each figure carries its trust label, nothing
//! is estimated here, and rows without a recorded saving are left out.

use std::fmt::Write as _;

use crate::core::savings_ledger::RoiReport;
use crate::core::security_events::SecurityCounts;
use crate::core::value::proof::{ChainCheck, Proof};
use crate::core::wrapped::format_tokens;

/// The default file name of the offline report.
pub(crate) const DEFAULT_OUTPUT: &str = "lean-ctx-report.html";

/// Same basis wording as `lean-ctx roi`.
const MEASUREMENT_BASIS: &str =
    "local token counts (before vs. after lean-ctx), not provider-billed usage";

/// Writes the offline report to `output` and prints a short summary on stderr.
pub(crate) fn write_report(output: &str) {
    let proof = crate::core::value::proof::build(None, true);
    let roi =
        crate::core::savings_ledger::roi_report(crate::core::agent_identity::current_agent_id());
    let html = render(&proof, &roi);
    if let Err(e) = std::fs::write(output, &html) {
        eprintln!("Error writing {output}: {e}");
        std::process::exit(1);
    }
    let record = Record::of(&proof.security);
    eprintln!("Wrote {output} ({:.1} KB)", html.len() as f64 / 1024.0);
    eprintln!(
        "  Delivered {} · withheld {} · redacted {} · blocked {}",
        delivered(&proof),
        record.withheld,
        record.redacted,
        record.blocked
    );
    eprintln!(
        "  Saved {} tokens (measured, savings ledger){}",
        format_tokens(proof.net_saved()),
        if proof.tampered() {
            " — a chain failed verification: not proof"
        } else {
            ""
        }
    );
}

/// The audit trail's decisions, grouped as the report shows them.
struct Record {
    withheld: u64,
    redacted: u64,
    blocked: u64,
}

/// Tool calls the audit trail recorded without a guard decision: what was
/// delivered as asked. The savings ledger meters only results that saved
/// something, so it undercounts deliveries.
fn delivered(proof: &Proof) -> u64 {
    (proof.audit.entries as u64).saturating_sub(proof.audit_evidence.entries)
}

impl Record {
    fn of(counts: &SecurityCounts) -> Self {
        Self {
            withheld: counts.content_withheld,
            redacted: counts.secrets_redacted.saturating_add(counts.pii_redacted),
            blocked: counts.path_blocked.saturating_add(counts.shell_blocked),
        }
    }
}

fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

const MEASURED: &str = r#"<span class="trust measured">✓ measured</span>"#;
const DERIVED: &str = r#"<span class="trust derived">≈ derived</span>"#;
const ESTIMATED: &str = r#"<span class="trust estimated">≈ estimated</span>"#;

fn tile(out: &mut String, label: &str, value: u64, note: &str) {
    let _ = write!(
        out,
        r#"<div class="tile"><div class="tile-label">{label}</div><div class="tile-value">{}</div><div class="tile-note">{MEASURED} {note}</div></div>"#,
        group_digits(value)
    );
}

fn chain_row(out: &mut String, check: &ChainCheck) {
    let (class, state) = if check.intact {
        ("good", "Intact".to_string())
    } else {
        (
            "critical",
            format!(
                "Tampered at entry {}",
                check.first_invalid_at.unwrap_or_default()
            ),
        )
    };
    let _ = write!(
        out,
        r#"<tr><td>{}</td><td class="{class}">{state}</td><td class="num">{}</td></tr>"#,
        escape_html(check.name),
        group_digits(check.entries as u64)
    );
}

fn short_hash(hash: Option<&String>) -> String {
    hash.map_or_else(|| "—".to_string(), |h| h.chars().take(12).collect())
}

/// The report page. A pure function of its inputs.
pub(crate) fn render(proof: &Proof, roi: &RoiReport) -> String {
    let record = Record::of(&proof.security);
    let mut body = String::new();

    if proof.tampered() {
        body.push_str(
            r#"<div class="banner critical">A chain failed verification: the numbers below are <strong>not proof</strong>. <code>lean-ctx savings verify</code> and <code>lean-ctx verify</code> show where.</div>"#,
        );
    }

    // What the record shows: delivered, withheld, redacted, blocked.
    body.push_str("<section><h2>What was delivered, withheld and redacted</h2>");
    body.push_str(r#"<p class="lead">Decisions recorded in the signed, hash-chained audit trail and the savings ledger. Counts only: the record holds no file content, path or secret.</p><div class="tiles">"#);
    tile(
        &mut body,
        "Delivered",
        delivered(proof),
        "tool calls without a guard decision · audit trail",
    );
    tile(
        &mut body,
        "Withheld",
        record.withheld,
        "sources withheld · audit trail",
    );
    tile(
        &mut body,
        "Redacted",
        record.redacted,
        "secrets and personal data · audit trail",
    );
    tile(
        &mut body,
        "Blocked",
        record.blocked,
        "paths and commands · audit trail",
    );
    body.push_str("</div>");
    let phrases = crate::core::value::format::security_phrases(&proof.security);
    if phrases.is_empty() {
        body.push_str(r#"<p class="empty">No guard decision recorded yet.</p>"#);
    } else {
        body.push_str(r#"<ul class="facts">"#);
        for phrase in phrases {
            let _ = write!(
                body,
                "<li>{MEASURED} {} <span class=\"src\">audit trail</span></li>",
                escape_html(&phrase)
            );
        }
        body.push_str("</ul>");
    }
    body.push_str("</section>");

    // Savings, with the label each number has earned.
    body.push_str("<section><h2>Savings</h2>");
    if proof.ledger_evidence.entries == 0 {
        body.push_str(r#"<p class="empty">No savings recorded yet.</p>"#);
    } else {
        body.push_str(r#"<ul class="facts">"#);
        let _ = write!(
            body,
            "<li>{MEASURED} <strong>{}</strong> tokens kept out of context <span class=\"src\">savings ledger, {} events</span></li>",
            group_digits(proof.net_saved()),
            group_digits(proof.ledger_evidence.entries)
        );
        if proof.tokens_bounced > 0 {
            let _ = write!(
                body,
                "<li>{MEASURED} net of {} tokens the agent re-read <span class=\"src\">savings ledger</span></li>",
                group_digits(proof.tokens_bounced)
            );
        }
        if let Some(pct) = proof.saved_pct() {
            let _ = write!(
                body,
                "<li>{DERIVED} {pct:.0}% of {} tool-input tokens <span class=\"src\">net saved ÷ baseline</span></li>",
                group_digits(proof.tokens_baseline)
            );
        }
        if roi.turns > 0 {
            let _ = write!(
                body,
                "<li>{DERIVED} {} tokens after lean-ctx's own context <span class=\"src\">{} tok/turn × {} turns</span></li>",
                escape_html(&roi.net_after_overhead_tokens.to_string()),
                group_digits(roi.injected_overhead_tokens_per_turn),
                group_digits(roi.turns)
            );
        }
        if roi.saved_usd > 0.0 {
            let _ = write!(
                body,
                "<li>{ESTIMATED} ${:.2} at list prices <span class=\"src\">signed ROI batch</span></li>",
                roi.saved_usd
            );
        }
        body.push_str("</ul>");
        let _ = write!(body, r#"<p class="basis">Basis: {MEASUREMENT_BASIS}.</p>"#);

        let tools: Vec<&(String, u64)> = roi.top_tools.iter().filter(|(_, t)| *t > 0).collect();
        if !tools.is_empty() {
            body.push_str(r#"<h3>Top tools</h3><table><thead><tr><th>Tool</th><th class="num">Tokens saved</th></tr></thead><tbody>"#);
            for (tool, tokens) in tools.into_iter().take(10) {
                let _ = write!(
                    body,
                    r#"<tr><td>{}</td><td class="num">{}</td></tr>"#,
                    escape_html(tool),
                    group_digits(*tokens)
                );
            }
            body.push_str("</tbody></table>");
        }
        let models: Vec<&(String, u64, f64)> =
            roi.top_models.iter().filter(|(_, t, _)| *t > 0).collect();
        if !models.is_empty() {
            body.push_str(r#"<h3>Top models</h3><table><thead><tr><th>Model</th><th class="num">Tokens saved</th><th class="num">≈ $ at list prices</th></tr></thead><tbody>"#);
            for (model, tokens, usd) in models.into_iter().take(10) {
                let _ = write!(
                    body,
                    r#"<tr><td>{}</td><td class="num">{}</td><td class="num">${usd:.2}</td></tr>"#,
                    escape_html(model),
                    group_digits(*tokens)
                );
            }
            body.push_str("</tbody></table>");
        }
    }
    body.push_str("</section>");

    // Integrity: the chains every number above rests on.
    body.push_str(r#"<section><h2>Integrity</h2><table><thead><tr><th>Chain</th><th>State</th><th class="num">Entries</th></tr></thead><tbody>"#);
    chain_row(&mut body, &proof.ledger);
    chain_row(&mut body, &proof.audit);
    body.push_str("</tbody></table><ul class=\"facts plain\">");
    let signature = match (roi.signed, roi.signer_public_key.as_deref()) {
        (true, Some(key)) => format!(
            "Ed25519, device key <code>{}…</code>",
            escape_html(key.get(..16).unwrap_or(key))
        ),
        _ => "not signed on this device".to_string(),
    };
    let _ = write!(body, "<li>ROI batch signature: {signature}</li>");
    if proof.ledger_evidence.entries > 0 {
        let _ = write!(
            body,
            "<li>Ledger evidence <code>{}</code> … <code>{}</code></li>",
            escape_html(&short_hash(proof.ledger_evidence.first_hash.as_ref())),
            escape_html(&short_hash(proof.ledger_evidence.last_hash.as_ref()))
        );
    }
    if proof.audit_evidence.entries > 0 {
        let _ = write!(
            body,
            "<li>Audit evidence <code>{}</code> … <code>{}</code></li>",
            escape_html(&short_hash(proof.audit_evidence.first_hash.as_ref())),
            escape_html(&short_hash(proof.audit_evidence.last_hash.as_ref()))
        );
    }
    body.push_str("<li>A signed bundle for an auditor: <code>lean-ctx audit evidence --out bundle.zip</code></li></ul></section>");

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>LeanCTX Report</title>
<style>{CSS}</style>
</head>
<body>
<main>
<header><div class="eyebrow">LeanCTX Cockpit · Report</div><h1>Report</h1><p class="meta">All projects on this device · generated {created} · lean-ctx {version}</p></header>
{body}
<footer>Offline export of the Cockpit report (<code>lean-ctx cockpit --export</code>). On a terminal, <code>lean-ctx cockpit --export</code> opens the live view.</footer>
</main>
</body>
</html>
"#,
        created = escape_html(&roi.created_at),
        version = escape_html(env!("CARGO_PKG_VERSION")),
    )
}

/// The Cockpit's Ops tokens (light and dark), system fonts only: the page
/// works offline and loads nothing.
const CSS: &str = r#"
:root{--canvas:#f6f7fc;--panel:#e9ecfa;--surface:#fff;--foreground:#171c31;--secondary:#555e78;--muted:#7a839c;--hairline:#ccd2e5;--hairline-soft:#e1e5f2;--highlight:#365bc5;--tint:#e4e9fa;--good:#0b7a3e;--warning:#9a6400;--critical:#c02f2f;--font:"Helvetica Neue",Arial,sans-serif;--mono:ui-monospace,SFMono-Regular,Menlo,monospace;color-scheme:light}
@media (prefers-color-scheme:dark){:root{--canvas:#111626;--panel:#1b2034;--surface:#1b2034;--foreground:#f1f3ff;--secondary:#afb8d0;--muted:#8a93ad;--hairline:#353e5a;--hairline-soft:#2a3150;--highlight:#668cff;--tint:#263055;--good:#4cc77f;--warning:#f5c04d;--critical:#f07c7c;color-scheme:dark}}
*{box-sizing:border-box}html,body{margin:0;background:var(--canvas);color:var(--foreground);font-family:var(--font);font-size:14px;line-height:1.5}
main{max-width:960px;margin:0 auto;padding:32px 16px 48px}
.eyebrow{color:var(--highlight);font-weight:500;font-size:12.5px;letter-spacing:.02em}
h1{font-size:28px;font-weight:500;margin:4px 0}h2{font-size:15px;font-weight:500;margin:0 0 4px}h3{font-size:14px;font-weight:500;margin:20px 0 8px;color:var(--secondary)}
.meta,.lead,.basis,.empty,footer{color:var(--secondary)}.basis,footer{font-size:12.5px}
section{background:var(--surface);box-shadow:0 0 0 1px var(--hairline-soft);border-radius:12px;padding:20px;margin:16px 0}
.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:16px;margin:16px 0}
.tile{background:var(--canvas);border-radius:6px;padding:14px 16px;box-shadow:0 0 0 1px var(--hairline-soft)}
.tile-label{color:var(--secondary);font-size:12.5px}.tile-value{font-size:28px;font-variant-numeric:tabular-nums}.tile-note{color:var(--muted);font-size:12px}
.facts{list-style:none;padding:0;margin:8px 0}.facts li{padding:6px 0;border-bottom:1px solid var(--hairline-soft)}.facts li:last-child{border-bottom:0}
.src{color:var(--muted);font-size:12.5px;margin-left:6px}
.trust{display:inline-block;font-size:12px;border-radius:4px;padding:0 6px;margin-right:4px}
.measured{color:var(--good);background:color-mix(in srgb,var(--good) 12%,transparent)}
.derived,.estimated{color:var(--secondary);background:var(--panel)}
table{width:100%;border-collapse:collapse;font-variant-numeric:tabular-nums}th,td{text-align:left;padding:8px;border-bottom:1px solid var(--hairline-soft)}th{color:var(--muted);font-weight:500;font-size:12.5px}
.num{text-align:right}.good{color:var(--good)}.critical{color:var(--critical)}
code{font-family:var(--mono);font-size:12.5px;background:var(--panel);border-radius:4px;padding:0 4px;overflow-wrap:anywhere}
.banner{border-radius:6px;padding:12px 16px;margin:16px 0}.banner.critical{background:color-mix(in srgb,var(--critical) 12%,transparent);color:var(--critical)}
footer{margin-top:24px}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::value::proof::Evidence;

    fn chain(name: &'static str, entries: usize, intact: bool) -> ChainCheck {
        ChainCheck {
            name,
            path: None,
            entries,
            intact,
            first_invalid_at: (!intact).then_some(3),
        }
    }

    fn proof(events: u64, security: SecurityCounts, intact: bool) -> Proof {
        Proof {
            session_id: None,
            ledger: chain("savings ledger", events as usize, true),
            audit: chain("audit trail", 4, intact),
            tokens_baseline: 10_000,
            tokens_saved: 4_200,
            tokens_bounced: 200,
            ledger_evidence: Evidence {
                entries: events,
                first_hash: Some("a".repeat(64)),
                last_hash: Some("b".repeat(64)),
            },
            security,
            audit_evidence: Evidence::default(),
            live: None,
            active_days: Default::default(),
        }
    }

    fn roi(tools: &serde_json::Value) -> RoiReport {
        serde_json::from_value(serde_json::json!({
            "period": "all", "created_at": "2026-10-30T12:00:00Z", "lean_ctx_version": "test",
            "agent_id": "a", "last_entry_hash": "h", "chain_valid": true, "signed": false,
            "signer_public_key": null, "total_events": 3, "saved_tokens": 4200,
            "net_saved_tokens": 4000, "saved_usd": 0.05, "avg_saved_tokens_per_event": 1.0,
            "avg_saved_usd_per_event": 0.0, "top_models": [], "top_tools": tools
        }))
        .expect("roi report")
    }

    #[test]
    fn the_report_shows_the_record_with_trust_labels_and_no_visualizer() {
        let security = SecurityCounts {
            content_withheld: 2,
            secrets_redacted: 3,
            path_blocked: 1,
            ..SecurityCounts::default()
        };
        let html = render(
            &proof(3, security, true),
            &roi(&serde_json::json!([["ctx_read", 4000], ["ctx_tree", 0]])),
        );
        assert!(html.contains("<title>LeanCTX Report</title>"));
        assert!(!html.contains("Visualizer"));
        assert!(html.contains("What was delivered, withheld and redacted"));
        assert!(html.contains("2 sources withheld"));
        assert!(html.contains("3 secrets kept out of context"));
        assert!(html.contains("✓ measured"));
        assert!(
            html.contains("≈ derived"),
            "the share of the baseline is derived"
        );
        assert!(html.contains("4,000"), "net of re-reads: 4,200 − 200");
        assert!(html.contains("ctx_read"));
        assert!(
            !html.contains("ctx_tree"),
            "a row without a recorded saving is left out"
        );
        assert!(!html.contains("not proof"));
    }

    #[test]
    fn a_tampered_chain_says_not_proof_and_an_empty_record_says_so() {
        let html = render(
            &proof(0, SecurityCounts::default(), false),
            &roi(&serde_json::json!([])),
        );
        assert!(html.contains("not proof"));
        assert!(html.contains("Tampered at entry 3"));
        assert!(html.contains("No guard decision recorded yet."));
        assert!(html.contains("No savings recorded yet."));
        assert!(!html.contains("Top tools"));
    }

    #[test]
    fn labels_are_escaped() {
        let html = render(
            &proof(1, SecurityCounts::default(), true),
            &roi(&serde_json::json!([["<script>", 5]])),
        );
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }
}
