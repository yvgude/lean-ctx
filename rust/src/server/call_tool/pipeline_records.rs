// SPDX-License-Identifier: Apache-2.0

//! Post-dispatch persistence: checkpoints, context IR and the ledger.

#[allow(clippy::wildcard_imports)]
use super::*;

pub(in crate::server) async fn record_checkpoint(
    server: &LeanCtxServer,
    intent: Option<McpCheckpointIntent>,
    result: &mut CallToolResult,
) -> crate::core::execution_lifecycle::StageDisposition {
    let Some(intent) = intent else {
        return crate::core::execution_lifecycle::StageDisposition::Skipped(
            "terminal result has no checkpoint cadence",
        );
    };
    if !intent.enabled {
        return crate::core::execution_lifecycle::StageDisposition::Skipped(
            "checkpoint disabled for tool",
        );
    }
    if !server.increment_and_check() {
        return crate::core::execution_lifecycle::StageDisposition::Skipped("checkpoint not due");
    }
    let Some(checkpoint) = server.auto_checkpoint().await else {
        return crate::core::execution_lifecycle::StageDisposition::Skipped(
            "checkpoint unavailable",
        );
    };
    if intent.output_visible && crate::core::protocol::meta_visible() {
        if let Ok(checkpoint) = policy_guard::protect_result("ctx_session", &checkpoint) {
            result.content.push(ContentBlock::text(format!(
                "--- AUTO CHECKPOINT ---\n{checkpoint}"
            )));
        }
    }
    crate::core::execution_lifecycle::StageDisposition::Applied
}

pub(in crate::server) async fn record_context_ir(
    server: &LeanCtxServer,
    processed: &McpProcessed,
) -> crate::core::execution_lifecycle::StageDisposition {
    let (Some(ir), Some(intent)) = (&server.context_ir, processed.ir.as_ref()) else {
        return crate::core::execution_lifecycle::StageDisposition::Skipped(
            "Context IR unavailable for terminal or binary result",
        );
    };
    let input = crate::core::context_ir::RecordIrInput {
        kind: intent.kind.clone(),
        tool: &intent.tool,
        client_name: None,
        agent_id: None,
        path: intent.path.as_deref(),
        command: intent.command.as_deref(),
        pattern: intent.pattern.as_deref(),
        input_tokens: intent.input_tokens,
        output_tokens: intent.output_tokens,
        duration: intent.duration,
        content_excerpt: &intent.content_excerpt,
    };
    ir.write().await.record(input);
    crate::core::execution_lifecycle::StageDisposition::Applied
}

pub(in crate::server) async fn record_ledger(
    server: &LeanCtxServer,
    processed: &McpProcessed,
) -> Result<crate::core::execution_lifecycle::StageDisposition, ErrorData> {
    let mut applied = false;
    if let Some(intent) = processed.ledger.as_ref() {
        applied = true;
        let result = std::panic::AssertUnwindSafe(async {
            let active_task = {
                let session = server.session.read().await;
                session.task.as_ref().map(|task| task.description.clone())
            };
            let mut ledger = server.ledger.write().await;
            let overlay = crate::core::context_overlay::OverlayStore::load_project(
                &std::path::PathBuf::from(intent.project_root.as_deref().unwrap_or(".")),
            );
            let gate_result = context_gate::post_dispatch_record_with_task(
                &intent.read_path,
                &intent.mode_used,
                intent.output_tokens,
                intent.sent_tokens,
                &mut ledger,
                &overlay,
                active_task.as_deref(),
                intent.project_root.as_deref(),
            );
            let entry = ledger.read_entry(&intent.read_path);
            drop(ledger);
            crate::core::context_ledger::ContextLedger::persist_read(entry).await?;
            if let Some(hint) = &gate_result.eviction_hint {
                tracing::debug!("deferred eviction hint: {hint}");
            }
            if intent.wants_elicitation
                && let Some(hint) = &gate_result.elicitation_hint
            {
                tracing::debug!("deferred elicitation hint: {hint}");
            }
            if let Some(hint) = &gate_result.prefetch_hint {
                tracing::debug!("deferred FEP prefetch hint: {hint}");
            }
            if gate_result.resource_changed
                && let Some(peer) = server.peer.read().await.as_ref()
            {
                notifications::send_resource_updated(peer, notifications::RESOURCE_URI_SUMMARY)
                    .await;
            }
            Ok::<(), String>(())
        })
        .catch_unwind()
        .await;
        result
            .map_err(|_| ErrorData::internal_error("ledger write panicked", None))?
            .map_err(|error| ErrorData::internal_error(error, None))?;
    }
    if let Some(intent) = processed.receipt.as_ref() {
        applied = true;
        server
            .record_receipt_and_cost(
                &intent.name,
                intent.args.as_ref(),
                intent.action.as_deref(),
                &intent.result_text,
                intent.output_token_count,
            )
            .await;
    }
    if applied {
        Ok(crate::core::execution_lifecycle::StageDisposition::Applied)
    } else {
        Ok(crate::core::execution_lifecycle::StageDisposition::Skipped(
            "no ledger or receipt intent",
        ))
    }
}
