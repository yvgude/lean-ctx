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
    pub cache_hit_rate: f64,
    pub files_touched: u64,
}

impl WrappedReport {
    pub fn generate(period: &str) -> Self {
        let store = stats::load();
        let sessions = SessionState::list_sessions();

        let (tokens_saved, tokens_input, total_commands) = match period {
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

        let env_model = std::env::var("LEAN_CTX_MODEL")
            .or_else(|_| std::env::var("LCTX_MODEL"))
            .ok();
        let pricing = crate::core::gain::model_pricing::ModelPricing::load();
        let quote = pricing.quote(env_model.as_deref());
        let cost_avoided_usd = quote.cost.estimate_usd(tokens_saved, 0, 0, 0);

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

        let cache_hit_rate = if tokens_input > 0 {
            tokens_saved as f64 / tokens_input as f64 * 100.0
        } else {
            0.0
        };

        let files_touched: u64 = sessions.iter().map(|s| s.tool_calls as u64).sum();

        WrappedReport {
            period: period.to_string(),
            tokens_saved,
            tokens_input,
            cost_avoided_usd,
            total_commands,
            sessions_count,
            top_commands,
            cache_hit_rate,
            files_touched,
        }
    }

    pub fn format_ascii(&self) -> String {
        let period_label = match self.period.as_str() {
            "week" => format!("Week of {}", chrono::Utc::now().format("%B %d, %Y")),
            "month" => format!("Month of {}", chrono::Utc::now().format("%B %Y")),
            _ => "All Time".to_string(),
        };

        let saved_str = format_tokens(self.tokens_saved);
        let cost_str = format!("${:.2}", self.cost_avoided_usd);

        let top_str = if self.top_commands.is_empty() {
            "No data yet".to_string()
        } else {
            self.top_commands
                .iter()
                .take(3)
                .map(|(cmd, _, pct)| format!("{cmd} {pct:.0}%"))
                .collect::<Vec<_>>()
                .join(" | ")
        };

        let width = 48;
        let border = "\u{2500}".repeat(width);

        format!(
            "\n LeanCTX Wrapped \u{2014} {period_label}\n \
             {border}\n  \
             {saved_str} tokens saved      {cost_str} avoided\n  \
             {sessions} sessions            {cmds} commands\n  \
             Top: {top_str}\n  \
             Cache efficiency: {cache:.1}%\n \
             {border}\n  \
             \"Your AI saw only what mattered.\"\n  \
             leanctx.com\n",
            sessions = self.sessions_count,
            cmds = self.total_commands,
            cache = self.cache_hit_rate,
        )
    }

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

        format!(
            "WRAPPED [{}]: {} tok saved, {} avoided, {} sessions, {} cmds | Top: {} | Cache: {:.1}%",
            self.period, saved_str, cost_str, self.sessions_count,
            self.total_commands, top_str, self.cache_hit_rate,
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

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}
