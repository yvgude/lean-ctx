//! Free-Energy Budget — optimal context allocation under token constraints.
//!
//! The LLM context window is a finite resource. This module allocates budget
//! across context columns (filesystem, providers, knowledge) to minimize
//! "free energy" — the gap between what the LLM knows and what it needs.
//!
//! Scientific basis: Free Energy Principle (Friston 2010). The system minimizes
//! surprise (unexpected tokens) by allocating budget to the most informative sources.
//!
//! Algorithm:
//!   1. Each column reports its saliency score and estimated token cost.
//!   2. Budget is allocated proportionally to saliency / cost (efficiency ratio).
//!   3. A minimum floor ensures every active column gets at least some budget.

/// A context column's budget request.
#[derive(Debug, Clone)]
pub struct ColumnBudgetRequest {
    pub column_id: String,
    pub saliency_score: f64,
    pub estimated_tokens: usize,
    pub minimum_tokens: usize,
}

/// The allocated budget for each column.
#[derive(Debug, Clone)]
pub struct ColumnBudgetAllocation {
    pub column_id: String,
    pub allocated_tokens: usize,
    pub fraction: f64,
}

/// Allocate a total token budget across multiple context columns.
///
/// Uses efficiency-weighted allocation: columns with high saliency per token
/// get more budget. Every active column gets at least `floor_fraction` of the
/// total budget (default 5%).
#[must_use]
pub fn allocate_budget(
    total_budget: usize,
    requests: &[ColumnBudgetRequest],
    floor_fraction: f64,
) -> Vec<ColumnBudgetAllocation> {
    if requests.is_empty() || total_budget == 0 {
        return Vec::new();
    }

    let floor = (total_budget as f64 * floor_fraction.clamp(0.0, 0.5)) as usize;
    let total_floor = floor * requests.len();
    let distributable = total_budget.saturating_sub(total_floor);

    let efficiencies: Vec<f64> = requests
        .iter()
        .map(|r| {
            let cost = r.estimated_tokens.max(1) as f64;
            r.saliency_score / cost
        })
        .collect();

    let total_efficiency: f64 = efficiencies.iter().sum();

    requests
        .iter()
        .enumerate()
        .map(|(i, req)| {
            let proportional = if total_efficiency > 0.0 {
                (efficiencies[i] / total_efficiency * distributable as f64) as usize
            } else {
                distributable / requests.len()
            };

            let allocated = (floor + proportional)
                .max(req.minimum_tokens)
                .min(total_budget);
            let fraction = allocated as f64 / total_budget as f64;

            ColumnBudgetAllocation {
                column_id: req.column_id.clone(),
                allocated_tokens: allocated,
                fraction,
            }
        })
        .collect()
}

/// Compute the "free energy" — how much information gap remains after allocation.
/// Lower is better. 0.0 means all requested tokens were fully satisfied.
#[must_use]
pub fn free_energy(
    requests: &[ColumnBudgetRequest],
    allocations: &[ColumnBudgetAllocation],
) -> f64 {
    if requests.is_empty() {
        return 0.0;
    }

    let total_requested: f64 = requests.iter().map(|r| r.estimated_tokens as f64).sum();
    let total_allocated: f64 = allocations.iter().map(|a| a.allocated_tokens as f64).sum();

    if total_requested == 0.0 {
        return 0.0;
    }

    ((total_requested - total_allocated) / total_requested).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, saliency: f64, tokens: usize) -> ColumnBudgetRequest {
        ColumnBudgetRequest {
            column_id: id.into(),
            saliency_score: saliency,
            estimated_tokens: tokens,
            minimum_tokens: 0,
        }
    }

    #[test]
    fn allocate_empty_returns_empty() {
        assert!(allocate_budget(1000, &[], 0.05).is_empty());
    }

    #[test]
    fn allocate_single_column_gets_all() {
        let reqs = vec![request("fs", 1.0, 500)];
        let allocs = allocate_budget(1000, &reqs, 0.05);

        assert_eq!(allocs.len(), 1);
        assert!(allocs[0].allocated_tokens >= 950);
    }

    #[test]
    fn high_saliency_gets_more_budget() {
        let reqs = vec![
            request("important", 0.9, 500),
            request("unimportant", 0.1, 500),
        ];
        let allocs = allocate_budget(1000, &reqs, 0.05);

        assert!(allocs[0].allocated_tokens > allocs[1].allocated_tokens);
    }

    #[test]
    fn efficient_column_gets_more_budget() {
        let reqs = vec![
            request("efficient", 0.5, 100),   // 0.005 per token
            request("expensive", 0.5, 10000), // 0.00005 per token
        ];
        let allocs = allocate_budget(2000, &reqs, 0.05);

        assert!(allocs[0].allocated_tokens > allocs[1].allocated_tokens);
    }

    #[test]
    fn floor_ensures_minimum_allocation() {
        let reqs = vec![request("dominant", 0.99, 100), request("tiny", 0.01, 100)];
        let allocs = allocate_budget(1000, &reqs, 0.1);

        assert!(allocs[1].allocated_tokens >= 100);
    }

    #[test]
    fn free_energy_zero_when_fully_satisfied() {
        let reqs = vec![request("a", 1.0, 500)];
        let allocs = vec![ColumnBudgetAllocation {
            column_id: "a".into(),
            allocated_tokens: 500,
            fraction: 1.0,
        }];
        assert!((free_energy(&reqs, &allocs)).abs() < f64::EPSILON);
    }

    #[test]
    fn free_energy_positive_when_under_budget() {
        let reqs = vec![request("a", 1.0, 1000)];
        let allocs = vec![ColumnBudgetAllocation {
            column_id: "a".into(),
            allocated_tokens: 500,
            fraction: 0.5,
        }];
        let fe = free_energy(&reqs, &allocs);
        assert!(fe > 0.0);
        assert!((fe - 0.5).abs() < f64::EPSILON);
    }
}
