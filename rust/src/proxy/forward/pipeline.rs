use std::time::Instant;

use crate::proxy::prose_patterns::remove_seen_instruction_lines;
use axum::http::HeaderValue;
use serde_json::Value;
use std::collections::HashSet;

use crate::{
    core::{
        config::PipelineConfig,
        knowledge_router::ContextAdvice,
        tokens::{COUNTING_FAMILY, count_tokens_for},
    },
    proxy::{
        adaptive_policy::select_policy,
        dedup::ContentAddressedDedup,
        determinism_guard,
        effort_routing::score_complexity,
        live_zone::{compress_live_only, detect_live_zone},
        pre_optimize::classify_task,
        prose_compress::compress_prose,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageReport {
    pub name: &'static str,
    pub tokens_saved: u64,
    pub duration_us: u64,
    pub skipped: bool,
    ran: bool,
}

#[derive(Debug, Clone)]
pub struct PipelineReport {
    pub stages_run: Vec<StageReport>,
    pub total_tokens_before: u64,
    pub total_tokens_after: u64,
    pub total_savings_pct: f32,
    pub(crate) effort_complexity: Option<u8>,
}

impl PipelineReport {
    pub(crate) fn apply_effort_budget(&self, request: &mut Value) {
        if let Some(complexity) = self.effort_complexity {
            crate::proxy::effort_routing::apply_effort_budget(request, complexity);
        }
    }

    pub(crate) fn apply_response_headers(&self, headers: &mut axum::http::HeaderMap) {
        insert_header(
            headers,
            "x-leanctx-pipeline-stages",
            &self.headline_stage_names(),
        );
        insert_header(
            headers,
            "x-leanctx-total-savings",
            &format!("{:.0}%", self.total_savings_pct),
        );

        if let Some(fastest) = self.fastest_stage() {
            insert_header(
                headers,
                "x-leanctx-fastest-stage",
                &format!("{} ({}us)", fastest.name, fastest.duration_us),
            );
        }
    }

    fn headline_stage_names(&self) -> String {
        ["live_zone", "dedup", "prose", "effort"]
            .into_iter()
            .filter(|name| {
                self.stages_run
                    .iter()
                    .any(|stage| stage.name == *name && stage.ran)
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    fn fastest_stage(&self) -> Option<&StageReport> {
        self.stages_run
            .iter()
            .filter(|stage| stage.ran && !stage.skipped)
            .min_by_key(|stage| stage.duration_us)
    }
}

pub struct CompressionPipeline;

impl CompressionPipeline {
    pub fn run(messages: &mut Vec<Value>, config: &PipelineConfig) -> PipelineReport {
        Self::run_with_context_advice(messages, config, None)
    }

    /// Runs the normal pipeline while preserving message content identified by
    /// KnowledgeRouter as source-reference context.
    pub fn run_with_context_advice(
        messages: &mut Vec<Value>,
        config: &PipelineConfig,
        context_advice: Option<&ContextAdvice>,
    ) -> PipelineReport {
        let original_messages = messages.clone();
        let total_tokens_before = messages_tokens(messages);
        let mut stages_run = Vec::with_capacity(7);

        let started = Instant::now();
        let live_zone = detect_live_zone(messages);
        let mut live_messages = messages.split_off(live_zone.boundary_turn);
        stages_run.push(stage_report("live_zone", true, 0, started));

        let dedup_started = Instant::now();
        let dedup_saved = if config.enable_dedup {
            let before = messages_tokens(&live_messages);
            let mut dedup = ContentAddressedDedup::new();
            let _ = dedup.dedup_messages(&mut live_messages);
            before.saturating_sub(messages_tokens(&live_messages))
        } else {
            0
        };
        stages_run.push(stage_report(
            "dedup",
            config.enable_dedup,
            dedup_saved,
            dedup_started,
        ));

        let task_class = task_class(&live_messages);
        let policy_started = Instant::now();
        let policy = select_policy(task_class);
        stages_run.push(stage_report("adaptive_policy", true, 0, policy_started));

        let prose_started = Instant::now();
        let prose_saved = if config.enable_prose {
            let before = messages_tokens(&live_messages);
            compress_live_prose(&mut live_messages, task_class, context_advice);
            before.saturating_sub(messages_tokens(&live_messages))
        } else {
            0
        };
        stages_run.push(stage_report(
            "prose",
            config.enable_prose,
            prose_saved,
            prose_started,
        ));

        let tool_started = Instant::now();
        let tool_saved = if config.enable_prose {
            let before = messages_tokens(&live_messages);
            let _ = compress_live_only(&mut live_messages, COUNTING_FAMILY, policy);
            before.saturating_sub(messages_tokens(&live_messages))
        } else {
            0
        };
        stages_run.push(stage_report(
            "tool_results",
            config.enable_prose,
            tool_saved,
            tool_started,
        ));

        let effort_started = Instant::now();
        let effort_complexity = config.enable_effort.then(|| {
            let mut all_messages = messages.clone();
            all_messages.extend(live_messages.clone());
            score_complexity(&all_messages)
        });
        stages_run.push(stage_report(
            "effort",
            config.enable_effort,
            0,
            effort_started,
        ));

        messages.append(&mut live_messages);

        let determinism_started = Instant::now();
        let _proof = determinism_guard::verify_determinism(&original_messages, messages);
        stages_run.push(stage_report(
            "determinism_guard",
            true,
            0,
            determinism_started,
        ));

        let mut total_tokens_after = messages_tokens(messages);
        let mut total_savings_pct = savings_pct(total_tokens_before, total_tokens_after);
        let should_revert = total_tokens_before > 0 && total_savings_pct < config.min_savings_pct;
        if should_revert {
            *messages = original_messages;
            total_tokens_after = total_tokens_before;
            total_savings_pct = 0.0;
            for stage in &mut stages_run {
                if matches!(stage.name, "dedup" | "prose" | "tool_results") {
                    stage.tokens_saved = 0;
                    stage.skipped = true;
                }
            }
        }

        PipelineReport {
            stages_run,
            total_tokens_before,
            total_tokens_after,
            total_savings_pct,
            effort_complexity,
        }
    }
}

fn stage_report(name: &'static str, ran: bool, tokens_saved: u64, started: Instant) -> StageReport {
    StageReport {
        name,
        tokens_saved,
        duration_us: duration_us(started),
        skipped: !ran || tokens_saved == 0,
        ran,
    }
}

fn duration_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn messages_tokens(messages: &[Value]) -> u64 {
    messages
        .iter()
        .filter_map(|message| serde_json::to_string(message).ok())
        .map(|message| count_tokens_for(&message, COUNTING_FAMILY) as u64)
        .sum()
}

fn savings_pct(before: u64, after: u64) -> f32 {
    if before == 0 {
        0.0
    } else {
        (before.saturating_sub(after) as f32 / before as f32) * 100.0
    }
}

fn task_class(messages: &[Value]) -> &'static str {
    let text = messages.iter().rev().find_map(|message| {
        (message.get("role").and_then(Value::as_str) == Some("user"))
            .then(|| message.get("content").and_then(Value::as_str))
            .flatten()
    });
    classify_task(text)
}

fn compress_live_prose(
    messages: &mut [Value],
    task_class: &str,
    context_advice: Option<&ContextAdvice>,
) {
    let mut seen_instruction_lines = HashSet::new();
    for message in messages {
        if message.get("role").and_then(Value::as_str) == Some("tool") {
            continue;
        }

        if context_advice.is_some_and(|advice| {
            serde_json::to_string(message)
                .map(|serialized| advice.protects(&serialized))
                .unwrap_or(false)
        }) {
            continue;
        }

        let Some(content) = message.get_mut("content") else {
            continue;
        };

        match content {
            Value::String(text) => compress_text(text, task_class),
            Value::Array(blocks) => {
                for block in blocks {
                    let Some(block) = block.as_object_mut() else {
                        continue;
                    };
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(Value::String(text)) = block.get_mut("text") {
                            remove_seen_instruction_lines(text, &mut seen_instruction_lines);
                            compress_text(text, task_class);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn compress_text(text: &mut String, task_class: &str) {
    let result = compress_prose(text, Some(task_class));
    if result.compressed_tokens < result.original_tokens && result.compressed.len() < text.len() {
        *text = result.compressed;
    }
}

fn insert_header(headers: &mut axum::http::HeaderMap, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        axum::http::header::HeaderName::try_from(name),
        HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CompressionPipeline, PipelineConfig};

    fn verbose_text() -> String {
        (0..200)
            .map(|index| {
                format!(
                    "This repeated operational detail is not needed for the current task {index}."
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn pipeline_runs_all_stages_in_order() {
        let mut messages = vec![json!({"role": "user", "content": verbose_text()})];

        let report = CompressionPipeline::run(&mut messages, &PipelineConfig::default());

        assert_eq!(
            report
                .stages_run
                .iter()
                .map(|stage| stage.name)
                .collect::<Vec<_>>(),
            vec![
                "live_zone",
                "dedup",
                "adaptive_policy",
                "prose",
                "tool_results",
                "effort",
                "determinism_guard",
            ]
        );
    }

    #[test]
    fn disabled_stage_is_skipped() {
        let mut messages = vec![json!({"role": "user", "content": verbose_text()})];
        let config = PipelineConfig {
            enable_dedup: false,
            min_savings_pct: 0.0,
            ..PipelineConfig::default()
        };

        let report = CompressionPipeline::run(&mut messages, &config);

        let dedup = report
            .stages_run
            .iter()
            .find(|stage| stage.name == "dedup")
            .expect("dedup stage report");
        assert!(dedup.skipped);
        assert!(!dedup.ran);
    }

    #[test]
    fn report_accumulates_savings() {
        let repeated = verbose_text();
        let mut messages = vec![
            json!({"role": "user", "content": "Fix the bug"}),
            json!({"role": "assistant", "content": &repeated}),
            json!({"role": "user", "content": "Try again"}),
            json!({"role": "assistant", "content": &repeated}),
        ];
        let config = PipelineConfig {
            enable_prose: false,
            min_savings_pct: 0.0,
            ..PipelineConfig::default()
        };

        let report = CompressionPipeline::run(&mut messages, &config);

        assert!(
            report.total_savings_pct >= 0.0,
            "pipeline should not produce negative savings: {:.1}%",
            report.total_savings_pct
        );
        let stage_total: u64 = report
            .stages_run
            .iter()
            .map(|stage| stage.tokens_saved)
            .sum();
        assert_eq!(
            report.total_tokens_before - report.total_tokens_after,
            stage_total
        );
    }

    #[test]
    fn empty_messages_are_safe() {
        let mut messages = Vec::new();

        let report = CompressionPipeline::run(&mut messages, &PipelineConfig::default());

        assert!(messages.is_empty());
        assert_eq!(report.total_tokens_before, 0);
        assert_eq!(report.total_tokens_after, 0);
    }
}
