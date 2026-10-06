// SPDX-License-Identifier: Apache-2.0
//! Where the gateway's human-facing summary goes (acceptance case 56).
//!
//! A redaction count is for the human: the `[REDACTED:…]` markers already tell
//! the model what happened. When the host shows lean-ctx out of band (the
//! Claude Code status line, fed from receipts), that count stays out of the
//! model's context. Notices that change how the model must treat content —
//! untrusted, not fully inspected, withheld — are always delivered in band.

use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// `[context_gateway] hud`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HudPlacement {
    /// Out of band when the host shows lean-ctx's status line, else in band.
    #[default]
    Auto,
    /// Always append the summary to the tool output.
    InBand,
    /// Never append redaction counts; rely on the status line and `inspect`.
    StatusLine,
}

impl HudPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::InBand => "in_band",
            Self::StatusLine => "status_line",
        }
    }

    /// Whether pure redaction counts belong in the model-facing output.
    pub(super) fn redaction_counts_in_band(self) -> bool {
        match self {
            Self::InBand => true,
            Self::StatusLine => false,
            Self::Auto => !claude_status_line_active(),
        }
    }
}

/// The compact HUD metric of one round, derived from its receipt only
/// (acceptance cases 52 and 55): `LeanCTX 🛡 31.2k → 6.8k ↓78%`, then what the
/// gateway did. `None` when the round delivered nothing.
#[must_use]
pub fn compact_metric(
    receipt: &lean_ctx_protocol::context_gateway::ContextDecisionReceiptV1,
) -> Option<String> {
    let tokens = receipt.tokens;
    if tokens.delivered == 0 && tokens.original == 0 {
        return None;
    }
    let mut line = format!(
        "LeanCTX 🛡  {} → {}",
        kilo(tokens.original),
        kilo(tokens.delivered)
    );
    if tokens.original > tokens.delivered && tokens.original > 0 {
        let saved = (tokens.original - tokens.delivered) * 100 / tokens.original;
        line.push_str(&format!("  ↓{saved}%"));
    }
    let mut parts = Vec::new();
    let security = receipt.security;
    if security.redactions > 0 {
        parts.push(format!("{} redacted", security.redactions));
    }
    if security.blocked_objects > 0 {
        parts.push(format!("{} withheld", security.blocked_objects));
    }
    if security.injection_signals > 0 {
        parts.push(format!(
            "{} injection signal(s)",
            security.injection_signals
        ));
    }
    let sources = receipt.sources;
    if sources.inspected > 1 {
        parts.push(format!(
            "{}/{} sources used",
            sources.selected, sources.inspected
        ));
    }
    if !parts.is_empty() {
        line.push_str(" · ");
        line.push_str(&parts.join(" · "));
    }
    Some(line)
}

fn kilo(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

const REFRESH: Duration = Duration::from_mins(1);

/// This process serves Claude Code and lean-ctx's status line is installed.
fn claude_status_line_active() -> bool {
    if std::env::var_os("CLAUDECODE").is_none() {
        return false;
    }
    static CACHE: OnceLock<Mutex<Option<(Instant, bool)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut cached = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, active)) = *cached
        && at.elapsed() < REFRESH
    {
        return active;
    }
    let active = dirs::home_dir().is_some_and(|home| {
        let path = crate::core::editor_registry::claude_state_dir(&home).join("settings.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| crate::core::jsonc::parse_jsonc(&content).ok())
            .and_then(|settings| {
                settings
                    .get("statusLine")
                    .and_then(|line| line.get("command"))
                    .and_then(serde_json::Value::as_str)
                    .map(crate::hooks::agents::is_lean_ctx_statusline)
            })
            .unwrap_or(false)
    });
    *cached = Some((Instant::now(), active));
    active
}
