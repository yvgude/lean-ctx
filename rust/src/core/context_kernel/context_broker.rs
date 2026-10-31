//! Client-adaptive token budget for a client context window.

use super::client_profile::ClientEfficiencyProfile;

/// Token allocation computed for a client context window.
#[derive(Debug, Clone, Copy)]
pub struct BrokerBudget {
    /// Tokens allocated to request context.
    pub context_tokens: usize,
    /// Tokens allocated to kernel instructions.
    pub kernel_tokens: usize,
    /// Tokens allocated to tool schemas.
    pub schema_tokens: usize,
}

/// Computes the context budget according to client efficiency constraints.
pub struct ContextBroker {
    profile: ClientEfficiencyProfile,
}

impl ContextBroker {
    /// Creates a broker for a client efficiency profile.
    pub fn new(profile: ClientEfficiencyProfile) -> Self {
        Self { profile }
    }

    /// Splits the context window 70/10/20 between context, kernel, and schemas.
    pub fn compute_budget(&self) -> BrokerBudget {
        let window = self.profile.context_window;
        BrokerBudget {
            context_tokens: window.saturating_mul(70) / 100,
            kernel_tokens: window.saturating_mul(10) / 100,
            schema_tokens: window.saturating_mul(20) / 100,
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::ContextBroker;
    use crate::core::context_kernel::client_profile::ProfileBuilder;

    #[test]
    fn budget_split_proportional() {
        let profile = ProfileBuilder::new("broker-test")
            .context_window(100_000)
            .build();
        let budget = ContextBroker::new(profile).compute_budget();
        assert_eq!(budget.context_tokens, 70_000);
        assert_eq!(budget.kernel_tokens, 10_000);
        assert_eq!(budget.schema_tokens, 20_000);
    }
}
