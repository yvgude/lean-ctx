//! Shareable SVG "Wrapped" card.
//!
//! Renders [`WrappedReport`] as a self-contained 1200x630 SVG (the social/OG image
//! size). It is pure string building — no external crates, fonts, or assets — so the
//! output is portable, diff-friendly, and can be posted directly or rasterised to PNG
//! by any standard SVG tool. All data-derived text is XML-escaped.

use crate::core::wrapped::{WrappedReport, format_tokens};

/// Social/OG card dimensions.
const CARD_W: u32 = 1200;
const CARD_H: u32 = 630;

impl WrappedReport {
    /// Renders a polished, dependency-free SVG share card.
    #[must_use]
    pub fn to_svg(&self) -> String {
        let period_label = match self.period.as_str() {
            "week" => format!("Week of {}", chrono::Utc::now().format("%b %d, %Y")),
            "month" => format!("Month of {}", chrono::Utc::now().format("%B %Y")),
            _ => "All Time".to_string(),
        };

        let saved = format_tokens(self.tokens_saved);
        let cost = format!("${:.2}", self.cost_avoided_usd);
        let est = if self.pricing_estimated {
            " (est.)"
        } else {
            ""
        };
        let secondary = self.svg_secondary_metrics();
        // Model line is only meaningful when a model was shared (older cards). Minimal cards omit it.
        let model_line = if self.model_key.is_empty() {
            String::new()
        } else {
            format!(
                r##"  <text x="70" y="606" fill="#475569" font-size="17">priced at {}{}</text>"##,
                escape(&self.model_key),
                est
            )
        };

        let spark = self.svg_sparkline();
        let top = self.svg_top_commands();
        let bounce_note = if self.bounce_tokens > 0 {
            format!(
                " - {} bounce",
                crate::core::wrapped::format_tokens(self.bounce_tokens)
            )
        } else {
            String::new()
        };

        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{CARD_W}" height="{CARD_H}" viewBox="0 0 {CARD_W} {CARD_H}" font-family="Inter, system-ui, -apple-system, Segoe UI, Roboto, sans-serif">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#0b1020"/>
      <stop offset="1" stop-color="#131a2e"/>
    </linearGradient>
    <linearGradient id="accent" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0" stop-color="#34d399"/>
      <stop offset="1" stop-color="#22d3ee"/>
    </linearGradient>
  </defs>
  <rect width="{CARD_W}" height="{CARD_H}" fill="url(#bg)"/>
  <rect x="0" y="0" width="{CARD_W}" height="8" fill="url(#accent)"/>

  <text x="70" y="92" fill="#e5e7eb" font-size="34" font-weight="700">lean-ctx <tspan fill="#34d399">Wrapped</tspan></text>
  <text x="70" y="130" fill="#94a3b8" font-size="24">{period}</text>

  <text x="70" y="300" fill="#34d399" font-size="138" font-weight="800" font-family="ui-monospace, SFMono-Regular, Menlo, monospace">{saved}</text>
  <text x="76" y="346" fill="#94a3b8" font-size="26">tokens saved</text>

  <text x="730" y="252" fill="#22d3ee" font-size="84" font-weight="800" font-family="ui-monospace, SFMono-Regular, Menlo, monospace">{cost}</text>
  <text x="734" y="292" fill="#94a3b8" font-size="24">cost avoided{est}</text>

{secondary}
{spark}
{top}
  <text x="70" y="582" fill="#64748b" font-size="19">Savings = measured original - compressed{bounce_note} tokens · USD = upper bound</text>
{model_line}
  <text x="1130" y="592" text-anchor="end" fill="#34d399" font-size="26" font-weight="700">leanctx.com</text>
</svg>"##,
            period = escape(&period_label),
        )
    }

    /// The secondary metric row: compression + energy always; commands/sessions only when present
    /// (older or local cards). Energy is derived from tokens — the same J/token basis as the
    /// community metrics page — so showing it shares no extra data. Laid out left-to-right so a
    /// minimal card (just compression + energy) looks balanced and a rich one fills the row.
    fn svg_secondary_metrics(&self) -> String {
        let mut items: Vec<(String, &str)> = vec![
            (format!("{:.1}%", self.compression_rate_pct), "compression"),
            (
                crate::core::energy::format_for_tokens(self.tokens_saved),
                "energy saved",
            ),
        ];
        if self.total_commands > 0 {
            items.push((self.total_commands.to_string(), "commands"));
        }
        if self.sessions_count > 0 {
            items.push((self.sessions_count.to_string(), "sessions"));
        }
        let xs = [70, 360, 650, 940];
        let mut out = String::new();
        for (i, (val, label)) in items.iter().take(xs.len()).enumerate() {
            let x = xs[i];
            out.push_str(&format!(
                "  <text x=\"{x}\" y=\"412\" fill=\"#e5e7eb\" font-size=\"44\" font-weight=\"700\" font-family=\"ui-monospace, SFMono-Regular, Menlo, monospace\">{val}</text>\n  <text x=\"{lx}\" y=\"442\" fill=\"#94a3b8\" font-size=\"22\">{label}</text>",
                lx = x + 2,
                val = escape(val),
            ));
            if i + 1 < items.len() {
                out.push('\n');
            }
        }
        out
    }

    /// A subtle accent-gradient sparkline of daily savings. Empty when there is not
    /// enough history to be meaningful (fewer than two non-zero days).
    fn svg_sparkline(&self) -> String {
        let vals = &self.daily_savings;
        if vals.iter().filter(|v| **v > 0).count() < 2 {
            return String::new();
        }
        let max = (*vals.iter().max().unwrap_or(&1)).max(1) as f64;
        let (x0, x1) = (70.0_f64, 1130.0_f64);
        let baseline = 515.0_f64;
        let height = 55.0_f64;
        let n = vals.len().max(2);
        let dx = (x1 - x0) / (n as f64 - 1.0);
        let mut points = String::new();
        for (i, v) in vals.iter().enumerate() {
            let x = x0 + dx * i as f64;
            let y = baseline - (*v as f64 / max) * height;
            points.push_str(&format!("{x:.1},{y:.1} "));
        }
        format!(
            "  <polyline fill=\"none\" stroke=\"url(#accent)\" stroke-width=\"3\" stroke-linejoin=\"round\" stroke-linecap=\"round\" points=\"{}\"/>",
            points.trim()
        )
    }

    /// The top three commands as a single muted line. Empty when no command data.
    fn svg_top_commands(&self) -> String {
        if self.top_commands.is_empty() {
            return String::new();
        }
        let joined = self
            .top_commands
            .iter()
            .take(3)
            .map(|(cmd, _, pct)| format!("{cmd} {pct:.0}%"))
            .collect::<Vec<_>>()
            .join("    ·    ");
        format!(
            "  <text x=\"70\" y=\"548\" fill=\"#cbd5e1\" font-size=\"22\">top  {}</text>",
            escape(&joined)
        )
    }
}

/// Minimal XML text escaping for data-derived strings.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use crate::core::wrapped::WrappedReport;

    fn sample() -> WrappedReport {
        WrappedReport {
            period: "all".into(),
            tokens_saved: 348_300_000,
            tokens_input: 580_000_000,
            cost_avoided_usd: 870.81,
            total_commands: 17_055,
            sessions_count: 67,
            top_commands: vec![
                ("ctx_search".into(), 100, 60.0),
                ("cli_grep <x>".into(), 80, 85.0),
                ("cli_shell".into(), 50, 37.0),
            ],
            compression_rate_pct: 60.2,
            files_touched: 1_234,
            daily_savings: vec![10, 50, 30, 30, 80, 80, 20, 5, 5, 40, 60, 40, 5, 50, 15],
            bounce_tokens: 0,
            model_key: "claude-3.5-sonnet".into(),
            pricing_estimated: false,
            percentile: Some(99),
        }
    }

    #[test]
    fn svg_is_well_formed_and_branded() {
        let svg = sample().to_svg();
        assert!(svg.starts_with("<svg"), "must be an SVG document");
        assert!(svg.trim_end().ends_with("</svg>"), "must close the svg tag");
        assert!(svg.contains("leanctx.com"), "must carry the brand footer");
        assert!(svg.contains("Wrapped"));
        assert!(svg.contains("tokens saved"));
        // Headline metric value is rendered.
        assert!(svg.contains("348.3M"), "must render formatted tokens saved");
    }

    #[test]
    fn svg_states_methodology_and_model() {
        let svg = sample().to_svg();
        assert!(
            svg.contains("upper bound"),
            "must state USD is an upper bound"
        );
        assert!(
            svg.contains("claude-3.5-sonnet"),
            "must name the pricing model"
        );
    }

    #[test]
    fn svg_escapes_command_names() {
        let svg = sample().to_svg();
        // The command "cli_grep <x>" must not leak a raw '<x>' that would break XML.
        assert!(
            svg.contains("cli_grep &lt;x&gt;"),
            "command names must be escaped"
        );
    }

    #[test]
    fn svg_minimal_card_shows_energy_and_omits_empty_fields() {
        // A card published by a current (minimal) client carries no command/session counts or
        // model — the card must still render cleanly: energy + compression, nothing zeroed.
        let mut r = sample();
        r.total_commands = 0;
        r.sessions_count = 0;
        r.model_key = String::new();
        r.top_commands = vec![];
        let svg = r.to_svg();
        assert!(
            svg.contains(">energy saved<"),
            "energy is always shown:\n{svg}"
        );
        assert!(svg.contains(">compression<"), "compression is always shown");
        assert!(!svg.contains(">commands<"), "no commands label when zero");
        assert!(!svg.contains(">sessions<"), "no sessions label when zero");
        assert!(!svg.contains("priced at"), "no model line when model empty");
    }

    #[test]
    fn svg_omits_sparkline_without_history() {
        let mut r = sample();
        r.daily_savings = vec![0];
        let svg = r.to_svg();
        assert!(
            !svg.contains("<polyline"),
            "no sparkline without enough history"
        );
        // Card still renders the rest.
        assert!(svg.contains("</svg>"));
    }
}
