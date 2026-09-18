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
        prose_compress::{CompressionStrategy, ProseCompressor},
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
        // #1545: this proof used to be bound to `_proof` and dropped — a
        // live-zone disagreement with the forwarder's guard then shipped a
        // mutated body over the wire unchecked (observed as upstream 400s and
        // silent cache busts). Fail closed like the forwarder: an unstable
        // proof reverts every compression stage below.
        let proof = determinism_guard::verify_determinism(&original_messages, messages);
        let determinism_unstable = !proof.is_stable;
        if determinism_unstable {
            tracing::warn!("lean-ctx pipeline determinism violation: reverting compression stages");
        }
        stages_run.push(stage_report(
            "determinism_guard",
            true,
            0,
            determinism_started,
        ));

        // #1789: a stage that emptied a turn must revert, however good its
        // savings look. Checked alongside the determinism proof because neither
        // that proof nor the savings floor below can observe this failure mode.
        let content_destroyed = destroys_content(&original_messages, messages);
        if content_destroyed {
            tracing::warn!(
                "lean-ctx pipeline: compression emptied a message that had content; \
                 reverting compression stages"
            );
        }

        let mut total_tokens_after = messages_tokens(messages);
        let mut total_savings_pct = savings_pct(total_tokens_before, total_tokens_after);
        let should_revert = determinism_unstable
            || content_destroyed
            || (total_tokens_before > 0 && total_savings_pct < config.min_savings_pct);
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

/// Compress one conversation turn.
///
/// #1789: this deliberately pins the strategy to [`CompressionStrategy::Light`]
/// instead of letting `ProseCompressor::new` infer `Aggressive` from the
/// presence of a task hint. `Aggressive` *drops* whole sections that carry no
/// task/technical keyword — sound for a document, where the surviving sections
/// still carry the content, but a chat turn is a single paragraph, so dropping
/// its only section deletes the message. `Light` keeps every section and still
/// applies the lossless pattern pass, which is the only squeeze a conversation
/// turn may safely receive. Tool output keeps its section-dropping compressor
/// in the `tool_results` stage, where whole-message deletion is impossible.
fn compress_text(text: &mut String, task_class: &str) {
    let result = ProseCompressor::new(Some(task_class))
        .with_strategy(CompressionStrategy::Light)
        .compress(text);
    if result.compressed_tokens < result.original_tokens
        && result.compressed.len() < text.len()
        && !result.compressed.trim().is_empty()
    {
        *text = result.compressed;
    }
}

/// All text a message carries, across both wire shapes: a plain string
/// (`content: "…"`, OpenAI) and a block array (`content: [{type:"text",…}]`,
/// Anthropic).
fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// True when compression emptied a message that arrived with text (#1789).
///
/// This is a *content* invariant, deliberately separate from the determinism
/// guard's *cache-prefix* invariant: emptying every live turn leaves the frozen
/// prefix byte-identical, so the determinism proof stays stable while the
/// request loses everything the model was supposed to read. Savings-based
/// reverting cannot catch it either — deleting all content scores as a perfect
/// ~100% saving and sails past `min_savings_pct`.
fn destroys_content(before: &[Value], after: &[Value]) -> bool {
    before.len() == after.len()
        && before.iter().zip(after).any(|(before, after)| {
            !message_text(before).trim().is_empty() && message_text(after).trim().is_empty()
        })
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

    // #1545: the determinism proof is enforcement now, not telemetry — a
    // stage that rewrites content inside the client's cache_control'd prefix
    // is reverted, so the wire body keeps the prefix byte-identical.
    #[test]
    fn cache_controlled_prefix_survives_pipeline_byte_identical() {
        let repeated = verbose_text();
        let mut messages = vec![
            json!({"role": "user", "cache_control": {"type": "ephemeral"}, "content": &repeated}),
            json!({"role": "assistant", "content": &repeated}),
            json!({"role": "user", "content": "Try again"}),
        ];
        let before_prefix = messages[0].to_string();
        let config = PipelineConfig {
            min_savings_pct: 0.0,
            ..PipelineConfig::default()
        };

        let _ = CompressionPipeline::run(&mut messages, &config);

        assert_eq!(
            messages[0].to_string(),
            before_prefix,
            "cache_control'd prefix must never ship mutated"
        );
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

    /// #1789: the proxy emptied every non-system turn on provider routes.
    ///
    /// The prose stage runs `ProseCompressor` with a task hint, which selects
    /// `CompressionStrategy::Aggressive`. That strategy drops any paragraph
    /// without a hardcoded English technical keyword — and a chat turn *is* a
    /// single paragraph, so dropping it deletes the whole message. The system
    /// turn survived only because it sits in the frozen prefix.
    #[test]
    fn conversation_turns_never_lose_their_content() {
        let mut messages = vec![
            json!({"role": "system",    "content": "Tu es un assistant francais."}),
            json!({"role": "user",      "content": "Bonjour, quelle est la capitale de la France ?"}),
            json!({"role": "assistant", "content": "La capitale de la France est Paris."}),
            json!({"role": "user",      "content": "Et combien d habitants environ ?"}),
        ];

        CompressionPipeline::run(&mut messages, &PipelineConfig::default());

        for message in &messages {
            let role = message["role"].as_str().unwrap_or_default();
            let content = message["content"].as_str().unwrap_or_default();
            assert!(
                !content.trim().is_empty(),
                "#1789: `{role}` turn was emptied by the compression pipeline; \
                 upstream would receive a conversation the model cannot see"
            );
        }
    }
}
