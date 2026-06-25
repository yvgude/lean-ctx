use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

static BUDGETS: Mutex<Option<HashMap<String, AgentBudget>>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBudget {
    pub agent_id: String,
    pub token_limit: usize,
    pub tokens_consumed: usize,
    pub reads_count: u32,
    pub last_reset: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetCheckResult {
    Allowed { remaining: usize },
    Exceeded { limit: usize, consumed: usize },
    Warning { remaining: usize, percent_used: f32 },
}

const WARNING_THRESHOLD: f32 = 0.80;

fn with_budgets<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<String, AgentBudget>) -> R,
{
    let mut guard = BUDGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

fn ensure_entry<'a>(
    map: &'a mut HashMap<String, AgentBudget>,
    agent_id: &str,
) -> &'a mut AgentBudget {
    map.entry(agent_id.to_string())
        .or_insert_with(|| AgentBudget {
            agent_id: agent_id.to_string(),
            token_limit: usize::MAX,
            tokens_consumed: 0,
            reads_count: 0,
            last_reset: chrono::Utc::now().to_rfc3339(),
        })
}

#[must_use]
pub fn check_budget(agent_id: &str, tokens_to_consume: usize) -> BudgetCheckResult {
    with_budgets(|map| {
        let budget = ensure_entry(map, agent_id);
        if budget.token_limit == usize::MAX || budget.token_limit == 0 {
            return BudgetCheckResult::Allowed {
                remaining: usize::MAX,
            };
        }

        let projected = budget.tokens_consumed.saturating_add(tokens_to_consume);
        if projected > budget.token_limit {
            return BudgetCheckResult::Exceeded {
                limit: budget.token_limit,
                consumed: budget.tokens_consumed,
            };
        }

        let percent_used = projected as f32 / budget.token_limit as f32;
        let remaining = budget.token_limit.saturating_sub(projected);

        if percent_used >= WARNING_THRESHOLD {
            BudgetCheckResult::Warning {
                remaining,
                percent_used,
            }
        } else {
            BudgetCheckResult::Allowed { remaining }
        }
    })
}

pub fn record_consumption(agent_id: &str, tokens: usize) {
    with_budgets(|map| {
        let budget = ensure_entry(map, agent_id);
        budget.tokens_consumed = budget.tokens_consumed.saturating_add(tokens);
        budget.reads_count += 1;
    });
}

#[must_use]
pub fn get_status(agent_id: &str) -> AgentBudget {
    with_budgets(|map| ensure_entry(map, agent_id).clone())
}

pub fn reset(agent_id: &str) {
    with_budgets(|map| {
        let budget = ensure_entry(map, agent_id);
        budget.tokens_consumed = 0;
        budget.reads_count = 0;
        budget.last_reset = chrono::Utc::now().to_rfc3339();
    });
}

/// Remove an agent's budget entry entirely. Safe only for agents that can no longer
/// issue reads (finished / dead PID) — a live agent would have its budget silently
/// reset to 0 on the next check. Bounds the BUDGETS map on long-lived daemons.
pub fn remove(agent_id: &str) {
    with_budgets(|map| {
        map.remove(agent_id);
    });
}

pub fn set_limit(agent_id: &str, limit: usize) {
    with_budgets(|map| {
        let budget = ensure_entry(map, agent_id);
        budget.token_limit = if limit == 0 { usize::MAX } else { limit };
    });
}

pub fn init_from_config() {
    let cfg_limit = crate::core::config::Config::load().agent_token_budget;
    if cfg_limit > 0 {
        with_budgets(|map| {
            for budget in map.values_mut() {
                if budget.token_limit == usize::MAX {
                    budget.token_limit = cfg_limit;
                }
            }
        });
    }
}

#[must_use]
pub fn default_limit_from_config() -> usize {
    let cfg_limit = crate::core::config::Config::load().agent_token_budget;
    if cfg_limit == 0 {
        usize::MAX
    } else {
        cfg_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_agent(name: &str) -> String {
        format!("test_agent_{name}_{:?}", std::thread::current().id())
    }

    #[test]
    fn unlimited_budget_always_allows() {
        let id = test_agent("unlimited");
        let result = check_budget(&id, 1_000_000);
        assert!(matches!(result, BudgetCheckResult::Allowed { .. }));
    }

    #[test]
    fn set_limit_and_exceed() {
        let id = test_agent("exceed");
        set_limit(&id, 1000);
        record_consumption(&id, 800);
        let result = check_budget(&id, 300);
        assert!(matches!(
            result,
            BudgetCheckResult::Exceeded {
                limit: 1000,
                consumed: 800
            }
        ));
    }

    #[test]
    fn warning_at_80_percent() {
        let id = test_agent("warning");
        set_limit(&id, 1000);
        record_consumption(&id, 700);
        let result = check_budget(&id, 100);
        assert!(matches!(result, BudgetCheckResult::Warning { .. }));
    }

    #[test]
    fn reset_clears_consumption() {
        let id = test_agent("reset");
        set_limit(&id, 1000);
        record_consumption(&id, 900);
        reset(&id);
        let status = get_status(&id);
        assert_eq!(status.tokens_consumed, 0);
        assert_eq!(status.reads_count, 0);
    }

    #[test]
    fn zero_limit_means_unlimited() {
        let id = test_agent("zero");
        set_limit(&id, 0);
        let result = check_budget(&id, 1_000_000);
        assert!(matches!(result, BudgetCheckResult::Allowed { .. }));
    }

    #[test]
    fn record_increments_reads_count() {
        let id = test_agent("reads");
        record_consumption(&id, 100);
        record_consumption(&id, 200);
        let status = get_status(&id);
        assert_eq!(status.reads_count, 2);
        assert_eq!(status.tokens_consumed, 300);
    }
}
