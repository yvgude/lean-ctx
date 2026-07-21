use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxPreloadTool;

impl McpTool for CtxPreloadTool {
    fn name(&self) -> &'static str {
        "ctx_preload"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_preload",
            "Caches task-relevant files, returns L-curve-optimized summary.\n\
             WORKFLOW: call at session start or when switching tasks, before ctx_read.\n\
             ANTIPATTERN: not for reading individual files — use ctx_read instead.\n\
             ~50-100 tokens vs ~5000 for individual reads (~50x savings).",
            json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Task description (short English)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Project root"
                    }
                },
                "required": ["task"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let task = get_str(args, "task").unwrap_or_default();

        let resolved_path = if get_str(args, "path").is_some() {
            if let Some(p) = ctx.resolved_path("path") {
                Some(p.to_string())
            } else if let Some(err) = ctx.path_error("path") {
                return Err(ErrorData::invalid_params(format!("path: {err}"), None));
            } else {
                None
            }
        } else if let Some(ref session) = ctx.session {
            let guard = crate::server::bounded_lock::read(session, "ctx_preload:session_root");
            guard.as_ref().and_then(|g| g.project_root.clone())
        } else {
            None
        };

        // Never let `handle` fall back to "." (the daemon CWD, which is not the
        // project): resolve against the dispatch-provided root so graph-relative
        // preload candidates (e.g. `rust/src/core/foo.rs`) jail against the real
        // project root in every IDE, even when no explicit `path` was passed.
        let resolved_path = resolved_path.or_else(|| {
            let root = ctx.project_root.trim();
            (!root.is_empty()).then(|| root.to_string())
        });

        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(mut cache_guard) = crate::server::bounded_lock::write(cache, "ctx_preload:cache")
        else {
            return Ok(ToolOutput::simple(
                "[preload skipped — cache temporarily unavailable]".to_string(),
            ));
        };
        let (mut result, _usable) = crate::tools::ctx_preload::handle(
            &mut cache_guard,
            &task,
            resolved_path.as_deref(),
            ctx.crp_mode,
        );

        let provider_hints = predict_and_prefetch(&task, &mut cache_guard, &ctx.project_root);
        if !provider_hints.is_empty() {
            result.push_str(&provider_hints);
        }

        drop(cache_guard);

        if let Some(ref session_lock) = ctx.session {
            if let Some(mut session_guard) =
                crate::server::bounded_lock::write(session_lock, "ctx_preload:session_write")
                && (session_guard.active_structured_intent.is_none()
                    || session_guard
                        .active_structured_intent
                        .as_ref()
                        .is_none_or(|i| i.confidence < 0.6))
            {
                session_guard.set_task(&task, Some("preload"));
            }

            if let Some(session_guard) =
                crate::server::bounded_lock::read(session_lock, "ctx_preload:session_read")
                && let Some(ref intent) = session_guard.active_structured_intent
                && let Some(ref ledger_lock) = ctx.ledger
            {
                let Some(ledger) =
                    crate::server::bounded_lock::read(ledger_lock, "ctx_preload:ledger")
                else {
                    return Ok(ToolOutput::simple(result));
                };
                if !ledger.entries.is_empty() {
                    let known: Vec<String> = session_guard
                        .files_touched
                        .iter()
                        .map(|f| f.path.clone())
                        .collect();
                    let deficit =
                        crate::core::context_deficit::detect_deficit(&ledger, intent, &known);
                    if !deficit.suggested_files.is_empty() {
                        result.push_str("\n\n--- SUGGESTED FILES ---");
                        for s in &deficit.suggested_files {
                            result.push_str(&format!(
                                "\n  {} ({:?}, ~{} tok, mode: {})",
                                s.path, s.reason, s.estimated_tokens, s.recommended_mode
                            ));
                        }
                    }

                    let pressure = ledger.pressure();
                    if pressure.utilization > 0.7 {
                        let plan = ledger.reinjection_plan(intent, 0.6);
                        if !plan.actions.is_empty() {
                            result.push_str("\n\n--- REINJECTION PLAN ---");
                            result.push_str(&format!(
                                "\n  Context pressure: {:.0}% -> target: 60%",
                                pressure.utilization * 100.0
                            ));
                            for a in &plan.actions {
                                result.push_str(&format!(
                                    "\n  {} : {} -> {} (frees ~{} tokens)",
                                    a.path, a.current_mode, a.new_mode, a.tokens_freed
                                ));
                            }
                            result.push_str(&format!(
                                "\n  Total freeable: {} tokens",
                                plan.total_tokens_freed
                            ));
                        }
                    }
                }
            }
        }

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some("preload".to_string()),
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}

/// Use Active Inference to predict useful provider data and prefetch it.
/// Stores results in session cache (synchronous) and triggers deep
/// indexing (BM25, Graph, Knowledge) in a background thread when
/// `providers.auto_index` is enabled.
fn predict_and_prefetch(
    task: &str,
    cache: &mut crate::core::cache::SessionCache,
    project_root: &str,
) -> String {
    crate::core::providers::init::init_with_project_root(Some(std::path::Path::new(project_root)));
    let registry = crate::core::providers::registry::global_registry();
    let available = registry.available_provider_ids();
    if available.is_empty() {
        return String::new();
    }

    let mut bandit = crate::core::provider_bandit::ProviderBandit::load(project_root);
    let predictions =
        crate::core::active_inference::predict_preloads(task, &available, &mut bandit, 2);

    if predictions.is_empty() {
        return String::new();
    }
    let task_type = crate::core::active_inference::infer_task_type(&task.to_lowercase());

    let cfg = crate::core::config::Config::load();
    let auto_index = cfg.providers.auto_index;
    let mut all_artifacts = Vec::new();

    let mut out = String::from("\n\n--- PROVIDER PRELOAD ---");
    let mut prefetched = 0usize;

    for pred in &predictions {
        let params = crate::core::providers::provider_trait::ProviderParams {
            limit: Some(5),
            ..Default::default()
        };

        match registry.execute_as_chunks(&pred.provider_id, &pred.action, &params) {
            Ok(chunks) => {
                // Active-inference feedback: a provider that actually returned
                // context for this task type is a positive prediction error.
                bandit.update(&task_type, &pred.provider_id, !chunks.is_empty());
                let artifacts = crate::core::consolidation::consolidate(&chunks);
                for entry in &artifacts.cache_entries {
                    cache.store(&entry.uri, &entry.content);
                    prefetched += 1;
                }
                if auto_index && !artifacts.is_empty() {
                    all_artifacts.push(artifacts);
                }
                out.push_str(&format!(
                    "\n  {} {} → {} items cached (confidence: {:.0}%)",
                    pred.provider_id,
                    pred.action,
                    chunks.len(),
                    pred.confidence * 100.0,
                ));
            }
            Err(e) => {
                // A failed/empty provider is a negative prediction error — learn
                // not to bet on it for this task type next time.
                bandit.update(&task_type, &pred.provider_id, false);
                tracing::debug!(
                    "[preload] provider {}/{} failed: {e}",
                    pred.provider_id,
                    pred.action,
                );
            }
        }
    }

    // Persist the learning even when nothing prefetched — negative outcomes are
    // exactly what we want the bandit to remember.
    let _ = bandit.save(project_root);

    if prefetched == 0 {
        return String::new();
    }

    if !all_artifacts.is_empty() {
        let root = project_root.to_string();
        std::thread::spawn(move || {
            let merged = merge_preload_artifacts(&all_artifacts);
            crate::tools::ctx_provider::apply_artifacts_to_stores(&merged, &root);
        });
    }

    out
}

fn merge_preload_artifacts(
    all: &[crate::core::consolidation::ConsolidationArtifacts],
) -> crate::core::consolidation::ConsolidationArtifacts {
    let mut merged = crate::core::consolidation::ConsolidationArtifacts::default();
    for a in all {
        merged.bm25_chunks.extend(a.bm25_chunks.clone());
        merged.edges.extend(a.edges.clone());
        merged.facts.extend(a.facts.clone());
        merged.cache_entries.extend(a.cache_entries.clone());
    }
    merged
}
