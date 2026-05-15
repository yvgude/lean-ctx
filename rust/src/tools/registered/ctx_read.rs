use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::server::tool_trait::{get_bool, get_int, get_str, McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxReadTool;

impl McpTool for CtxReadTool {
    fn name(&self) -> &'static str {
        "ctx_read"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_read",
            "Read file (cached, compressed). Cached re-reads can be ~13 tok when unchanged. Auto-selects optimal mode. \
Modes: full|map|signatures|diff|aggressive|entropy|task|reference|lines:N-M. fresh=true forces a disk re-read.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute file path to read" },
                    "mode": {
                        "type": "string",
                        "description": "Compression mode (default: full). Use 'map' for context-only files. For line ranges: 'lines:N-M' (e.g. 'lines:400-500')."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "Read from this line number to end of file. Implies fresh=true (disk re-read) to avoid stale snippets."
                    },
                    "fresh": {
                        "type": "boolean",
                        "description": "Bypass cache and force a full re-read. Use when running as a subagent that may not have the parent's context."
                    }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path = ctx
            .resolved_path("path")
            .ok_or_else(|| ErrorData::invalid_params("path is required", None))?
            .to_string();

        self.handle_inner(args, ctx, &path)
    }
}

impl CtxReadTool {
    #[allow(clippy::unused_self)]
    fn handle_inner(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
        path: &str,
    ) -> Result<ToolOutput, ErrorData> {
        let session_lock = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let cache_lock = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;

        let current_task = {
            let session = session_lock.blocking_read();
            session.task.as_ref().map(|t| t.description.clone())
        };
        let task_ref = current_task.as_deref();

        let profile = crate::core::profiles::active_profile();
        let mut mode = if let Some(m) = get_str(args, "mode") {
            m
        } else if profile.read.default_mode_effective() == "auto" {
            let cache = cache_lock.blocking_read();
            crate::tools::ctx_smart_read::select_mode_with_task(&cache, path, task_ref)
        } else {
            profile.read.default_mode_effective().to_string()
        };

        let mut fresh = get_bool(args, "fresh").unwrap_or(false);
        let start_line = get_int(args, "start_line");
        if let Some(sl) = start_line {
            let sl = sl.max(1_i64);
            mode = format!("lines:{sl}-999999");
            fresh = true;
        }

        let gate_result = crate::server::context_gate::pre_dispatch_read(
            path,
            &mode,
            task_ref,
            Some(&ctx.project_root),
        );
        if let Some(overridden) = gate_result.overridden_mode {
            mode = overridden;
        }

        let mode = if crate::tools::ctx_read::is_instruction_file(path) {
            "full".to_string()
        } else {
            auto_degrade_read_mode(&mode)
        };

        if mode.starts_with("lines:") {
            fresh = true;
        }

        if crate::core::binary_detect::is_binary_file(path) {
            let msg = crate::core::binary_detect::binary_file_message(path);
            return Err(ErrorData::invalid_params(msg, None));
        }
        {
            let cap = crate::core::limits::max_read_bytes() as u64;
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > cap {
                    let msg = format!(
                        "File too large ({} bytes, limit {} bytes via LCTX_MAX_READ_BYTES). \
                         Use mode=\"lines:1-100\" for partial reads or increase the limit.",
                        meta.len(),
                        cap
                    );
                    return Err(ErrorData::invalid_params(msg, None));
                }
            }
        }

        // Acquire cache write lock for minimal duration: read + extract, then drop
        let (output, resolved_mode, original, is_cache_hit, file_ref, cache_stats) = {
            let mut cache = cache_lock.blocking_write();
            let read_output = if fresh {
                crate::tools::ctx_read::handle_fresh_with_task_resolved(
                    &mut cache,
                    path,
                    &mode,
                    ctx.crp_mode,
                    task_ref,
                )
            } else {
                crate::tools::ctx_read::handle_with_task_resolved(
                    &mut cache,
                    path,
                    &mode,
                    ctx.crp_mode,
                    task_ref,
                )
            };
            let content = read_output.content;
            let rmode = read_output.resolved_mode;
            let orig = cache.get(path).map_or(0, |e| e.original_tokens);
            let hit = content.contains(" cached ");
            let fref = cache.file_ref_map().get(path).cloned();
            let stats = cache.get_stats();
            let stats_snapshot = (stats.total_reads, stats.cache_hits);
            (content, rmode, orig, hit, fref, stats_snapshot)
        };

        let output_tokens = crate::core::tokens::count_tokens(&output);
        let saved = original.saturating_sub(output_tokens);

        // Session updates (short lock)
        let mut ensured_root: Option<String> = None;
        let project_root_snapshot;
        {
            let mut session = session_lock.blocking_write();
            session.touch_file(path, file_ref.as_deref(), &resolved_mode, original);
            if is_cache_hit {
                session.record_cache_hit();
            }
            if session.active_structured_intent.is_none() && session.files_touched.len() >= 2 {
                let touched: Vec<String> = session
                    .files_touched
                    .iter()
                    .map(|f| f.path.clone())
                    .collect();
                let inferred =
                    crate::core::intent_engine::StructuredIntent::from_file_patterns(&touched);
                if inferred.confidence >= 0.4 {
                    session.active_structured_intent = Some(inferred);
                }
            }
            let root_missing = session
                .project_root
                .as_deref()
                .is_none_or(|r| r.trim().is_empty());
            if root_missing {
                if let Some(root) = crate::core::protocol::detect_project_root(path) {
                    session.project_root = Some(root.clone());
                    ensured_root = Some(root);
                }
            }
            project_root_snapshot = session
                .project_root
                .clone()
                .unwrap_or_else(|| ".".to_string());
        }

        if let Some(root) = ensured_root.as_deref() {
            crate::core::index_orchestrator::ensure_all_background(root);
        }

        crate::core::heatmap::record_file_access(path, original, saved);

        // Mode predictor + feedback — no locks needed, uses snapshots from above
        {
            let sig = crate::core::mode_predictor::FileSignature::from_path(path, original);
            let density = if output_tokens > 0 {
                original as f64 / output_tokens as f64
            } else {
                1.0
            };
            let outcome = crate::core::mode_predictor::ModeOutcome {
                mode: resolved_mode.clone(),
                tokens_in: original,
                tokens_out: output_tokens,
                density: density.min(1.0),
            };
            let mut predictor = crate::core::mode_predictor::ModePredictor::new();
            predictor.set_project_root(&project_root_snapshot);
            predictor.record(sig, outcome);
            predictor.save();

            let ext = std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string();
            let thresholds = crate::core::adaptive_thresholds::thresholds_for_path(path);
            let feedback_outcome = crate::core::feedback::CompressionOutcome {
                session_id: format!("{}", std::process::id()),
                language: ext,
                entropy_threshold: thresholds.bpe_entropy,
                jaccard_threshold: thresholds.jaccard,
                total_turns: cache_stats.0 as u32,
                tokens_saved: saved as u64,
                tokens_original: original as u64,
                cache_hits: cache_stats.1 as u32,
                total_reads: cache_stats.0 as u32,
                task_completed: true,
                timestamp: chrono::Local::now().to_rfc3339(),
            };
            let mut store = crate::core::feedback::FeedbackStore::load();
            store.project_root = Some(project_root_snapshot.clone());
            store.record_outcome(feedback_outcome);
        }

        // NOTE: pipeline_stats, context_ir, and ledger updates are handled by the
        // dispatch layer's record_call flow. Agent registration requires server.agent_id
        // which is not available in ToolContext; it will be added when ToolContext is
        // extended with the remaining server state fields.

        Ok(ToolOutput {
            text: output,
            original_tokens: original,
            saved_tokens: saved,
            mode: Some(resolved_mode),
            path: Some(path.to_string()),
        })
    }
}

fn auto_degrade_read_mode(mode: &str) -> String {
    use crate::core::degradation_policy::DegradationVerdictV1;
    let profile = crate::core::profiles::active_profile();
    if !profile.degradation.enforce_effective() {
        return mode.to_string();
    }
    let policy = crate::core::degradation_policy::evaluate_v1_for_tool("ctx_read", None);
    match policy.decision.verdict {
        DegradationVerdictV1::Ok => mode.to_string(),
        DegradationVerdictV1::Warn => match mode {
            "full" => "map".to_string(),
            other => other.to_string(),
        },
        DegradationVerdictV1::Throttle => match mode {
            "full" | "map" => "signatures".to_string(),
            other => other.to_string(),
        },
        DegradationVerdictV1::Block => "signatures".to_string(),
    }
}
