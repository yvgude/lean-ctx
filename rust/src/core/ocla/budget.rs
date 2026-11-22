//! In-memory hierarchical budget enforcement for OCLA request admission.

use std::collections::{HashMap, HashSet};

use lean_ctx_time::UtcDateTime;

use super::types::{OclaError, OclaResult};

/// A budget's organizational level and stable identifier.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum BudgetScope {
    Org(String),
    Team(String),
    User(String),
}

/// Daily token and USD caps for one scope.
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetLimit {
    pub scope: BudgetScope,
    pub max_tokens_per_day: u64,
    pub max_usd_per_day: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Consumption {
    tokens: u64,
    usd: f64,
}

/// In-memory daily consumption ledger with explicit org/team/user ancestry.
#[derive(Clone, Debug, Default)]
pub struct BudgetLedger {
    limits: HashMap<BudgetScope, BudgetLimit>,
    parents: HashMap<BudgetScope, BudgetScope>,
    consumption: HashMap<(BudgetScope, i64), Consumption>,
}

impl BudgetLedger {
    /// Creates an empty budget ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces the cap for a scope.
    pub fn set_limit(&mut self, limit: BudgetLimit) {
        self.limits.insert(limit.scope.clone(), limit);
    }

    /// Associates a user with a team or a team with an org.
    pub fn set_parent(&mut self, child: BudgetScope, parent: BudgetScope) {
        self.parents.insert(child, parent);
    }

    /// Checks the requested tokens against every configured ancestor cap.
    pub fn check_budget(&self, scope: &BudgetScope, tokens: u64) -> OclaResult<()> {
        self.check_budget_with_cost(scope, tokens, 0.0)
    }

    /// Checks both tokens and USD against every configured ancestor cap.
    pub fn check_budget_with_cost(
        &self,
        scope: &BudgetScope,
        tokens: u64,
        usd: f64,
    ) -> OclaResult<()> {
        if !usd.is_finite() || usd < 0.0 {
            return Err(OclaError::InvalidRequest(
                "budget cost must be finite and non-negative".to_string(),
            ));
        }

        let day = current_day();
        for current in self.lineage(scope)? {
            let Some(limit) = self.limits.get(&current) else {
                continue;
            };
            let consumed = self
                .consumption
                .get(&(current.clone(), day))
                .copied()
                .unwrap_or_default();
            if !limit.max_usd_per_day.is_finite() || limit.max_usd_per_day < 0.0 {
                return Err(OclaError::InvalidRequest(format!(
                    "invalid USD budget for {current:?}"
                )));
            }
            if tokens > limit.max_tokens_per_day.saturating_sub(consumed.tokens)
                || consumed.usd + usd >= limit.max_usd_per_day
            {
                return Err(OclaError::InvalidRequest(format!(
                    "budget exceeded for {current:?}"
                )));
            }
        }
        Ok(())
    }

    /// Records usage for the scope and all configured ancestors.
    pub fn record_consumption(&mut self, scope: &BudgetScope, tokens: u64, usd: f64) {
        let Ok(lineage) = self.lineage(scope) else {
            return;
        };
        let day = current_day();
        let usd = if usd.is_finite() && usd > 0.0 {
            usd
        } else {
            0.0
        };
        for current in lineage {
            let consumed = self.consumption.entry((current, day)).or_default();
            consumed.tokens = consumed.tokens.saturating_add(tokens);
            consumed.usd += usd;
        }
    }

    /// Returns today's consumed tokens for a scope.
    pub fn consumed_tokens(&self, scope: &BudgetScope) -> u64 {
        self.consumed(scope).tokens
    }

    /// Returns today's consumed USD for a scope.
    pub fn consumed_usd(&self, scope: &BudgetScope) -> f64 {
        self.consumed(scope).usd
    }

    fn consumed(&self, scope: &BudgetScope) -> Consumption {
        self.consumption
            .get(&(scope.clone(), current_day()))
            .copied()
            .unwrap_or_default()
    }

    fn lineage(&self, scope: &BudgetScope) -> OclaResult<Vec<BudgetScope>> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        let mut current = scope.clone();
        loop {
            if !seen.insert(current.clone()) {
                return Err(OclaError::InvalidRequest(
                    "budget hierarchy contains a cycle".to_string(),
                ));
            }
            result.push(current.clone());
            let Some(parent) = self.parents.get(&current) else {
                break;
            };
            current = parent.clone();
        }
        Ok(result)
    }
}

fn current_day() -> i64 {
    UtcDateTime::now().timestamp().div_euclid(86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(scope: BudgetScope, tokens: u64) -> BudgetLimit {
        BudgetLimit {
            scope,
            max_tokens_per_day: tokens,
            max_usd_per_day: 100.0,
        }
    }

    #[test]
    fn user_budget_rejects_when_team_is_over_limit() {
        let user = BudgetScope::User("alice".to_string());
        let team = BudgetScope::Team("platform".to_string());
        let org = BudgetScope::Org("acme".to_string());
        let mut ledger = BudgetLedger::new();
        ledger.set_limit(limit(user.clone(), 1_000));
        ledger.set_limit(limit(team.clone(), 100));
        ledger.set_limit(limit(org, 10_000));
        ledger.set_parent(user.clone(), team.clone());
        ledger.set_parent(team, BudgetScope::Org("acme".to_string()));

        ledger.record_consumption(&user, 100, 1.0);
        assert!(matches!(
            ledger.check_budget(&user, 1),
            Err(OclaError::InvalidRequest(message)) if message.contains("Team")
        ));
    }

    #[test]
    fn consumption_cascades_to_team_and_org() {
        let user = BudgetScope::User("alice".to_string());
        let team = BudgetScope::Team("platform".to_string());
        let org = BudgetScope::Org("acme".to_string());
        let mut ledger = BudgetLedger::new();
        ledger.set_parent(user.clone(), team.clone());
        ledger.set_parent(team.clone(), org.clone());

        ledger.record_consumption(&user, 42, 2.5);
        assert_eq!(ledger.consumed_tokens(&user), 42);
        assert_eq!(ledger.consumed_tokens(&team), 42);
        assert_eq!(ledger.consumed_tokens(&org), 42);
        assert!((ledger.consumed_usd(&org) - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_cap_is_checked_and_invalid_cost_is_rejected() {
        let scope = BudgetScope::Org("acme".to_string());
        let mut ledger = BudgetLedger::new();
        ledger.set_limit(BudgetLimit {
            scope: scope.clone(),
            max_tokens_per_day: 100,
            max_usd_per_day: 5.0,
        });

        assert!(ledger.check_budget_with_cost(&scope, 1, 6.0).is_err());
        assert!(ledger.check_budget_with_cost(&scope, 1, -1.0).is_err());
        ledger.record_consumption(&scope, 1, 5.0);
        assert!(ledger.check_budget(&scope, 1).is_err());
    }

    #[test]
    fn hierarchy_cycles_are_rejected() {
        let a = BudgetScope::Team("a".to_string());
        let b = BudgetScope::Org("b".to_string());
        let mut ledger = BudgetLedger::new();
        ledger.set_parent(a.clone(), b.clone());
        ledger.set_parent(b.clone(), a.clone());
        assert!(ledger.check_budget(&a, 1).is_err());
    }
}
