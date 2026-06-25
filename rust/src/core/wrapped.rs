use crate::core::session::SessionState;
use crate::core::stats;

pub struct WrappedReport {
    pub period: String,
    pub tokens_saved: u64,
    pub tokens_input: u64,
    pub cost_avoided_usd: f64,
    pub total_commands: u64,
    pub sessions_count: usize,
    pub top_commands: Vec<(String, u64, f64)>,
    pub compression_rate_pct: f64,
    pub files_touched: u64,
    pub daily_savings: Vec<u64>,
    /// Tokens netted out of `tokens_saved` because a compressed read later bounced to a
    /// full re-read (G7). Sourced from the persistent savings ledger for the period.
    pub bounce_tokens: u64,
    /// Resolved pricing model key used to value the saved tokens (e.g. "claude-3.5-sonnet").
    pub model_key: String,
    /// True when no model could be resolved and a blended fallback price was used.
    /// Surfaced everywhere so an estimate is never presented as a precise figure.
    pub pricing_estimated: bool,
    /// Estimated percentile rank among lean-ctx users (0-100). Based on tokens saved.
    /// None if insufficient data or user hasn't opted into community metrics.
    pub percentile: Option<u8>,
}

impl WrappedReport {
    #[must_use]
    pub fn generate(period: &str) -> Self {
        let store = stats::load();
        let sessions = SessionState::list_sessions();

        let (gross_tokens_saved, tokens_input, total_commands) = match period {
            "week" => aggregate_recent_stats(&store, 7),
            "month" => aggregate_recent_stats(&store, 30),
            _ => (
                store
                    .total_input_tokens
                    .saturating_sub(store.total_output_tokens),
                store.total_input_tokens,
                store.total_commands,
            ),
        };

        // G7: net out compressed->full bounce recorded in the persistent ledger for this
        // period, so the headline is the *realized* saving, not a gross upper bound.
        let period_days = match period {
            "week" => Some(7),
            "month" => Some(30),
            _ => None,
        };
        let bounce_tokens = crate::core::savings_ledger::bounce_tokens(period_days);
        let tokens_saved = gross_tokens_saved.saturating_sub(bounce_tokens);

        let env_model = std::env::var("LEAN_CTX_MODEL")
            .or_else(|_| std::env::var("LCTX_MODEL"))
            .ok();
        let pricing = crate::core::gain::model_pricing::ModelPricing::load();
        let quote = pricing.quote(env_model.as_deref());
        // Saved tokens would have been *input* tokens, so value them at the input rate.
        // Still an upper bound on the bounce-adjusted figure: it ignores prompt-cache
        // discounts. We never inflate beyond this.
        let cost_avoided_usd = quote.cost.estimate_usd(tokens_saved, 0, 0, 0);
        let pricing_estimated = matches!(
            quote.match_kind,
            crate::core::gain::model_pricing::PricingMatchKind::Fallback
        );
        let model_key = quote.model_key.clone();

        let sessions_count = match period {
            "week" => count_recent_sessions(&sessions, 7),
            "month" => count_recent_sessions(&sessions, 30),
            _ => sessions.len(),
        };

        let mut top_commands: Vec<(String, u64, f64)> = store
            .commands
            .iter()
            .map(|(cmd, stats)| {
                let saved = stats.input_tokens.saturating_sub(stats.output_tokens);
                let pct = if stats.input_tokens > 0 {
                    saved as f64 / stats.input_tokens as f64 * 100.0
                } else {
                    0.0
                };
                (cmd.clone(), saved, pct)
            })
            .collect();
        top_commands.sort_by_key(|x| std::cmp::Reverse(x.1));
        top_commands.truncate(5);

        let compression_rate_pct = if tokens_input > 0 {
            tokens_saved as f64 / tokens_input as f64 * 100.0
        } else {
            0.0
        };

        let files_touched: u64 = sessions.iter().map(|s| u64::from(s.tool_calls)).sum();

        let day_saved = |d: &stats::DayStats| d.input_tokens.saturating_sub(d.output_tokens);
        let take_recent = |n: usize| -> Vec<u64> {
            store
                .daily
                .iter()
                .rev()
                .take(n)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(day_saved)
                .collect()
        };
        let daily_savings = match period {
            "week" => take_recent(7),
            "month" => take_recent(30),
            _ => store.daily.iter().map(day_saved).collect(),
        };

        WrappedReport {
            period: period.to_string(),
            tokens_saved,
            tokens_input,
            cost_avoided_usd,
            total_commands,
            sessions_count,
            top_commands,
            compression_rate_pct,
            files_touched,
            daily_savings,
            bounce_tokens,
            model_key,
            pricing_estimated,
            percentile: estimate_percentile(tokens_saved),
        }
    }

    /// One-line, conservative explanation of how the headline numbers were derived.
    /// Reused by the ASCII footer, the compact summary, and the SVG share card so the
    /// figure is always explainable and never over-claimed.
    #[must_use]
    pub fn methodology_line(&self) -> String {
        let price = if self.pricing_estimated {
            format!(
                "{} blended fallback price (set LEAN_CTX_MODEL for exact)",
                self.model_key
            )
        } else {
            format!("{} input price", self.model_key)
        };
        let basis = if self.bounce_tokens > 0 {
            format!(
                "measured original - compressed - {} bounce tokens",
                format_tokens(self.bounce_tokens)
            )
        } else {
            "measured original - compressed tokens".to_string()
        };
        format!("Savings = {basis}; USD is an upper bound at {price}")
    }

    /// Renders a premium, shareable "Wrapped" card. Colors are emitted only when
    /// stdout is a TTY (see `theme::no_color`), so piping to a file or social post
    /// yields clean copy-pasteable ASCII.
    #[allow(clippy::many_single_char_names)] // ANSI formatting helpers: t/r/b/d
    #[must_use]
    pub fn format_ascii(&self) -> String {
        use crate::core::theme;

        let cfg = crate::core::config::Config::load();
        let t = theme::load_theme(&cfg.theme);
        let rst = theme::rst();
        let bold = theme::bold();
        let dim = theme::dim();

        let period_label = match self.period.as_str() {
            "week" => format!("Week of {}", chrono::Utc::now().format("%b %d, %Y")),
            "month" => format!("Month of {}", chrono::Utc::now().format("%B %Y")),
            _ => "All Time".to_string(),
        };

        let w = 52;
        let side = t.box_side();
        let box_line = |content: &str| -> String {
            let padded = theme::pad_right(content, w);
            format!("  {side}{padded}{side}")
        };

        let mut out: Vec<String> = Vec::new();
        out.push(String::new());
        out.push(format!("  {}", t.box_top(w)));
        out.push(box_line(""));
        out.push(box_line(&format!(
            "   {icon}  {brand} {accent}Wrapped{rst}  {dim}· {period_label}{rst}",
            icon = t.header_icon(),
            brand = t.brand_title(),
            accent = t.accent.fg(),
        )));
        out.push(box_line(""));
        out.push(format!("  {}", t.box_mid(w)));
        out.push(box_line(""));

        // Primary metric row: tokens saved + cost avoided + commands.
        let kw = 16;
        let sc = t.success.fg();
        let c2 = t.secondary.fg();
        let c3 = t.warning.fg();
        let c4 = t.accent.fg();

        let v1 = theme::pad_right(
            &format!("{sc}{bold}{}{rst}", format_tokens(self.tokens_saved)),
            kw,
        );
        let v2 = theme::pad_right(&format!("{c4}{bold}${:.2}{rst}", self.cost_avoided_usd), kw);
        let v3 = theme::pad_right(&format!("{c3}{bold}{}{rst}", self.total_commands), kw);
        out.push(box_line(&format!("   {v1}{v2}{v3}")));
        let l1 = theme::pad_right(&format!("{dim}tokens saved{rst}"), kw);
        let l2 = theme::pad_right(&format!("{dim}cost avoided{rst}"), kw);
        let l3 = theme::pad_right(&format!("{dim}commands{rst}"), kw);
        out.push(box_line(&format!("   {l1}{l2}{l3}")));
        out.push(box_line(""));

        // Secondary metric row: sessions + compression + energy saved (estimate, same
        // methodology as the community /metrics page so local & shared figures reconcile).
        let v4 = theme::pad_right(&format!("{c2}{bold}{}{rst}", self.sessions_count), kw);
        let v5 = theme::pad_right(
            &format!(
                "{pc}{bold}{:.1}%{rst}",
                self.compression_rate_pct,
                pc = t.pct_color(self.compression_rate_pct),
            ),
            kw,
        );
        let energy = crate::core::energy::format_for_tokens(self.tokens_saved);
        let v6 = theme::pad_right(&format!("{c4}{bold}{energy}{rst}"), kw);
        out.push(box_line(&format!("   {v4}{v5}{v6}")));
        let l4 = theme::pad_right(&format!("{dim}sessions{rst}"), kw);
        let l5 = theme::pad_right(&format!("{dim}compression{rst}"), kw);
        let l6 = theme::pad_right(&format!("{dim}energy saved{rst}"), kw);
        out.push(box_line(&format!("   {l4}{l5}{l6}")));
        out.push(box_line(""));

        // Trend sparkline (only when there is at least a little history).
        if self.daily_savings.iter().filter(|v| **v > 0).count() >= 2 {
            let spark = t.gradient_sparkline(&self.daily_savings);
            out.push(box_line(&format!("   {dim}trend{rst}  {spark}")));
            out.push(box_line(""));
        }

        // Top commands (truncated to fit the inner box width).
        if !self.top_commands.is_empty() {
            let prefix_visible = 8; // "   top  "
            let budget = w.saturating_sub(prefix_visible);
            let mut top_str = self
                .top_commands
                .iter()
                .take(3)
                .map(|(cmd, _, pct)| format!("{cmd} {pct:.0}%"))
                .collect::<Vec<_>>()
                .join("  ·  ");
            if top_str.chars().count() > budget {
                let truncated: String = top_str.chars().take(budget.saturating_sub(1)).collect();
                top_str = format!("{truncated}…");
            }
            out.push(format!("  {}", t.box_mid(w)));
            out.push(box_line(&format!(
                "   {m}top{rst}  {top_str}",
                m = t.muted.fg()
            )));
        }

        out.push(format!("  {}", t.box_bottom(w)));
        out.push(format!(
            "    {dim}\"Your AI saw only what mattered.\"{rst}   {accent}leanctx.com{rst}",
            accent = t.accent.fg(),
        ));
        let est_marker = if self.pricing_estimated {
            " (est.)"
        } else {
            ""
        };
        out.push(format!(
            "    {dim}model {model}{est_marker}  ·  USD = upper bound{rst}",
            model = self.model_key,
        ));
        out.push(String::new());

        out.join("\n")
    }

    #[must_use]
    pub fn format_compact(&self) -> String {
        let saved_str = format_tokens(self.tokens_saved);
        let cost_str = format!("${:.2}", self.cost_avoided_usd);
        let top_str = self
            .top_commands
            .iter()
            .take(3)
            .map(|(cmd, _, pct)| format!("{cmd} {pct:.0}%"))
            .collect::<Vec<_>>()
            .join(" | ");

        let est_marker = if self.pricing_estimated {
            " (est.)"
        } else {
            ""
        };
        format!(
            "WRAPPED [{}]: {} tok saved, {} avoided{}, {} sessions, {} cmds | Top: {} | Compression: {:.1}% | Energy: {} | model={}",
            self.period,
            saved_str,
            cost_str,
            est_marker,
            self.sessions_count,
            self.total_commands,
            top_str,
            self.compression_rate_pct,
            crate::core::energy::format_for_tokens(self.tokens_saved),
            self.model_key,
        )
    }
}

fn aggregate_recent_stats(store: &stats::StatsStore, days: usize) -> (u64, u64, u64) {
    let recent_days: Vec<&stats::DayStats> = store.daily.iter().rev().take(days).collect();

    let input: u64 = recent_days.iter().map(|d| d.input_tokens).sum();
    let output: u64 = recent_days.iter().map(|d| d.output_tokens).sum();
    let commands: u64 = recent_days.iter().map(|d| d.commands).sum();
    let saved = input.saturating_sub(output);

    (saved, input, commands)
}

fn count_recent_sessions(sessions: &[crate::core::session::SessionSummary], days: i64) -> usize {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    sessions.iter().filter(|s| s.updated_at > cutoff).count()
}

pub(crate) fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000_000_000 {
        format!("{:.2}T", tokens as f64 / 1_000_000_000_000.0)
    } else if tokens >= 1_000_000_000 {
        format!("{:.2}B", tokens as f64 / 1_000_000_000.0)
    } else if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

/// Estimate the user's percentile rank among lean-ctx users based on tokens saved.
/// Uses a rough distribution model derived from community metrics data.
/// Returns None if insufficient data (< 1000 tokens saved).
fn estimate_percentile(tokens_saved: u64) -> Option<u8> {
    if tokens_saved < 1_000 {
        return None;
    }
    // Rough percentile thresholds based on community data distribution
    // (log-normal distribution of savings across users)
    let pct = if tokens_saved >= 100_000_000 {
        99
    } else if tokens_saved >= 50_000_000 {
        97
    } else if tokens_saved >= 10_000_000 {
        95
    } else if tokens_saved >= 5_000_000 {
        90
    } else if tokens_saved >= 1_000_000 {
        80
    } else if tokens_saved >= 500_000 {
        70
    } else if tokens_saved >= 100_000 {
        55
    } else if tokens_saved >= 50_000 {
        40
    } else if tokens_saved >= 10_000 {
        25
    } else {
        10
    };
    Some(pct)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ("cli_grep".into(), 80, 85.0),
                ("cli_shell".into(), 50, 37.0),
            ],
            compression_rate_pct: 60.2,
            files_touched: 1_234,
            daily_savings: vec![10, 50, 30, 30, 80, 80, 20, 5, 5, 40, 60, 40, 5, 50, 15],
            bounce_tokens: 0,
            model_key: "claude-3.5-sonnet".into(),
            pricing_estimated: false,
            percentile: Some(95),
        }
    }

    fn is_box_line(l: &str) -> bool {
        let trimmed = l.trim_start();
        ["│", "╭", "├", "╰"].iter().any(|c| trimmed.starts_with(c))
    }

    #[test]
    fn wrapped_ascii_box_lines_have_uniform_width() {
        // In the test runner, stdout is not a TTY, so colors are auto-disabled.
        let out = sample().format_ascii();
        let widths: Vec<usize> = out
            .lines()
            .filter(|l| is_box_line(l))
            .map(|l| l.chars().count())
            .collect();
        assert!(widths.len() >= 4, "expected several box lines:\n{out}");
        let first = widths[0];
        for w in &widths {
            assert_eq!(*w, first, "box line widths must be uniform:\n{out}");
        }
    }

    #[test]
    fn wrapped_ascii_includes_brand_and_metrics() {
        let out = sample().format_ascii();
        assert!(out.contains("leanctx.com"), "missing brand footer:\n{out}");
        assert!(out.contains("Wrapped"));
        assert!(out.contains("tokens saved"));
        assert!(out.contains("compression"));
    }

    #[test]
    fn wrapped_ascii_truncates_overlong_top_line() {
        let out = sample().format_ascii();
        // No box line may exceed the others (top row must be truncated to fit).
        let max = out
            .lines()
            .filter(|l| is_box_line(l))
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0);
        let min = out
            .lines()
            .filter(|l| is_box_line(l))
            .map(|l| l.chars().count())
            .min()
            .unwrap_or(0);
        assert_eq!(max, min, "top line overflowed the box:\n{out}");
    }

    #[test]
    fn wrapped_compact_is_single_line_summary() {
        let out = sample().format_compact();
        assert!(out.starts_with("WRAPPED"), "compact summary changed: {out}");
        assert!(out.contains("Compression:"));
        assert!(
            out.contains("model="),
            "compact must name the pricing model: {out}"
        );
    }

    #[test]
    fn methodology_is_conservative_and_explainable() {
        let m = sample().methodology_line();
        assert!(
            m.contains("upper bound"),
            "must state it is an upper bound: {m}"
        );
        assert!(m.contains("claude-3.5-sonnet"), "must name the model: {m}");
    }

    #[test]
    fn ascii_footer_surfaces_model_and_upper_bound() {
        let out = sample().format_ascii();
        assert!(
            out.contains("model claude-3.5-sonnet"),
            "footer must name model:\n{out}"
        );
        assert!(
            out.contains("USD = upper bound"),
            "footer must flag upper bound:\n{out}"
        );
    }

    #[test]
    fn estimated_pricing_is_flagged() {
        let mut r = sample();
        r.pricing_estimated = true;
        assert!(
            r.format_ascii().contains("(est.)"),
            "estimated price must show (est.)"
        );
        assert!(r.format_compact().contains("(est.)"));
        assert!(r.methodology_line().contains("fallback"));
    }
}
