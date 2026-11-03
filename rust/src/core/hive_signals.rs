// SPDX-License-Identifier: Apache-2.0
//! Engine signal counts (K10, K12, K13) and redacted-secret hits (K21),
//! named where they happen: an output served truncated or fetched back, a
//! rule that refused or would have, programs no rule covers, a secret the
//! redaction path removed.
//!
//! This module only names what happened, as closed wire names
//! (`FilterName`, `RuleId`, `UnregulatedName` of
//! `lean_ctx_protocol::hive::metrics_v2`), never a path, a command line or
//! a mode argument. Where a record goes, and whether one is kept at all, is
//! the Hive connector's: it registers a [`SignalRecorder`] at start-up
//! ([`register_signal_recorder`]). Without one (library use, tests)
//! nothing is recorded. These are counts only; no refusal depends on them.

use std::sync::OnceLock;

use lean_ctx_protocol::hive::metrics_v2::{
    FilterName, KNOWN_PROGRAMS, OTHER, ReadMode, RuleId, UnregulatedName,
};
use serde::{Deserialize, Serialize};

/// What one record counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Truncated,
    Refetched,
    Blocked,
    WouldBlock,
    UnregulatedProgram,
    UnregulatedHost,
    UnregulatedMcpServer,
}

/// Where signals and secret hits are recorded.
#[derive(Clone, Copy)]
pub struct SignalRecorder {
    /// Whether anything is recorded now.
    pub recording: fn() -> bool,
    /// Record one count of `kind` per name.
    pub record: fn(SignalKind, &[&str]),
    /// Record redacted secrets by their redaction labels.
    pub secrets: fn(&[&str]),
}

static RECORDER: OnceLock<SignalRecorder> = OnceLock::new();

/// Register the signal recorder. The first registration wins.
pub fn register_signal_recorder(recorder: SignalRecorder) {
    let _ = RECORDER.set(recorder);
}

/// Whether anything is recorded now, so a caller can skip work whose only
/// use is a record.
pub fn recording() -> bool {
    RECORDER.get().is_some_and(|r| (r.recording)())
}

pub fn record(kind: SignalKind, names: &[&str]) {
    if let Some(recorder) = RECORDER.get() {
        (recorder.record)(kind, names);
    }
}

/// K21: secrets the redaction path removed, by their labels. Never sees the
/// secret value; never fails the redaction.
pub fn note_secrets(labels: &[&str]) {
    if labels.is_empty() {
        return;
    }
    if let Some(recorder) = RECORDER.get() {
        (recorder.secrets)(labels);
    }
}

/// The filter of a read mode (`lines:10-20` is `lines`).
pub fn read_mode_filter(mode: &str) -> FilterName {
    let mode = ReadMode::of(mode);
    let wire = serde_json::to_value(mode)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| OTHER.to_string());
    FilterName::or_other(&wire)
}

/// The filter of a firewalled tool output.
pub fn output_filter(tool: &str) -> FilterName {
    FilterName::or_other(match tool {
        "ctx_shell" | "ctx_execute" => "shell_output",
        "ctx_search" => "search_output",
        _ => OTHER,
    })
}

/// A program as the known-program table names it, else `other`.
pub fn program_name(base: &str) -> UnregulatedName {
    let base = base.rsplit(['/', '\\']).next().unwrap_or(base);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    UnregulatedName::new(base)
        .filter(|_| KNOWN_PROGRAMS.contains(&base))
        .unwrap_or_else(UnregulatedName::other)
}

/// The rule the path jail decides under.
pub fn path_jail_rule() -> RuleId {
    RuleId::or_other("gateway.path_jail")
}

/// The rule secret-like paths and secret filters decide under.
pub fn secrets_rule() -> RuleId {
    RuleId::or_other("security.block_secrets")
}

/// The rule the shell allowlist decides under.
pub fn shell_rule() -> RuleId {
    RuleId::or_other("gateway.security_violation")
}

/// The rules a withholding admission decision refused under, from its
/// closed reason codes (never the destination or the content).
pub fn admission_rules<'a>(reason_codes: impl IntoIterator<Item = &'a str>) -> Vec<RuleId> {
    let mut rules: Vec<RuleId> = Vec::new();
    for code in reason_codes {
        let rule = match code {
            "secret.blocked" => "security.block_secrets",
            "pii.blocked" => "security.pii",
            "classification.blocked" => "security.classification",
            "destination.constraint_satisfied" | "destination.remote_allowed_once" => continue,
            c if c.starts_with("destination.") => "gateway.destination",
            c if c.starts_with("egress.") => "gateway.egress",
            _ => continue,
        };
        let rule = RuleId::or_other(rule);
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    rules
}

/// K10: an output served truncated, by filter.
pub fn truncated(filter: &FilterName) {
    record(SignalKind::Truncated, &[filter.as_str()]);
}

/// K10: a truncated output fetched back in full, by filter.
pub fn refetched(filter: &FilterName) {
    record(SignalKind::Refetched, &[filter.as_str()]);
}

/// K12: a refusal in force.
pub fn blocked(rule: &RuleId) {
    record(SignalKind::Blocked, &[rule.as_str()]);
}

/// K12: a refusal a rule in monitor mode would have made.
pub fn would_block(rule: &RuleId) {
    record(SignalKind::WouldBlock, &[rule.as_str()]);
}

/// K12: a refusal by `rule`, in force (`enforcing`) or in monitor mode.
pub fn refused(rule: &RuleId, enforcing: bool) {
    if enforcing {
        blocked(rule);
    } else {
        would_block(rule);
    }
}

/// K13: programs a command ran that no rule covers.
pub fn unregulated_programs(programs: &[UnregulatedName]) {
    let names: Vec<&str> = programs.iter().map(UnregulatedName::as_str).collect();
    if !names.is_empty() {
        record(SignalKind::UnregulatedProgram, &names);
    }
}

/// K12: an admission decision that withheld content, by the rules it names.
pub fn admission_refused<'a>(reason_codes: impl IntoIterator<Item = &'a str>) {
    if !recording() {
        return;
    }
    for rule in admission_rules(reason_codes) {
        blocked(&rule);
    }
}
