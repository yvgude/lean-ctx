//! Live Effective Tokens Per Accepted Outcome (ETPAO) measurement.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::coverage_class::CoverageClass;

/// Token usage and client metadata for one request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMetrics {
    /// Tokens supplied as model input.
    pub input_tokens: usize,
    /// Tokens produced by the model.
    pub output_tokens: usize,
    /// Tokens consumed by model reasoning.
    pub reasoning_tokens: usize,
    /// Tokens consumed by tool or response schemas.
    pub schema_tokens: usize,
    /// Tokens written to the prompt cache.
    pub cache_write_tokens: usize,
    /// Number of retries associated with the request.
    pub retry_count: usize,
    /// Stable identifier for the originating client.
    pub client_id: String,
    /// Context-control coverage available for the request.
    pub coverage_class: CoverageClass,
}

/// Observed result of one client request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeMetrics {
    /// Whether the client accepted the result.
    pub accepted: bool,
    /// Normalized result quality reported by the evaluator.
    pub quality_score: f64,
    /// Whether the result succeeded without a retry.
    pub first_pass: bool,
    /// Stable identifier for the originating client.
    pub client_id: String,
}

/// Aggregate snapshot of live ETPAO measurements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EtpaoSummary {
    /// Tokens consumed per accepted outcome across all clients.
    pub etpao: f64,
    /// Total tokens consumed by all recorded requests.
    pub total_tokens: usize,
    /// Number of recorded accepted outcomes.
    pub accepted_outcomes: usize,
    /// Fraction of outcomes that succeeded on the first pass.
    pub first_pass_rate: f64,
    /// Percentage of request tokens attributable to retried requests.
    pub retry_tax_pct: f64,
    /// ETPAO grouped by the debug name of each coverage class.
    pub by_coverage_class: HashMap<String, f64>,
}

/// In-memory collector for live request and outcome measurements.
#[derive(Debug, Clone, Default)]
pub struct EtpaoLive {
    requests: Vec<RequestMetrics>,
    outcomes: Vec<OutcomeMetrics>,
}

impl EtpaoLive {
    /// Creates an empty live measurement collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records token usage for one request.
    pub fn record_request(&mut self, req: RequestMetrics) {
        self.requests.push(req);
    }

    /// Records the evaluated outcome of one request.
    pub fn record_outcome(&mut self, outcome: OutcomeMetrics) {
        self.outcomes.push(outcome);
    }

    /// Returns aggregate tokens per accepted outcome.
    pub fn current_etpao(&self) -> f64 {
        let accepted = self.accepted_outcomes();
        if accepted == 0 {
            return 0.0;
        }
        let tokens = self
            .requests
            .iter()
            .map(total_request_tokens)
            .sum::<usize>();
        tokens as f64 / accepted as f64
    }

    /// Returns tokens per accepted outcome for a client with recorded requests.
    pub fn etpao_for_client(&self, client_id: &str) -> Option<f64> {
        let mut requests = self
            .requests
            .iter()
            .filter(|req| req.client_id == client_id);
        let first = requests.next()?;
        let tokens = requests.fold(total_request_tokens(first), |total, req| {
            total.saturating_add(total_request_tokens(req))
        });
        let accepted = self
            .outcomes
            .iter()
            .filter(|outcome| outcome.client_id == client_id && outcome.accepted)
            .count();
        Some(if accepted == 0 {
            0.0
        } else {
            tokens as f64 / accepted as f64
        })
    }

    /// Returns the fraction of tokens consumed by requests with retries.
    pub fn retry_tax(&self) -> f64 {
        let total = self
            .requests
            .iter()
            .map(total_request_tokens)
            .sum::<usize>();
        if total == 0 {
            return 0.0;
        }
        let retried = self
            .requests
            .iter()
            .filter(|req| req.retry_count > 0)
            .map(total_request_tokens)
            .sum::<usize>();
        retried as f64 / total as f64
    }

    /// Builds an aggregate serializable measurement snapshot.
    pub fn summary(&self) -> EtpaoSummary {
        let total_tokens = self.requests.iter().map(total_request_tokens).sum();
        let accepted_outcomes = self.accepted_outcomes();
        let first_pass_rate = if self.outcomes.is_empty() {
            0.0
        } else {
            self.outcomes
                .iter()
                .filter(|outcome| outcome.first_pass)
                .count() as f64
                / self.outcomes.len() as f64
        };
        EtpaoSummary {
            etpao: self.current_etpao(),
            total_tokens,
            accepted_outcomes,
            first_pass_rate,
            retry_tax_pct: self.retry_tax() * 100.0,
            by_coverage_class: self.etpao_by_coverage_class(),
        }
    }

    /// Returns the number of recorded requests.
    pub fn request_count(&self) -> usize {
        self.requests.len()
    }

    /// Returns factual input and output token totals across recorded requests.
    pub fn request_token_totals(&self) -> (usize, usize) {
        self.requests.iter().fold((0, 0), |totals, request| {
            (
                totals.0.saturating_add(request.input_tokens),
                totals.1.saturating_add(request.output_tokens),
            )
        })
    }

    /// Returns the number of recorded outcomes.
    pub fn outcome_count(&self) -> usize {
        self.outcomes.len()
    }

    fn accepted_outcomes(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.accepted)
            .count()
    }

    fn etpao_by_coverage_class(&self) -> HashMap<String, f64> {
        let mut totals = HashMap::<CoverageClass, usize>::new();
        for request in &self.requests {
            let total = totals.entry(request.coverage_class).or_default();
            *total = total.saturating_add(total_request_tokens(request));
        }
        totals
            .into_iter()
            .map(|(class, tokens)| {
                let accepted = self
                    .outcomes
                    .iter()
                    .filter(|outcome| {
                        outcome.accepted
                            && self.requests.iter().any(|request| {
                                request.coverage_class == class
                                    && request.client_id == outcome.client_id
                            })
                    })
                    .count();
                let etpao = if accepted == 0 {
                    0.0
                } else {
                    tokens as f64 / accepted as f64
                };
                (format!("{class:?}"), etpao)
            })
            .collect()
    }
}

/// Returns all token categories accounted for by one request.
pub fn total_request_tokens(req: &RequestMetrics) -> usize {
    req.input_tokens
        .saturating_add(req.output_tokens)
        .saturating_add(req.reasoning_tokens)
        .saturating_add(req.schema_tokens)
        .saturating_add(req.cache_write_tokens)
}

#[cfg(test)]
#[rustfmt::skip]
pub mod tests {
    use super::{EtpaoLive, OutcomeMetrics, RequestMetrics};
    use crate::core::context_kernel::coverage_class::CoverageClass;

    fn request(client: &str, tokens: usize, class: CoverageClass) -> RequestMetrics {
        RequestMetrics { input_tokens: tokens, output_tokens: 0, reasoning_tokens: 0,
            schema_tokens: 0, cache_write_tokens: 0, retry_count: 0,
            client_id: client.to_owned(), coverage_class: class }
    }

    fn outcome(client: &str, accepted: bool, first_pass: bool) -> OutcomeMetrics {
        OutcomeMetrics { accepted, quality_score: 1.0, first_pass,
            client_id: client.to_owned() }
    }

    #[test]
    fn empty_etpao_is_zero() { assert_eq!(EtpaoLive::new().current_etpao(), 0.0); }

    #[test]
    fn request_without_evaluated_outcome_is_zero() { let mut live = EtpaoLive::new(); live.record_request(request("a", 100, CoverageClass::FullInline)); assert_eq!(live.current_etpao(), 0.0); assert_eq!(live.etpao_for_client("a"), Some(0.0)); assert_eq!(live.summary().by_coverage_class.get("FullInline"), Some(&0.0)); }

    #[test]
    fn single_request_single_outcome() { let mut live = EtpaoLive::new(); live.record_request(request("a", 1_000, CoverageClass::FullInline)); live.record_outcome(outcome("a", true, true)); assert_eq!(live.current_etpao(), 1_000.0); }

    #[test]
    fn retry_tax_computed() { let mut live = EtpaoLive::new(); for client in ["a", "b", "c"] { live.record_request(request(client, 100, CoverageClass::ContextControlled)); } live.requests[1].retry_count = 1; assert!((live.retry_tax() - 1.0 / 3.0).abs() < f64::EPSILON); }

    #[test]
    fn etpao_for_client_filters() { let mut live = EtpaoLive::new(); live.record_request(request("a", 100, CoverageClass::FullInline)); live.record_request(request("b", 250, CoverageClass::FullInline)); live.record_outcome(outcome("a", true, true)); live.record_outcome(outcome("b", true, true)); assert_eq!(live.etpao_for_client("a"), Some(100.0)); assert_eq!(live.etpao_for_client("b"), Some(250.0)); assert_eq!(live.etpao_for_client("missing"), None); }

    #[test]
    fn summary_has_all_fields() { let mut live = EtpaoLive::new(); live.record_request(request("a", 100, CoverageClass::FullInline)); live.record_outcome(outcome("a", true, true)); let summary = live.summary(); assert_eq!((summary.etpao, summary.total_tokens), (100.0, 100)); assert_eq!((summary.accepted_outcomes, summary.first_pass_rate), (1, 1.0)); assert_eq!(summary.retry_tax_pct, 0.0); assert_eq!((live.request_count(), live.outcome_count()), (1, 1)); }

    #[test]
    fn by_coverage_class_separation() { let mut live = EtpaoLive::new(); live.record_request(request("inline", 100, CoverageClass::FullInline)); live.record_request(request("controlled", 300, CoverageClass::ContextControlled)); live.record_outcome(outcome("inline", true, true)); live.record_outcome(outcome("controlled", true, true)); let by_class = live.summary().by_coverage_class; assert_eq!(by_class.get("FullInline"), Some(&100.0)); assert_eq!(by_class.get("ContextControlled"), Some(&300.0)); }

    #[test]
    fn first_pass_rate_correct() { let mut live = EtpaoLive::new(); for index in 0..10 { live.record_outcome(outcome("a", true, index < 7)); } assert_eq!(live.summary().first_pass_rate, 0.7); }
}
