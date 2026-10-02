//! How strong is a gain number? Economic evidence levels for gain reporting.
//!
//! Tool-output token reduction is not a provider bill reduction: the provider bill
//! depends on turns, cache writes and reads, retries and the whole trajectory. A gain
//! figure therefore carries the evidence it rests on, and a figure the data path
//! cannot observe is reported as unknown — never filled in from the gross number.

use serde::{Deserialize, Serialize};

/// Evidence behind an economic figure, ordered from weakest to strongest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EconomicEvidence {
    /// Raw vs. transformed token counts computed locally; no traffic observed.
    #[default]
    LocalEstimate,
    /// lean-ctx saw the tool traffic it transformed (hooks, MCP tools) but not the
    /// provider request path — the integrated mode of subscription clients.
    ObservedToolTraffic,
    /// The provider's own counter priced the original request shape. A counterfactual
    /// for one request, not a bill for an alternative trajectory.
    ProviderCountedInput,
    /// The proxy carried the treatment requests and saw their provider usage; the
    /// savings side is still an estimate against an unobserved baseline.
    ProviderMeasuredUsage,
    /// Paired control/treatment provider cost. The only level that supports an
    /// end-to-end bill-saving claim.
    PairedControl,
}

impl EconomicEvidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::LocalEstimate => "local estimate",
            Self::ObservedToolTraffic => "observed tool traffic (provider path not visible)",
            Self::ProviderCountedInput => "provider-counted input (counterfactual)",
            Self::ProviderMeasuredUsage => "provider-measured usage (savings estimated)",
            Self::PairedControl => "paired control/treatment",
        }
    }

    /// Whether a net provider-bill figure can be computed at all: only when the
    /// provider request path was observed.
    pub fn bill_impact_observable(self) -> bool {
        self >= Self::ProviderMeasuredUsage
    }
}

/// The economic side of a gain summary, with unobservable figures left as `None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EconomicView {
    pub evidence: EconomicEvidence,
    pub net_bill_impact_tokens: Option<i64>,
    pub net_bill_impact_usd: Option<f64>,
    pub roi: Option<f64>,
}

/// Classify what was observed and keep only the figures the data path supports.
///
/// `provider_turns` is the number of provider requests the proxy carried; `0` means
/// the proxy is not in the request path (for example a Claude Code subscription,
/// where only hooks and MCP tools pass through lean-ctx). Then the net bill impact
/// and ROI are unknown — not the gross savings, and not zero.
pub fn economic_view(
    observed_commands: u64,
    provider_turns: u64,
    net_tokens_if_observed: i64,
    net_usd_if_observed: f64,
    tool_spend_usd: f64,
) -> EconomicView {
    let evidence = if provider_turns > 0 {
        EconomicEvidence::ProviderMeasuredUsage
    } else if observed_commands > 0 {
        EconomicEvidence::ObservedToolTraffic
    } else {
        EconomicEvidence::LocalEstimate
    };
    let observable = evidence.bill_impact_observable();
    EconomicView {
        evidence,
        net_bill_impact_tokens: observable.then_some(net_tokens_if_observed),
        net_bill_impact_usd: observable.then_some(net_usd_if_observed),
        roi: (observable && tool_spend_usd > 0.0).then(|| net_usd_if_observed / tool_spend_usd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrated_mode_never_reports_a_bill_impact_or_roi() {
        // Claude Code subscription: thousands of observed tool calls, no provider turn.
        let v = economic_view(4_200, 0, 1_500_000, 18.75, 3.0);
        assert_eq!(v.evidence, EconomicEvidence::ObservedToolTraffic);
        assert_eq!(
            v.net_bill_impact_tokens, None,
            "net must not fall back to gross"
        );
        assert_eq!(v.net_bill_impact_usd, None);
        assert_eq!(v.roi, None);
    }

    #[test]
    fn gateway_mode_reports_an_estimated_net_and_roi() {
        let v = economic_view(4_200, 900, -12_000, -0.4, 2.0);
        assert_eq!(v.evidence, EconomicEvidence::ProviderMeasuredUsage);
        assert_eq!(
            v.net_bill_impact_tokens,
            Some(-12_000),
            "a negative net is reported"
        );
        assert_eq!(v.roi, Some(-0.2));
        assert_ne!(
            v.evidence,
            EconomicEvidence::PairedControl,
            "measured usage is still not a paired bill claim"
        );
    }

    #[test]
    fn nothing_observed_is_a_local_estimate() {
        let v = economic_view(0, 0, 0, 0.0, 0.0);
        assert_eq!(v.evidence, EconomicEvidence::LocalEstimate);
        assert_eq!(v.net_bill_impact_usd, None);
    }
}
