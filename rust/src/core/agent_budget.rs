use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

static BUDGETS: Mutex<Option<HashMap<String, AgentBudget>>> = Mutex::new(None);
static TURN_BUDGETS: Mutex<Option<HashMap<String, TurnBudget>>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBudget {
    pub agent_id: String,
    pub token_limit: usize,
    pub tokens_consumed: usize,
    pub reads_count: u32,
    pub last_reset: String,
}

#[derive(Debug, Clone)]
pub struct TurnBudget {
    pub turn_id: u64,
    pub tokens_delivered: u64,
    pub limit: u64,
    pub started_at: std::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnBudgetStatus {
    Ok,
    Warning(u64),
    Exceeded,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetCheckResult {
    Allowed { remaining: usize },
    Exceeded { limit: usize, consumed: usize },
    Warning { remaining: usize, percent_used: f32 },
}

const WARNING_THRESHOLD: f32 = 0.80;

struct TurnBudgetAssessment {
    status: TurnBudgetStatus,
    consumed: u64,
    projected: u64,
    limit: u64,
}

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

fn assess_turn_budget(budget: &TurnBudget, additional_tokens: u64) -> TurnBudgetAssessment {
    let projected = budget.tokens_delivered.saturating_add(additional_tokens);
    let status = if budget.limit > 0 && projected > budget.limit {
        TurnBudgetStatus::Exceeded
    } else if budget.limit > 0 && u128::from(projected) * 100 >= u128::from(budget.limit) * 80 {
        TurnBudgetStatus::Warning(budget.limit.saturating_sub(projected))
    } else {
        TurnBudgetStatus::Ok
    };

    TurnBudgetAssessment {
        status,
        consumed: budget.tokens_delivered,
        projected,
        limit: budget.limit,
    }
}

fn projected_turn_budget_status(
    agent_id: &str,
    tokens_to_consume: u64,
) -> Option<TurnBudgetAssessment> {
    let mut guard = TURN_BUDGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .get_or_insert_with(HashMap::new)
        .get(agent_id)
        .map(|budget| assess_turn_budget(budget, tokens_to_consume))
}

fn budget_size(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn turn_budget_check_result(assessment: &TurnBudgetAssessment) -> BudgetCheckResult {
    match assessment.status {
        TurnBudgetStatus::Ok => BudgetCheckResult::Allowed {
            remaining: budget_size(assessment.limit.saturating_sub(assessment.projected)),
        },
        TurnBudgetStatus::Warning(remaining) => BudgetCheckResult::Warning {
            remaining: budget_size(remaining),
            percent_used: assessment.projected as f32 / assessment.limit as f32,
        },
        TurnBudgetStatus::Exceeded => BudgetCheckResult::Exceeded {
            limit: budget_size(assessment.limit),
            consumed: budget_size(assessment.consumed),
        },
    }
}

pub fn start_new_turn(agent_id: &str, limit: u64) {
    let mut guard = TURN_BUDGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let budgets = guard.get_or_insert_with(HashMap::new);
    let turn_id = budgets
        .get(agent_id)
        .map_or(1, |budget| budget.turn_id.saturating_add(1));
    budgets.insert(
        agent_id.to_string(),
        TurnBudget {
            turn_id,
            tokens_delivered: 0,
            limit,
            started_at: std::time::Instant::now(),
        },
    );
}

pub fn record_turn_delivery(agent_id: &str, tokens: u64) -> TurnBudgetStatus {
    let mut guard = TURN_BUDGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(budget) = guard.get_or_insert_with(HashMap::new).get_mut(agent_id) else {
        return TurnBudgetStatus::Ok;
    };

    budget.tokens_delivered = budget.tokens_delivered.saturating_add(tokens);
    assess_turn_budget(budget, 0).status
}

pub fn turn_budget_remaining(agent_id: &str) -> Option<u64> {
    let mut guard = TURN_BUDGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .get_or_insert_with(HashMap::new)
        .get(agent_id)
        .map(|budget| budget.limit.saturating_sub(budget.tokens_delivered))
}

pub fn check_budget(agent_id: &str, tokens_to_consume: usize) -> BudgetCheckResult {
    let turn_assessment = projected_turn_budget_status(agent_id, tokens_to_consume as u64);
    if let Some(assessment) = &turn_assessment {
        if matches!(assessment.status, TurnBudgetStatus::Exceeded) {
            return turn_budget_check_result(assessment);
        }
    }

    with_budgets(|map| {
        let budget = ensure_entry(map, agent_id);
        if budget.token_limit == usize::MAX || budget.token_limit == 0 {
            return turn_assessment.as_ref().map_or(
                BudgetCheckResult::Allowed {
                    remaining: usize::MAX,
                },
                turn_budget_check_result,
            );
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
        } else if let Some(assessment) = &turn_assessment {
            turn_budget_check_result(assessment)
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
    record_turn_delivery(agent_id, tokens as u64);
}

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
    TURN_BUDGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .remove(agent_id);
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

pub fn default_limit_from_config() -> usize {
    let cfg_limit = crate::core::config::Config::load().agent_token_budget;
    if cfg_limit == 0 {
        usize::MAX
    } else {
        cfg_limit
    }
}

#[cfg(test)]
pub mod tests {
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

    #[test]
    fn start_new_turn_resets_delivery_and_increments_id() {
        let id = test_agent("turn_reset");
        start_new_turn(&id, 1_000);
        record_turn_delivery(&id, 600);
        start_new_turn(&id, 500);

        let budget = TURN_BUDGETS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(HashMap::new)
            .get(&id)
            .cloned()
            .unwrap();
        assert_eq!(budget.turn_id, 2);
        assert_eq!(budget.tokens_delivered, 0);
        assert_eq!(budget.limit, 500);
        assert!(budget.started_at.elapsed().as_secs() < 1);
        remove(&id);
    }

    #[test]
    fn turn_delivery_warns_at_eighty_percent() {
        let id = test_agent("turn_warning");
        start_new_turn(&id, 1_000);

        assert_eq!(
            record_turn_delivery(&id, 800),
            TurnBudgetStatus::Warning(200)
        );
        assert_eq!(turn_budget_remaining(&id), Some(200));
        remove(&id);
    }

    #[test]
    fn turn_delivery_exceeds_limit() {
        let id = test_agent("turn_exceeded");
        start_new_turn(&id, 1_000);

        assert_eq!(record_turn_delivery(&id, 1_001), TurnBudgetStatus::Exceeded);
        assert_eq!(turn_budget_remaining(&id), Some(0));
        remove(&id);
    }

    #[test]
    fn check_budget_uses_active_turn_budget() {
        let id = test_agent("turn_check");
        start_new_turn(&id, 1_000);
        record_turn_delivery(&id, 700);

        assert!(matches!(
            check_budget(&id, 100),
            BudgetCheckResult::Warning { remaining: 200, .. }
        ));
        assert!(matches!(
            check_budget(&id, 301),
            BudgetCheckResult::Exceeded {
                limit: 1_000,
                consumed: 700
            }
        ));
        remove(&id);
    }
}
