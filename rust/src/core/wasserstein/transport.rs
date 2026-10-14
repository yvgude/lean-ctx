//! Entropy-regularized optimal transport via Sinkhorn scaling.
//!
//! Solves the Kantorovich transport problem with entropic regularization for
//! relevance-weighted token allocation across context chunks.

const MIN_DENOMINATOR: f64 = 1.0e-300;
const CONVERGENCE_TOLERANCE: f64 = 1.0e-8;

/// Cost matrix entry describing the distance from a query to a file chunk.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CostEntry {
    /// Index of the file containing the chunk.
    pub file_idx: usize,
    /// Index of the chunk within its file.
    pub chunk_idx: usize,
    /// Non-negative transport cost; lower values indicate greater relevance.
    pub cost: f64,
}

/// Computes an entropy-regularized optimal transport plan with Sinkhorn scaling.
///
/// The returned triples contain `(source_index, target_index, amount)`. Invalid
/// dimensions, empty marginals, non-positive regularization, and zero total mass
/// yield an empty plan. Demand is rescaled to the supply mass when their totals
/// differ, making the balancing problem well-defined.
pub(crate) fn sinkhorn_plan(
    supply: &[f64],
    demand: &[f64],
    cost_matrix: &[Vec<f64>],
    epsilon: f64,
    max_iters: usize,
) -> Vec<(usize, usize, f64)> {
    let Some((kernel, balanced_supply, balanced_demand)) =
        prepare_problem(supply, demand, cost_matrix, epsilon)
    else {
        return Vec::new();
    };

    let (u, v, _) = scale_kernel(&kernel, &balanced_supply, &balanced_demand, max_iters);
    let mut plan = Vec::with_capacity(supply.len().saturating_mul(demand.len()));

    for (source_idx, row) in kernel.iter().enumerate() {
        for (target_idx, kernel_value) in row.iter().enumerate() {
            let amount = u[source_idx] * kernel_value * v[target_idx];
            if amount.is_finite() && amount > 0.0 {
                plan.push((source_idx, target_idx, amount));
            }
        }
    }

    plan
}

fn prepare_problem(
    supply: &[f64],
    demand: &[f64],
    cost_matrix: &[Vec<f64>],
    epsilon: f64,
) -> Option<(Vec<Vec<f64>>, Vec<f64>, Vec<f64>)> {
    if supply.is_empty()
        || demand.is_empty()
        || !epsilon.is_finite()
        || epsilon <= 0.0
        || cost_matrix.len() != supply.len()
        || cost_matrix.iter().any(|row| row.len() != demand.len())
    {
        return None;
    }

    let clean_supply: Vec<f64> = supply.iter().copied().map(non_negative).collect();
    let mut clean_demand: Vec<f64> = demand.iter().copied().map(non_negative).collect();
    let supply_mass: f64 = clean_supply.iter().sum();
    let demand_mass: f64 = clean_demand.iter().sum();
    if supply_mass <= 0.0 || demand_mass <= 0.0 {
        return None;
    }

    let demand_scale = supply_mass / demand_mass;
    for value in &mut clean_demand {
        *value *= demand_scale;
    }

    let kernel = cost_matrix
        .iter()
        .map(|row| {
            row.iter()
                .map(|cost| {
                    let clean_cost = if cost.is_finite() {
                        cost.max(0.0)
                    } else {
                        f64::MAX
                    };
                    (-clean_cost / epsilon).exp().max(MIN_DENOMINATOR)
                })
                .collect()
        })
        .collect();

    Some((kernel, clean_supply, clean_demand))
}

fn non_negative(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

fn scale_kernel(
    kernel: &[Vec<f64>],
    supply: &[f64],
    demand: &[f64],
    max_iters: usize,
) -> (Vec<f64>, Vec<f64>, usize) {
    let mut u = vec![1.0; supply.len()];
    let mut v = vec![1.0; demand.len()];
    let mut completed_iters = 0;

    for iteration in 0..max_iters {
        let previous_u = u.clone();
        for (source_idx, row) in kernel.iter().enumerate() {
            let denominator = row
                .iter()
                .zip(&v)
                .map(|(kernel_value, scale)| kernel_value * scale)
                .sum::<f64>()
                .max(MIN_DENOMINATOR);
            u[source_idx] = supply[source_idx] / denominator;
        }

        for target_idx in 0..demand.len() {
            let denominator = kernel
                .iter()
                .zip(&u)
                .map(|(row, scale)| row[target_idx] * scale)
                .sum::<f64>()
                .max(MIN_DENOMINATOR);
            v[target_idx] = demand[target_idx] / denominator;
        }

        completed_iters = iteration + 1;
        let max_change = u
            .iter()
            .zip(previous_u)
            .map(|(current, previous)| (current - previous).abs())
            .fold(0.0_f64, f64::max);
        if max_change < CONVERGENCE_TOLERANCE {
            break;
        }
    }

    (u, v, completed_iters)
}

#[cfg(test)]
mod tests {
    use super::{prepare_problem, scale_kernel, sinkhorn_plan};

    #[test]
    fn uniform_supply_demand_gives_equal_allocation() {
        let plan = sinkhorn_plan(&[100.0], &[50.0, 50.0], &[vec![0.5, 0.5]], 0.1, 100);
        assert_eq!(plan.len(), 2);
        assert!((plan[0].2 - 50.0).abs() < 1.0e-6);
        assert!((plan[1].2 - 50.0).abs() < 1.0e-6);
    }

    #[test]
    fn high_cost_gets_less_transport() {
        let costs = [vec![0.0, 10.0], vec![10.0, 0.0]];
        let plan = sinkhorn_plan(&[50.0, 50.0], &[50.0, 50.0], &costs, 0.1, 100);
        let low_cost_mass: f64 = plan
            .iter()
            .filter(|(source, target, _)| source == target)
            .map(|(_, _, amount)| amount)
            .sum();
        let high_cost_mass: f64 = plan
            .iter()
            .filter(|(source, target, _)| source != target)
            .map(|(_, _, amount)| amount)
            .sum();
        assert!(low_cost_mass > high_cost_mass);
    }

    #[test]
    fn sinkhorn_converges_within_100_iters() {
        let costs = [vec![0.1, 0.9], vec![0.8, 0.2]];
        let (kernel, supply, demand) = prepare_problem(&[60.0, 40.0], &[45.0, 55.0], &costs, 0.1)
            .expect("valid transport problem");
        let (_, _, iterations) = scale_kernel(&kernel, &supply, &demand, 100);
        assert!(iterations < 100);
    }

    #[test]
    fn empty_demand_returns_empty_plan() {
        {
            let found = &sinkhorn_plan(&[100.0], &[], &[vec![]], 0.1, 100);
            assert!(found.is_empty(), "{found:?}");
        }
    }
}
