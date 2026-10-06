use super::types::BenchmarkResult;

pub(crate) fn format_terminal(result: &BenchmarkResult) -> String {
    let sep = "\u{2550}".repeat(72);
    let thin = "\u{2500}".repeat(72);
    let mut out = Vec::new();
    out.push(format!("  {sep}"));
    out.push(format!(
        "  Benchmark Result \u{2014} {} v{}",
        result.spec_id, result.spec_version
    ));
    out.push(format!("  {sep}"));
    out.push(format!(
        "  Agent: {}  |  Model: {}  |  Runtime: {}",
        result.agent, result.model, result.runtime_version
    ));
    out.push(format!("  Profile: {}", result.profile_hash));
    out.push(format!("  {thin}"));
    out.push(format!(
        "  {:<20} {:>6} {:>10} {:>10} {:>10}",
        "Task", "Pass", "Cost", "Quality", "Latency"
    ));
    out.push(format!("  {thin}"));
    for o in &result.outcomes {
        let status = if o.passed { "\u{2713}" } else { "\u{2717}" };
        out.push(format!(
            "  {:<20} {:>6} {:>9.4}$ {:>9.1}% {:>8}ms",
            &o.task_id[..o.task_id.len().min(20)],
            status,
            o.cost_usd,
            o.quality_score * 100.0,
            o.latency_ms
        ));
    }
    out.push(format!("  {thin}"));
    let s = &result.summary;
    let floor_status = if s.quality_floor_met {
        "PASS"
    } else {
        "FAILED"
    };
    out.push(format!(
        "  Pass rate: {:.1}%  |  Cost/task: ${:.4}  |  Quality: {:.1}%  |  Floor: {}",
        s.pass_rate * 100.0,
        s.cost_per_task_usd,
        s.mean_quality * 100.0,
        floor_status
    ));
    out.push(format!("  {sep}"));
    out.join("\n")
}

pub(crate) fn format_markdown(result: &BenchmarkResult) -> String {
    let mut out = Vec::new();
    out.push(format!(
        "# Benchmark: {} v{}\n",
        result.spec_id, result.spec_version
    ));
    out.push(format!("**Agent:** {}  ", result.agent));
    out.push(format!("**Model:** {}  ", result.model));
    out.push(format!("**Profile:** `{}`\n", result.profile_hash));
    out.push("| Task | Pass | Cost | Quality | Latency |".into());
    out.push("|---|---|---|---|---|".into());
    for o in &result.outcomes {
        let status = if o.passed { "\u{2713}" } else { "\u{2717}" };
        out.push(format!(
            "| {} | {} | ${:.4} | {:.1}% | {}ms |",
            o.task_id,
            status,
            o.cost_usd,
            o.quality_score * 100.0,
            o.latency_ms
        ));
    }
    let s = &result.summary;
    let floor = if s.quality_floor_met {
        "PASS"
    } else {
        "**FAILED**"
    };
    out.push(format!(
        "\n**Summary:** {:.1}% pass | ${:.4}/task | {:.1}% quality | Floor: {}",
        s.pass_rate * 100.0,
        s.cost_per_task_usd,
        s.mean_quality * 100.0,
        floor
    ));
    out.join("\n")
}

pub(crate) fn format_json(result: &BenchmarkResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::benchmark_spec::types::{
        BenchmarkEvaluation, BenchmarkOutcome, BenchmarkSummary,
    };

    fn sample_result() -> BenchmarkResult {
        let outcomes = vec![BenchmarkOutcome {
            task_id: "explore-repo".into(),
            passed: true,
            cost_usd: 0.143,
            quality_score: 0.962,
            latency_ms: 41000,
            tokens_input: 15000,
            tokens_output: 3000,
            error: None,
            evaluation: Some(BenchmarkEvaluation {
                evaluator_id: "qa-f1-v1".into(),
                metric: "f1".into(),
                score: 0.962,
                passed: true,
                detail: String::new(),
                output_digest: "digest".into(),
            }),
            execution_receipt_ref: None,
            capability_observation: None,
        }];
        let summary = BenchmarkSummary::from_outcomes(&outcomes, 0.95);
        BenchmarkResult {
            spec_id: "leanbench".into(),
            spec_version: "1.0.0".into(),
            profile_hash: "abc123".into(),
            agent: "codex".into(),
            model: "gpt-4".into(),
            runtime_version: "1.9.0".into(),
            outcomes,
            summary,
            completed_at: "2026-08-21T09:30:00Z".into(),
        }
    }

    #[test]
    fn terminal_contains_pass_rate() {
        let output = format_terminal(&sample_result());
        assert!(output.contains("PASS"));
    }

    #[test]
    fn markdown_contains_table() {
        let output = format_markdown(&sample_result());
        assert!(output.contains("| explore-repo"));
    }

    #[test]
    fn json_is_valid() {
        let output = format_json(&sample_result());
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["agent"], "codex");
        assert!(
            parsed["outcomes"][0]
                .get("capability_observation")
                .is_none()
        );
        let legacy: BenchmarkResult = serde_json::from_value(parsed.clone()).unwrap();
        assert!(legacy.outcomes[0].capability_observation.is_none());
        assert_eq!(serde_json::to_value(legacy).unwrap(), parsed);
    }

    #[test]
    fn json_preserves_capability_observation_without_provider_facts() {
        use crate::core::ocla::invocation::{
            CAPABILITY_OBSERVATION_SCHEMA_VERSION, CapabilityObservationV1,
        };
        let mut result = sample_result();
        let observation = CapabilityObservationV1 {
            schema_version: CAPABILITY_OBSERVATION_SCHEMA_VERSION,
            task_id: result.outcomes[0].task_id.clone(),
            capability_id: "test.agent".into(),
            capability_version: "1.0.0".into(),
            success: true,
            input_tokens: 3,
            output_tokens: 2,
            latency_ms: 47,
            failure_mode: None,
            output_ref: Some(crate::core::ocla::invocation::evidence_ref("task output")),
            metrics: std::collections::BTreeMap::from([(
                "local_payload_token_measurement".into(),
                1,
            )]),
        };
        result.outcomes[0].capability_observation = Some(observation.clone());
        let json = format_json(&result);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["outcomes"][0]["capability_observation"],
            serde_json::to_value(&observation).unwrap()
        );
        assert_eq!(
            parsed["outcomes"][0]["capability_observation"]["metrics"],
            serde_json::json!({"local_payload_token_measurement": 1})
        );
        let decoded: BenchmarkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(
            decoded.outcomes[0].capability_observation.as_ref(),
            Some(&observation)
        );
        assert!(decoded.outcomes[0].execution_receipt_ref.is_none());
        assert!(!decoded.summary.receipt_evidence_complete);
    }
}
