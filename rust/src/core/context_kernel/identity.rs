//! Identity and cost-center attribution for context-kernel requests.

use std::cmp::Reverse;
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Identity metadata attached to a context-kernel caller.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CallerIdentity {
    /// Stable user identifier, when known.
    pub user_id: Option<String>,
    /// Stable team identifier, when known.
    pub team_id: Option<String>,
    /// Enterprise cost center charged for the request, when known.
    pub cost_center: Option<String>,
    /// Functional role of the caller.
    pub role: CallerRole,
    /// Session identifier used when no user identifier is available.
    pub session_id: Option<String>,
}

/// Functional role associated with a caller.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum CallerRole {
    /// Software developer using context directly.
    #[default]
    Developer,
    /// Human reviewer evaluating generated work.
    Reviewer,
    /// Autonomous or delegated software agent.
    Agent,
    /// Internal system process.
    System,
    /// Administrative caller.
    Admin,
}

/// Accumulated token and outcome metrics for one caller identity.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IdentityAttribution {
    /// Identity represented by this attribution entry.
    pub identity: CallerIdentity,
    /// Tokens delivered after context processing.
    pub tokens_consumed: usize,
    /// Tokens avoided through context processing.
    pub tokens_saved: usize,
    /// Number of requests attributed to the identity.
    pub request_count: usize,
    /// Number of accepted outcomes attributed to the identity.
    pub accepted_outcomes: usize,
}

/// In-memory attribution ledger grouped by the strongest available identity key.
#[derive(Debug, Clone, Default)]
pub struct IdentityLedger {
    entries: std::collections::HashMap<String, IdentityAttribution>,
}

impl IdentityLedger {
    /// Creates an empty identity ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records factual token usage and savings for a caller without asserting an outcome.
    pub fn record_usage(&mut self, identity: &CallerIdentity, consumed: usize, saved: usize) {
        let key = Self::ledger_key(identity);
        let attribution = self
            .entries
            .entry(key)
            .or_insert_with(|| IdentityAttribution {
                identity: identity.clone(),
                tokens_consumed: 0,
                tokens_saved: 0,
                request_count: 0,
                accepted_outcomes: 0,
            });
        attribution.tokens_consumed = attribution.tokens_consumed.saturating_add(consumed);
        attribution.tokens_saved = attribution.tokens_saved.saturating_add(saved);
        attribution.request_count = attribution.request_count.saturating_add(1);
    }

    /// Records token usage, savings, and an explicit outcome acceptance for a caller.
    pub fn record(
        &mut self,
        identity: &CallerIdentity,
        consumed: usize,
        saved: usize,
        accepted: bool,
    ) {
        self.record_usage(identity, consumed, saved);
        if accepted {
            let key = Self::ledger_key(identity);
            if let Some(attribution) = self.entries.get_mut(&key) {
                attribution.accepted_outcomes = attribution.accepted_outcomes.saturating_add(1);
            }
        }
    }

    /// Returns attribution recorded under an identity key.
    #[must_use]
    pub fn attribution_for(&self, key: &str) -> Option<&IdentityAttribution> {
        self.entries.get(key)
    }

    /// Returns up to `limit` entries ordered by descending token consumption.
    #[must_use]
    pub fn top_consumers(&self, limit: usize) -> Vec<&IdentityAttribution> {
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_unstable_by_key(|entry| Reverse(entry.tokens_consumed));
        entries.truncate(limit);
        entries
    }

    /// Returns up to `limit` entries ordered by descending token savings.
    #[must_use]
    pub fn top_savers(&self, limit: usize) -> Vec<&IdentityAttribution> {
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_unstable_by_key(|entry| Reverse(entry.tokens_saved));
        entries.truncate(limit);
        entries
    }

    /// Returns total tokens consumed across all identities.
    #[must_use]
    pub fn total_tokens(&self) -> usize {
        self.entries.values().fold(0, |total, entry| {
            total.saturating_add(entry.tokens_consumed)
        })
    }

    /// Returns total tokens saved across all identities.
    #[must_use]
    pub fn total_savings(&self) -> usize {
        self.entries
            .values()
            .fold(0, |total, entry| total.saturating_add(entry.tokens_saved))
    }

    /// Summarizes distinct identities and aggregate token efficiency.
    #[must_use]
    pub fn summary(&self) -> IdentityLedgerSummary {
        let users: HashSet<_> = self
            .entries
            .values()
            .filter_map(|entry| entry.identity.user_id.as_deref())
            .collect();
        let teams: HashSet<_> = self
            .entries
            .values()
            .filter_map(|entry| entry.identity.team_id.as_deref())
            .collect();
        let total_tokens = self.total_tokens();
        let total_savings = self.total_savings();
        let original_tokens = total_tokens.saturating_add(total_savings);
        let savings_rate = if original_tokens == 0 {
            0.0
        } else {
            total_savings as f64 / original_tokens as f64
        };

        IdentityLedgerSummary {
            total_users: users.len(),
            total_teams: teams.len(),
            total_tokens,
            total_savings,
            savings_rate,
        }
    }

    fn ledger_key(identity: &CallerIdentity) -> String {
        identity
            .user_id
            .as_ref()
            .or(identity.session_id.as_ref())
            .or(identity.team_id.as_ref())
            .or(identity.cost_center.as_ref())
            .cloned()
            .unwrap_or_else(|| {
                match identity.role {
                    CallerRole::Developer => "developer",
                    CallerRole::Reviewer => "reviewer",
                    CallerRole::Agent => "agent",
                    CallerRole::System => "system",
                    CallerRole::Admin => "admin",
                }
                .to_owned()
            })
    }
}

/// Aggregate identity-ledger metrics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IdentityLedgerSummary {
    /// Number of distinct known users.
    pub total_users: usize,
    /// Number of distinct known teams.
    pub total_teams: usize,
    /// Tokens consumed across all identities.
    pub total_tokens: usize,
    /// Tokens saved across all identities.
    pub total_savings: usize,
    /// Fraction of original tokens eliminated by context processing.
    pub savings_rate: f64,
}

#[cfg(test)]
pub mod tests {
    use super::{CallerIdentity, CallerRole, IdentityLedger};

    fn identity(user: &str, team: &str) -> CallerIdentity {
        CallerIdentity {
            user_id: Some(user.to_owned()),
            team_id: Some(team.to_owned()),
            ..CallerIdentity::default()
        }
    }

    #[test]
    fn identity_default() {
        assert_eq!(CallerIdentity::default().role, CallerRole::Developer);
    }

    #[test]
    fn ledger_record_single() {
        let mut ledger = IdentityLedger::new();
        ledger.record(&identity("alice", "platform"), 80, 20, true);

        let entry = ledger
            .attribution_for("alice")
            .expect("alice must have attribution");
        assert_eq!(entry.tokens_consumed, 80);
        assert_eq!(entry.tokens_saved, 20);
        assert_eq!(entry.request_count, 1);
        assert_eq!(entry.accepted_outcomes, 1);
    }

    #[test]
    fn ledger_usage_does_not_invent_an_outcome() {
        let mut ledger = IdentityLedger::new();
        ledger.record_usage(&identity("alice", "platform"), 80, 20);

        let entry = ledger
            .attribution_for("alice")
            .expect("alice must have attribution");
        assert_eq!(entry.request_count, 1);
        assert_eq!(entry.accepted_outcomes, 0);
    }

    #[test]
    fn ledger_record_multiple_same_user() {
        let mut ledger = IdentityLedger::new();
        let caller = identity("alice", "platform");
        ledger.record(&caller, 80, 20, true);
        ledger.record(&caller, 40, 10, false);

        let entry = ledger
            .attribution_for("alice")
            .expect("alice must have attribution");
        assert_eq!(entry.tokens_consumed, 120);
        assert_eq!(entry.tokens_saved, 30);
        assert_eq!(entry.request_count, 2);
        assert_eq!(entry.accepted_outcomes, 1);
    }

    #[test]
    fn top_consumers_sorted() {
        let mut ledger = IdentityLedger::new();
        ledger.record(&identity("small", "one"), 10, 20, false);
        ledger.record(&identity("large", "two"), 90, 5, false);

        let consumers = ledger.top_consumers(2);
        assert_eq!(consumers[0].identity.user_id.as_deref(), Some("large"));
        assert_eq!(consumers[1].identity.user_id.as_deref(), Some("small"));
    }

    #[test]
    fn top_savers_sorted() {
        let mut ledger = IdentityLedger::new();
        ledger.record(&identity("small", "one"), 90, 5, false);
        ledger.record(&identity("large", "two"), 10, 20, false);

        let savers = ledger.top_savers(2);
        assert_eq!(savers[0].identity.user_id.as_deref(), Some("large"));
        assert_eq!(savers[1].identity.user_id.as_deref(), Some("small"));
    }

    #[test]
    fn summary_counts_unique() {
        let mut ledger = IdentityLedger::new();
        ledger.record(&identity("alice", "platform"), 80, 20, true);
        ledger.record(&identity("bob", "platform"), 70, 30, false);

        let summary = ledger.summary();
        assert_eq!(summary.total_users, 2);
        assert_eq!(summary.total_teams, 1);
        assert_eq!(summary.total_tokens, 150);
        assert_eq!(summary.total_savings, 50);
        assert!((summary.savings_rate - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn serde_roundtrip() {
        let original = CallerIdentity {
            user_id: Some("alice".to_owned()),
            team_id: Some("platform".to_owned()),
            cost_center: Some("engineering".to_owned()),
            role: CallerRole::Reviewer,
            session_id: Some("session-1".to_owned()),
        };
        let json = serde_json::to_string(&original).expect("identity must serialize");
        let decoded = serde_json::from_str(&json).expect("identity must deserialize");

        assert_eq!(original, decoded);
    }
}

/// Canonical task context carrying identifiers for the task lifecycle.
/// Propagated through all runtime surfaces (envelope, MCP, proxy, hooks, events).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskContext {
    pub task_id: String,
    pub trace_id: String,
    pub parent_task_id: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub project_id: Option<String>,
}

impl TaskContext {
    pub fn new_root() -> Self {
        Self {
            task_id: uuid::Uuid::new_v4().to_string(),
            trace_id: uuid::Uuid::new_v4().to_string(),
            parent_task_id: None,
            session_id: None,
            agent_id: None,
            project_id: None,
        }
    }

    pub fn new(task_id: impl Into<String>, trace_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            trace_id: trace_id.into(),
            parent_task_id: None,
            session_id: None,
            agent_id: None,
            project_id: None,
        }
    }
}
