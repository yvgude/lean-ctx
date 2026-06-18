//! Post-dispatch side-effect stages for `LeanCtxServer::call_tool_guarded`
//! (issue #144).
//!
//! These blocks run *after* a tool produced its (already post-processed) output
//! and perform pure side effects — they never mutate the text returned to the
//! model. They are tightly coupled to `&self` (locks, peers, spawned tasks), so
//! they live as thin `LeanCtxServer` methods rather than free functions. Moving
//! them out of the guarded path keeps that function a readable orchestrator;
//! behaviour, ordering and await points are identical to the inlined versions.

#[allow(clippy::wildcard_imports)]
use super::*;

impl LeanCtxServer {
    /// Record the tool receipt, infer/apply intent, persist the session when due,
    /// trigger auto-consolidation, and attribute token cost. All work is either
    /// synchronous bookkeeping under the session lock or fire-and-forget blocking
    /// tasks; nothing here feeds back into the tool output.
    pub(super) async fn record_receipt_and_cost(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, serde_json::Value>>,
        action: Option<&str>,
        result_text: &str,
        output_token_count: usize,
    ) {
        let input = helpers::canonical_args_string(args);
        let input_md5 = helpers::hash_fast(&input);
        let output_md5 = helpers::hash_fast(result_text);
        let agent_id = self.agent_id.read().await.clone();
        let client_name = self.client_name.read().await.clone();
        let mut explicit_intent: Option<(
            crate::core::intent_protocol::IntentRecord,
            Option<String>,
            String,
        )> = None;

        let pending_session_save = {
            let empty_args = serde_json::Map::new();
            let args_map = args.unwrap_or(&empty_args);
            let mut session = self.session.write().await;
            session.record_tool_receipt(
                name,
                action,
                &input_md5,
                &output_md5,
                agent_id.as_deref(),
                Some(&client_name),
            );

            if let Some(intent) = crate::core::intent_protocol::infer_from_tool_call(
                name,
                action,
                args_map,
                session.project_root.as_deref(),
            ) {
                let is_explicit =
                    intent.source == crate::core::intent_protocol::IntentSource::Explicit;
                let root = session.project_root.clone();
                let sid = session.id.clone();
                session.record_intent(intent.clone());
                if is_explicit {
                    explicit_intent = Some((intent, root, sid));
                }
            }
            if session.should_save() {
                session.prepare_save().ok()
            } else {
                None
            }
        };

        if let Some(prepared) = pending_session_save {
            let ir_clone = self.context_ir.clone();
            tokio::task::spawn_blocking(move || {
                let _ = prepared.write_to_disk();
                if let Some(ir) = ir_clone
                    && let Ok(ir_guard) = ir.try_read()
                {
                    ir_guard.save();
                }
            });
        }

        if let Some((intent, root, session_id)) = explicit_intent {
            let _ = crate::core::intent_protocol::apply_side_effects(
                &intent,
                root.as_deref(),
                &session_id,
            );
        }

        if self.autonomy.is_enabled() {
            let (calls, project_root) = {
                let session = self.session.read().await;
                (session.stats.total_tool_calls, session.project_root.clone())
            };

            if let Some(root) = project_root
                && crate::tools::autonomy::should_auto_consolidate(&self.autonomy, calls)
            {
                let root_clone = root.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = crate::core::consolidation_engine::consolidate_latest(
                        &root_clone,
                        crate::core::consolidation_engine::ConsolidationBudgets::default(),
                    );
                });
            }
        }

        let agent_key = agent_id.unwrap_or_else(|| "unknown".to_string());
        let input_token_count = crate::core::tokens::count_tokens(&input) as u64;
        let output_token_count_u64 = output_token_count as u64;
        let name_owned = name.to_string();
        tokio::task::spawn_blocking(move || {
            let pricing = crate::core::gain::model_pricing::ModelPricing::load();
            // Honors a declared model for MCP-only IDEs (`[cost.models]`/default).
            let quote = pricing.quote_for_client(&client_name);
            let cost_usd = quote
                .cost
                .estimate_usd(input_token_count, output_token_count_u64, 0, 0);
            crate::core::budget_tracker::BudgetTracker::global().record_cost_usd(cost_usd);

            let mut store = crate::core::a2a::cost_attribution::CostStore::load();
            store.record_tool_call(
                &agent_key,
                &client_name,
                &name_owned,
                input_token_count,
                output_token_count_u64,
                0,
            );
            if let Err(e) = store.save() {
                tracing::warn!("lean-ctx: failed to persist cost attribution: {e}");
            }
        });
    }

    /// Context OS: persist the shared session snapshot and publish the matching
    /// bus events (primary `ToolCallRecorded` plus any secondary kind). No-op
    /// outside shared session mode. Fire-and-forget on a blocking task.
    pub(super) async fn persist_shared_context_os(
        &self,
        name: &str,
        action: Option<&str>,
        args: Option<&serde_json::Map<String, serde_json::Value>>,
    ) {
        if self.session_mode != crate::tools::SessionMode::Shared {
            return;
        }
        let ws = self.workspace_id.clone();
        let ch = self.channel_id.clone();
        let rt = self.context_os.clone();
        let agent = self.agent_id.read().await.clone();
        let tool = name.to_string();
        let tool_action = action.map(str::to_string);
        let tool_path = helpers::get_str(args, "path");
        let tool_category = helpers::get_str(args, "category");
        let tool_key = helpers::get_str(args, "key");
        let session_snapshot = self.session.read().await.clone();
        let session_task = session_snapshot.task.clone();
        tokio::task::spawn_blocking(move || {
            let Some(rt) = rt else {
                return;
            };
            let Some(root) = session_snapshot.project_root.as_deref() else {
                return;
            };
            rt.shared_sessions
                .persist_best_effort(root, &ws, &ch, &session_snapshot);
            rt.metrics.record_session_persisted();

            let mut base_payload = serde_json::json!({
                "tool": tool,
                "action": tool_action,
            });
            if let Some(ref p) = tool_path {
                base_payload["path"] = serde_json::Value::String(p.clone());
            }
            if let Some(ref c) = tool_category {
                base_payload["category"] = serde_json::Value::String(c.clone());
            }
            if let Some(ref k) = tool_key {
                base_payload["key"] = serde_json::Value::String(k.clone());
            }
            if let Some(ref t) = session_task {
                base_payload["reasoning"] = serde_json::Value::String(t.description.clone());
            }

            if rt
                .bus
                .append(
                    &ws,
                    &ch,
                    &crate::core::context_os::ContextEventKindV1::ToolCallRecorded,
                    agent.as_deref(),
                    base_payload.clone(),
                )
                .is_some()
            {
                rt.metrics.record_event_appended();
                rt.metrics.record_event_broadcast();
            }

            if let Some(secondary) =
                crate::core::context_os::secondary_event_kind(&tool, tool_action.as_deref())
                && rt
                    .bus
                    .append(&ws, &ch, &secondary, agent.as_deref(), base_payload)
                    .is_some()
            {
                rt.metrics.record_event_appended();
                rt.metrics.record_event_broadcast();
            }
        });
    }
}
