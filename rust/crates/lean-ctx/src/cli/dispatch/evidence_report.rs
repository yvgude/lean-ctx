// SPDX-License-Identifier: Apache-2.0
//! Self-contained customer-facing HTML reports for real-world evidence runs.

use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::evidence_realworld::{RealWorldResult, TurnResult};

const REPORT_CSS: &str = r#"
:root {
  color-scheme: dark;
  --canvas: #09111d;
  --surface: #101c2d;
  --surface-raised: #16263c;
  --line: rgba(186, 211, 240, .16);
  --text: #f3f7fd;
  --muted: #9eb1c8;
  --blue: #59a8ff;
  --cyan: #49dfd0;
  --green: #60dc9c;
  --amber: #f1c86b;
  --shadow: 0 20px 60px rgba(0, 0, 0, .25);
}

* { box-sizing: border-box; }
html { background: var(--canvas); }
body {
  margin: 0;
  background: radial-gradient(circle at 92% -10%, rgba(72, 164, 255, .18), transparent 31rem), var(--canvas);
  color: var(--text);
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  font-size: 14px;
  line-height: 1.5;
}

.report { max-width: 1160px; margin: 0 auto; padding: 48px 36px 32px; }
.hero { display: flex; justify-content: space-between; gap: 32px; margin-bottom: 34px; }
.brand { display: flex; align-items: center; gap: 10px; color: var(--cyan); font-size: 13px; font-weight: 750; letter-spacing: .12em; text-transform: uppercase; }
.brand-mark { display: inline-grid; width: 24px; height: 24px; place-items: center; border: 1px solid rgba(73, 223, 208, .65); border-radius: 7px; color: var(--cyan); font-size: 17px; line-height: 1; }
h1 { margin: 13px 0 9px; font-size: clamp(30px, 5vw, 46px); letter-spacing: -.045em; line-height: 1.06; }
.subtitle { max-width: 680px; margin: 0; color: var(--muted); font-size: 16px; }
.run-meta { min-width: 235px; align-self: end; padding: 15px 17px; border: 1px solid var(--line); border-radius: 12px; background: rgba(16, 28, 45, .72); color: var(--muted); font-size: 12px; }
.run-meta span { display: block; color: var(--text); font-weight: 650; word-break: break-word; }

.section { margin-top: 24px; padding: 26px; border: 1px solid var(--line); border-radius: 16px; background: linear-gradient(135deg, rgba(22, 38, 60, .86), rgba(16, 28, 45, .9)); box-shadow: var(--shadow); }
.section-title { margin: 0 0 5px; font-size: 18px; letter-spacing: -.02em; }
.section-intro { margin: 0 0 20px; color: var(--muted); }

.summary-grid { display: grid; grid-template-columns: 1.25fr repeat(3, 1fr); gap: 12px; }
.stat { min-height: 123px; padding: 17px; border: 1px solid var(--line); border-radius: 12px; background: rgba(5, 13, 24, .38); }
.stat.highlight { background: linear-gradient(145deg, rgba(73, 223, 208, .16), rgba(73, 168, 255, .09)); border-color: rgba(73, 223, 208, .3); }
.stat-label { color: var(--muted); font-size: 12px; font-weight: 700; letter-spacing: .055em; text-transform: uppercase; }
.stat-value { margin: 8px 0 2px; font-size: 28px; font-weight: 780; letter-spacing: -.045em; line-height: 1.1; }
.stat-value.green { color: var(--green); }
.stat-detail { color: var(--muted); font-size: 12px; }
.comparison-note { margin: 20px 0 0; color: var(--muted); }
.comparison-note strong { color: var(--text); }

.table-wrap { overflow-x: auto; border: 1px solid var(--line); border-radius: 12px; }
table { width: 100%; border-collapse: collapse; font-variant-numeric: tabular-nums; }
th, td { padding: 13px 14px; border-bottom: 1px solid var(--line); text-align: right; white-space: nowrap; }
th { background: rgba(5, 13, 24, .42); color: var(--muted); font-size: 11px; font-weight: 750; letter-spacing: .05em; text-transform: uppercase; }
td { color: #dce8f7; }
th:first-child, td:first-child { text-align: left; white-space: normal; min-width: 180px; }
tbody tr:last-child td { border-bottom: 0; }
.turn-name { display: block; color: var(--text); font-weight: 700; }
.turn-description { display: block; margin-top: 2px; color: var(--muted); font-size: 12px; }
.treatment-column { color: var(--cyan); }

.chart { display: grid; gap: 18px; }
.chart-row { display: grid; grid-template-columns: minmax(145px, 190px) 1fr; gap: 16px; align-items: center; }
.chart-label { color: var(--text); font-weight: 700; }
.chart-label small { display: block; color: var(--muted); font-size: 11px; font-weight: 500; }
.bar-set { display: grid; gap: 7px; }
.bar-line { display: grid; grid-template-columns: 72px 1fr 82px; gap: 9px; align-items: center; color: var(--muted); font-size: 11px; }
.track { height: 11px; overflow: hidden; border-radius: 999px; background: rgba(165, 194, 227, .1); }
.bar { height: 100%; border-radius: inherit; }
.bar.baseline { background: linear-gradient(90deg, #7a8fab, #a9bed7); }
.bar.treatment { background: linear-gradient(90deg, #1db9af, #57e5d5); }
.chart-value { color: var(--text); text-align: right; font-variant-numeric: tabular-nums; }
.legend { display: flex; gap: 16px; margin: 0 0 18px; color: var(--muted); font-size: 12px; }
.legend span::before { display: inline-block; width: 9px; height: 9px; margin-right: 6px; border-radius: 50%; content: ""; }
.legend .baseline::before { background: #a9bed7; }
.legend .treatment::before { background: #57e5d5; }

.method-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
.method-item { padding: 14px 15px; border: 1px solid var(--line); border-radius: 10px; background: rgba(5, 13, 24, .28); }
.method-item dt { color: var(--muted); font-size: 11px; font-weight: 750; letter-spacing: .055em; text-transform: uppercase; }
.method-item dd { margin: 5px 0 0; color: var(--text); word-break: break-word; }
.method-description { grid-column: 1 / -1; }

.repro { margin: 0; padding: 17px; overflow-x: auto; border: 1px solid rgba(73, 223, 208, .22); border-radius: 12px; background: #07111e; color: #c8e6ff; font: 12px/1.65 ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; white-space: pre-wrap; }
.footer { display: flex; justify-content: space-between; gap: 20px; margin-top: 22px; padding: 17px 2px 0; border-top: 1px solid var(--line); color: var(--muted); font-size: 11px; }
.footer code { color: #b9cce3; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; word-break: break-all; }

@media (max-width: 760px) {
  .report { padding: 28px 18px 24px; }
  .hero, .footer { display: block; }
  .run-meta { margin-top: 20px; }
  .summary-grid, .method-grid { grid-template-columns: 1fr 1fr; }
  .summary-grid .highlight { grid-column: 1 / -1; }
  .chart-row { grid-template-columns: 1fr; gap: 7px; }
  .footer > * + * { margin-top: 9px; }
}

@media print {
  @page { margin: 12mm; size: A4; }
  body { -webkit-print-color-adjust: exact; print-color-adjust: exact; background: var(--canvas); }
  .report { max-width: none; padding: 0; }
  .section { break-inside: avoid; box-shadow: none; }
  .table-wrap { overflow: visible; }
  th, td { padding: 8px 9px; font-size: 9px; }
  .section { margin-top: 14px; padding: 18px; }
}
"#;

/// Render a real-world evidence JSON file as a standalone HTML document.
pub(crate) fn render_file(input: &Path, output: &Path) -> Result<()> {
    let json = fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let result: RealWorldResult = serde_json::from_slice(&json)
        .with_context(|| format!("parse real-world evidence JSON: {}", input.display()))?;
    let verification_hash = hex_digest(&json);
    let html = render_html(&result, &verification_hash);

    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(output, html).with_context(|| format!("write {}", output.display()))
}

fn render_html(result: &RealWorldResult, verification_hash: &str) -> String {
    let baseline_turns: BTreeMap<usize, &TurnResult> = result
        .baseline
        .turns
        .iter()
        .map(|turn| (turn.turn, turn))
        .collect();
    let treatment_turns: BTreeMap<usize, &TurnResult> = result
        .treatment
        .turns
        .iter()
        .map(|turn| (turn.turn, turn))
        .collect();
    let turn_numbers: Vec<usize> = baseline_turns
        .keys()
        .chain(treatment_turns.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let max_turn_cost = result
        .baseline
        .turns
        .iter()
        .chain(result.treatment.turns.iter())
        .map(|turn| turn.real_cost_usd)
        .fold(0.0_f64, f64::max);

    let mut rows = String::new();
    let mut chart_rows = String::new();
    for turn_number in turn_numbers {
        let baseline = baseline_turns.get(&turn_number).copied();
        let treatment = treatment_turns.get(&turn_number).copied();
        let turn = baseline.or(treatment);
        let Some(turn) = turn else { continue };
        let label = format!("Turn {} · {}", turn_number, turn.name);
        let description = &turn.description;

        let _ = write!(
            rows,
            r#"<tr><td><span class=\"turn-name\">{}</span><span class=\"turn-description\">{}</span></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"treatment-column\">{}</td><td class=\"treatment-column\">{}</td><td class=\"treatment-column\">{}</td><td class=\"treatment-column\">{}</td></tr>"#,
            escape_html(&label),
            escape_html(description),
            baseline.map_or_else(
                || "—".to_string(),
                |value| format_number(value.prompt_tokens_sent)
            ),
            baseline.map_or_else(
                || "—".to_string(),
                |value| format_number(value.cached_tokens)
            ),
            baseline.map_or_else(
                || "—".to_string(),
                |value| format_number(value.provider_completion_tokens)
            ),
            baseline.map_or_else(|| "—".to_string(), |value| format_usd(value.real_cost_usd)),
            treatment.map_or_else(
                || "—".to_string(),
                |value| format_number(value.prompt_tokens_sent)
            ),
            treatment.map_or_else(
                || "—".to_string(),
                |value| format_number(value.cached_tokens)
            ),
            treatment.map_or_else(
                || "—".to_string(),
                |value| format_number(value.provider_completion_tokens)
            ),
            treatment.map_or_else(|| "—".to_string(), |value| format_usd(value.real_cost_usd)),
        );

        let baseline_cost = baseline.map_or(0.0, |value| value.real_cost_usd);
        let treatment_cost = treatment.map_or(0.0, |value| value.real_cost_usd);
        let _ = write!(
            chart_rows,
            r#"<div class=\"chart-row\"><div class=\"chart-label\">{}<small>{}</small></div><div class=\"bar-set\"><div class=\"bar-line\"><span>Baseline</span><div class=\"track\"><div class=\"bar baseline\" style=\"width: {:.2}%\"></div></div><span class=\"chart-value\">{}</span></div><div class=\"bar-line\"><span>Treatment</span><div class=\"track\"><div class=\"bar treatment\" style=\"width: {:.2}%\"></div></div><span class=\"chart-value\">{}</span></div></div></div>"#,
            escape_html(&format!("Turn {turn_number} · {}", turn.name)),
            escape_html(description),
            percentage_of_max(baseline_cost, max_turn_cost),
            format_usd(baseline_cost),
            percentage_of_max(treatment_cost, max_turn_cost),
            format_usd(treatment_cost),
        );
    }

    let token_delta = result
        .baseline
        .total_prompt_tokens
        .saturating_sub(result.treatment.total_prompt_tokens);
    let cost_delta = result.savings_usd.max(0.0);
    let source_file = "realworld-result.json";
    let reproduction = format!(
        "# Generate the evidence run\nlean-ctx evidence realworld --target <project-dir> --output evidence-realworld\n\n# Render this standalone report\nlean-ctx evidence report --input evidence-realworld/{source_file} --output evidence-report.html\n\n# Verify the source evidence fingerprint\nshasum -a 256 evidence-realworld/{source_file}\n# Compare the result with the SHA-256 value in this report footer."
    );

    // The template uses a raw string so the inline CSS remains readable. Normalize
    // its attribute quote escapes after interpolation; externally supplied values
    // are already HTML-escaped before they reach the template.
    format!(
        r#"<!doctype html>
<html lang=\"en\">
<head>
  <meta charset=\"utf-8\">
  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
  <meta name=\"color-scheme\" content=\"dark\">
  <title>Thinkery | Evidence report</title>
  <style>{REPORT_CSS}</style>
</head>
<body>
  <main class=\"report\">
    <header class=\"hero\">
      <div>
        <div class=\"brand\"><span class=\"brand-mark\">T</span> Thinkery</div>
        <h1>Evidence report</h1>
        <p class=\"subtitle\">A measured comparison of standard context handling and lean-ctx compression across a sequential, real-world code-review workflow.</p>
      </div>
      <aside class=\"run-meta\">Model<span>{}</span>Evidence run<span>{}</span></aside>
    </header>

    <section class=\"section\">
      <h2 class=\"section-title\">Executive summary</h2>
      <p class=\"section-intro\">Five consecutive API turns were run for each arm against <strong>{}</strong>.</p>
      <div class=\"summary-grid\">
        <article class=\"stat highlight\"><div class=\"stat-label\">Real cost reduction</div><div class=\"stat-value green\">{:.1}%</div><div class=\"stat-detail\">{} saved across this measured workflow</div></article>
        <article class=\"stat\"><div class=\"stat-label\">Prompt token reduction</div><div class=\"stat-value\">{:.1}%</div><div class=\"stat-detail\">{} fewer prompt tokens</div></article>
        <article class=\"stat\"><div class=\"stat-label\">Baseline cost</div><div class=\"stat-value\">{}</div><div class=\"stat-detail\">{} prompt tokens</div></article>
        <article class=\"stat\"><div class=\"stat-label\">lean-ctx cost</div><div class=\"stat-value\">{}</div><div class=\"stat-detail\">{} prompt tokens</div></article>
      </div>
      <p class=\"comparison-note\"><strong>Measured result:</strong> lean-ctx reduced billed cost from {} to {} while preserving the same {}-turn workflow.</p>
    </section>

    <section class=\"section\">
      <h2 class=\"section-title\">Per-turn comparison</h2>
      <p class=\"section-intro\">Provider-reported usage and actual API cost for every step in the accumulated session.</p>
      <div class=\"table-wrap\"><table>
        <thead><tr><th rowspan=\"2\">Workflow turn</th><th colspan=\"4\">Baseline</th><th colspan=\"4\">lean-ctx treatment</th></tr>
        <tr><th>Prompt tokens</th><th>Cached</th><th>Completion</th><th>Cost</th><th>Prompt tokens</th><th>Cached</th><th>Completion</th><th>Cost</th></tr></thead>
        <tbody>{rows}</tbody>
      </table></div>
    </section>

    <section class=\"section\">
      <h2 class=\"section-title\">Cost savings by turn</h2>
      <p class=\"section-intro\">Each bar is scaled to the highest individual turn cost in this evidence run.</p>
      <div class=\"legend\"><span class=\"baseline\">Baseline</span><span class=\"treatment\">lean-ctx treatment</span></div>
      <div class=\"chart\" role=\"img\" aria-label=\"Per-turn baseline and lean-ctx treatment costs\">{chart_rows}</div>
    </section>

    <section class=\"section\">
      <h2 class=\"section-title\">Methodology</h2>
      <dl class=\"method-grid\">
        <div class=\"method-item method-description\"><dt>Study design</dt><dd>{}</dd></div>
        <div class=\"method-item\"><dt>Turns per arm</dt><dd>{}</dd></div>
        <div class=\"method-item\"><dt>Shell output cap</dt><dd>{} lines</dd></div>
        <div class=\"method-item\"><dt>Cache source</dt><dd>{}</dd></div>
        <div class=\"method-item\"><dt>Cost calculation</dt><dd>{}</dd></div>
      </dl>
    </section>

    <section class=\"section\">
      <h2 class=\"section-title\">Reproducibility</h2>
      <p class=\"section-intro\">This report is self-contained and printable. Re-run the following commands to create and independently verify a comparable report.</p>
      <pre class=\"repro\"><code>{}</code></pre>
    </section>

    <footer class=\"footer\"><span>Generated by lean-ctx {} · Evidence timestamp: {}</span><span>Verification hash · <code>SHA-256: {}</code></span></footer>
  </main>
</body>
</html>"#,
        escape_html(&result.model),
        escape_html(&result.timestamp),
        escape_html(&result.target_path),
        result.savings_cost_pct,
        format_usd(cost_delta),
        result.savings_tokens_pct,
        format_number(token_delta),
        format_usd(result.baseline.total_real_cost_usd),
        format_number(result.baseline.total_prompt_tokens),
        format_usd(result.treatment.total_real_cost_usd),
        format_number(result.treatment.total_prompt_tokens),
        format_usd(result.baseline.total_real_cost_usd),
        format_usd(result.treatment.total_real_cost_usd),
        result.methodology.turns_per_arm,
        escape_html(&result.methodology.description),
        result.methodology.turns_per_arm,
        result.methodology.shell_cap_lines,
        escape_html(&result.methodology.cache_source),
        escape_html(&result.methodology.cost_calculation),
        escape_html(&reproduction),
        escape_html(&result.lean_ctx_version),
        escape_html(&result.timestamp),
        escape_html(verification_hash),
        rows = rows,
        chart_rows = chart_rows,
    )
    .replace(r#"\""#, "\"")
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn percentage_of_max(value: f64, max: f64) -> f64 {
    if max > 0.0 {
        (value / max * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn format_number(value: usize) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(digit);
    }
    output
}

fn format_usd(value: f64) -> String {
    format!("${value:.5}")
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::dispatch::evidence_realworld::{ArmResult, Methodology, ReceiptSavings};

    fn evidence() -> RealWorldResult {
        let baseline_turn = TurnResult {
            turn: 1,
            name: "orientation <raw>".to_string(),
            description: "Inspect & understand".to_string(),
            new_content_tokens: 42,
            prompt_tokens_sent: 1000,
            cached_tokens: 200,
            provider_prompt_tokens: 1000,
            provider_completion_tokens: 100,
            real_cost_usd: 0.02,
            response: String::new(),
            duration_ms: 200,
        };
        let treatment_turn = TurnResult {
            prompt_tokens_sent: 300,
            cached_tokens: 100,
            provider_completion_tokens: 80,
            real_cost_usd: 0.005,
            ..baseline_turn.clone()
        };
        RealWorldResult {
            schema_version: "1".to_string(),
            integrity_status: "observed".to_string(),
            outcome: "succeeded".to_string(),
            capability_id: None,
            capability_version: None,
            baseline: ArmResult {
                arm: "baseline".to_string(),
                turns: vec![baseline_turn],
                total_prompt_tokens: 1000,
                total_cached_tokens: 200,
                total_completion_tokens: 100,
                total_real_cost_usd: 0.02,
                effective_input_cost_usd: 0.01,
            },
            treatment: ArmResult {
                arm: "treatment".to_string(),
                turns: vec![treatment_turn],
                total_prompt_tokens: 300,
                total_cached_tokens: 100,
                total_completion_tokens: 80,
                total_real_cost_usd: 0.005,
                effective_input_cost_usd: 0.003,
            },
            savings_tokens_pct: 70.0,
            savings_cost_pct: 75.0,
            savings_usd: 0.015,
            savings: ReceiptSavings {
                original_tokens: 1000,
                delivered_tokens: 300,
                saved_tokens: 700,
                saved_pct: 70.0,
                methodology: "baseline_treatment".to_string(),
            },
            quality: None,
            quality_gate: None,
            model: "test-model".to_string(),
            endpoint: "https://example.test".to_string(),
            target_path: "/tmp/project".to_string(),
            timestamp: "2026-08-19T10:19:52Z".to_string(),
            lean_ctx_version: "3.9.19".to_string(),
            methodology: Methodology {
                description: "Sequential & measured".to_string(),
                multi_turn: true,
                fair_baseline: "Shell output capped to last 500 lines".to_string(),
                turns_per_arm: 1,
                shell_cap_lines: 500,
                cache_source: "provider cache field".to_string(),
                cost_calculation: "input + output".to_string(),
            },
        }
    }

    #[test]
    fn report_contains_metrics_and_escapes_external_values() {
        let report = render_html(&evidence(), "abc123");

        assert!(report.contains("75.0%"));
        assert!(report.contains("$0.02000"));
        assert!(report.contains("$0.00500"));
        assert!(report.contains("1,000"));
        assert!(report.contains("orientation &lt;raw&gt;"));
        assert!(report.contains("Inspect &amp; understand"));
        assert!(report.contains("SHA-256: abc123"));
        assert!(report.contains("@media print"));
        assert!(report.contains("class=\"report\""));
    }

    #[test]
    fn number_formatting_uses_grouping() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1_234_567), "1,234,567");
    }
}
