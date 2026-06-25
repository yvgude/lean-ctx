use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `ContextRadar` aggregates all context sources into a single budget model.
/// Data flows in from: hooks (JSONL), proxy introspector, rules scanner, session cache.
pub struct ContextRadar {
    pub events: Vec<RadarEvent>,
    pub rules_tokens: RulesTokens,
    pub window_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadarEvent {
    pub ts: u64,
    pub event_type: String,
    pub tokens: usize,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RulesTokens {
    pub files: Vec<(String, usize)>,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct BudgetBreakdown {
    pub window_size: usize,
    pub system_prompt_tokens: usize,
    pub user_message_tokens: usize,
    pub agent_response_tokens: usize,
    pub lean_ctx_tool_tokens: usize,
    pub other_mcp_tokens: usize,
    pub native_read_tokens: usize,
    pub shell_tokens: usize,
    pub thinking_tokens: usize,
    pub tracked_total: usize,
    pub available: usize,
    pub compaction_count: usize,
    pub session_total_tokens: usize,
    pub session_user_tokens: usize,
    pub session_agent_tokens: usize,
    pub session_lctx_tokens: usize,
    pub session_mcp_tokens: usize,
    pub session_native_tokens: usize,
    pub session_shell_tokens: usize,
    pub session_thinking_tokens: usize,
    pub source: String,
}

impl ContextRadar {
    #[must_use]
    pub fn new(window_size: usize) -> Self {
        Self {
            events: Vec::new(),
            rules_tokens: RulesTokens::default(),
            window_size,
        }
    }

    #[must_use]
    pub fn load(data_dir: &Path, window_size: usize) -> Self {
        let mut radar = Self::new(window_size);
        radar.load_events(data_dir);
        radar.scan_rules();
        radar
    }

    fn load_events(&mut self, data_dir: &Path) {
        let mut all: Vec<RadarEvent> = Vec::new();

        let prev_path = data_dir.join("context_radar.prev.jsonl");
        if let Ok(content) = std::fs::read_to_string(&prev_path) {
            for line in content.lines() {
                if let Ok(ev) = serde_json::from_str::<RadarEvent>(line) {
                    all.push(ev);
                }
            }
        }

        let radar_path = data_dir.join("context_radar.jsonl");
        if let Ok(content) = std::fs::read_to_string(&radar_path) {
            for line in content.lines() {
                if let Ok(ev) = serde_json::from_str::<RadarEvent>(line) {
                    all.push(ev);
                }
            }
        }

        const MAX_EVENTS: usize = 50_000;
        if all.len() > MAX_EVENTS {
            self.events = all[all.len() - MAX_EVENTS..].to_vec();
        } else {
            self.events = all;
        }
    }

    pub fn scan_rules(&mut self) {
        let Some(home) = crate::core::home::resolve_home_dir() else {
            return;
        };

        let cwd = std::env::current_dir().unwrap_or_default();
        let mut files: Vec<(String, usize)> = Vec::new();

        let paths_to_scan: Vec<PathBuf> = vec![
            cwd.join(".cursorrules"),
            cwd.join("AGENTS.md"),
            cwd.join("CLAUDE.md"),
            cwd.join("CODEBUDDY.md"),
            cwd.join("LEAN-CTX.md"),
            home.join(".cursor").join("rules"),
            home.join(".cursorrules"),
            cwd.join(".cursor").join("rules"),
        ];

        for path in &paths_to_scan {
            if path.is_file() {
                if Self::is_rules_file(path)
                    && let Ok(content) = std::fs::read_to_string(path)
                {
                    let tokens = content.len() / 4;
                    if tokens > 0 {
                        files.push((path.display().to_string(), tokens));
                    }
                }
            } else if path.is_dir()
                && let Ok(entries) = std::fs::read_dir(path)
            {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file()
                        && Self::is_rules_file(&p)
                        && let Ok(content) = std::fs::read_to_string(&p)
                    {
                        let tokens = content.len() / 4;
                        if tokens > 0 {
                            files.push((p.display().to_string(), tokens));
                        }
                    }
                }
            }
        }

        let total = files.iter().map(|(_, t)| *t).sum();
        self.rules_tokens = RulesTokens { files, total };
    }

    fn is_rules_file(path: &Path) -> bool {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        if name.starts_with('.') && name != ".cursorrules" {
            return false;
        }
        if name.contains(".bak") || name.contains(".tmp") || name.contains(".swp") {
            return false;
        }
        if name == ".cursorrules"
            || name == "AGENTS.md"
            || name == "CLAUDE.md"
            || name == "CODEBUDDY.md"
            || name == "LEAN-CTX.md"
        {
            return true;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        matches!(ext, "md" | "mdc" | "markdown" | "txt")
    }

    #[must_use]
    pub fn budget_breakdown(&self) -> BudgetBreakdown {
        let mut compaction_count = 0;
        let mut last_compaction_idx: Option<usize> = None;

        for (i, event) in self.events.iter().enumerate() {
            if event.event_type == "compaction" {
                compaction_count += 1;
                last_compaction_idx = Some(i);
            }
        }

        let current_window_start = last_compaction_idx.map_or(0, |i| i + 1);
        let current_events = &self.events[current_window_start..];

        let (mut user_cur, mut agent_cur, mut lctx_cur, mut mcp_cur) = (0, 0, 0, 0);
        let (mut native_cur, mut shell_cur, mut thinking_cur) = (0, 0, 0);
        let (mut user_all, mut agent_all, mut lctx_all, mut mcp_all) = (0, 0, 0, 0);
        let (mut native_all, mut shell_all, mut thinking_all) = (0, 0, 0);

        for event in &self.events {
            Self::classify_event(
                event,
                &mut user_all,
                &mut agent_all,
                &mut lctx_all,
                &mut mcp_all,
                &mut native_all,
                &mut shell_all,
                &mut thinking_all,
            );
        }
        for event in current_events {
            Self::classify_event(
                event,
                &mut user_cur,
                &mut agent_cur,
                &mut lctx_cur,
                &mut mcp_cur,
                &mut native_cur,
                &mut shell_cur,
                &mut thinking_cur,
            );
        }

        let system_prompt_tokens = self.rules_tokens.total;
        let tracked_total = system_prompt_tokens
            + user_cur
            + agent_cur
            + lctx_cur
            + mcp_cur
            + native_cur
            + shell_cur;
        let available = self.window_size.saturating_sub(tracked_total);

        let session_total = system_prompt_tokens
            + user_all
            + agent_all
            + lctx_all
            + mcp_all
            + native_all
            + shell_all;

        BudgetBreakdown {
            window_size: self.window_size,
            system_prompt_tokens,
            user_message_tokens: user_cur,
            agent_response_tokens: agent_cur,
            lean_ctx_tool_tokens: lctx_cur,
            other_mcp_tokens: mcp_cur,
            native_read_tokens: native_cur,
            shell_tokens: shell_cur,
            thinking_tokens: thinking_cur,
            tracked_total,
            available,
            compaction_count,
            session_total_tokens: session_total,
            session_user_tokens: user_all,
            session_agent_tokens: agent_all,
            session_lctx_tokens: lctx_all,
            session_mcp_tokens: mcp_all,
            session_native_tokens: native_all,
            session_shell_tokens: shell_all,
            session_thinking_tokens: thinking_all,
            source: "hooks + rules-scan".to_string(),
        }
    }

    fn classify_event(
        event: &RadarEvent,
        user: &mut usize,
        agent: &mut usize,
        lctx: &mut usize,
        mcp: &mut usize,
        native: &mut usize,
        shell: &mut usize,
        thinking: &mut usize,
    ) {
        match event.event_type.as_str() {
            "user_message" => *user += event.tokens,
            "agent_response" => *agent += event.tokens,
            "mcp_call" => {
                let is_leanctx = event
                    .detail
                    .as_deref()
                    .is_some_and(|d| d.contains("lean-ctx"))
                    || event
                        .tool_name
                        .as_deref()
                        .is_some_and(|t| t.starts_with("ctx_"));
                if is_leanctx {
                    *lctx += event.tokens;
                } else {
                    *mcp += event.tokens;
                }
            }
            "native_tool" | "file_read" => *native += event.tokens,
            "shell" => *shell += event.tokens,
            "thinking" => *thinking += event.tokens,
            _ => {}
        }
    }

    #[must_use]
    pub fn format_display(&self) -> String {
        let b = self.budget_breakdown();
        let pct = |tokens: usize| -> f64 {
            if b.window_size == 0 {
                0.0
            } else {
                (tokens as f64 / b.window_size as f64 * 100.0).min(100.0)
            }
        };
        let bar = |tokens: usize| -> String {
            let width = (pct(tokens) / 2.0).min(40.0) as usize;
            "█".repeat(width)
        };

        let mut out = String::new();
        out.push_str(&format!(
            "CONTEXT RADAR — Current Window ({:.0}k)\n",
            b.window_size as f64 / 1000.0
        ));
        if b.compaction_count > 0 {
            out.push_str(&format!(
                "  (after {} compaction(s) — showing current window only)\n",
                b.compaction_count
            ));
        }
        out.push_str(&format!(
            "  System Prompt (est.): {:>8} tok {:>5.1}%  {}\n",
            fmt_num(b.system_prompt_tokens),
            pct(b.system_prompt_tokens),
            bar(b.system_prompt_tokens)
        ));
        out.push_str(&format!(
            "  User Messages:        {:>8} tok {:>5.1}%  {}\n",
            fmt_num(b.user_message_tokens),
            pct(b.user_message_tokens),
            bar(b.user_message_tokens)
        ));
        out.push_str(&format!(
            "  Agent Responses:      {:>8} tok {:>5.1}%  {}\n",
            fmt_num(b.agent_response_tokens),
            pct(b.agent_response_tokens),
            bar(b.agent_response_tokens)
        ));
        out.push_str(&format!(
            "  lean-ctx Tools:       {:>8} tok {:>5.1}%  {}\n",
            fmt_num(b.lean_ctx_tool_tokens),
            pct(b.lean_ctx_tool_tokens),
            bar(b.lean_ctx_tool_tokens)
        ));
        out.push_str(&format!(
            "  Other MCP:            {:>8} tok {:>5.1}%  {}\n",
            fmt_num(b.other_mcp_tokens),
            pct(b.other_mcp_tokens),
            bar(b.other_mcp_tokens)
        ));
        out.push_str(&format!(
            "  Native Reads:         {:>8} tok {:>5.1}%  {}\n",
            fmt_num(b.native_read_tokens),
            pct(b.native_read_tokens),
            bar(b.native_read_tokens)
        ));
        out.push_str(&format!(
            "  Shell Output:         {:>8} tok {:>5.1}%  {}\n",
            fmt_num(b.shell_tokens),
            pct(b.shell_tokens),
            bar(b.shell_tokens)
        ));
        out.push_str("  ──────────────────────────────────────────\n");
        out.push_str(&format!(
            "  TRACKED:              {:>8} tok {:>5.1}%\n",
            fmt_num(b.tracked_total),
            pct(b.tracked_total)
        ));
        out.push_str(&format!(
            "  Available:            {:>8} tok {:>5.1}%\n",
            fmt_num(b.available),
            pct(b.available)
        ));
        if b.thinking_tokens > 0 {
            out.push_str(&format!(
                "  Thinking (not in window): {:>5} tok\n",
                fmt_num(b.thinking_tokens)
            ));
        }
        if b.session_total_tokens > b.tracked_total {
            out.push_str(&format!(
                "\n  SESSION TOTAL:        {:>8} tok (across {} compaction(s))\n",
                fmt_num(b.session_total_tokens),
                b.compaction_count
            ));
        }
        out.push_str(&format!("  Source: {}\n", b.source));
        out
    }
}

fn fmt_num(n: usize) -> String {
    if n >= 1000 {
        format!("{},{:03}", n / 1000, n % 1000)
    } else {
        n.to_string()
    }
}

/// Default context window size based on client name.
#[must_use]
pub fn default_window_for_client(client: &str) -> usize {
    if let Some((_model, window)) = crate::hook_handlers::load_detected_model() {
        return window;
    }
    match client.to_lowercase().as_str() {
        "gemini" => 1_000_000,
        "windsurf" | "zed" | "copilot" => 128_000,
        _ => 200_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_breakdown_empty() {
        let radar = ContextRadar::new(200_000);
        let b = radar.budget_breakdown();
        assert_eq!(b.window_size, 200_000);
        assert_eq!(b.tracked_total, 0);
        assert_eq!(b.available, 200_000);
    }

    fn ev(
        ts: u64,
        event_type: &str,
        tokens: usize,
        tool_name: Option<&str>,
        detail: Option<&str>,
    ) -> RadarEvent {
        RadarEvent {
            ts,
            event_type: event_type.to_string(),
            tokens,
            tool_name: tool_name.map(String::from),
            detail: detail.map(String::from),
            content: None,
            model: None,
            conversation_id: None,
        }
    }

    #[test]
    fn budget_breakdown_with_events() {
        let mut radar = ContextRadar::new(200_000);
        radar.events.push(ev(1000, "user_message", 500, None, None));
        radar
            .events
            .push(ev(1001, "agent_response", 2000, None, None));
        radar
            .events
            .push(ev(1002, "shell", 300, None, Some("git status")));
        let b = radar.budget_breakdown();
        assert_eq!(b.user_message_tokens, 500);
        assert_eq!(b.agent_response_tokens, 2000);
        assert_eq!(b.shell_tokens, 300);
        assert_eq!(b.tracked_total, 2800);
        assert_eq!(b.available, 200_000 - 2800);
    }

    #[test]
    fn budget_breakdown_resets_after_compaction() {
        let mut radar = ContextRadar::new(100_000);
        radar.events.push(ev(1, "user_message", 50_000, None, None));
        radar.events.push(ev(2, "compaction", 0, None, None));
        radar.events.push(ev(3, "user_message", 10_000, None, None));
        let b = radar.budget_breakdown();
        assert_eq!(
            b.user_message_tokens, 10_000,
            "only counts since compaction"
        );
        assert_eq!(b.available, 90_000);
        assert_eq!(b.compaction_count, 1);
        assert_eq!(b.session_user_tokens, 60_000, "session total includes all");
    }

    #[test]
    fn format_display_not_empty() {
        let radar = ContextRadar::new(200_000);
        let display = radar.format_display();
        assert!(display.contains("CONTEXT RADAR"));
        assert!(display.contains("200k"));
    }

    #[test]
    fn default_window_sizes() {
        // If a detected model file exists on the system, default_window_for_client
        // returns that model's window for all clients. Skip client-specific asserts
        // in that case and only verify the function returns a reasonable value.
        if crate::hook_handlers::load_detected_model().is_some() {
            let w = default_window_for_client("cursor");
            assert!(
                (128_000..=2_000_000).contains(&w),
                "window {w} out of range"
            );
        } else {
            assert_eq!(default_window_for_client("cursor"), 200_000);
            assert_eq!(default_window_for_client("gemini"), 1_000_000);
            assert_eq!(default_window_for_client("windsurf"), 128_000);
        }
    }
}
