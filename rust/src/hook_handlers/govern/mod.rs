// SPDX-License-Identifier: Apache-2.0

//! `lean-ctx hook govern` — the governor's single PostToolUse handler for
//! Claude Code (governor design v1, slices 2 and 9).
//!
//! It only ever edits the *newest* tool result the host hands it, through the
//! documented `PostToolUse.hookSpecificOutput.updatedToolOutput` channel, and
//! otherwise emits nothing (the documented no-op: the host keeps the native
//! result byte for byte). Earlier turns, the system prompt and the tool list
//! are never touched, so no provider cache prefix is invalidated.
//!
//! Rules, first match wins:
//! - **R1.read** — the existing re-read dedup ([`super::read_dedup`]), now one
//!   rule of the governor instead of its own hook.
//! - **R1.bash** — an identical re-run of a pure viewer command (`cat`, `ls`,
//!   `git status` …) with no edit or side-effecting command in between.
//! - **R2** — strictly lossless repeat collapse of large, non-diagnostic Bash
//!   output ([`crate::core::patterns::lossless_collapse`]).
//! - **R4** — template collapse of large Bash output whose lines differ only in
//!   timestamps, numbers or ids ([`crate::core::patterns::template_collapse`]);
//!   the full output goes to the archive and the result names its id. Unlike
//!   R2 it also runs on output with error lines: those stay visible line by
//!   line, and on file viewers unless they read code or data files
//!   (`bash::r4_may_collapse`). A live rule beats a shadow one; among
//!   shadow rules R2 wins.
//! - **R3** — output already in context; shadow only in v1.
//!
//! Every rule runs under a mode: `on` replaces the result, `shadow` writes
//! only a decision receipt, `off` skips it. Per owner decision (O-3, O-5) R1.bash,
//! R2 and R4 ship as `shadow` until harness A measures them; R1.read keeps today's
//! live read-dedup behaviour. A receipt goes to the savings ledger only after
//! the output has been printed within the deadline, so a receipt never claims
//! a replacement the host did not get.
//!
//! Reliability: one internal deadline ([`DEADLINE`]) covers stdin and the
//! decision; past it the hook prints nothing and the host keeps the native
//! output. The installer additionally sets an explicit 5 s host timeout.

mod bash;
mod diagnostics;
mod enforce;
#[cfg(test)]
mod tests;

use std::sync::mpsc;
use std::time::Duration;

use super::read_dedup;

pub use enforce::handle_rewrite_enforce;

/// Internal deadline for the whole hook (stdin + decision). Past it the hook
/// emits nothing and the host keeps the native output.
pub(crate) const DEADLINE: Duration = Duration::from_millis(1500);

/// Host timeout the installer writes for every governor hook, in seconds.
pub(crate) const HOST_TIMEOUT_SECS: u64 = 5;

/// Smallest saving worth a replacement (design §2 G).
const MIN_SAVED_TOKENS: usize = 128;

/// Version of the rule triggers and thresholds, carried in every receipt.
const RULE_VERSION: u32 = 1;

/// Host label in receipts; the governor ships on Claude Code only.
const HOST: &str = "claude-code";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuleMode {
    Off,
    Shadow,
    On,
}

impl RuleMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "no" => Some(Self::Off),
            "shadow" | "log" | "log-only" => Some(Self::Shadow),
            "on" | "1" | "true" | "yes" => Some(Self::On),
            _ => None,
        }
    }

    fn from_env(var: &str, default: Self) -> Self {
        std::env::var(var)
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or(default)
    }
}

/// Effective mode per rule: `LEAN_CTX_GOVERNOR` caps every rule, the
/// per-rule variables pick within that cap. R3 can never exceed `shadow` in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Policy {
    pub r1_read: RuleMode,
    pub r1_bash: RuleMode,
    pub r2: RuleMode,
    pub r3: RuleMode,
    pub r4: RuleMode,
}

impl Policy {
    pub(crate) fn from_env() -> Self {
        let cap = RuleMode::from_env("LEAN_CTX_GOVERNOR", RuleMode::On);
        let rule = |var: &str, default: RuleMode| RuleMode::from_env(var, default).min(cap);
        Self {
            r1_read: rule("LEAN_CTX_GOVERNOR_R1_READ", RuleMode::On),
            r1_bash: rule("LEAN_CTX_GOVERNOR_R1_BASH", RuleMode::Shadow),
            r2: rule("LEAN_CTX_GOVERNOR_R2", RuleMode::Shadow),
            r3: rule("LEAN_CTX_GOVERNOR_R3", RuleMode::Shadow).min(RuleMode::Shadow),
            r4: rule("LEAN_CTX_GOVERNOR_R4", RuleMode::Shadow),
        }
    }
}

/// A decision receipt, written after the output was delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub rule_id: &'static str,
    pub mode: RuleMode,
    pub baseline_tokens: usize,
    pub actual_tokens: usize,
    pub session_hash: String,
}

impl Receipt {
    fn decision(&self) -> &'static str {
        if self.mode == RuleMode::On {
            "applied"
        } else {
            "shadow"
        }
    }

    fn record(&self) {
        let would_save = self.baseline_tokens.saturating_sub(self.actual_tokens);
        let delivered = if self.mode == RuleMode::On {
            self.actual_tokens
        } else {
            self.baseline_tokens
        };
        crate::core::savings_ledger::record_governor_event(
            self.baseline_tokens,
            delivered,
            crate::core::savings_ledger::event::GovernorReceipt {
                rule_id: self.rule_id.to_string(),
                rule_version: RULE_VERSION,
                decision: self.decision().to_string(),
                host: HOST.to_string(),
                session_hash: self.session_hash.clone(),
                would_save_tokens: would_save as u64,
            },
        );
    }
}

/// What the hook does: `output` empty = passthrough.
#[derive(Debug, Default)]
pub(crate) struct Decision {
    pub output: String,
    pub receipt: Option<Receipt>,
    /// Stats of an applied R1.read hit (dashboard counters, as before).
    read_stats: Option<read_dedup::DedupHit>,
}

impl Decision {
    fn from_rule(
        mode: RuleMode,
        output: String,
        rule_id: &'static str,
        baseline: usize,
        actual: usize,
        session_id: &str,
    ) -> Self {
        let receipt = Receipt {
            rule_id,
            mode,
            baseline_tokens: baseline,
            actual_tokens: actual,
            session_hash: read_dedup::session_hash(session_id),
        };
        Self {
            output: if mode == RuleMode::On {
                output
            } else {
                String::new()
            },
            receipt: Some(receipt),
            read_stats: None,
        }
    }
}

pub fn handle_govern() {
    if super::is_disabled() {
        return;
    }
    let policy = Policy::from_env();
    let decision = run_with_deadline(DEADLINE, move || {
        super::read_stdin_with_timeout(DEADLINE)
            .map(|input| decide(&input, &policy))
            .unwrap_or_default()
    });
    let Some(decision) = decision else {
        return;
    };
    if !decision.output.is_empty() {
        use std::io::Write;
        let mut out = std::io::stdout();
        if out
            .write_all(decision.output.as_bytes())
            .and_then(|()| out.flush())
            .is_err()
        {
            return;
        }
    }
    if let Some(hit) = &decision.read_stats {
        hit.record_stats();
    }
    if let Some(receipt) = &decision.receipt {
        receipt.record();
    }
}

/// Run `work` under a hard deadline. `None` = too slow: the caller emits
/// nothing and the host keeps the native output. The worker is abandoned
/// (it only sends to a channel and dies with the process); its record writes
/// are atomic, so an abandoned write cannot leave a torn record.
pub(crate) fn run_with_deadline<T, F>(deadline: Duration, work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(deadline).ok()
}

/// The governor decision for one hook payload. Pure apart from the
/// per-session record files under the temp dir.
pub(crate) fn decide(input: &str, policy: &Policy) -> Decision {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(input) else {
        return Decision::default();
    };
    let session_id = v.get("session_id").and_then(|s| s.as_str()).unwrap_or("");
    match v.get("hook_event_name").and_then(|e| e.as_str()) {
        Some("PreCompact" | "PostCompact") => {
            purge(session_id);
            return Decision::default();
        }
        Some("SessionStart") => {
            let source = v.get("source").and_then(|s| s.as_str()).unwrap_or("");
            if matches!(source, "clear" | "compact" | "resume") {
                purge(session_id);
            }
            return Decision::default();
        }
        Some("PostToolUse") | None => {}
        Some(_) => return Decision::default(),
    }
    if session_id.is_empty() {
        return Decision::default();
    }
    match super::payload::resolve_tool_name(&v).as_deref() {
        Some("Read") => r1_read(&v, policy, session_id),
        Some(name) if read_dedup::CTX_READ_TOOLS.contains(&name) => r1_read(&v, policy, session_id),
        Some("Bash") => bash::decide(&v, policy, session_id),
        Some("Edit" | "Write" | "MultiEdit" | "NotebookEdit") => {
            bash::bump_epoch(session_id);
            Decision::default()
        }
        _ => Decision::default(),
    }
}

fn purge(session_id: &str) {
    if !session_id.is_empty() {
        read_dedup::purge_session(session_id);
    }
}

fn r1_read(v: &serde_json::Value, policy: &Policy, session_id: &str) -> Decision {
    if policy.r1_read == RuleMode::Off {
        return Decision::default();
    }
    let Some(hit) = read_dedup::dedup_hit_governed(v) else {
        return Decision::default();
    };
    if hit.original_tokens.saturating_sub(hit.stub_tokens) < MIN_SAVED_TOKENS {
        return Decision::default();
    }
    let mut decision = Decision::from_rule(
        policy.r1_read,
        hit.output.clone(),
        "R1.read",
        hit.original_tokens,
        hit.stub_tokens,
        session_id,
    );
    if policy.r1_read == RuleMode::On {
        decision.read_stats = Some(hit);
    }
    decision
}
