use crate::core::adaptive_mode_policy::AdaptiveModePolicyStore;
use crate::core::llm_feedback::{LlmFeedbackEvent, LlmFeedbackStore};

pub fn record(event: &LlmFeedbackEvent) -> Result<String, String> {
    LlmFeedbackStore::record(event.clone())?;
    let mut policy = AdaptiveModePolicyStore::load();
    policy.update_from_feedback(event);
    policy.save()?;
    Ok("feedback recorded".to_string())
}

pub fn status() -> String {
    let s = LlmFeedbackStore::status();
    format!(
        "ctx_feedback status\n path: {}\n bytes: {}\n retention: max_events={} max_bytes={}",
        s.path.display(),
        s.bytes,
        s.max_events,
        s.max_bytes
    )
}

pub fn report(limit: usize) -> String {
    let s = LlmFeedbackStore::summarize(limit);
    if s.total_events == 0 {
        return "No LLM feedback recorded yet.".to_string();
    }

    let mut lines = vec![
        format!("LLM Feedback Report (last {limit} events)"),
        format!(" total_events: {}", s.total_events),
        format!(" avg_output_ratio: {:.2}", s.avg_output_ratio),
        format!(" max_output_tokens: {}", s.max_output_tokens),
        format!(" max_output_ratio: {:.2}", s.max_output_ratio),
    ];
    if let Some(ms) = s.avg_latency_ms {
        lines.push(format!(" avg_latency_ms: {ms:.0}"));
    }

    if !s.by_model.is_empty() {
        lines.push(" by_model:".to_string());
        for (model, m) in s.by_model.iter().take(10) {
            let mut row = format!(
                "  {model}: n={} avg_ratio={:.2} max_out={}",
                m.events, m.avg_output_ratio, m.max_output_tokens
            );
            if let Some(ms) = m.avg_latency_ms {
                row.push_str(&format!(" avg_ms={ms:.0}"));
            }
            lines.push(row);
        }
    }

    lines.join("\n")
}

pub fn json(limit: usize) -> String {
    let status = LlmFeedbackStore::status();
    let summary = LlmFeedbackStore::summarize(limit);
    let recent = LlmFeedbackStore::recent(limit.min(200));
    serde_json::json!({
        "status": status,
        "summary": summary,
        "recent": recent,
    })
    .to_string()
}

pub fn reset() -> String {
    match (LlmFeedbackStore::reset(), AdaptiveModePolicyStore::reset()) {
        (Ok(()), Ok(())) => "LLM feedback has been reset.".to_string(),
        (a, b) => {
            let mut errs = Vec::new();
            if let Err(e) = a {
                errs.push(format!("feedback: {e}"));
            }
            if let Err(e) = b {
                errs.push(format!("policy: {e}"));
            }
            format!("Error resetting feedback: {}", errs.join("; "))
        }
    }
}
