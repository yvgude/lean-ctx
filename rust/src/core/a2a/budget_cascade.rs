use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

const DEFAULT_CHILD_FRACTION: f64 = 0.5;
const DEFAULT_MINIMUM_BUDGET: u64 = 1_000;
const DEFAULT_MAXIMUM_BUDGET: u64 = 500_000;
const MAX_CASCADE_DEPTH: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetAllocation {
    pub parent_budget_tokens: u64,
    pub parent_used_tokens: u64,
    pub child_fraction: f64,
    pub minimum_budget: u64,
    pub maximum_budget: u64,
}

impl Default for BudgetAllocation {
    fn default() -> Self {
        Self {
            parent_budget_tokens: 0,
            parent_used_tokens: 0,
            child_fraction: DEFAULT_CHILD_FRACTION,
            minimum_budget: DEFAULT_MINIMUM_BUDGET,
            maximum_budget: DEFAULT_MAXIMUM_BUDGET,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CascadedBudget {
    pub allocated_tokens: u64,
    pub parent_remaining: u64,
    pub depth: u32,
    pub lineage: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CascadeError {
    ZeroAllocation,
    DepthLimitExceeded { depth: u32, maximum_depth: u32 },
    LineageCycle { agent_id: String },
}

impl fmt::Display for CascadeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroAllocation => {
                formatter.write_str("cascaded budget must be greater than zero")
            }
            Self::DepthLimitExceeded {
                depth,
                maximum_depth,
            } => write!(
                formatter,
                "cascade depth {depth} exceeds maximum depth {maximum_depth}"
            ),
            Self::LineageCycle { agent_id } => {
                write!(
                    formatter,
                    "cascade lineage contains cycle at agent {agent_id}"
                )
            }
        }
    }
}

impl std::error::Error for CascadeError {}

pub fn cascade_budget(allocation: &BudgetAllocation) -> CascadedBudget {
    let parent_remaining = allocation
        .parent_budget_tokens
        .saturating_sub(allocation.parent_used_tokens);
    let fractional_budget = (parent_remaining as f64 * allocation.child_fraction) as u64;
    let allocated_tokens = fractional_budget
        .max(allocation.minimum_budget)
        .min(allocation.maximum_budget);

    CascadedBudget {
        allocated_tokens,
        parent_remaining,
        depth: 0,
        lineage: Vec::new(),
    }
}

pub fn validate_cascade(budget: &CascadedBudget) -> Result<(), CascadeError> {
    if budget.allocated_tokens == 0 {
        return Err(CascadeError::ZeroAllocation);
    }
    if budget.depth > MAX_CASCADE_DEPTH {
        return Err(CascadeError::DepthLimitExceeded {
            depth: budget.depth,
            maximum_depth: MAX_CASCADE_DEPTH,
        });
    }

    let mut seen = HashSet::with_capacity(budget.lineage.len());
    for agent_id in &budget.lineage {
        if !seen.insert(agent_id) {
            return Err(CascadeError::LineageCycle {
                agent_id: agent_id.clone(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_cascade_allocates_default_fraction() {
        let allocation = BudgetAllocation {
            parent_budget_tokens: 100_000,
            parent_used_tokens: 20_000,
            ..BudgetAllocation::default()
        };

        let budget = cascade_budget(&allocation);

        assert_eq!(budget.allocated_tokens, 40_000);
        assert_eq!(budget.parent_remaining, 80_000);
        assert_eq!(budget.depth, 0);
        assert!(budget.lineage.is_empty(), "{:?}", budget.lineage);
        assert_eq!(validate_cascade(&budget), Ok(()));
    }

    #[test]
    fn cascade_saturates_remaining_and_applies_bounds() {
        let exhausted = BudgetAllocation {
            parent_budget_tokens: 10,
            parent_used_tokens: 20,
            minimum_budget: 1_000,
            maximum_budget: 5_000,
            ..BudgetAllocation::default()
        };
        let capped = BudgetAllocation {
            parent_budget_tokens: 2_000_000,
            parent_used_tokens: 0,
            child_fraction: 1.0,
            minimum_budget: 1_000,
            maximum_budget: 500_000,
        };

        assert_eq!(cascade_budget(&exhausted).parent_remaining, 0);
        assert_eq!(cascade_budget(&exhausted).allocated_tokens, 1_000);
        assert_eq!(cascade_budget(&capped).allocated_tokens, 500_000);
    }

    #[test]
    fn validation_rejects_depth_above_limit() {
        let budget = CascadedBudget {
            allocated_tokens: 1_000,
            parent_remaining: 2_000,
            depth: 6,
            lineage: vec!["parent".to_string()],
        };

        assert_eq!(
            validate_cascade(&budget),
            Err(CascadeError::DepthLimitExceeded {
                depth: 6,
                maximum_depth: 5,
            })
        );
    }

    #[test]
    fn validation_rejects_zero_budget() {
        let budget = CascadedBudget {
            allocated_tokens: 0,
            parent_remaining: 0,
            depth: 0,
            lineage: Vec::new(),
        };

        assert_eq!(validate_cascade(&budget), Err(CascadeError::ZeroAllocation));
    }

    #[test]
    fn validation_rejects_lineage_cycle() {
        let budget = CascadedBudget {
            allocated_tokens: 1_000,
            parent_remaining: 2_000,
            depth: 2,
            lineage: vec!["root".to_string(), "child".to_string(), "root".to_string()],
        };

        assert_eq!(
            validate_cascade(&budget),
            Err(CascadeError::LineageCycle {
                agent_id: "root".to_string(),
            })
        );
    }
}
