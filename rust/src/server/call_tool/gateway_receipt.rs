// SPDX-License-Identifier: Apache-2.0
//! One Context Gateway Decision Receipt per tool call (G4).
//!
//! After the final result exists, the call's admission decisions become a
//! validated receipt that binds the returned bytes. The receipt is persisted
//! content-addressed, and its security actions are recorded once through the
//! signed audit trail, which is where the status line and `lean-ctx value`
//! take their numbers from. A receipt that fails to persist is never claimed:
//! `lean-ctx inspect` only shows stored receipts.

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::core::context_admission::capture::{AdmissionCapture, CallIdentity, Delivered};
use crate::core::context_admission::receipt_store;

fn delivered_text(result: &Result<CallToolResult, ErrorData>) -> (String, bool) {
    match result {
        Ok(result) => {
            let text = result
                .content
                .iter()
                .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            (text, result.is_error == Some(true))
        }
        Err(error) => (error.message.to_string(), true),
    }
}

impl LeanCtxServer {
    pub(super) async fn finish_gateway_receipt(
        &self,
        capture: std::sync::Arc<AdmissionCapture>,
        result: &Result<CallToolResult, ErrorData>,
        tool: &str,
    ) {
        if capture.is_empty() {
            return;
        }
        let (text, is_error) = delivered_text(result);
        let agent_id = self.agent_id.read().await.clone();
        let project_root = self
            .session
            .read()
            .await
            .project_root
            .clone()
            .unwrap_or_else(|| ".".to_owned());
        let tool = tool.to_owned();
        let recorded = tokio::task::spawn_blocking(move || {
            let delivered = Delivered {
                tokens: crate::core::tokens::count_tokens(&text) as u64,
                text: &text,
                is_error,
            };
            let identity = CallIdentity {
                agent_id: agent_id.as_deref(),
                destination: None,
            };
            let receipt =
                crate::core::context_admission::capture::finish(&capture, &identity, &delivered)?;
            let tally = capture.security_tally();
            let audit_agent = agent_id.as_deref().unwrap_or("unknown");
            crate::core::security_events::record(&tool, audit_agent, &tally);
            match receipt_store::persist(&receipt, &project_root, capture.task_scope().as_deref()) {
                Ok(digest) => {
                    // The digest enters the signed, hash-chained audit trail:
                    // a stored receipt can later be checked against it.
                    if !tally.is_empty() {
                        crate::core::security_events::anchor_receipt(
                            &tool,
                            audit_agent,
                            digest.hex(),
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!("context gateway receipt not persisted: {error}");
                }
            }
            Some(!tally.is_empty())
        })
        .await
        .ok()
        .flatten();
        if recorded == Some(true) {
            self.publish_value_snapshot().await;
        }
    }
}
